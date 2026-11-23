// sitegen: a native port of makefun-site 1.0.0's build(input), for the "Build a static site faster" bounty.
// Reads $MAKEFUN_INPUT_DIR/cases.json (default /input), writes $MAKEFUN_OUTPUT_DIR/answers.json (default /output).
// Strings are kept as WTF-8 bytes so lone surrogates from JSON escapes survive; every JS rule the reference leans on
// (UTF-16 code-unit order, JS \s, `.` excluding line terminators, NFKD slugs) is reproduced explicitly.
//
// Speed notes: one prefaulted mmap region holds the input, a bump arena (nothing is ever freed) and the output; JSON
// strings are decoded in place in the input buffer and borrowed everywhere; every string a template can print is kept
// already JSON-escaped ("J-form"), so pages are rendered straight into the answers.json buffer; musl's byte-loop
// memcpy/memcmp are replaced; the process ends with _exit (exit_group) right after one write.

#![no_main]
#![allow(static_mut_refs)]
#![allow(clippy::missing_safety_doc)]

mod slugtab;

use core::arch::x86_64::*;
use core::ffi::{c_char, c_void};
use std::alloc::{GlobalAlloc, Layout};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

type B = Vec<u8>;
type S = &'static [u8];

// ================================================================== libc, memory

extern "C" {
    fn getenv(n: *const c_char) -> *const c_char;
    fn open(p: *const c_char, f: i32, ...) -> i32;
    fn pread(fd: i32, b: *mut c_void, n: usize, off: i64) -> isize;
    fn write(fd: i32, b: *const c_void, n: usize) -> isize;
    fn lseek(fd: i32, off: i64, wh: i32) -> i64;
    fn mmap(a: *mut c_void, l: usize, p: i32, f: i32, fd: i32, o: i64) -> *mut c_void;
    fn _exit(c: i32) -> !;
    fn strlen(p: *const c_char) -> usize;
}

const PROT_RW: i32 = 3;
const MAP_PRIVATE_ANON: i32 = 0x22;
const MAP_FIXED: i32 = 0x10;
const MAP_NORESERVE: i32 = 0x4000;
const MAP_POPULATE: i32 = 0x8000;

#[inline(always)]
unsafe fn cpy(d: *mut u8, s: *const u8, n: usize) {
    if n <= 16 {
        if n >= 8 {
            let a = (s as *const u64).read_unaligned();
            let b = (s.add(n - 8) as *const u64).read_unaligned();
            (d as *mut u64).write_unaligned(a);
            (d.add(n - 8) as *mut u64).write_unaligned(b);
        } else if n >= 4 {
            let a = (s as *const u32).read_unaligned();
            let b = (s.add(n - 4) as *const u32).read_unaligned();
            (d as *mut u32).write_unaligned(a);
            (d.add(n - 4) as *mut u32).write_unaligned(b);
        } else if n > 0 {
            let a = *s;
            let m = *s.add(n >> 1);
            let b = *s.add(n - 1);
            *d = a;
            *d.add(n >> 1) = m;
            *d.add(n - 1) = b;
        }
        return;
    }
    if n <= 32 {
        let a = _mm_loadu_si128(s as *const __m128i);
        let b = _mm_loadu_si128(s.add(n - 16) as *const __m128i);
        _mm_storeu_si128(d as *mut __m128i, a);
        _mm_storeu_si128(d.add(n - 16) as *mut __m128i, b);
        return;
    }
    if n <= 64 {
        let a = _mm_loadu_si128(s as *const __m128i);
        let b = _mm_loadu_si128(s.add(16) as *const __m128i);
        let c = _mm_loadu_si128(s.add(n - 32) as *const __m128i);
        let e = _mm_loadu_si128(s.add(n - 16) as *const __m128i);
        _mm_storeu_si128(d as *mut __m128i, a);
        _mm_storeu_si128(d.add(16) as *mut __m128i, b);
        _mm_storeu_si128(d.add(n - 32) as *mut __m128i, c);
        _mm_storeu_si128(d.add(n - 16) as *mut __m128i, e);
        return;
    }
    core::arch::asm!("rep movsb", inout("rcx") n => _, inout("rdi") d => _, inout("rsi") s => _, options(nostack, preserves_flags));
}

#[no_mangle]
pub unsafe extern "C" fn memcpy(d: *mut c_void, s: *const c_void, n: usize) -> *mut c_void {
    cpy(d as *mut u8, s as *const u8, n);
    d
}

#[no_mangle]
pub unsafe extern "C" fn memmove(dv: *mut c_void, sv: *const c_void, n: usize) -> *mut c_void {
    let (d, s) = (dv as *mut u8, sv as *const u8);
    // the small paths load everything before storing, so they are overlap-safe
    if n <= 64 || (d as usize).wrapping_sub(s as usize) >= n {
        cpy(d, s, n);
    } else {
        core::arch::asm!("std", "rep movsb", "cld", inout("rcx") n => _, inout("rdi") d.add(n - 1) => _, inout("rsi") s.add(n - 1) => _, options(nostack));
    }
    dv
}

#[no_mangle]
pub unsafe extern "C" fn memcmp(av: *const c_void, bv: *const c_void, n: usize) -> i32 {
    let (a, b) = (av as *const u8, bv as *const u8);
    let mut i = 0;
    while i + 8 <= n {
        let x = (a.add(i) as *const u64).read_unaligned();
        let y = (b.add(i) as *const u64).read_unaligned();
        if x != y {
            return if x.swap_bytes() < y.swap_bytes() { -1 } else { 1 };
        }
        i += 8;
    }
    while i < n {
        let (x, y) = (*a.add(i), *b.add(i));
        if x != y {
            return x as i32 - y as i32;
        }
        i += 1;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn bcmp(av: *const c_void, bv: *const c_void, n: usize) -> i32 {
    let (a, b) = (av as *const u8, bv as *const u8);
    if n < 8 {
        let mut i = 0;
        while i < n {
            if *a.add(i) != *b.add(i) {
                return 1;
            }
            i += 1;
        }
        return 0;
    }
    let mut i = 0;
    while i + 8 < n {
        if (a.add(i) as *const u64).read_unaligned() != (b.add(i) as *const u64).read_unaligned() {
            return 1;
        }
        i += 8;
    }
    ((a.add(n - 8) as *const u64).read_unaligned() != (b.add(n - 8) as *const u64).read_unaligned()) as i32
}

static mut CUR: usize = 0;
static mut END: usize = 0;
static mut LAST: usize = 0;

struct Bump;

#[cold]
#[inline(never)]
unsafe fn arena_grow(size: usize, align: usize) -> *mut u8 {
    let sz = (size + align + (64 << 20)) & !4095;
    let p = mmap(core::ptr::null_mut(), sz, PROT_RW, MAP_PRIVATE_ANON | MAP_NORESERVE, -1, 0) as *mut u8;
    if p as isize == -1 {
        _exit(71);
    }
    let q = (p as usize + align - 1) & !(align - 1);
    CUR = q + size;
    END = p as usize + sz;
    LAST = q;
    q as *mut u8
}

unsafe impl GlobalAlloc for Bump {
    #[inline(always)]
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let a = l.align();
        let p = (CUR + a - 1) & !(a - 1);
        let e = p + l.size();
        if e > END {
            return arena_grow(l.size(), a);
        }
        CUR = e;
        LAST = p;
        p as *mut u8
    }
    #[inline(always)]
    unsafe fn dealloc(&self, _p: *mut u8, _l: Layout) {}
    #[inline]
    unsafe fn realloc(&self, p: *mut u8, l: Layout, ns: usize) -> *mut u8 {
        if p as usize == LAST && LAST + ns <= END {
            CUR = LAST + ns;
            return p;
        }
        if ns <= l.size() {
            return p;
        }
        let q = self.alloc(Layout::from_size_align_unchecked(ns, l.align()));
        cpy(q, p, l.size());
        q
    }
}

#[global_allocator]
static GA: Bump = Bump;

#[inline(always)]
fn leak(v: B) -> S {
    // nothing is ever freed, so a Vec's buffer lives for the whole run
    v.leak()
}

#[inline(always)]
unsafe fn sl(p: *const u8, n: usize) -> S {
    core::slice::from_raw_parts(p, n)
}

// ================================================================== hashing

#[derive(Default, Clone, Copy)]
struct Fx(u64);
const FXK: u64 = 0xf1357aea2e62a9c5;
impl Hasher for Fx {
    #[inline]
    fn write(&mut self, b: &[u8]) {
        let mut h = self.0;
        let mut i = 0;
        while i + 8 <= b.len() {
            h = (h ^ u64::from_le_bytes(b[i..i + 8].try_into().unwrap())).wrapping_mul(FXK);
            i += 8;
        }
        if i < b.len() {
            let mut t = 0u64;
            for (k, &c) in b[i..].iter().enumerate() {
                t |= (c as u64) << (k * 8);
            }
            h = (h ^ t).wrapping_mul(FXK);
        }
        self.0 = h;
    }
    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.0 = (self.0 ^ i as u64).wrapping_mul(FXK);
    }
    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.0 = (self.0 ^ i as u64).wrapping_mul(FXK);
    }
    #[inline]
    fn finish(&self) -> u64 {
        self.0.rotate_left(26)
    }
}
type Fxb = BuildHasherDefault<Fx>;
type HMap<K, V> = HashMap<K, V, Fxb>;
type HSet<K> = HashSet<K, Fxb>;

// ================================================================== SSE2 scanners

#[inline(always)]
unsafe fn ld(p: *const u8) -> __m128i {
    _mm_loadu_si128(p as *const __m128i)
}
#[inline(always)]
unsafe fn eqc(v: __m128i, c: u8) -> __m128i {
    _mm_cmpeq_epi8(v, _mm_set1_epi8(c as i8))
}
#[inline(always)]
unsafe fn ctrl(v: __m128i) -> __m128i {
    // v <= 0x1F (unsigned)
    _mm_cmpeq_epi8(_mm_min_epu8(v, _mm_set1_epi8(0x1F)), v)
}

macro_rules! scanner {
    ($name:ident, |$v:ident| $sse:expr, |$c:ident| $sc:expr) => {
        #[inline(always)]
        fn $name(s: &[u8], mut i: usize) -> usize {
            let n = s.len();
            unsafe {
                let p = s.as_ptr();
                while i + 16 <= n {
                    let $v = ld(p.add(i));
                    let m = _mm_movemask_epi8($sse) as u32;
                    if m != 0 {
                        return i + m.trailing_zeros() as usize;
                    }
                    i += 16;
                }
                if i < n && (p.add(i) as usize & 4095) <= 4096 - 16 {
                    // the 16 bytes stay inside the page holding s[i], so the load cannot fault; bytes past the end
                    // are masked off
                    let $v = ld(p.add(i));
                    let m = _mm_movemask_epi8($sse) as u32 & ((1u32 << (n - i)) - 1);
                    return if m != 0 { i + m.trailing_zeros() as usize } else { n };
                }
            }
            while i < n {
                let $c = s[i];
                if $sc {
                    return i;
                }
                i += 1;
            }
            n
        }
    };
}

// a byte JSON-escaping must look at
scanner!(scan_j, |v| _mm_or_si128(_mm_or_si128(eqc(v, b'"'), eqc(v, b'\\')), _mm_or_si128(ctrl(v), eqc(v, 0xED))), |c| c < 0x20
    || c == b'"'
    || c == b'\\'
    || c == 0xED);
// a byte HTML+JSON-escaping must look at
scanner!(
    scan_hj,
    |v| _mm_or_si128(
        _mm_or_si128(_mm_or_si128(eqc(v, b'"'), eqc(v, b'\\')), _mm_or_si128(ctrl(v), eqc(v, 0xED))),
        _mm_or_si128(_mm_or_si128(eqc(v, b'&'), eqc(v, b'<')), _mm_or_si128(eqc(v, b'>'), eqc(v, b'\'')))
    ),
    |c| c < 0x20 || matches!(c, b'"' | b'\\' | 0xED | b'&' | b'<' | b'>' | b'\'')
);
// a byte HTML-escaping a J-form string must look at
scanner!(
    scan_ej,
    |v| _mm_or_si128(_mm_or_si128(eqc(v, b'\\'), eqc(v, b'&')), _mm_or_si128(_mm_or_si128(eqc(v, b'<'), eqc(v, b'>')), eqc(v, b'\''))),
    |c| matches!(c, b'\\' | b'&' | b'<' | b'>' | b'\'')
);
// a JSON string's end or escape
scanner!(scan_q, |v| _mm_or_si128(eqc(v, b'"'), eqc(v, b'\\')), |c| c == b'"' || c == b'\\');
// an inline-markdown special or a byte HTML+JSON-escaping must look at
scanner!(
    scan_hji,
    |v| _mm_or_si128(
        _mm_or_si128(
            _mm_or_si128(_mm_or_si128(eqc(v, b'"'), eqc(v, b'\\')), _mm_or_si128(ctrl(v), eqc(v, 0xED))),
            _mm_or_si128(_mm_or_si128(eqc(v, b'&'), eqc(v, b'<')), _mm_or_si128(eqc(v, b'>'), eqc(v, b'\'')))
        ),
        _mm_or_si128(_mm_or_si128(eqc(v, b'`'), eqc(v, b'*')), eqc(v, b'['))
    ),
    |c| c < 0x20 || matches!(c, b'"' | b'\\' | 0xED | b'&' | b'<' | b'>' | b'\'' | b'`' | b'*' | b'[')
);
scanner!(scan_cr, |v| eqc(v, b'\r'), |c| c == b'\r');
// a possible line terminator: \n, \r or the lead byte of U+2028/U+2029
scanner!(scan_lt, |v| _mm_or_si128(_mm_or_si128(eqc(v, b'\n'), eqc(v, b'\r')), eqc(v, 0xE2)), |c| c == b'\n' || c == b'\r' || c == 0xE2);

/// First index >= i holding byte a or byte b (s.len() if none).
#[inline(always)]
fn scan2(s: &[u8], mut i: usize, a: u8, b: u8) -> usize {
    let n = s.len();
    unsafe {
        let p = s.as_ptr();
        let va = _mm_set1_epi8(a as i8);
        let vb = _mm_set1_epi8(b as i8);
        while i + 16 <= n {
            let v = ld(p.add(i));
            let m = _mm_movemask_epi8(_mm_or_si128(_mm_cmpeq_epi8(v, va), _mm_cmpeq_epi8(v, vb))) as u32;
            if m != 0 {
                return i + m.trailing_zeros() as usize;
            }
            i += 16;
        }
        if i < n && (p.add(i) as usize & 4095) <= 4096 - 16 {
            let v = ld(p.add(i));
            let m = _mm_movemask_epi8(_mm_or_si128(_mm_cmpeq_epi8(v, va), _mm_cmpeq_epi8(v, vb))) as u32 & ((1u32 << (n - i)) - 1);
            return if m != 0 { i + m.trailing_zeros() as usize } else { n };
        }
    }
    while i < n {
        if s[i] == a || s[i] == b {
            return i;
        }
        i += 1;
    }
    n
}

fn memchr(c: u8, s: &[u8], from: usize) -> Option<usize> {
    if from > s.len() {
        return None;
    }
    let j = scan2(s, from, c, c);
    if j < s.len() {
        Some(j)
    } else {
        None
    }
}

/// Appends a short slice with one or two 16-byte moves when that is safe (spare capacity behind `out`, and the
/// source load stays inside the page of its first byte); longer or unsafe cases use extend_from_slice.
#[inline(always)]
fn app(out: &mut B, s: &[u8]) {
    let n = s.len();
    if n == 0 {
        // an empty slice may carry a dangling pointer
        return;
    }
    let len = out.len();
    let sp = s.as_ptr();
    unsafe {
        if n <= 16 && out.capacity() - len >= 16 && (sp as usize & 4095) <= 4096 - 16 {
            _mm_storeu_si128(out.as_mut_ptr().add(len) as *mut __m128i, ld(sp));
            out.set_len(len + n);
            return;
        }
        if n <= 32 && out.capacity() - len >= 32 && (sp as usize & 4095) <= 4096 - 32 {
            let d = out.as_mut_ptr().add(len);
            _mm_storeu_si128(d as *mut __m128i, ld(sp));
            _mm_storeu_si128(d.add(16) as *mut __m128i, ld(sp.add(16)));
            out.set_len(len + n);
            return;
        }
    }
    out.extend_from_slice(s);
}

// ================================================================== JSON input (decoded in place)

enum J {
    Null,
    Bool,
    Num(f64),
    Str(S),
    Arr(Vec<J>),
    Obj(Vec<(S, J)>),
}

impl J {
    fn get(&self, k: &[u8]) -> Option<&J> {
        if let J::Obj(v) = self {
            // last duplicate wins, as in JSON.parse
            for (kk, vv) in v.iter().rev() {
                if *kk == k {
                    return Some(vv);
                }
            }
        }
        None
    }
    fn s(&self) -> S {
        match self {
            J::Str(s) => s,
            _ => b"",
        }
    }
    fn n(&self) -> f64 {
        match self {
            J::Num(n) => *n,
            _ => 0.0,
        }
    }
}

/// Parser over a mutable, zero-padded buffer: strings are unescaped in place (never longer than their source).
struct P {
    s: *mut u8,
    n: usize,
    i: usize,
}


unsafe fn put_cp(d: *mut u8, cp: u32) -> usize {
    if cp < 0x80 {
        *d = cp as u8;
        1
    } else if cp < 0x800 {
        *d = 0xC0 | (cp >> 6) as u8;
        *d.add(1) = 0x80 | (cp & 0x3F) as u8;
        2
    } else if cp < 0x10000 {
        *d = 0xE0 | (cp >> 12) as u8;
        *d.add(1) = 0x80 | ((cp >> 6) & 0x3F) as u8;
        *d.add(2) = 0x80 | (cp & 0x3F) as u8;
        3
    } else {
        *d = 0xF0 | (cp >> 18) as u8;
        *d.add(1) = 0x80 | ((cp >> 12) & 0x3F) as u8;
        *d.add(2) = 0x80 | ((cp >> 6) & 0x3F) as u8;
        *d.add(3) = 0x80 | (cp & 0x3F) as u8;
        4
    }
}


impl P {
    #[inline(always)]
    fn at(&self, k: usize) -> u8 {
        unsafe { *self.s.add(k) }
    }
    #[inline(always)]
    fn ws(&mut self) {
        while matches!(self.at(self.i), b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }
    fn hex4(&self, k: usize) -> u32 {
        let mut v = 0u32;
        for j in 0..4 {
            let c = self.at(k + j);
            v = v * 16
                + match c {
                    b'0'..=b'9' => (c - b'0') as u32,
                    b'a'..=b'f' => (c - b'a' + 10) as u32,
                    b'A'..=b'F' => (c - b'A' + 10) as u32,
                    _ => 0,
                };
        }
        v
    }
    fn string(&mut self) -> S {
        self.i += 1;
        let st = self.i;
        let all = unsafe { sl(self.s, self.n) };
        let mut r = scan_q(all, st);
        if r >= self.n || self.at(r) == b'"' {
            self.i = r + 1;
            return unsafe { sl(self.s.add(st), r.min(self.n) - st) };
        }
        let mut w = r;
        loop {
            let c = self.at(r);
            if c == b'"' {
                self.i = r + 1;
                return unsafe { sl(self.s.add(st), w - st) };
            }
            if c != b'\\' && r - w >= 16 {
                // the write position trails by 16+ bytes, so whole 16-byte blocks can be moved while scanning:
                // every store lands on bytes already consumed (the buffer is zero-padded past the input)
                unsafe {
                    let q = _mm_set1_epi8(b'"' as i8);
                    let bs = _mm_set1_epi8(b'\\' as i8);
                    loop {
                        let v = ld(self.s.add(r));
                        _mm_storeu_si128(self.s.add(w) as *mut __m128i, v);
                        let m = _mm_movemask_epi8(_mm_or_si128(_mm_cmpeq_epi8(v, q), _mm_cmpeq_epi8(v, bs))) as u32;
                        if m != 0 {
                            let k = m.trailing_zeros() as usize;
                            r += k;
                            w += k;
                            break;
                        }
                        r += 16;
                        w += 16;
                        if r >= self.n {
                            break;
                        }
                    }
                }
                if r >= self.n {
                    let e = w - (r - self.n);
                    self.i = self.n;
                    return unsafe { sl(self.s.add(st), e - st) };
                }
                continue;
            }
            if c != b'\\' {
                let k = scan_q(all, r).min(self.n);
                unsafe { core::ptr::copy(self.s.add(r), self.s.add(w), k - r) };
                w += k - r;
                r = k;
                if r >= self.n {
                    self.i = r;
                    return unsafe { sl(self.s.add(st), w - st) };
                }
                continue;
            }
            let e = self.at(r + 1);
            r += 2;
            let b = match e {
                b'n' => b'\n',
                b't' => b'\t',
                b'r' => b'\r',
                b'b' => 8,
                b'f' => 12,
                b'u' => {
                    let mut cp = self.hex4(r);
                    r += 4;
                    if (0xD800..0xDC00).contains(&cp) && self.at(r) == b'\\' && self.at(r + 1) == b'u' {
                        let lo = self.hex4(r + 2);
                        if (0xDC00..0xE000).contains(&lo) {
                            cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                            r += 6;
                        }
                    }
                    w += unsafe { put_cp(self.s.add(w), cp) };
                    continue;
                }
                other => other,
            };
            unsafe { *self.s.add(w) = b };
            w += 1;
        }
    }
    fn val(&mut self) -> J {
        self.ws();
        let c = self.at(self.i);
        match c {
            b'{' => {
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.at(self.i) == b'}' {
                    self.i += 1;
                    return J::Obj(v);
                }
                loop {
                    self.ws();
                    if self.i >= self.n {
                        return J::Obj(v);
                    }
                    let k = self.string();
                    self.ws();
                    self.i += 1; // :
                    let x = self.val();
                    v.push((k, x));
                    self.ws();
                    let d = self.at(self.i);
                    self.i += 1;
                    if d != b',' {
                        return J::Obj(v);
                    }
                }
            }
            b'[' => {
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.at(self.i) == b']' {
                    self.i += 1;
                    return J::Arr(v);
                }
                loop {
                    if self.i >= self.n {
                        return J::Arr(v);
                    }
                    v.push(self.val());
                    self.ws();
                    let d = self.at(self.i);
                    self.i += 1;
                    if d != b',' {
                        return J::Arr(v);
                    }
                }
            }
            b'"' => J::Str(self.string()),
            b't' => {
                self.i += 4;
                J::Bool
            }
            b'f' => {
                self.i += 5;
                J::Bool
            }
            b'n' => {
                self.i += 4;
                J::Null
            }
            _ => {
                let st = self.i;
                while matches!(self.at(self.i), b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                    self.i += 1;
                }
                if self.i == st {
                    self.i += 1;
                    return J::Null;
                }
                let t = std::str::from_utf8(unsafe { sl(self.s.add(st), self.i - st) }).unwrap_or("0");
                J::Num(t.parse::<f64>().unwrap_or(0.0))
            }
        }
    }
}

// ================================================================== text helpers

#[inline]
fn decode(s: &[u8], i: usize) -> (u32, usize) {
    let c = s[i];
    if c < 0x80 {
        (c as u32, 1)
    } else if c < 0xE0 && i + 1 < s.len() {
        ((((c & 0x1F) as u32) << 6) | (s[i + 1] & 0x3F) as u32, 2)
    } else if c < 0xF0 && i + 2 < s.len() {
        ((((c & 0x0F) as u32) << 12) | (((s[i + 1] & 0x3F) as u32) << 6) | (s[i + 2] & 0x3F) as u32, 3)
    } else if i + 3 < s.len() {
        (
            (((c & 0x07) as u32) << 18) | (((s[i + 1] & 0x3F) as u32) << 12) | (((s[i + 2] & 0x3F) as u32) << 6) | (s[i + 3] & 0x3F) as u32,
            4,
        )
    } else {
        (c as u32, 1)
    }
}

#[inline]
fn is_ws(cp: u32) -> bool {
    matches!(cp, 9..=13 | 0x20 | 0xA0 | 0x1680 | 0x2000..=0x200A | 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000 | 0xFEFF)
}

fn char_start_back(s: &[u8], mut k: usize, lo: usize) -> usize {
    while k > lo && (s[k] & 0xC0) == 0x80 {
        k -= 1;
    }
    k
}

/// String.prototype.trim
fn js_trim(s: &[u8]) -> &[u8] {
    let mut a = 0;
    while a < s.len() {
        let (cp, l) = decode(s, a);
        if !is_ws(cp) {
            break;
        }
        a += l;
    }
    let mut b = s.len();
    while b > a {
        let k = char_start_back(s, b - 1, a);
        let (cp, _) = decode(s, k);
        if !is_ws(cp) {
            break;
        }
        b = k;
    }
    &s[a..b]
}

fn has_ws(s: &[u8]) -> bool {
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        if c < 0x80 {
            if matches!(c, 9..=13 | 0x20) {
                return true;
            }
            i += 1;
            continue;
        }
        let (cp, l) = decode(s, i);
        if is_ws(cp) {
            return true;
        }
        i += l;
    }
    false
}

/// Whether `s` holds a character `.` does not match (\n \r U+2028 U+2029).
fn has_line_term(s: &[u8]) -> bool {
    let mut i = 0;
    loop {
        i = scan_lt(s, i);
        if i >= s.len() {
            return false;
        }
        let c = s[i];
        if c == b'\n' || c == b'\r' {
            return true;
        }
        if i + 2 < s.len() && s[i + 1] == 0x80 && (s[i + 2] == 0xA8 || s[i + 2] == 0xA9) {
            return true;
        }
        i += 1;
    }
}

#[inline]
fn all_sp_tab(s: &[u8]) -> bool {
    s.iter().all(|&c| c == b' ' || c == b'\t')
}

#[inline]
fn trim_sp_tab(s: &[u8]) -> &[u8] {
    let mut a = 0;
    let mut b = s.len();
    while a < b && (s[a] == b' ' || s[a] == b'\t') {
        a += 1;
    }
    while b > a && (s[b - 1] == b' ' || s[b - 1] == b'\t') {
        b -= 1;
    }
    &s[a..b]
}

fn find(s: &[u8], what: &[u8], from: usize) -> Option<usize> {
    if what.len() == 1 {
        return memchr(what[0], s, from);
    }
    if from > s.len() {
        return None;
    }
    let mut j = from;
    while j + what.len() <= s.len() {
        if s[j] == what[0] && &s[j..j + what.len()] == what {
            return Some(j);
        }
        j += 1;
    }
    None
}

const HEX: &[u8; 16] = b"0123456789abcdef";

#[inline(never)]
fn push_u(out: &mut B, cp: u32) {
    out.extend_from_slice(&[
        b'\\',
        b'u',
        HEX[(cp >> 12 & 15) as usize],
        HEX[(cp >> 8 & 15) as usize],
        HEX[(cp >> 4 & 15) as usize],
        HEX[(cp & 15) as usize],
    ]);
}

/// The JSON escape of byte `s[j]` (one of scan_j's hits); returns the next index.
#[inline(always)]
fn json_one(out: &mut B, s: &[u8], j: usize) -> usize {
    let c = s[j];
    match c {
        b'"' => out.extend_from_slice(b"\\\""),
        b'\\' => out.extend_from_slice(b"\\\\"),
        b'\n' => out.extend_from_slice(b"\\n"),
        b'\t' => out.extend_from_slice(b"\\t"),
        b'\r' => out.extend_from_slice(b"\\r"),
        0xED => {
            if j + 1 < s.len() && s[j + 1] >= 0xA0 {
                let (cp, l) = decode(s, j);
                push_u(out, cp);
                return j + l;
            }
            out.push(0xED);
        }
        _ => push_u(out, c as u32),
    }
    j + 1
}

/// JSON string body escape (plain -> J-form).
fn esc_j(out: &mut B, s: &[u8]) {
    let mut i = 0;
    loop {
        let j = scan_j(s, i);
        app(out, &s[i..j]);
        if j >= s.len() {
            return;
        }
        i = json_one(out, s, j);
    }
}

/// The HTML+JSON escape of byte `s[j]` (one of scan_hj's hits); returns the next index.
#[inline(always)]
fn hj_one(out: &mut B, s: &[u8], j: usize) -> usize {
    let r: &[u8] = match s[j] {
        b'&' => b"&amp;",
        b'<' => b"&lt;",
        b'>' => b"&gt;",
        b'"' => b"&quot;",
        b'\'' => b"&#39;",
        _ => return json_one(out, s, j),
    };
    app(out, r);
    j + 1
}

/// HTML escape then JSON escape (plain -> J-form of the escaped HTML).
fn esc_hj(out: &mut B, s: &[u8]) {
    let mut i = 0;
    loop {
        let j = scan_hj(s, i);
        app(out, &s[i..j]);
        if j >= s.len() {
            return;
        }
        let r: &[u8] = match s[j] {
            b'&' => b"&amp;",
            b'<' => b"&lt;",
            b'>' => b"&gt;",
            b'"' => b"&quot;",
            b'\'' => b"&#39;",
            _ => {
                i = json_one(out, s, j);
                continue;
            }
        };
        app(out, r);
        i = j + 1;
    }
}

/// HTML escape of a J-form string, kept in J-form: J(E(x)) from J(x).
fn esc_ej(out: &mut B, s: &[u8]) {
    let mut i = 0;
    loop {
        let j = scan_ej(s, i);
        app(out, &s[i..j]);
        if j >= s.len() {
            return;
        }
        let r: &[u8] = match s[j] {
            b'&' => b"&amp;",
            b'<' => b"&lt;",
            b'>' => b"&gt;",
            b'\'' => b"&#39;",
            _ => {
                // a JSON escape: \" is a quote, anything else passes through whole
                let d = s[j + 1];
                if d == b'"' {
                    out.extend_from_slice(b"&quot;");
                } else {
                    out.extend_from_slice(&[b'\\', d]);
                }
                i = j + 2;
                continue;
            }
        };
        app(out, r);
        i = j + 1;
    }
}

/// J-form of a plain string (borrowed when nothing needs escaping).
fn jf(s: S) -> S {
    if scan_j(s, 0) == s.len() {
        return s;
    }
    let mut o = Vec::with_capacity(s.len() + 16);
    esc_j(&mut o, s);
    leak(o)
}

fn push_num(out: &mut B, n: i64) {
    let mut t = [0u8; 24];
    let mut k = t.len();
    let neg = n < 0;
    let mut v = n.unsigned_abs();
    loop {
        k -= 1;
        t[k] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    if neg {
        k -= 1;
        t[k] = b'-';
    }
    out.extend_from_slice(&t[k..]);
}

fn slug_token(cp: u32) -> &'static [u8] {
    let r = slugtab::RANGES;
    let (mut lo, mut hi) = (0usize, r.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        let e = r[mid];
        if cp < e.0 {
            hi = mid;
        } else if cp > e.1 {
            lo = mid + 1;
        } else {
            return &slugtab::BLOB[e.2 as usize..e.2 as usize + e.3 as usize];
        }
    }
    b"-"
}

fn slugify(s: &[u8]) -> S {
    let mut out: B = Vec::with_capacity(s.len().min(72) + 8);
    let mut gap = false;
    let mut i = 0;
    #[inline(always)]
    fn feed(c: u8, out: &mut B, gap: &mut bool) {
        if c == b'-' {
            *gap = !out.is_empty();
        } else {
            if *gap {
                out.push(b'-');
            }
            *gap = false;
            out.push(c);
        }
    }
    while i < s.len() {
        let c = s[i];
        if c < 0x80 {
            let k = if c.is_ascii_uppercase() {
                c + 32
            } else if c.is_ascii_lowercase() || c.is_ascii_digit() {
                c
            } else {
                b'-'
            };
            feed(k, &mut out, &mut gap);
            i += 1;
        } else {
            let (cp, l) = decode(s, i);
            for &k in slug_token(cp) {
                feed(k, &mut out, &mut gap);
            }
            i += l;
        }
    }
    out.truncate(64);
    while out.last() == Some(&b'-') {
        out.pop();
    }
    if out.is_empty() {
        return b"untitled";
    }
    leak(out)
}

fn unique_slug(slug: S, taken: &mut HSet<S>) -> S {
    if taken.insert(slug) {
        return slug;
    }
    let mut n = 2i64;
    loop {
        let mut c = Vec::with_capacity(slug.len() + 4);
        c.extend_from_slice(slug);
        c.push(b'-');
        push_num(&mut c, n);
        let c = leak(c);
        if taken.insert(c) {
            return c;
        }
        n += 1;
    }
}

const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

fn long_date(d: &[u8]) -> S {
    let day = (d[8] - b'0') as i64 * 10 + (d[9] - b'0') as i64;
    let m = ((d[5] - b'0') * 10 + (d[6] - b'0')) as usize;
    let mut o = Vec::with_capacity(20);
    push_num(&mut o, day);
    o.push(b' ');
    o.extend_from_slice(MONTHS[m - 1].as_bytes());
    o.push(b' ');
    o.extend_from_slice(&d[0..4]);
    leak(o)
}

fn units(cp: u32) -> (u32, u32) {
    if cp >= 0x10000 {
        (0xD800 + ((cp - 0x10000) >> 10), 0xDC00 + (cp & 0x3FF))
    } else {
        (cp, 0)
    }
}

/// JS default string comparison (UTF-16 code units) over WTF-8 bytes.
fn cmp16(a: &[u8], b: &[u8]) -> Ordering {
    let n = a.len().min(b.len());
    let mut i = 0;
    while i < n && a[i] == b[i] {
        i += 1;
    }
    if i == n {
        return a.len().cmp(&b.len());
    }
    if a[i] < 0x80 && b[i] < 0x80 {
        return a[i].cmp(&b[i]);
    }
    let j = char_start_back(a, i, 0);
    let (ca, _) = decode(a, j);
    let (cb, _) = decode(b, j);
    units(ca).cmp(&units(cb))
}

fn js_stringify(t: &[u8]) -> B {
    let mut o = vec![b'"'];
    let mut i = 0;
    while i < t.len() {
        let c = t[i];
        match c {
            b'"' => o.extend_from_slice(b"\\\""),
            b'\\' => o.extend_from_slice(b"\\\\"),
            b'\n' => o.extend_from_slice(b"\\n"),
            b'\r' => o.extend_from_slice(b"\\r"),
            b'\t' => o.extend_from_slice(b"\\t"),
            8 => o.extend_from_slice(b"\\b"),
            12 => o.extend_from_slice(b"\\f"),
            0..=0x1F => push_u(&mut o, c as u32),
            0xED if i + 1 < t.len() && t[i + 1] >= 0xA0 => {
                let (cp, l) = decode(t, i);
                push_u(&mut o, cp);
                i += l;
                continue;
            }
            _ => o.push(c),
        }
        i += 1;
    }
    o.push(b'"');
    o
}

// ================================================================== front matter

#[derive(Default)]
struct Data {
    title: Option<S>,
    date: Option<S>,
    tags: Option<Vec<S>>,
    draft: Option<bool>,
    layout: Option<S>,
    slug: Option<S>,
    summary: Option<S>,
}

fn string_of(raw: S) -> Result<S, &'static str> {
    if raw[0] == b'"' {
        if raw.len() < 2 || raw[raw.len() - 1] != b'"' {
            return Err("a quoted string is not closed");
        }
        let last = raw.len() - 1;
        let inner = &raw[1..last];
        if !inner.iter().any(|&c| c == b'\\' || c == b'"') {
            return Ok(inner);
        }
        let mut out = Vec::with_capacity(raw.len());
        let mut i = 1;
        while i < last {
            let c = raw[i];
            if c == b'\\' {
                let n = raw[i + 1];
                if (n != b'"' && n != b'\\') || i + 1 == last {
                    return Err("a quoted string holds an escape other than \\\" or \\\\");
                }
                out.push(n);
                i += 2;
            } else if c == b'"' {
                return Err("a quoted string holds a stray quote");
            } else {
                out.push(c);
                i += 1;
            }
        }
        return Ok(leak(out));
    }
    if raw[0] == b'[' {
        return Err("a bare string opens on [");
    }
    Ok(raw)
}

fn days_in(y: u32, m: u32) -> u32 {
    if m == 2 {
        return if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 { 29 } else { 28 };
    }
    if matches!(m, 4 | 6 | 9 | 11) {
        30
    } else {
        31
    }
}

fn is_name(s: &[u8]) -> bool {
    !s.is_empty() && s.iter().all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

fn is_slug(s: &[u8]) -> bool {
    if s.is_empty() || s.len() > 64 {
        return false;
    }
    let mut prev_dash = true;
    for &c in s {
        if c == b'-' {
            if prev_dash {
                return false;
            }
            prev_dash = true;
        } else if c.is_ascii_lowercase() || c.is_ascii_digit() {
            prev_dash = false;
        } else {
            return false;
        }
    }
    !prev_dash
}

fn err_key(key: &[u8], msg: &str) -> B {
    let mut e = b"key \"".to_vec();
    e.extend_from_slice(key);
    e.extend_from_slice(b"\": ");
    e.extend_from_slice(msg.as_bytes());
    e
}

fn parse_fm(text: S) -> Result<(Data, usize), B> {
    let first_end = memchr(b'\n', text, 0).unwrap_or(text.len());
    if &text[..first_end] != b"---" {
        return Err(b"no front matter".to_vec());
    }
    let lines_from = first_end + 1;
    let mut pos = lines_from;
    let mut lines_to = 0;
    let mut body = None;
    while pos <= text.len() && first_end < text.len() {
        let e = memchr(b'\n', text, pos).unwrap_or(text.len());
        if &text[pos..e] == b"---" {
            body = Some(if e < text.len() { e + 1 } else { text.len() });
            lines_to = pos;
            break;
        }
        pos = e + 1;
    }
    let body = match body {
        Some(b) => b,
        None => return Err(b"the front matter is not closed".to_vec()),
    };
    let mut d = Data::default();
    let mut pos = lines_from;
    let mut li = 0usize;
    while pos < lines_to {
        let e = memchr(b'\n', text, pos).unwrap();
        let line = &text[pos..e];
        pos = e + 1;
        li += 1;
        if all_sp_tab(line) || line[0] == b'#' {
            continue;
        }
        // LINE = /^([a-z_]+):(?: (.*))?$/
        let mut k = 0;
        while k < line.len() && (line[k].is_ascii_lowercase() || line[k] == b'_') {
            k += 1;
        }
        let ok = k > 0 && k < line.len() && line[k] == b':' && {
            let rest = &line[k + 1..];
            rest.is_empty() || (rest[0] == b' ' && !has_line_term(&rest[1..]))
        };
        if !ok {
            return Err(format!("line {} is not \"key: value\"", li + 1).into_bytes());
        }
        let key = &line[..k];
        let rest = &line[k + 1..];
        let mut raw = if rest.is_empty() { rest } else { &rest[1..] };
        while let Some(&c) = raw.last() {
            if c == b' ' || c == b'\t' {
                raw = &raw[..raw.len() - 1];
            } else {
                break;
            }
        }
        let known = matches!(key, b"title" | b"date" | b"tags" | b"draft" | b"layout" | b"slug" | b"summary");
        if !known {
            let mut e = b"unknown key \"".to_vec();
            e.extend_from_slice(key);
            e.push(b'"');
            return Err(e);
        }
        let twice = match key {
            b"title" => d.title.is_some(),
            b"date" => d.date.is_some(),
            b"tags" => d.tags.is_some(),
            b"draft" => d.draft.is_some(),
            b"layout" => d.layout.is_some(),
            b"slug" => d.slug.is_some(),
            _ => d.summary.is_some(),
        };
        if twice {
            let mut e = b"key \"".to_vec();
            e.extend_from_slice(key);
            e.extend_from_slice(b"\" is given twice");
            return Err(e);
        }
        if raw.is_empty() {
            let mut e = b"key \"".to_vec();
            e.extend_from_slice(key);
            e.extend_from_slice(b"\" has no value");
            return Err(e);
        }
        match key {
            b"title" | b"summary" => {
                let v = string_of(raw).map_err(|m| err_key(key, m))?;
                if key == b"title" {
                    d.title = Some(v)
                } else {
                    d.summary = Some(v)
                }
            }
            b"date" => {
                let ok = raw.len() == 10
                    && raw[4] == b'-'
                    && raw[7] == b'-'
                    && [0, 1, 2, 3, 5, 6, 8, 9].iter().all(|&i| raw[i].is_ascii_digit())
                    && {
                        let num = |a: usize, b: usize| raw[a..b].iter().fold(0u32, |v, &c| v * 10 + (c - b'0') as u32);
                        let (y, m, dd) = (num(0, 4), num(5, 7), num(8, 10));
                        (1970..=2099).contains(&y) && (1..=12).contains(&m) && dd >= 1 && dd <= days_in(y, m)
                    };
                if !ok {
                    return Err(b"key \"date\": not a date from 1970-01-01 to 2099-12-31 (YYYY-MM-DD)".to_vec());
                }
                d.date = Some(raw);
            }
            b"tags" => {
                if raw[0] != b'[' || raw[raw.len() - 1] != b']' || raw.len() < 2 {
                    return Err(b"key \"tags\": not a list [a, b]".to_vec());
                }
                let inner = &raw[1..raw.len() - 1];
                if js_trim(inner).is_empty() {
                    d.tags = Some(Vec::new());
                } else {
                    let mut tags = Vec::new();
                    for part in inner.split(|&c| c == b',') {
                        let t = js_trim(part);
                        if t.is_empty() || t.iter().any(|&c| c == b'"' || c == b'[' || c == b']') {
                            return Err(b"key \"tags\": an empty tag, or one holding \" [ or ]".to_vec());
                        }
                        tags.push(t);
                    }
                    d.tags = Some(tags);
                }
            }
            b"draft" => {
                d.draft = Some(match raw {
                    b"true" => true,
                    b"false" => false,
                    _ => return Err(b"key \"draft\": true or false".to_vec()),
                })
            }
            b"layout" => {
                if !is_name(raw) {
                    return Err(b"key \"layout\": not a template name".to_vec());
                }
                d.layout = Some(raw);
            }
            _ => {
                if !is_slug(raw) {
                    return Err(b"key \"slug\": not a slug".to_vec());
                }
                d.slug = Some(raw);
            }
        }
    }
    if d.title.is_none() {
        return Err(b"no title".to_vec());
    }
    Ok((d, body))
}

// ================================================================== markdown (rendered straight to J-form)

struct LinkEnv<'x> {
    dir: S,
    base: S,
    by_source: &'x HMap<S, usize>,
    urls: &'x [S],
    statics: &'x HSet<S>,
    src_path: S,
    broken: &'x mut HSet<B>,
}

fn resolve_path(dir: &[u8], rel: &[u8], parts: &mut Vec<&'static [u8]>) -> Option<B> {
    parts.clear();
    let dir: S = unsafe { sl(dir.as_ptr(), dir.len()) };
    let rel: S = unsafe { sl(rel.as_ptr(), rel.len()) };
    if !dir.is_empty() {
        parts.extend(dir.split(|&c| c == b'/'));
    }
    for seg in rel.split(|&c| c == b'/') {
        if seg.is_empty() || seg == b"." {
            continue;
        }
        if seg == b".." {
            parts.pop()?;
        } else {
            parts.push(seg);
        }
    }
    Some(parts.join(&b'/'))
}

impl<'x> LinkEnv<'x> {
    fn link(&mut self, target: &[u8], out: &mut B, parts: &mut Vec<&'static [u8]>) {
        let scheme = {
            if !target.is_empty() && target[0].is_ascii_alphabetic() {
                let mut k = 1;
                while k < target.len() && (target[k].is_ascii_alphanumeric() || matches!(target[k], b'+' | b'.' | b'-')) {
                    k += 1;
                }
                k < target.len() && target[k] == b':'
            } else {
                false
            }
        };
        if scheme || target[0] == b'/' || target[0] == b'#' {
            esc_hj(out, target);
            return;
        }
        let hash = memchr(b'#', target, 0);
        let (rel, frag) = match hash {
            Some(h) => (&target[..h], &target[h..]),
            None => (target, &b""[..]),
        };
        let path = if rel.is_empty() { None } else { resolve_path(self.dir, rel, parts) };
        if let Some(p) = &path {
            if let Some(&si) = self.by_source.get(p.as_slice()) {
                esc_hj(out, self.urls[si]);
                esc_hj(out, frag);
                return;
            }
            if self.statics.contains(p.as_slice()) {
                esc_hj(out, self.base);
                out.push(b'/');
                esc_hj(out, p);
                esc_hj(out, frag);
                return;
            }
        }
        let mut m = b"broken-link ".to_vec();
        m.extend_from_slice(self.src_path);
        m.extend_from_slice(b" -> ");
        m.extend_from_slice(target);
        self.broken.insert(m);
        esc_hj(out, target);
    }
}

#[inline]
fn escapable(c: u8) -> bool {
    matches!(c, b'\\' | b'`' | b'*' | b'_' | b'[' | b']' | b'(' | b')' | b'#' | b'+' | b'-' | b'.' | b'!' | b'>')
}

fn find_unescaped(s: &[u8], what: &[u8], from: usize) -> Option<usize> {
    let mut j = from;
    let w0 = what[0];
    while j + what.len() <= s.len() {
        j = scan2(s, j, b'\\', w0);
        if j + what.len() > s.len() {
            return None;
        }
        if s[j] == b'\\' && j + 1 < s.len() && escapable(s[j + 1]) {
            j += 2;
            continue;
        }
        if s[j..].starts_with(what) {
            return Some(j);
        }
        j += 1;
    }
    None
}

fn find_single_star(s: &[u8], from: usize) -> Option<usize> {
    let mut j = from;
    while j < s.len() {
        j = scan2(s, j, b'\\', b'*');
        if j >= s.len() {
            return None;
        }
        let c = s[j];
        if c == b'\\' && j + 1 < s.len() && escapable(s[j + 1]) {
            j += 2;
            continue;
        }
        if c != b'*' {
            j += 1;
            continue;
        }
        if s.get(j + 1) == Some(&b'*') {
            j += 2;
            continue;
        }
        return Some(j);
    }
    None
}

fn inline(s: &[u8], env: &mut LinkEnv, out: &mut B, parts: &mut Vec<&'static [u8]>) {
    let mut i = 0;
    let n = s.len();
    while i < n {
        let c = s[i];
        match c {
            b'\\' if i + 1 < n && escapable(s[i + 1]) => {
                esc_hj(out, &s[i + 1..i + 2]);
                i += 2;
            }
            b'`' => match memchr(b'`', s, i + 1) {
                Some(j) if j > i + 1 => {
                    out.extend_from_slice(b"<code>");
                    esc_hj(out, &s[i + 1..j]);
                    out.extend_from_slice(b"</code>");
                    i = j + 1;
                }
                _ => {
                    out.push(b'`');
                    i += 1;
                }
            },
            b'*' => {
                if s.get(i + 1) == Some(&b'*') {
                    match find_unescaped(s, b"**", i + 2) {
                        Some(j) if j > i + 2 => {
                            out.extend_from_slice(b"<strong>");
                            inline(&s[i + 2..j], env, out, parts);
                            out.extend_from_slice(b"</strong>");
                            i = j + 2;
                        }
                        _ => {
                            out.extend_from_slice(b"**");
                            i += 2;
                        }
                    }
                    continue;
                }
                match find_single_star(s, i + 1) {
                    Some(j) if j > i + 1 => {
                        out.extend_from_slice(b"<em>");
                        inline(&s[i + 1..j], env, out, parts);
                        out.extend_from_slice(b"</em>");
                        i = j + 1;
                    }
                    _ => {
                        out.push(b'*');
                        i += 1;
                    }
                }
            }
            b'[' => {
                let k = find_unescaped(s, b"]", i + 1);
                let m = match k {
                    Some(k) if s.get(k + 1) == Some(&b'(') => memchr(b')', s, k + 2),
                    _ => None,
                };
                if let (Some(k), Some(m)) = (k, m) {
                    let target = &s[k + 2..m];
                    if !target.is_empty() && !has_ws(target) {
                        out.extend_from_slice(b"<a href=\\\"");
                        env.link(target, out, parts);
                        out.extend_from_slice(b"\\\">");
                        inline(&s[i + 1..k], env, out, parts);
                        out.extend_from_slice(b"</a>");
                        i = m + 1;
                        continue;
                    }
                }
                out.push(b'[');
                i += 1;
            }
            _ => {
                // a plain run up to the next inline special, escaped on the way
                let mut k = i;
                if c == b'\\' {
                    out.extend_from_slice(b"\\\\");
                    k = i + 1;
                }
                loop {
                    let j = scan_hji(s, k);
                    app(out, &s[k..j]);
                    if j >= n {
                        i = n;
                        break;
                    }
                    if matches!(s[j], b'\\' | b'`' | b'*' | b'[') {
                        i = j;
                        break;
                    }
                    k = hj_one(out, s, j);
                }
            }
        }
    }
}

fn heading_of(l: &[u8]) -> Option<(usize, &[u8])> {
    let mut n = 0;
    while n < l.len() && l[n] == b'#' {
        n += 1;
    }
    if n == 0 || n > 6 || n >= l.len() || (l[n] != b' ' && l[n] != b'\t') {
        return None;
    }
    if has_line_term(l) {
        return None;
    }
    let t = trim_sp_tab(&l[n..]);
    if t.is_empty() {
        None
    } else {
        Some((n, t))
    }
}

fn fence_open(l: &[u8]) -> Option<&[u8]> {
    if !l.starts_with(b"```") {
        return None;
    }
    let mut k = 3;
    while k < l.len() && (l[k].is_ascii_alphanumeric() || matches!(l[k], b'_' | b'+' | b'-')) {
        k += 1;
    }
    if all_sp_tab(&l[k..]) {
        Some(&l[3..k])
    } else {
        None
    }
}

fn fence_close(l: &[u8]) -> bool {
    l.starts_with(b"```") && all_sp_tab(&l[3..])
}

fn is_hr(l: &[u8]) -> bool {
    (l.starts_with(b"---") || l.starts_with(b"***")) && all_sp_tab(&l[3..])
}

fn bullet(l: &[u8]) -> Option<&[u8]> {
    if l.len() >= 2 && (l[0] == b'-' || l[0] == b'*') && l[1] == b' ' && !has_line_term(&l[2..]) {
        Some(&l[2..])
    } else {
        None
    }
}

fn ordered(l: &[u8]) -> Option<(i64, &[u8])> {
    let mut k = 0;
    while k < l.len() && l[k].is_ascii_digit() {
        k += 1;
    }
    if k == 0 || k > 9 || k + 1 >= l.len() || l[k] != b'.' || l[k + 1] != b' ' {
        return None;
    }
    let rest = &l[k + 2..];
    if has_line_term(rest) {
        return None;
    }
    let v = l[..k].iter().fold(0i64, |v, &c| v * 10 + (c - b'0') as i64);
    Some((v, rest))
}

fn continued(l: &[u8]) -> bool {
    let mut n = 0;
    while n < l.len() && l[n] == b' ' {
        n += 1;
    }
    n >= 2 && n < l.len() && !is_ws(decode(l, n).0)
}

fn ends_paragraph(l: &[u8]) -> bool {
    all_sp_tab(l) || fence_open(l).is_some() || heading_of(l).is_some() || is_hr(l)
}

struct Scratch {
    lines: Vec<(u32, u32)>,
    tmp: B,
    items: Vec<(usize, usize)>,
    ids: HSet<S>,
    parts: Vec<&'static [u8]>,
}

struct MdOut {
    toc: Vec<(i64, S, usize, usize)>,
    excerpt: (usize, usize),
}

/// Renders `src` (a source's body) as J-form HTML appended to `html`.
fn render_md(src: &[u8], env: &mut LinkEnv, html: &mut B, sc: &mut Scratch) -> MdOut {
    let lines = &mut sc.lines;
    lines.clear();
    {
        // as src.split('\n'): one pass, walking the newline bits of each 16-byte block
        let mut st = 0u32;
        let n = src.len();
        let mut i = 0usize;
        unsafe {
            let p = src.as_ptr();
            let nl = _mm_set1_epi8(b'\n' as i8);
            while i + 16 <= n {
                let mut m = _mm_movemask_epi8(_mm_cmpeq_epi8(ld(p.add(i)), nl)) as u32;
                while m != 0 {
                    let e = (i + m.trailing_zeros() as usize) as u32;
                    lines.push((st, e));
                    st = e + 1;
                    m &= m - 1;
                }
                i += 16;
            }
        }
        while i < n {
            if src[i] == b'\n' {
                lines.push((st, i as u32));
                st = i as u32 + 1;
            }
            i += 1;
        }
        lines.push((st, n as u32));
    }
    let ln = |k: usize| -> &[u8] {
        let (a, b) = lines[k];
        &src[a as usize..b as usize]
    };
    sc.ids.clear();
    let tmp = &mut sc.tmp;
    let parts = &mut sc.parts;
    let mut toc = Vec::new();
    let mut excerpt: Option<(usize, usize)> = None;
    let mut i = 0;
    let n = lines.len();
    while i < n {
        let line = ln(i);
        if all_sp_tab(line) {
            i += 1;
            continue;
        }
        if let Some(lang) = fence_open(line) {
            i += 1;
            html.extend_from_slice(b"<pre><code");
            if !lang.is_empty() {
                html.extend_from_slice(b" class=\\\"language-");
                esc_hj(html, lang);
                html.extend_from_slice(b"\\\"");
            }
            html.push(b'>');
            while i < n && !fence_close(ln(i)) {
                esc_hj(html, ln(i));
                html.extend_from_slice(b"\\n");
                i += 1;
            }
            i += 1;
            html.extend_from_slice(b"</code></pre>\\n");
            continue;
        }
        if let Some((depth, text)) = heading_of(line) {
            let id = unique_slug(slugify(text), &mut sc.ids);
            let d = b'0' + depth as u8;
            html.extend_from_slice(&[b'<', b'h', d]);
            html.extend_from_slice(b" id=\\\"");
            html.extend_from_slice(id);
            html.extend_from_slice(b"\\\">");
            let hs = html.len();
            inline(text, env, html, parts);
            let he = html.len();
            html.extend_from_slice(&[b'<', b'/', b'h', d, b'>', b'\\', b'n']);
            if depth == 2 || depth == 3 {
                toc.push((depth as i64, id, hs, he));
            }
            i += 1;
            continue;
        }
        if is_hr(line) {
            html.extend_from_slice(b"<hr>\\n");
            i += 1;
            continue;
        }
        if line[0] == b'>' {
            tmp.clear();
            let mut any = false;
            while i < n && !ln(i).is_empty() && ln(i)[0] == b'>' {
                let l = ln(i);
                let l = if l.len() > 1 && l[1] == b' ' { &l[2..] } else { &l[1..] };
                let t = trim_sp_tab(l);
                if !t.is_empty() {
                    if any {
                        tmp.push(b'\n');
                    }
                    tmp.extend_from_slice(t);
                    any = true;
                }
                i += 1;
            }
            if !any {
                html.extend_from_slice(b"<blockquote>\\n</blockquote>\\n");
            } else {
                html.extend_from_slice(b"<blockquote>\\n<p>");
                inline(tmp, env, html, parts);
                html.extend_from_slice(b"</p>\\n</blockquote>\\n");
            }
            continue;
        }
        let b = bullet(line);
        let o = if b.is_none() { ordered(line) } else { None };
        if b.is_some() || o.is_some() {
            let is_ord = o.is_some();
            tmp.clear();
            let items = &mut sc.items;
            items.clear();
            while i < n {
                let l = ln(i);
                let m = if is_ord { ordered(l).map(|x| x.1) } else { bullet(l) };
                if let Some(t) = m {
                    let a = tmp.len();
                    tmp.extend_from_slice(trim_sp_tab(t));
                    items.push((a, tmp.len()));
                    i += 1;
                } else if continued(l) {
                    tmp.push(b'\n');
                    tmp.extend_from_slice(trim_sp_tab(l));
                    items.last_mut().unwrap().1 = tmp.len();
                    i += 1;
                } else {
                    break;
                }
            }
            if is_ord {
                let start = o.unwrap().0;
                if start != 1 {
                    html.extend_from_slice(b"<ol start=\\\"");
                    push_num(html, start);
                    html.extend_from_slice(b"\\\">\\n");
                } else {
                    html.extend_from_slice(b"<ol>\\n");
                }
            } else {
                html.extend_from_slice(b"<ul>\\n");
            }
            for &(a, b) in items.iter() {
                html.extend_from_slice(b"<li>");
                inline(&tmp[a..b], env, html, parts);
                html.extend_from_slice(b"</li>\\n");
            }
            html.extend_from_slice(if is_ord { b"</ol>\\n" } else { b"</ul>\\n" });
            continue;
        }
        // the paragraph is its lines trimmed and joined by \n: when no line needs trimming that is the source
        // slice itself
        let p0 = i;
        let mut clean = trim_sp_tab(line).len() == line.len();
        i += 1;
        while i < n && !ends_paragraph(ln(i)) {
            let l = ln(i);
            clean &= trim_sp_tab(l).len() == l.len();
            i += 1;
        }
        let para: &[u8] = if clean {
            &src[lines[p0].0 as usize..lines[i - 1].1 as usize]
        } else {
            tmp.clear();
            tmp.extend_from_slice(trim_sp_tab(line));
            for k in p0 + 1..i {
                tmp.push(b'\n');
                tmp.extend_from_slice(trim_sp_tab(ln(k)));
            }
            tmp
        };
        html.extend_from_slice(b"<p>");
        let st = html.len();
        inline(para, env, html, parts);
        if excerpt.is_none() {
            excerpt = Some((st, html.len()));
        }
        html.extend_from_slice(b"</p>\\n");
    }
    MdOut { toc, excerpt: excerpt.unwrap_or((0, 0)) }
}

// ================================================================== templates

// global names a path can open on
const G_OTHER: u8 = 0;
const G_SITE: u8 = 1;
const G_TAGS: u8 = 2;
const G_RECENT: u8 = 3;
const G_PAGE: u8 = 4;
const G_POSTS: u8 = 5;
const G_PAGER: u8 = 6;
const G_TAG: u8 = 7;
const G_LOOP: u8 = 8;

#[derive(Clone, Copy, PartialEq, Eq)]
enum F {
    Other,
    Title,
    Url,
    Date,
    DateLong,
    Summary,
    Excerpt,
    ReadingMinutes,
    Tags,
    Slug,
    Content,
    Toc,
    Newer,
    Older,
    IsPost,
    Name,
    Count,
    Posts,
    Number,
    Total,
    NewerUrl,
    OlderUrl,
    Index,
    First,
    Last,
    Length,
    Level,
    Id,
    Html,
    BaseUrl,
}

fn field_of(s: &[u8]) -> F {
    match s {
        b"title" => F::Title,
        b"url" => F::Url,
        b"date" => F::Date,
        b"date_long" => F::DateLong,
        b"summary" => F::Summary,
        b"excerpt" => F::Excerpt,
        b"reading_minutes" => F::ReadingMinutes,
        b"tags" => F::Tags,
        b"slug" => F::Slug,
        b"content" => F::Content,
        b"toc" => F::Toc,
        b"newer" => F::Newer,
        b"older" => F::Older,
        b"is_post" => F::IsPost,
        b"name" => F::Name,
        b"count" => F::Count,
        b"posts" => F::Posts,
        b"number" => F::Number,
        b"total" => F::Total,
        b"newer_url" => F::NewerUrl,
        b"older_url" => F::OlderUrl,
        b"index" => F::Index,
        b"first" => F::First,
        b"last" => F::Last,
        b"length" => F::Length,
        b"level" => F::Level,
        b"id" => F::Id,
        b"html" => F::Html,
        b"base_url" => F::BaseUrl,
        _ => F::Other,
    }
}

struct Path {
    first: S,
    g: u8,
    rest: Vec<F>,
}

enum Node {
    Text(S),
    Var(Path),
    Raw(Path),
    If(bool, Path, Vec<Node>, Option<Vec<Node>>),
    For(S, Path, Vec<Node>),
    Include(S, usize),
}

enum Tok {
    Text(S),
    Var(Path),
    Raw(Path),
    Tag(S),
}

fn terr(name: &[u8], msg: &[u8]) -> B {
    let mut e = b"template \"".to_vec();
    e.extend_from_slice(name);
    e.extend_from_slice(b"\": ");
    e.extend_from_slice(msg);
    e
}

fn path_of(name: &[u8], t: S) -> Result<Path, B> {
    let ok = !t.is_empty()
        && t.split(|&c| c == b'.').all(|seg| {
            !seg.is_empty() && (seg[0].is_ascii_alphabetic() || seg[0] == b'_') && seg.iter().all(|&c| c.is_ascii_alphanumeric() || c == b'_')
        });
    if !ok {
        let mut m = js_stringify(t);
        m.extend_from_slice(b" is not a path");
        return Err(terr(name, &m));
    }
    let mut it = t.split(|&c| c == b'.');
    let first = it.next().unwrap();
    let g = match first {
        b"site" => G_SITE,
        b"tags" => G_TAGS,
        b"recent" => G_RECENT,
        b"page" => G_PAGE,
        b"posts" => G_POSTS,
        b"pager" => G_PAGER,
        b"tag" => G_TAG,
        b"loop" => G_LOOP,
        _ => G_OTHER,
    };
    Ok(Path { first, g, rest: it.map(field_of).collect() })
}

fn text_tok(text: &mut B) -> Tok {
    let t = std::mem::take(text);
    let mut o = Vec::with_capacity(t.len() + t.len() / 8 + 8);
    esc_j(&mut o, &t);
    Tok::Text(leak(o))
}

fn tokenize(name: &[u8], src: S) -> Result<Vec<Tok>, B> {
    let mut toks = Vec::new();
    let mut text: B = Vec::new();
    let mut i = 0;
    let n = src.len();
    while i < n {
        let open = match memchr(b'{', src, i) {
            Some(o) if o != n - 1 => o,
            _ => {
                text.extend_from_slice(&src[i..]);
                break;
            }
        };
        let next = src[open + 1];
        if next != b'{' && next != b'%' {
            text.extend_from_slice(&src[i..open + 1]);
            i = open + 1;
            continue;
        }
        text.extend_from_slice(&src[i..open]);
        if next == b'{' {
            let raw = src.get(open + 2) == Some(&b'{');
            let k = if raw { 3 } else { 2 };
            let close = match find(src, if raw { b"}}}" } else { b"}}" }, open + k) {
                Some(c) => c,
                None => return Err(terr(name, if raw { b"a {{{ is not closed" } else { b"a {{ is not closed" })),
            };
            if !text.is_empty() {
                toks.push(text_tok(&mut text));
            }
            let p = path_of(name, js_trim(&src[open + k..close]))?;
            toks.push(if raw { Tok::Raw(p) } else { Tok::Var(p) });
            i = close + k;
            continue;
        }
        let close = match find(src, b"%}", open + 2) {
            Some(c) => c,
            None => return Err(terr(name, b"a {% is not closed")),
        };
        let end = close + 2;
        let line_start = src[..open].iter().rposition(|&c| c == b'\n').map(|p| p + 1).unwrap_or(0);
        let nl = memchr(b'\n', src, end);
        let line_end = nl.unwrap_or(n);
        let standalone = all_sp_tab(&src[line_start..open]) && all_sp_tab(&src[end..line_end]);
        if standalone {
            let d = open - line_start;
            let len = text.len() as isize;
            let mut e = len - d as isize;
            if e < 0 {
                e = (len + e).max(0);
            }
            text.truncate(e as usize);
        }
        if !text.is_empty() {
            toks.push(text_tok(&mut text));
        }
        toks.push(Tok::Tag(js_trim(&src[open + 2..close])));
        i = if standalone { nl.map(|p| p + 1).unwrap_or(n) } else { end };
    }
    if !text.is_empty() {
        toks.push(text_tok(&mut text));
    }
    Ok(toks)
}

fn all_non_ws(s: &[u8]) -> bool {
    !s.is_empty() && !has_ws(s)
}

fn is_ident(s: &[u8]) -> bool {
    !s.is_empty() && (s[0].is_ascii_alphabetic() || s[0] == b'_') && s.iter().all(|&c| c.is_ascii_alphanumeric() || c == b'_')
}

struct Frame {
    is_for: bool,
    not: bool,
    name: S,
    path: Option<Path>,
    body: Vec<Node>,
    els: Option<Vec<Node>>,
}

fn parse_template(name: &[u8], src: S) -> Result<Vec<Node>, B> {
    let toks = tokenize(name, src)?;
    let mut root: Vec<Node> = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    fn cur<'a>(root: &'a mut Vec<Node>, stack: &'a mut [Frame]) -> &'a mut Vec<Node> {
        match stack.last_mut() {
            None => root,
            Some(f) => match &mut f.els {
                Some(e) => e,
                None => &mut f.body,
            },
        }
    }
    for t in toks {
        match t {
            Tok::Text(x) => cur(&mut root, &mut stack).push(Node::Text(x)),
            Tok::Var(p) => cur(&mut root, &mut stack).push(Node::Var(p)),
            Tok::Raw(p) => cur(&mut root, &mut stack).push(Node::Raw(p)),
            Tok::Tag(tag) => {
                // FOR
                if let Some(r) = tag.strip_prefix(b"for ".as_slice()) {
                    let mut k = 0;
                    while k < r.len() && (r[k].is_ascii_alphanumeric() || r[k] == b'_') {
                        k += 1;
                    }
                    if is_ident(&r[..k]) && r[k..].starts_with(b" in ") && all_non_ws(&r[k + 4..]) {
                        let p = path_of(name, &r[k + 4..])?;
                        stack.push(Frame { is_for: true, not: false, name: &r[..k], path: Some(p), body: Vec::new(), els: None });
                        continue;
                    }
                }
                if let Some(r) = tag.strip_prefix(b"if ".as_slice()) {
                    let m = if r.starts_with(b"not ") && all_non_ws(&r[4..]) {
                        Some((true, &r[4..]))
                    } else if all_non_ws(r) {
                        Some((false, r))
                    } else {
                        None
                    };
                    if let Some((not, pt)) = m {
                        let p = path_of(name, pt)?;
                        stack.push(Frame { is_for: false, not, name: b"", path: Some(p), body: Vec::new(), els: None });
                        continue;
                    }
                }
                if tag == b"else" {
                    match stack.last_mut() {
                        Some(f) if !f.is_for && f.els.is_none() => f.els = Some(Vec::new()),
                        _ => return Err(terr(name, b"an else outside an if")),
                    }
                    continue;
                }
                if tag == b"endfor" || tag == b"endif" {
                    let want_for = tag == b"endfor";
                    match stack.last() {
                        Some(f) if f.is_for == want_for => {}
                        _ => {
                            let mut m = b"an ".to_vec();
                            m.extend_from_slice(tag);
                            m.extend_from_slice(b" with no open ");
                            m.extend_from_slice(&tag[3..]);
                            return Err(terr(name, &m));
                        }
                    }
                    let f = stack.pop().unwrap();
                    let p = f.path.unwrap();
                    let node = if f.is_for { Node::For(f.name, p, f.body) } else { Node::If(f.not, p, f.body, f.els) };
                    cur(&mut root, &mut stack).push(node);
                    continue;
                }
                if tag.len() >= 11 && tag.starts_with(b"include \"") && tag[tag.len() - 1] == b'"' && is_name(&tag[9..tag.len() - 1]) {
                    cur(&mut root, &mut stack).push(Node::Include(&tag[9..tag.len() - 1], usize::MAX));
                    continue;
                }
                let mut m = b"{% ".to_vec();
                m.extend_from_slice(tag);
                m.extend_from_slice(b" %} is not a tag");
                return Err(terr(name, &m));
            }
        }
    }
    if let Some(f) = stack.last() {
        return Err(terr(name, if f.is_for { b"a for is not closed" } else { b"a if is not closed" }));
    }
    Ok(root)
}

fn resolve_includes(nodes: &mut [Node], idx: &HMap<S, usize>) {
    for n in nodes {
        match n {
            Node::Include(name, k) => *k = idx.get(name).copied().unwrap_or(usize::MAX),
            Node::If(_, _, a, b) => {
                resolve_includes(a, idx);
                if let Some(b) = b {
                    resolve_includes(b, idx);
                }
            }
            Node::For(_, _, a) => resolve_includes(a, idx),
            _ => {}
        }
    }
}

// ================================================================== build model

struct Src {
    path: S,
    data: Data,
    body: S,
    post: bool,
    layout: usize,
    slug: S,
    out_dir: S,
    url: S,
    url_j: S,
    title_j: S,
    summary_j: S,
    html: S,
    html_r: (usize, usize),
    toc: Vec<(i64, S, S)>,
    toc_r: Vec<(i64, S, usize, usize)>,
    excerpt: S,
    excerpt_r: (usize, usize),
    date_long: S,
    reading: i64,
    tag_refs: Vec<(S, S, S)>,
    newer: Option<usize>,
    older: Option<usize>,
}

struct Tag {
    name_j: S,
    slug: S,
    url_j: S,
    posts: Vec<usize>,
}

#[derive(Clone, Copy)]
enum O {
    Site,
    Page(usize),
    Card(usize),
    Link(usize),
    TagRef(usize, usize),
    Tag(usize),
    TagSum(usize),
    Pager(usize),
    Loop(usize, usize),
    TocE(usize, usize),
}

#[derive(Clone, Copy)]
enum A {
    Recent,
    IdxPosts(usize),
    TagPosts(usize),
    AllTags,
    CardTags(usize),
    Toc(usize),
}

/// A template value; every Str is J-form.
#[derive(Clone, Copy)]
enum V {
    Undef,
    Null,
    Bool(bool),
    Num(i64),
    Str(S),
    Obj(O),
    Arr(A),
}

#[derive(Clone, Copy)]
enum Top {
    Page(usize),
    Index(usize),
    Tag(usize),
    Tags,
}

struct LoopFrame {
    name: S,
    x: V,
    idx: usize,
    len: usize,
}

struct Ctx<'b> {
    srcs: &'b [Src],
    posts: &'b [usize],
    tags: &'b [Tag],
    recent: usize,
    per: usize,
    pages: usize,
    pager_urls: &'b [S],
    site_title: S,
    base: S,
    tpl: &'b [Vec<Node>],
}

impl<'b> Ctx<'b> {
    fn arr_len(&self, a: A) -> usize {
        match a {
            A::Recent => self.recent.min(self.posts.len()),
            A::IdxPosts(k) => {
                let st = (k - 1) * self.per;
                self.posts.len().saturating_sub(st).min(self.per)
            }
            A::TagPosts(t) => self.tags[t].posts.len(),
            A::AllTags => self.tags.len(),
            A::CardTags(s) => self.srcs[s].tag_refs.len(),
            A::Toc(s) => self.srcs[s].toc.len(),
        }
    }
    fn arr_get(&self, a: A, i: usize) -> V {
        V::Obj(match a {
            A::Recent => O::Card(self.posts[i]),
            A::IdxPosts(k) => O::Card(self.posts[(k - 1) * self.per + i]),
            A::TagPosts(t) => O::Card(self.tags[t].posts[i]),
            A::AllTags => O::TagSum(i),
            A::CardTags(s) => O::TagRef(s, i),
            A::Toc(s) => O::TocE(s, i),
        })
    }
    #[inline]
    fn card_field(&self, s: usize, f: F) -> V {
        let x = &self.srcs[s];
        match f {
            F::Title => V::Str(x.title_j),
            F::Url => V::Str(x.url_j),
            F::Date => V::Str(x.data.date.unwrap_or(b"")),
            F::DateLong => V::Str(x.date_long),
            F::Summary => V::Str(x.summary_j),
            F::Excerpt => V::Str(x.excerpt),
            F::ReadingMinutes => V::Num(x.reading),
            F::Tags => V::Arr(A::CardTags(s)),
            _ => V::Undef,
        }
    }
    fn prop(&self, v: V, f: F) -> V {
        match v {
            V::Arr(a) => {
                if f == F::Length {
                    V::Num(self.arr_len(a) as i64)
                } else {
                    V::Undef
                }
            }
            V::Obj(o) => match o {
                O::Site => match f {
                    F::Title => V::Str(self.site_title),
                    F::BaseUrl => V::Str(self.base),
                    _ => V::Undef,
                },
                O::Card(s) => self.card_field(s, f),
                O::Page(s) => {
                    let x = &self.srcs[s];
                    match f {
                        F::Slug => V::Str(x.slug),
                        F::Content => V::Str(x.html),
                        F::Toc => V::Arr(A::Toc(s)),
                        F::Newer => x.newer.map(|p| V::Obj(O::Link(p))).unwrap_or(V::Null),
                        F::Older => x.older.map(|p| V::Obj(O::Link(p))).unwrap_or(V::Null),
                        F::IsPost => V::Bool(x.post),
                        _ => self.card_field(s, f),
                    }
                }
                O::Link(s) => match f {
                    F::Title | F::Url | F::Date | F::DateLong => self.card_field(s, f),
                    _ => V::Undef,
                },
                O::TagRef(s, k) => {
                    let t = &self.srcs[s].tag_refs[k];
                    match f {
                        F::Name => V::Str(t.0),
                        F::Slug => V::Str(t.1),
                        F::Url => V::Str(t.2),
                        _ => V::Undef,
                    }
                }
                O::Tag(t) | O::TagSum(t) => {
                    let tg = &self.tags[t];
                    match f {
                        F::Name => V::Str(tg.name_j),
                        F::Slug => V::Str(tg.slug),
                        F::Url => V::Str(tg.url_j),
                        F::Count => V::Num(tg.posts.len() as i64),
                        F::Posts if matches!(o, O::Tag(_)) => V::Arr(A::TagPosts(t)),
                        _ => V::Undef,
                    }
                }
                O::Pager(k) => match f {
                    F::Number => V::Num(k as i64),
                    F::Total => V::Num(self.pages as i64),
                    F::Url => V::Str(self.pager_urls[k - 1]),
                    F::NewerUrl => V::Str(if k == 1 { b"" } else { self.pager_urls[k - 2] }),
                    F::OlderUrl => V::Str(if k == self.pages { b"" } else { self.pager_urls[k] }),
                    _ => V::Undef,
                },
                O::Loop(i, n) => match f {
                    F::Index => V::Num(i as i64 + 1),
                    F::First => V::Bool(i == 0),
                    F::Last => V::Bool(i + 1 == n),
                    F::Length => V::Num(n as i64),
                    _ => V::Undef,
                },
                O::TocE(s, i) => {
                    let t = &self.srcs[s].toc[i];
                    match f {
                        F::Level => V::Num(t.0),
                        F::Id => V::Str(t.1),
                        F::Html => V::Str(t.2),
                        _ => V::Undef,
                    }
                }
            },
            _ => V::Undef,
        }
    }
    fn lookup(&self, path: &Path, top: Top, frames: &[LoopFrame]) -> V {
        let mut v = V::Undef;
        let mut found = false;
        if let Some(fr) = frames.last() {
            if path.g == G_LOOP {
                v = V::Obj(O::Loop(fr.idx, fr.len));
                found = true;
            } else {
                for fr in frames.iter().rev() {
                    if path.first == fr.name {
                        v = fr.x;
                        found = true;
                        break;
                    }
                }
            }
        }
        if !found {
            v = match (path.g, top) {
                (G_SITE, _) => V::Obj(O::Site),
                (G_TAGS, _) => V::Arr(A::AllTags),
                (G_RECENT, _) => V::Arr(A::Recent),
                (G_PAGE, Top::Page(s)) => V::Obj(O::Page(s)),
                (G_POSTS, Top::Index(k)) => V::Arr(A::IdxPosts(k)),
                (G_PAGER, Top::Index(k)) => V::Obj(O::Pager(k)),
                (G_TAG, Top::Tag(t)) => V::Obj(O::Tag(t)),
                _ => V::Undef,
            };
        }
        for &f in &path.rest {
            if matches!(v, V::Undef) {
                return v;
            }
            v = self.prop(v, f);
        }
        v
    }
    fn text_of(&self, v: V, esc: bool, out: &mut B) {
        match v {
            V::Str(s) => {
                if esc {
                    esc_ej(out, s)
                } else {
                    app(out, s)
                }
            }
            V::Num(n) => push_num(out, n),
            V::Bool(b) => out.extend_from_slice(if b { b"true" } else { b"false" }),
            V::Arr(a) => {
                let n = self.arr_len(a);
                for i in 0..n {
                    if i > 0 {
                        out.extend_from_slice(b", ");
                    }
                    self.text_of(self.arr_get(a, i), esc, out);
                }
            }
            _ => {}
        }
    }
    fn truthy(&self, v: V) -> bool {
        match v {
            V::Arr(a) => self.arr_len(a) > 0,
            V::Undef | V::Null => false,
            V::Bool(b) => b,
            V::Num(n) => n != 0,
            V::Str(s) => !s.is_empty(),
            V::Obj(_) => true,
        }
    }
    fn render(&self, nodes: &[Node], top: Top, frames: &mut Vec<LoopFrame>, depth: usize, out: &mut B) {
        for n in nodes {
            match n {
                Node::Text(t) => app(out, t),
                Node::Var(p) => {
                    let v = self.lookup(p, top, frames);
                    self.text_of(v, true, out);
                }
                Node::Raw(p) => {
                    let v = self.lookup(p, top, frames);
                    self.text_of(v, false, out);
                }
                Node::If(not, p, then, els) => {
                    let v = self.truthy(self.lookup(p, top, frames));
                    let br = if v != *not { Some(then) } else { els.as_ref() };
                    if let Some(br) = br {
                        self.render(br, top, frames, depth, out);
                    }
                }
                Node::For(name, p, body) => {
                    if let V::Arr(a) = self.lookup(p, top, frames) {
                        let len = self.arr_len(a);
                        for k in 0..len {
                            frames.push(LoopFrame { name, x: self.arr_get(a, k), idx: k, len });
                            self.render(body, top, frames, depth, out);
                            frames.pop();
                        }
                    }
                }
                Node::Include(_, k) => {
                    if *k != usize::MAX && depth < 8 {
                        self.render(&self.tpl[*k], top, frames, depth + 1, out);
                    }
                }
            }
        }
    }
}

fn dir_of(p: S) -> S {
    match p.iter().rposition(|&c| c == b'/') {
        Some(k) => &p[..k],
        None => b"",
    }
}

/// Words as /\S+/g counts them.
fn words_of(s: &[u8]) -> usize {
    let mut n = 0usize;
    let mut i = 0;
    let len = s.len();
    unsafe {
        let p = s.as_ptr();
        // carry: byte 0 is 0xFF when the character before the block is whitespace (or the text starts there)
        let mut carry = _mm_cvtsi32_si128(0xFF);
        let mut acc = _mm_setzero_si128();
        let mut k = 0u32;
        while i + 16 <= len {
            let v = ld(p.add(i));
            // only these lead bytes can start a non-ASCII whitespace character (U+00A0, U+1680, U+2000..U+200A,
            // U+2028, U+2029, U+202F, U+205F, U+3000, U+FEFF); other non-ASCII bytes are never whitespace, and a
            // character's bytes are contiguous non-whitespace, so it starts a word at most once
            let lead = _mm_or_si128(
                _mm_or_si128(eqc(v, 0xC2), eqc(v, 0xE1)),
                _mm_or_si128(_mm_or_si128(eqc(v, 0xE2), eqc(v, 0xE3)), eqc(v, 0xEF)),
            );
            if _mm_movemask_epi8(lead) == 0 {
                // whitespace here is 0x20 or 9..=13
                let t = _mm_sub_epi8(v, _mm_set1_epi8(9));
                let ws = _mm_or_si128(eqc(v, b' '), _mm_cmpeq_epi8(_mm_min_epu8(t, _mm_set1_epi8(4)), t));
                let prev = _mm_or_si128(_mm_slli_si128(ws, 1), carry);
                let starts = _mm_andnot_si128(ws, prev);
                acc = _mm_sub_epi8(acc, starts);
                carry = _mm_srli_si128(ws, 15);
                i += 16;
                k += 1;
                if k == 255 {
                    let sad = _mm_sad_epu8(acc, _mm_setzero_si128());
                    n += (_mm_cvtsi128_si64(sad) + _mm_cvtsi128_si64(_mm_unpackhi_epi64(sad, sad))) as usize;
                    acc = _mm_setzero_si128();
                    k = 0;
                }
                continue;
            }
            let mut inw = _mm_cvtsi128_si32(carry) & 0xFF == 0;
            let end = i + 16;
            while i < end {
                let c = s[i];
                let (w, l) = if c < 0x80 {
                    (matches!(c, 9..=13 | 0x20), 1)
                } else if c < 0xC0 {
                    // a continuation byte whose lead byte ended the previous block: part of that character
                    i += 1;
                    continue;
                } else {
                    let (cp, l) = decode(s, i);
                    (is_ws(cp), l)
                };
                if !w && !inw {
                    n += 1;
                }
                inw = !w;
                i += l;
            }
            carry = _mm_cvtsi32_si128(if inw { 0 } else { 0xFF });
        }
        let sad = _mm_sad_epu8(acc, _mm_setzero_si128());
        n += (_mm_cvtsi128_si64(sad) + _mm_cvtsi128_si64(_mm_unpackhi_epi64(sad, sad))) as usize;
        let mut inw = _mm_cvtsi128_si32(carry) & 0xFF == 0;
        while i < len {
            let c = s[i];
            let (w, l) = if c < 0x80 {
                (matches!(c, 9..=13 | 0x20), 1)
            } else if c < 0xC0 {
                i += 1;
                continue;
            } else {
                let (cp, l) = decode(s, i);
                (is_ws(cp), l)
            };
            if !w && !inw {
                n += 1;
            }
            inw = !w;
            i += l;
        }
    }
    n
}

#[inline]
fn key_start(out: &mut B, first: &mut bool) {
    if !*first {
        out.push(b',');
    }
    *first = false;
    out.push(b'"');
}

fn put_plain(out: &mut B, first: &mut bool, k: &[u8], v: &[u8]) {
    key_start(out, first);
    esc_j(out, k);
    out.extend_from_slice(b"\":\"");
    esc_j(out, v);
    out.push(b'"');
}

fn build(input: &J, out: &mut B, ofd: i32) {
    let sc = &mut Scratch { lines: Vec::with_capacity(4096), tmp: Vec::with_capacity(16384), items: Vec::new(), ids: HSet::default(), parts: Vec::new() };
    let mut report: Vec<B> = Vec::new();
    // templates
    let mut tnames: Vec<S> = Vec::new();
    let mut tpl: Vec<Vec<Node>> = Vec::new();
    let mut tidx: HMap<S, usize> = HMap::default();
    if let Some(J::Obj(ts)) = input.get(b"templates") {
        let mut names: Vec<(S, &J)> = Vec::new();
        for (k, v) in ts {
            if let Some(e) = names.iter_mut().find(|e| e.0 == *k) {
                e.1 = v;
            } else {
                names.push((k, v));
            }
        }
        names.sort_by(|a, b| cmp16(a.0, b.0));
        for (k, v) in names {
            match parse_template(k, v.s()) {
                Ok(nodes) => {
                    tidx.insert(k, tpl.len());
                    tnames.push(k);
                    tpl.push(nodes);
                }
                Err(e) => report.push(e),
            }
        }
    }
    for t in tpl.iter_mut() {
        resolve_includes(t, &tidx);
    }
    let site = input.get(b"site").unwrap();
    let site_title = site.get(b"title").map(|x| x.s()).unwrap_or(b"");
    let mut base: S = site.get(b"base_url").map(|x| x.s()).unwrap_or(b"");
    while base.last() == Some(&b'/') {
        base = &base[..base.len() - 1];
    }
    let per = site.get(b"per_page").map(|x| x.n()).unwrap_or(10.0) as usize;
    let recent = site.get(b"recent").map(|x| x.n()).unwrap_or(0.0).max(0.0) as usize;

    // sources
    let mut files: Vec<(S, S)> = Vec::new();
    if let Some(J::Arr(fs)) = input.get(b"files") {
        for f in fs {
            files.push((f.get(b"path").map(|x| x.s()).unwrap_or(b""), f.get(b"content").map(|x| x.s()).unwrap_or(b"")));
        }
    }
    files.sort_by(|a, b| cmp16(a.0, b.0));
    let mut statics: Vec<(S, S)> = Vec::new();
    let mut srcs: Vec<Src> = Vec::new();
    for &(path, content) in &files {
        if !path.ends_with(b".md") {
            statics.push((path, content));
            continue;
        }
        let text: S = if scan_cr(content, 0) < content.len() {
            let mut t = Vec::with_capacity(content.len());
            let mut i = 0;
            while i < content.len() {
                if content[i] == b'\r' {
                    t.push(b'\n');
                    if i + 1 < content.len() && content[i + 1] == b'\n' {
                        i += 1;
                    }
                } else {
                    t.push(content[i]);
                }
                i += 1;
            }
            leak(t)
        } else {
            content
        };
        let refuse = |report: &mut Vec<B>, why: &[u8]| {
            let mut m = b"refused ".to_vec();
            m.extend_from_slice(path);
            m.extend_from_slice(b": ");
            m.extend_from_slice(why);
            report.push(m);
        };
        let (data, body_off) = match parse_fm(text) {
            Ok(x) => x,
            Err(e) => {
                refuse(&mut report, &e);
                continue;
            }
        };
        let post = path.starts_with(b"posts/");
        if path.starts_with(b"tags/") {
            refuse(&mut report, b"tags/ is the generator's own");
            continue;
        }
        if path.starts_with(b"page/") {
            refuse(&mut report, b"page/ is the generator's own");
            continue;
        }
        if post && data.date.is_none() {
            refuse(&mut report, b"a post needs a date");
            continue;
        }
        let layout: S = data.layout.unwrap_or(if post { b"post" } else { b"page" });
        let li = match tidx.get(layout) {
            Some(&k) => k,
            None => {
                let mut m = b"layout \"".to_vec();
                m.extend_from_slice(layout);
                m.extend_from_slice(b"\" is no template that parses");
                refuse(&mut report, &m);
                continue;
            }
        };
        if data.draft == Some(true) {
            let mut m = b"draft ".to_vec();
            m.extend_from_slice(path);
            report.push(m);
            continue;
        }
        let title_j = jf(data.title.unwrap());
        let summary_j = data.summary.map(jf).unwrap_or(b"");
        srcs.push(Src {
            path,
            data,
            body: &text[body_off..],
            post,
            layout: li,
            slug: b"",
            out_dir: b"",
            url: b"",
            url_j: b"",
            title_j,
            summary_j,
            html: b"",
            html_r: (0, 0),
            toc: Vec::new(),
            toc_r: Vec::new(),
            excerpt: b"",
            excerpt_r: (0, 0),
            date_long: b"",
            reading: 1,
            tag_refs: Vec::new(),
            newer: None,
            older: None,
        });
    }

    // permalinks
    let mut taken: HMap<S, HSet<S>> = HMap::default();
    let mut by_source: HMap<S, usize> = HMap::default();
    let mut urls: Vec<S> = Vec::with_capacity(srcs.len());
    for (si, s) in srcs.iter_mut().enumerate() {
        let dir: S = if s.post { b"posts" } else { dir_of(s.path) };
        let set = taken.entry(dir).or_insert_with(|| {
            let mut h = HSet::default();
            if dir.is_empty() {
                h.insert(b"posts".as_slice());
                h.insert(b"tags".as_slice());
                h.insert(b"page".as_slice());
            }
            h
        });
        let want = match s.data.slug {
            Some(x) => x,
            None => slugify(s.data.title.unwrap()),
        };
        let slug = unique_slug(want, set);
        let mut od = Vec::with_capacity(dir.len() + slug.len() + 1);
        if !dir.is_empty() {
            od.extend_from_slice(dir);
            od.push(b'/');
        }
        od.extend_from_slice(slug);
        let od = leak(od);
        let mut url = Vec::with_capacity(base.len() + od.len() + 2);
        url.extend_from_slice(base);
        url.push(b'/');
        url.extend_from_slice(od);
        url.push(b'/');
        let url = leak(url);
        s.slug = slug;
        s.out_dir = od;
        s.url = url;
        s.url_j = jf(url);
        urls.push(url);
        by_source.insert(s.path, si);
    }

    // markdown, into one J-form buffer for the case
    let static_paths: HSet<S> = statics.iter().map(|x| x.0).collect();
    let mut broken: HSet<B> = HSet::default();
    let total_body: usize = srcs.iter().map(|s| s.body.len()).sum();
    let mut hbuf: B = Vec::with_capacity(total_body + total_body / 2 + 4096);
    for s in srcs.iter_mut() {
        let mut env = LinkEnv {
            dir: dir_of(s.path),
            base,
            by_source: &by_source,
            urls: &urls,
            statics: &static_paths,
            src_path: s.path,
            broken: &mut broken,
        };
        let st = hbuf.len();
        let md = render_md(s.body, &mut env, &mut hbuf, sc);
        s.html_r = (st, hbuf.len());
        s.toc_r = md.toc;
        s.excerpt_r = md.excerpt;
        s.date_long = s.data.date.map(long_date).unwrap_or(b"");
        s.reading = words_of(s.body).div_ceil(200).max(1) as i64;
    }
    let hb: S = leak(hbuf);
    for s in srcs.iter_mut() {
        s.html = &hb[s.html_r.0..s.html_r.1];
        s.excerpt = &hb[s.excerpt_r.0..s.excerpt_r.1];
        s.toc = s.toc_r.iter().map(|&(l, id, a, b)| (l, id, &hb[a..b])).collect();
    }

    // collections
    let mut posts: Vec<usize> = (0..srcs.len()).filter(|&i| srcs[i].post).collect();
    posts.sort_by(|&x, &y| cmp16(srcs[y].data.date.unwrap(), srcs[x].data.date.unwrap()).then_with(|| cmp16(srcs[x].slug, srcs[y].slug)));
    for (k, &p) in posts.iter().enumerate() {
        srcs[p].newer = if k == 0 { None } else { Some(posts[k - 1]) };
        srcs[p].older = if k + 1 == posts.len() { None } else { Some(posts[k + 1]) };
    }

    // tags
    let mut tags: Vec<Tag> = Vec::new();
    let mut by_slug: HMap<S, usize> = HMap::default();
    let own_tags = |s: &Src| -> Vec<(S, S)> {
        let mut v: Vec<(S, S)> = Vec::new();
        if let Some(ts) = &s.data.tags {
            for &name in ts {
                let sl = slugify(name);
                if !v.iter().any(|x| x.1 == sl) {
                    v.push((name, sl));
                }
            }
        }
        v
    };
    for &p in &posts {
        let mut refs = Vec::new();
        for (name, sl) in own_tags(&srcs[p]) {
            let ti = *by_slug.entry(sl).or_insert_with(|| {
                let mut url = Vec::with_capacity(base.len() + sl.len() + 7);
                url.extend_from_slice(base);
                url.extend_from_slice(b"/tags/");
                url.extend_from_slice(sl);
                url.push(b'/');
                tags.push(Tag { name_j: jf(name), slug: sl, url_j: jf(leak(url)), posts: Vec::new() });
                tags.len() - 1
            });
            tags[ti].posts.push(p);
            let t = &tags[ti];
            refs.push((t.name_j, t.slug, t.url_j));
        }
        srcs[p].tag_refs = refs;
    }
    for si in 0..srcs.len() {
        if srcs[si].post {
            continue;
        }
        let refs: Vec<(S, S, S)> = own_tags(&srcs[si])
            .into_iter()
            .map(|(name, sl)| match by_slug.get(sl) {
                Some(&ti) => (tags[ti].name_j, tags[ti].slug, tags[ti].url_j),
                None => (jf(name), sl, b"".as_slice()),
            })
            .collect();
        srcs[si].tag_refs = refs;
    }
    tags.sort_by(|x, y| y.posts.len().cmp(&x.posts.len()).then_with(|| cmp16(x.slug, y.slug)));

    // layouts
    let per_eff = per.max(1);
    let pages = posts.len().div_ceil(per_eff).max(1);
    let pager_urls: Vec<S> = (1..=pages)
        .map(|k| {
            let mut u = base.to_vec();
            if k == 1 {
                u.push(b'/');
            } else {
                u.extend_from_slice(b"/page/");
                push_num(&mut u, k as i64);
                u.push(b'/');
            }
            jf(leak(u))
        })
        .collect();
    let tpl_of = |name: &[u8]| tidx.get(name).map(|&k| &tpl[k]);
    let ctx = Ctx {
        srcs: &srcs,
        posts: &posts,
        tags: &tags,
        recent,
        per: per_eff,
        pages,
        pager_urls: &pager_urls,
        site_title: jf(site_title),
        base: jf(base),
        tpl: &tpl,
    };
    out.push(b'{');
    let mut first = true;
    let mut written: HSet<B> = HSet::default();
    let mut frames: Vec<LoopFrame> = Vec::new();
    for (si, s) in srcs.iter().enumerate() {
        let mut key = Vec::with_capacity(s.out_dir.len() + 11);
        key.extend_from_slice(s.out_dir);
        key.extend_from_slice(b"/index.html");
        key_start(out, &mut first);
        esc_j(out, &key);
        out.extend_from_slice(b"\":\"");
        ctx.render(&tpl[s.layout], Top::Page(si), &mut frames, 0, out);
        out.push(b'"');
        flush_if(out, ofd);
        written.insert(key);
    }
    if let Some(t) = tpl_of(b"index") {
        for k in 1..=pages {
            let key = if k == 1 {
                b"index.html".to_vec()
            } else {
                let mut v = b"page/".to_vec();
                push_num(&mut v, k as i64);
                v.extend_from_slice(b"/index.html");
                v
            };
            key_start(out, &mut first);
            out.extend_from_slice(&key);
            out.extend_from_slice(b"\":\"");
            ctx.render(t, Top::Index(k), &mut frames, 0, out);
            out.push(b'"');
            flush_if(out, ofd);
            written.insert(key);
        }
    }
    if let Some(t) = tpl_of(b"tag") {
        for (ti, tg) in tags.iter().enumerate() {
            let mut key = b"tags/".to_vec();
            key.extend_from_slice(tg.slug);
            key.extend_from_slice(b"/index.html");
            key_start(out, &mut first);
            out.extend_from_slice(&key);
            out.extend_from_slice(b"\":\"");
            ctx.render(t, Top::Tag(ti), &mut frames, 0, out);
            out.push(b'"');
            flush_if(out, ofd);
            written.insert(key);
        }
    }
    if let Some(t) = tpl_of(b"tags") {
        key_start(out, &mut first);
        out.extend_from_slice(b"tags/index.html\":\"");
        ctx.render(t, Top::Tags, &mut frames, 0, out);
        out.push(b'"');
        written.insert(b"tags/index.html".to_vec());
    }
    for &(p, c) in &statics {
        if written.contains(p) {
            let mut m = b"shadowed ".to_vec();
            m.extend_from_slice(p);
            m.extend_from_slice(b": a built page has its path");
            report.push(m);
        } else {
            written.insert(p.to_vec());
            if p != b"build-report.txt" {
                put_plain(out, &mut first, p, c);
            }
        }
    }
    // report
    report.extend(broken);
    report.sort_by(|a, b| cmp16(a, b));
    let npages = srcs.iter().filter(|s| !s.post).count();
    let mut rep: B = Vec::new();
    for l in &report {
        rep.extend_from_slice(l);
        rep.push(b'\n');
    }
    rep.extend_from_slice(b"built ");
    push_num(&mut rep, npages as i64);
    rep.extend_from_slice(b" pages, ");
    push_num(&mut rep, posts.len() as i64);
    rep.extend_from_slice(b" posts, ");
    push_num(&mut rep, tags.len() as i64);
    rep.extend_from_slice(b" tags and ");
    push_num(&mut rep, statics.len() as i64);
    rep.extend_from_slice(b" static files\n");
    put_plain(out, &mut first, b"build-report.txt", &rep);
    out.push(b'}');
}

// ================================================================== entry

const FLUSH: usize = 512 << 10;

unsafe fn write_all(fd: i32, b: &[u8]) {
    let mut off = 0usize;
    while off < b.len() {
        let r = write(fd, b.as_ptr().add(off) as *const c_void, b.len() - off);
        if r <= 0 {
            _exit(74);
        }
        off += r as usize;
    }
}

#[inline]
fn flush_if(out: &mut B, fd: i32) {
    if out.len() >= FLUSH {
        unsafe { write_all(fd, out) };
        out.clear();
    }
}

/// `$var` (or `def` when unset) + `file`, NUL-terminated, in `buf`.
unsafe fn cpath(buf: &mut [u8; 4096], var: &[u8], def: &[u8], file: &[u8]) -> *const c_char {
    let e = getenv(var.as_ptr() as *const c_char);
    let dir: &[u8] = if e.is_null() { def } else { sl(e as *const u8, strlen(e)) };
    let n = dir.len().min(4096 - file.len() - 1);
    buf[..n].copy_from_slice(&dir[..n]);
    buf[n..n + file.len()].copy_from_slice(file);
    buf[n + file.len()] = 0;
    buf.as_ptr() as *const c_char
}

#[inline]
fn pg(n: usize) -> usize {
    (n + 4095) & !4095
}

#[no_mangle]
pub unsafe extern "C" fn main(_argc: i32, _argv: *const *const u8) -> i32 {
    let mut pb = [0u8; 4096];
    let ip = cpath(&mut pb, b"MAKEFUN_INPUT_DIR\0", b"/input", b"/cases.json");
    let fd = open(ip, 0o2000000); // O_RDONLY | O_CLOEXEC
    if fd < 0 {
        _exit(66);
    }
    let n = lseek(fd, 0, 2).max(0) as usize;

    // one region: [input + zero pad][arena, prefaulted][output chunk, prefaulted][output reserve]
    // The arena is reset after every case (nothing of a case outlives it) and the output is flushed in chunks,
    // so the prefaulted part is sized for the largest case, not the whole run.
    let in_sz = pg(n + 64);
    let arena_sz = pg(n + n / 4 + 256 * 1024);
    let out_pop = pg(FLUSH + 256 * 1024);
    // holds one flush chunk plus the largest single entry (escaping grows a string at most 6x)
    let out_cap = pg(FLUSH + 8 * n + (16 << 20));
    let total = in_sz + arena_sz + out_cap;
    let base = mmap(core::ptr::null_mut(), total, PROT_RW, MAP_PRIVATE_ANON | MAP_NORESERVE, -1, 0) as *mut u8;
    if base as isize == -1 {
        _exit(71);
    }
    mmap(base as *mut c_void, in_sz + arena_sz + out_pop, PROT_RW, MAP_PRIVATE_ANON | MAP_FIXED | MAP_POPULATE, -1, 0);
    CUR = base as usize + in_sz;
    END = CUR + arena_sz;

    let mut got = 0usize;
    while got < n {
        let r = pread(fd, base.add(got) as *mut c_void, n - got, got as i64);
        if r <= 0 {
            break;
        }
        got += r as usize;
    }

    let op = cpath(&mut pb, b"MAKEFUN_OUTPUT_DIR\0", b"/output", b"/answers.json");
    let ofd = open(op, 0o1 | 0o100 | 0o1000 | 0o2000000, 0o644); // O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC
    if ofd < 0 {
        _exit(73);
    }

    let mut p = P { s: base, n: got, i: 0 };
    let root = p.val();
    let mut out: B = Vec::from_raw_parts(base.add(in_sz + arena_sz), 0, out_cap);
    let (mark_cur, mark_end) = (CUR, END);
    out.push(b'[');
    if let J::Arr(cases) = &root {
        for (i, c) in cases.iter().enumerate() {
            if i > 0 {
                out.push(b',');
            }
            match c.get(b"input") {
                Some(inp) => build(inp, &mut out, ofd),
                None => out.extend_from_slice(b"null"),
            }
            // nothing a case allocated is used again
            CUR = mark_cur;
            END = mark_end;
            LAST = 0;
        }
    }
    out.push(b']');
    write_all(ofd, &out);
    _exit(0);
}
