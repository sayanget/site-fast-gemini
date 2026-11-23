// sitegen: a native port of makefun-site 1.0.0's build(input), for the "Build a static site faster" bounty.
// Reads $MAKEFUN_INPUT_DIR/cases.json (default /input), writes $MAKEFUN_OUTPUT_DIR/answers.json (default /output).
// Strings are kept as WTF-8 bytes so lone surrogates from JSON escapes survive; every JS rule the reference leans on
// (UTF-16 code-unit order, JS \s, `.` excluding line terminators, NFKD slugs) is reproduced explicitly.

mod slugtab;

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

type B = Vec<u8>;

// ------------------------------------------------------------------ JSON input

enum J {
    Null,
    Bool(bool),
    Num(f64),
    Str(B),
    Arr(Vec<J>),
    Obj(Vec<(B, J)>),
}

impl J {
    fn get(&self, k: &[u8]) -> Option<&J> {
        if let J::Obj(v) = self {
            // last duplicate wins, as in JSON.parse
            for (kk, vv) in v.iter().rev() {
                if kk.as_slice() == k {
                    return Some(vv);
                }
            }
        }
        None
    }
    fn s(&self) -> &[u8] {
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

struct P<'a> {
    s: &'a [u8],
    i: usize,
}

fn push_cp(out: &mut B, cp: u32) {
    if cp < 0x80 {
        out.push(cp as u8);
    } else if cp < 0x800 {
        out.push(0xC0 | (cp >> 6) as u8);
        out.push(0x80 | (cp & 0x3F) as u8);
    } else if cp < 0x10000 {
        out.push(0xE0 | (cp >> 12) as u8);
        out.push(0x80 | ((cp >> 6) & 0x3F) as u8);
        out.push(0x80 | (cp & 0x3F) as u8);
    } else {
        out.push(0xF0 | (cp >> 18) as u8);
        out.push(0x80 | ((cp >> 12) & 0x3F) as u8);
        out.push(0x80 | ((cp >> 6) & 0x3F) as u8);
        out.push(0x80 | (cp & 0x3F) as u8);
    }
}

impl<'a> P<'a> {
    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }
    fn hex4(&mut self) -> u32 {
        let mut v = 0u32;
        for _ in 0..4 {
            let c = self.s[self.i];
            self.i += 1;
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
    fn string(&mut self) -> B {
        self.i += 1;
        let s = self.s;
        let start = self.i;
        let mut j = start;
        while s[j] != b'"' && s[j] != b'\\' {
            j += 1;
        }
        if s[j] == b'"' {
            self.i = j + 1;
            return s[start..j].to_vec();
        }
        let mut out = Vec::with_capacity((j - start) * 2 + 16);
        out.extend_from_slice(&s[start..j]);
        self.i = j;
        loop {
            let c = s[self.i];
            if c == b'"' {
                self.i += 1;
                return out;
            }
            if c != b'\\' {
                // copy a run
                let st = self.i;
                let mut k = st;
                while s[k] != b'"' && s[k] != b'\\' {
                    k += 1;
                }
                out.extend_from_slice(&s[st..k]);
                self.i = k;
                continue;
            }
            let e = s[self.i + 1];
            self.i += 2;
            match e {
                b'n' => out.push(b'\n'),
                b't' => out.push(b'\t'),
                b'r' => out.push(b'\r'),
                b'b' => out.push(8),
                b'f' => out.push(12),
                b'u' => {
                    let mut cp = self.hex4();
                    if (0xD800..0xDC00).contains(&cp) && self.i + 1 < s.len() && s[self.i] == b'\\' && s[self.i + 1] == b'u' {
                        let save = self.i;
                        self.i += 2;
                        let lo = self.hex4();
                        if (0xDC00..0xE000).contains(&lo) {
                            cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                        } else {
                            self.i = save;
                        }
                    }
                    push_cp(&mut out, cp);
                }
                other => out.push(other),
            }
        }
    }
    fn val(&mut self) -> J {
        self.ws();
        let c = self.s[self.i];
        match c {
            b'{' => {
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.s[self.i] == b'}' {
                    self.i += 1;
                    return J::Obj(v);
                }
                loop {
                    self.ws();
                    let k = self.string();
                    self.ws();
                    self.i += 1; // :
                    let x = self.val();
                    v.push((k, x));
                    self.ws();
                    let d = self.s[self.i];
                    self.i += 1;
                    if d == b'}' {
                        return J::Obj(v);
                    }
                }
            }
            b'[' => {
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.s[self.i] == b']' {
                    self.i += 1;
                    return J::Arr(v);
                }
                loop {
                    v.push(self.val());
                    self.ws();
                    let d = self.s[self.i];
                    self.i += 1;
                    if d == b']' {
                        return J::Arr(v);
                    }
                }
            }
            b'"' => J::Str(self.string()),
            b't' => {
                self.i += 4;
                J::Bool(true)
            }
            b'f' => {
                self.i += 5;
                J::Bool(false)
            }
            b'n' => {
                self.i += 4;
                J::Null
            }
            _ => {
                let st = self.i;
                while self.i < self.s.len() && matches!(self.s[self.i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                    self.i += 1;
                }
                let t = std::str::from_utf8(&self.s[st..self.i]).unwrap_or("0");
                J::Num(t.parse::<f64>().unwrap_or(0.0))
            }
        }
    }
}

// ------------------------------------------------------------------ text helpers

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
    while i < s.len() {
        let c = s[i];
        if c == b'\n' || c == b'\r' {
            return true;
        }
        if c == 0xE2 && i + 2 < s.len() && s[i + 1] == 0x80 && (s[i + 2] == 0xA8 || s[i + 2] == 0xA9) {
            return true;
        }
        i += 1;
    }
    false
}

fn all_sp_tab(s: &[u8]) -> bool {
    s.iter().all(|&c| c == b' ' || c == b'\t')
}

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
        let c = what[0];
        return s.get(from..).and_then(|t| t.iter().position(|&x| x == c)).map(|p| p + from);
    }
    if from > s.len() {
        return None;
    }
    let mut j = from;
    while j + what.len() <= s.len() {
        if &s[j..j + what.len()] == what {
            return Some(j);
        }
        j += 1;
    }
    None
}

#[inline]
fn esc_html(out: &mut B, s: &[u8]) {
    let mut st = 0;
    for (i, &c) in s.iter().enumerate() {
        let r: &[u8] = match c {
            b'&' => b"&amp;",
            b'<' => b"&lt;",
            b'>' => b"&gt;",
            b'"' => b"&quot;",
            b'\'' => b"&#39;",
            _ => continue,
        };
        out.extend_from_slice(&s[st..i]);
        out.extend_from_slice(r);
        st = i + 1;
    }
    out.extend_from_slice(&s[st..]);
}

fn push_num(out: &mut B, n: i64) {
    out.extend_from_slice(n.to_string().as_bytes());
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

fn slugify(s: &[u8]) -> B {
    let mut out: B = Vec::with_capacity(s.len().min(80));
    let mut gap = false;
    let mut i = 0;
    let mut feed = |c: u8, out: &mut B, gap: &mut bool| {
        if c == b'-' {
            *gap = !out.is_empty();
        } else {
            if *gap {
                out.push(b'-');
            }
            *gap = false;
            out.push(c);
        }
    };
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
        out.extend_from_slice(b"untitled");
    }
    out
}

fn unique_slug(slug: B, taken: &mut HashSet<B>) -> B {
    if !taken.contains(&slug) {
        taken.insert(slug.clone());
        return slug;
    }
    let mut n = 2i64;
    loop {
        let mut c = slug.clone();
        c.push(b'-');
        push_num(&mut c, n);
        if !taken.contains(&c) {
            taken.insert(c.clone());
            return c;
        }
        n += 1;
    }
}

const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

fn long_date(d: &[u8]) -> B {
    let day = (d[8] - b'0') as i64 * 10 + (d[9] - b'0') as i64;
    let m = ((d[5] - b'0') * 10 + (d[6] - b'0')) as usize;
    let mut o = Vec::with_capacity(20);
    push_num(&mut o, day);
    o.push(b' ');
    o.extend_from_slice(MONTHS[m - 1].as_bytes());
    o.push(b' ');
    o.extend_from_slice(&d[0..4]);
    o
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
            0..=0x1F => o.extend_from_slice(format!("\\u{:04x}", c).as_bytes()),
            0xED if i + 1 < t.len() && t[i + 1] >= 0xA0 => {
                let (cp, l) = decode(t, i);
                o.extend_from_slice(format!("\\u{:04x}", cp).as_bytes());
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

/// Bytes escaped for a JSON string body in our output.
fn json_esc(out: &mut B, s: &[u8]) {
    let mut st = 0;
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        if c >= 0x20 && c != b'"' && c != b'\\' && c != 0xED {
            i += 1;
            continue;
        }
        if c == 0xED && !(i + 1 < s.len() && s[i + 1] >= 0xA0) {
            i += 1;
            continue;
        }
        out.extend_from_slice(&s[st..i]);
        match c {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\t' => out.extend_from_slice(b"\\t"),
            b'\r' => out.extend_from_slice(b"\\r"),
            0xED => {
                let (cp, l) = decode(s, i);
                out.extend_from_slice(format!("\\u{:04x}", cp).as_bytes());
                i += l;
                st = i;
                continue;
            }
            _ => out.extend_from_slice(format!("\\u{:04x}", c).as_bytes()),
        }
        i += 1;
        st = i;
    }
    out.extend_from_slice(&s[st..]);
}

// ------------------------------------------------------------------ front matter

#[derive(Default)]
struct Data {
    title: Option<B>,
    date: Option<B>,
    tags: Option<Vec<B>>,
    draft: Option<bool>,
    layout: Option<B>,
    slug: Option<B>,
    summary: Option<B>,
}

fn string_of(raw: &[u8]) -> Result<B, &'static str> {
    if raw[0] == b'"' {
        if raw.len() < 2 || raw[raw.len() - 1] != b'"' {
            return Err("a quoted string is not closed");
        }
        let mut out = Vec::with_capacity(raw.len());
        let mut i = 1;
        let last = raw.len() - 1;
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
        return Ok(out);
    }
    if raw[0] == b'[' {
        return Err("a bare string opens on [");
    }
    Ok(raw.to_vec())
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

fn parse_fm(text: &[u8]) -> Result<(Data, usize), B> {
    let first_end = find(text, b"\n", 0).unwrap_or(text.len());
    if &text[..first_end] != b"---" {
        return Err(b"no front matter".to_vec());
    }
    let mut lines: Vec<(usize, usize)> = Vec::new();
    let mut pos = first_end + 1;
    let mut body = None;
    while pos <= text.len() && first_end < text.len() {
        let e = find(text, b"\n", pos).unwrap_or(text.len());
        if &text[pos..e] == b"---" {
            body = Some(if e < text.len() { e + 1 } else { text.len() });
            break;
        }
        lines.push((pos, e));
        pos = e + 1;
    }
    let body = match body {
        Some(b) => b,
        None => return Err(b"the front matter is not closed".to_vec()),
    };
    let mut d = Data::default();
    for (li, &(a, b)) in lines.iter().enumerate() {
        let line = &text[a..b];
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
            return Err(format!("line {} is not \"key: value\"", li + 2).into_bytes());
        }
        let key = &line[..k];
        let rest = &line[k + 1..];
        let mut raw = if rest.is_empty() { &rest[..] } else { &rest[1..] };
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
                d.date = Some(raw.to_vec());
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
                        tags.push(t.to_vec());
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
                d.layout = Some(raw.to_vec());
            }
            _ => {
                if !is_slug(raw) {
                    return Err(b"key \"slug\": not a slug".to_vec());
                }
                d.slug = Some(raw.to_vec());
            }
        }
    }
    if d.title.is_none() {
        return Err(b"no title".to_vec());
    }
    Ok((d, body))
}

// ------------------------------------------------------------------ markdown

struct LinkEnv<'x> {
    dir: &'x [u8],
    base: &'x [u8],
    by_source: &'x HashMap<B, usize>,
    urls: &'x [B],
    statics: &'x HashSet<B>,
    src_path: &'x [u8],
    broken: &'x mut HashSet<B>,
}

fn resolve_path(dir: &[u8], rel: &[u8]) -> Option<B> {
    let mut parts: Vec<&[u8]> = if dir.is_empty() { Vec::new() } else { dir.split(|&c| c == b'/').collect() };
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
    fn link(&mut self, target: &[u8], out: &mut B) {
        let scheme = {
            let mut k = 0;
            if !target.is_empty() && target[0].is_ascii_alphabetic() {
                k = 1;
                while k < target.len() && (target[k].is_ascii_alphanumeric() || matches!(target[k], b'+' | b'.' | b'-')) {
                    k += 1;
                }
                k < target.len() && target[k] == b':'
            } else {
                false
            }
        };
        if scheme || target[0] == b'/' || target[0] == b'#' {
            esc_html(out, target);
            return;
        }
        let hash = find(target, b"#", 0);
        let (rel, frag) = match hash {
            Some(h) => (&target[..h], &target[h..]),
            None => (target, &b""[..]),
        };
        let path = if rel.is_empty() { None } else { resolve_path(self.dir, rel) };
        if let Some(p) = &path {
            if let Some(&si) = self.by_source.get(p) {
                esc_html(out, &self.urls[si]);
                esc_html(out, frag);
                return;
            }
            if self.statics.contains(p) {
                esc_html(out, self.base);
                out.push(b'/');
                esc_html(out, p);
                esc_html(out, frag);
                return;
            }
        }
        let mut m = b"broken-link ".to_vec();
        m.extend_from_slice(self.src_path);
        m.extend_from_slice(b" -> ");
        m.extend_from_slice(target);
        self.broken.insert(m);
        esc_html(out, target);
    }
}

#[inline]
fn escapable(c: u8) -> bool {
    matches!(c, b'\\' | b'`' | b'*' | b'_' | b'[' | b']' | b'(' | b')' | b'#' | b'+' | b'-' | b'.' | b'!' | b'>')
}

fn find_unescaped(s: &[u8], what: &[u8], from: usize) -> Option<usize> {
    let mut j = from;
    while j + what.len() <= s.len() {
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

fn inline(s: &[u8], env: &mut LinkEnv, out: &mut B) {
    let mut i = 0;
    let n = s.len();
    while i < n {
        let c = s[i];
        match c {
            b'\\' if i + 1 < n && escapable(s[i + 1]) => {
                esc_html(out, &s[i + 1..i + 2]);
                i += 2;
            }
            b'`' => {
                match find(s, b"`", i + 1) {
                    Some(j) if j > i + 1 => {
                        out.extend_from_slice(b"<code>");
                        esc_html(out, &s[i + 1..j]);
                        out.extend_from_slice(b"</code>");
                        i = j + 1;
                    }
                    _ => {
                        out.push(b'`');
                        i += 1;
                    }
                }
            }
            b'*' => {
                if s.get(i + 1) == Some(&b'*') {
                    match find_unescaped(s, b"**", i + 2) {
                        Some(j) if j > i + 2 => {
                            out.extend_from_slice(b"<strong>");
                            inline(&s[i + 2..j], env, out);
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
                        inline(&s[i + 1..j], env, out);
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
                    Some(k) if s.get(k + 1) == Some(&b'(') => find(s, b")", k + 2),
                    _ => None,
                };
                if let (Some(k), Some(m)) = (k, m) {
                    let target = &s[k + 2..m];
                    if !target.is_empty() && !has_ws(target) {
                        out.extend_from_slice(b"<a href=\"");
                        env.link(target, out);
                        out.extend_from_slice(b"\">");
                        inline(&s[i + 1..k], env, out);
                        out.extend_from_slice(b"</a>");
                        i = m + 1;
                        continue;
                    }
                }
                out.push(b'[');
                i += 1;
            }
            _ => {
                let st = i;
                i += 1;
                while i < n && !matches!(s[i], b'\\' | b'`' | b'*' | b'[') {
                    i += 1;
                }
                esc_html(out, &s[st..i]);
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

struct Toc {
    level: i64,
    id: B,
    html: B,
}

fn render_md(src: &[u8], env: &mut LinkEnv) -> (B, Vec<Toc>, B) {
    let lines: Vec<&[u8]> = src.split(|&c| c == b'\n').collect();
    let mut ids: HashSet<B> = HashSet::new();
    let mut html: B = Vec::with_capacity(src.len() + src.len() / 4 + 64);
    let mut toc = Vec::new();
    let mut excerpt: Option<B> = None;
    let mut i = 0;
    let n = lines.len();
    let mut tmp: B = Vec::new();
    while i < n {
        let line = lines[i];
        if all_sp_tab(line) {
            i += 1;
            continue;
        }
        if let Some(lang) = fence_open(line) {
            i += 1;
            html.extend_from_slice(b"<pre><code");
            if !lang.is_empty() {
                html.extend_from_slice(b" class=\"language-");
                esc_html(&mut html, lang);
                html.push(b'"');
            }
            html.push(b'>');
            while i < n && !fence_close(lines[i]) {
                esc_html(&mut html, lines[i]);
                html.push(b'\n');
                i += 1;
            }
            i += 1;
            html.extend_from_slice(b"</code></pre>\n");
            continue;
        }
        if let Some((depth, text)) = heading_of(line) {
            let id = unique_slug(slugify(text), &mut ids);
            let mut h = Vec::new();
            inline(text, env, &mut h);
            html.extend_from_slice(format!("<h{} id=\"", depth).as_bytes());
            html.extend_from_slice(&id);
            html.extend_from_slice(b"\">");
            html.extend_from_slice(&h);
            html.extend_from_slice(format!("</h{}>\n", depth).as_bytes());
            if depth == 2 || depth == 3 {
                toc.push(Toc { level: depth as i64, id, html: h });
            }
            i += 1;
            continue;
        }
        if is_hr(line) {
            html.extend_from_slice(b"<hr>\n");
            i += 1;
            continue;
        }
        if line[0] == b'>' {
            tmp.clear();
            let mut any = false;
            while i < n && !lines[i].is_empty() && lines[i][0] == b'>' {
                let l = lines[i];
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
                html.extend_from_slice(b"<blockquote>\n</blockquote>\n");
            } else {
                html.extend_from_slice(b"<blockquote>\n<p>");
                inline(&tmp, env, &mut html);
                html.extend_from_slice(b"</p>\n</blockquote>\n");
            }
            continue;
        }
        let b = bullet(line);
        let o = if b.is_none() { ordered(line) } else { None };
        if b.is_some() || o.is_some() {
            let is_ord = o.is_some();
            let mut items: Vec<B> = Vec::new();
            while i < n {
                let m = if is_ord { ordered(lines[i]).map(|x| x.1) } else { bullet(lines[i]) };
                if let Some(t) = m {
                    items.push(trim_sp_tab(t).to_vec());
                    i += 1;
                } else if continued(lines[i]) {
                    let last = items.last_mut().unwrap();
                    last.push(b'\n');
                    last.extend_from_slice(trim_sp_tab(lines[i]));
                    i += 1;
                } else {
                    break;
                }
            }
            if is_ord {
                let start = o.unwrap().0;
                if start != 1 {
                    html.extend_from_slice(format!("<ol start=\"{}\">\n", start).as_bytes());
                } else {
                    html.extend_from_slice(b"<ol>\n");
                }
            } else {
                html.extend_from_slice(b"<ul>\n");
            }
            for it in &items {
                html.extend_from_slice(b"<li>");
                inline(it, env, &mut html);
                html.extend_from_slice(b"</li>\n");
            }
            html.extend_from_slice(if is_ord { b"</ol>\n" } else { b"</ul>\n" });
            continue;
        }
        tmp.clear();
        tmp.extend_from_slice(trim_sp_tab(line));
        i += 1;
        while i < n && !ends_paragraph(lines[i]) {
            tmp.push(b'\n');
            tmp.extend_from_slice(trim_sp_tab(lines[i]));
            i += 1;
        }
        html.extend_from_slice(b"<p>");
        let st = html.len();
        inline(&tmp, env, &mut html);
        if excerpt.is_none() {
            excerpt = Some(html[st..].to_vec());
        }
        html.extend_from_slice(b"</p>\n");
    }
    (html, toc, excerpt.unwrap_or_default())
}

// ------------------------------------------------------------------ templates

enum Node {
    Text(B),
    Var(Vec<B>),
    Raw(Vec<B>),
    If(bool, Vec<B>, Vec<Node>, Option<Vec<Node>>),
    For(B, Vec<B>, Vec<Node>),
    Include(B),
}

enum Tok {
    Text(B),
    Var(Vec<B>),
    Raw(Vec<B>),
    Tag(B),
}

fn terr(name: &[u8], msg: &[u8]) -> B {
    let mut e = b"template \"".to_vec();
    e.extend_from_slice(name);
    e.extend_from_slice(b"\": ");
    e.extend_from_slice(msg);
    e
}

fn path_of(name: &[u8], t: &[u8]) -> Result<Vec<B>, B> {
    let ok = !t.is_empty()
        && t.split(|&c| c == b'.').all(|seg| {
            !seg.is_empty() && (seg[0].is_ascii_alphabetic() || seg[0] == b'_') && seg.iter().all(|&c| c.is_ascii_alphanumeric() || c == b'_')
        });
    if !ok {
        let mut m = js_stringify(t);
        m.extend_from_slice(b" is not a path");
        return Err(terr(name, &m));
    }
    Ok(t.split(|&c| c == b'.').map(|x| x.to_vec()).collect())
}

fn tokenize(name: &[u8], src: &[u8]) -> Result<Vec<Tok>, B> {
    let mut toks = Vec::new();
    let mut text: B = Vec::new();
    let mut i = 0;
    let n = src.len();
    while i < n {
        let open = match find(src, b"{", i) {
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
                toks.push(Tok::Text(std::mem::take(&mut text)));
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
        let nl = find(src, b"\n", end);
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
            toks.push(Tok::Text(std::mem::take(&mut text)));
        }
        toks.push(Tok::Tag(js_trim(&src[open + 2..close]).to_vec()));
        i = if standalone { nl.map(|p| p + 1).unwrap_or(n) } else { end };
    }
    if !text.is_empty() {
        toks.push(Tok::Text(text));
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
    name: B,
    path: Vec<B>,
    body: Vec<Node>,
    els: Option<Vec<Node>>,
}

fn parse_template(name: &[u8], src: &[u8]) -> Result<Vec<Node>, B> {
    let toks = tokenize(name, src)?;
    let mut root: Vec<Node> = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    fn cur<'a>(root: &'a mut Vec<Node>, stack: &'a mut Vec<Frame>) -> &'a mut Vec<Node> {
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
                let tag = &tag[..];
                // FOR
                if let Some(r) = tag.strip_prefix(b"for ".as_slice()) {
                    let mut k = 0;
                    while k < r.len() && (r[k].is_ascii_alphanumeric() || r[k] == b'_') {
                        k += 1;
                    }
                    if is_ident(&r[..k]) && r[k..].starts_with(b" in ") && all_non_ws(&r[k + 4..]) {
                        let p = path_of(name, &r[k + 4..])?;
                        stack.push(Frame { is_for: true, not: false, name: r[..k].to_vec(), path: p, body: Vec::new(), els: None });
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
                        stack.push(Frame { is_for: false, not, name: Vec::new(), path: p, body: Vec::new(), els: None });
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
                    let node = if f.is_for { Node::For(f.name, f.path, f.body) } else { Node::If(f.not, f.path, f.body, f.els) };
                    cur(&mut root, &mut stack).push(node);
                    continue;
                }
                if tag.len() >= 11 && tag.starts_with(b"include \"") && tag[tag.len() - 1] == b'"' && is_name(&tag[9..tag.len() - 1]) {
                    cur(&mut root, &mut stack).push(Node::Include(tag[9..tag.len() - 1].to_vec()));
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

// ------------------------------------------------------------------ build model

struct Src {
    path: B,
    data: Data,
    body_off: usize,
    text: B,
    post: bool,
    layout: B,
    slug: B,
    out_dir: B,
    html: B,
    toc: Vec<Toc>,
    excerpt: B,
    date_long: B,
    reading: i64,
    tag_refs: Vec<(B, B, B)>,
    newer: Option<usize>,
    older: Option<usize>,
}

struct Tag {
    name: B,
    slug: B,
    url: B,
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

#[derive(Clone, Copy)]
enum V<'b> {
    Undef,
    Null,
    Bool(bool),
    Num(i64),
    Str(&'b [u8]),
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

struct LoopFrame<'b> {
    name: &'b [u8],
    x: V<'b>,
    idx: usize,
    len: usize,
}

struct Ctx<'b> {
    srcs: &'b [Src],
    urls: &'b [B],
    posts: &'b [usize],
    tags: &'b [Tag],
    recent: usize,
    per: usize,
    pages: usize,
    pager_urls: &'b [B],
    site_title: &'b [u8],
    base: &'b [u8],
    templates: &'b HashMap<B, Vec<Node>>,
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
    fn arr_get(&self, a: A, i: usize) -> V<'b> {
        V::Obj(match a {
            A::Recent => O::Card(self.posts[i]),
            A::IdxPosts(k) => O::Card(self.posts[(k - 1) * self.per + i]),
            A::TagPosts(t) => O::Card(self.tags[t].posts[i]),
            A::AllTags => O::TagSum(i),
            A::CardTags(s) => O::TagRef(s, i),
            A::Toc(s) => O::TocE(s, i),
        })
    }
    fn card_field(&self, s: usize, f: &[u8]) -> V<'b> {
        let x = &self.srcs[s];
        match f {
            b"title" => V::Str(x.data.title.as_deref().unwrap_or(b"")),
            b"url" => V::Str(&self.urls[s]),
            b"date" => V::Str(x.data.date.as_deref().unwrap_or(b"")),
            b"date_long" => V::Str(&x.date_long),
            b"summary" => V::Str(x.data.summary.as_deref().unwrap_or(b"")),
            b"excerpt" => V::Str(&x.excerpt),
            b"reading_minutes" => V::Num(x.reading),
            b"tags" => V::Arr(A::CardTags(s)),
            _ => V::Undef,
        }
    }
    fn prop(&self, v: V<'b>, f: &[u8]) -> V<'b> {
        match v {
            V::Arr(a) => {
                if f == b"length" {
                    V::Num(self.arr_len(a) as i64)
                } else {
                    V::Undef
                }
            }
            V::Obj(o) => match o {
                O::Site => match f {
                    b"title" => V::Str(self.site_title),
                    b"base_url" => V::Str(self.base),
                    _ => V::Undef,
                },
                O::Card(s) => self.card_field(s, f),
                O::Page(s) => {
                    let x = &self.srcs[s];
                    match f {
                        b"slug" => V::Str(&x.slug),
                        b"content" => V::Str(&x.html),
                        b"toc" => V::Arr(A::Toc(s)),
                        b"newer" => x.newer.map(|p| V::Obj(O::Link(p))).unwrap_or(V::Null),
                        b"older" => x.older.map(|p| V::Obj(O::Link(p))).unwrap_or(V::Null),
                        b"is_post" => V::Bool(x.post),
                        _ => self.card_field(s, f),
                    }
                }
                O::Link(s) => match f {
                    b"title" | b"url" | b"date" | b"date_long" => self.card_field(s, f),
                    _ => V::Undef,
                },
                O::TagRef(s, k) => {
                    let t = &self.srcs[s].tag_refs[k];
                    match f {
                        b"name" => V::Str(&t.0),
                        b"slug" => V::Str(&t.1),
                        b"url" => V::Str(&t.2),
                        _ => V::Undef,
                    }
                }
                O::Tag(t) | O::TagSum(t) => {
                    let tg = &self.tags[t];
                    match f {
                        b"name" => V::Str(&tg.name),
                        b"slug" => V::Str(&tg.slug),
                        b"url" => V::Str(&tg.url),
                        b"count" => V::Num(tg.posts.len() as i64),
                        b"posts" if matches!(o, O::Tag(_)) => V::Arr(A::TagPosts(t)),
                        _ => V::Undef,
                    }
                }
                O::Pager(k) => match f {
                    b"number" => V::Num(k as i64),
                    b"total" => V::Num(self.pages as i64),
                    b"url" => V::Str(&self.pager_urls[k - 1]),
                    b"newer_url" => V::Str(if k == 1 { b"" } else { &self.pager_urls[k - 2] }),
                    b"older_url" => V::Str(if k == self.pages { b"" } else { &self.pager_urls[k] }),
                    _ => V::Undef,
                },
                O::Loop(i, n) => match f {
                    b"index" => V::Num(i as i64 + 1),
                    b"first" => V::Bool(i == 0),
                    b"last" => V::Bool(i + 1 == n),
                    b"length" => V::Num(n as i64),
                    _ => V::Undef,
                },
                O::TocE(s, i) => {
                    let t = &self.srcs[s].toc[i];
                    match f {
                        b"level" => V::Num(t.level),
                        b"id" => V::Str(&t.id),
                        b"html" => V::Str(&t.html),
                        _ => V::Undef,
                    }
                }
            },
            _ => V::Undef,
        }
    }
    fn lookup(&self, path: &[B], top: Top, frames: &[LoopFrame<'b>]) -> V<'b> {
        let first = &path[0][..];
        let mut v = V::Undef;
        let mut found = false;
        for fr in frames.iter().rev() {
            if first == b"loop" {
                v = V::Obj(O::Loop(fr.idx, fr.len));
                found = true;
                break;
            }
            if first == fr.name {
                v = fr.x;
                found = true;
                break;
            }
        }
        if !found {
            v = match first {
                b"site" => V::Obj(O::Site),
                b"tags" => V::Arr(A::AllTags),
                b"recent" => V::Arr(A::Recent),
                _ => match (top, first) {
                    (Top::Page(s), b"page") => V::Obj(O::Page(s)),
                    (Top::Index(k), b"posts") => V::Arr(A::IdxPosts(k)),
                    (Top::Index(k), b"pager") => V::Obj(O::Pager(k)),
                    (Top::Tag(t), b"tag") => V::Obj(O::Tag(t)),
                    _ => V::Undef,
                },
            };
        }
        for f in &path[1..] {
            if matches!(v, V::Undef) {
                return v;
            }
            v = self.prop(v, f);
        }
        v
    }
    fn text_of(&self, v: V<'b>, esc: bool, out: &mut B) {
        match v {
            V::Str(s) => {
                if esc {
                    esc_html(out, s)
                } else {
                    out.extend_from_slice(s)
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
    fn truthy(&self, v: V<'b>) -> bool {
        match v {
            V::Arr(a) => self.arr_len(a) > 0,
            V::Undef | V::Null => false,
            V::Bool(b) => b,
            V::Num(n) => n != 0,
            V::Str(s) => !s.is_empty(),
            V::Obj(_) => true,
        }
    }
    fn render(&self, nodes: &'b [Node], top: Top, frames: &mut Vec<LoopFrame<'b>>, depth: usize, out: &mut B) {
        for n in nodes {
            match n {
                Node::Text(t) => out.extend_from_slice(t),
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
                Node::Include(name) => {
                    if let Some(t) = self.templates.get(name) {
                        if depth < 8 {
                            self.render(t, top, frames, depth + 1, out);
                        }
                    }
                }
            }
        }
    }
}

fn dir_of(p: &[u8]) -> &[u8] {
    match p.iter().rposition(|&c| c == b'/') {
        Some(k) => &p[..k],
        None => b"",
    }
}

fn words_of(s: &[u8]) -> usize {
    let mut n = 0;
    let mut inw = false;
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        let (w, l) = if c < 0x80 {
            (matches!(c, 9..=13 | 0x20), 1)
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
    n
}

fn put(out: &mut B, first: &mut bool, k: &[u8], v: &[u8]) {
    if !*first {
        out.push(b',');
    }
    *first = false;
    out.push(b'"');
    json_esc(out, k);
    out.extend_from_slice(b"\":\"");
    json_esc(out, v);
    out.push(b'"');
}

fn build(input: &J, out: &mut B) {
    let mut report: Vec<B> = Vec::new();
    // templates
    let mut templates: HashMap<B, Vec<Node>> = HashMap::new();
    if let Some(J::Obj(ts)) = input.get(b"templates") {
        let mut names: Vec<(&B, &J)> = Vec::new();
        for (k, v) in ts {
            if let Some(e) = names.iter_mut().find(|e| e.0 == k) {
                e.1 = v;
            } else {
                names.push((k, v));
            }
        }
        names.sort_by(|a, b| cmp16(a.0, b.0));
        for (k, v) in names {
            match parse_template(k, v.s()) {
                Ok(nodes) => {
                    templates.insert(k.clone(), nodes);
                }
                Err(e) => report.push(e),
            }
        }
    }
    let site = input.get(b"site").unwrap();
    let site_title = site.get(b"title").map(|x| x.s()).unwrap_or(b"");
    let mut base: &[u8] = site.get(b"base_url").map(|x| x.s()).unwrap_or(b"");
    while base.last() == Some(&b'/') {
        base = &base[..base.len() - 1];
    }
    let per = site.get(b"per_page").map(|x| x.n()).unwrap_or(10.0) as usize;
    let recent = site.get(b"recent").map(|x| x.n()).unwrap_or(0.0).max(0.0) as usize;

    // sources
    let mut files: Vec<(&[u8], &[u8])> = Vec::new();
    if let Some(J::Arr(fs)) = input.get(b"files") {
        for f in fs {
            files.push((f.get(b"path").map(|x| x.s()).unwrap_or(b""), f.get(b"content").map(|x| x.s()).unwrap_or(b"")));
        }
    }
    files.sort_by(|a, b| cmp16(a.0, b.0));
    let mut statics: Vec<(&[u8], &[u8])> = Vec::new();
    let mut srcs: Vec<Src> = Vec::new();
    for &(path, content) in &files {
        if !path.ends_with(b".md") {
            statics.push((path, content));
            continue;
        }
        let text: B = if content.contains(&b'\r') {
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
            t
        } else {
            content.to_vec()
        };
        let refuse = |report: &mut Vec<B>, why: &[u8]| {
            let mut m = b"refused ".to_vec();
            m.extend_from_slice(path);
            m.extend_from_slice(b": ");
            m.extend_from_slice(why);
            report.push(m);
        };
        let (data, body_off) = match parse_fm(&text) {
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
        let layout: B = data.layout.clone().unwrap_or_else(|| if post { b"post".to_vec() } else { b"page".to_vec() });
        if !templates.contains_key(&layout) {
            let mut m = b"layout \"".to_vec();
            m.extend_from_slice(&layout);
            m.extend_from_slice(b"\" is no template that parses");
            refuse(&mut report, &m);
            continue;
        }
        if data.draft == Some(true) {
            let mut m = b"draft ".to_vec();
            m.extend_from_slice(path);
            report.push(m);
            continue;
        }
        srcs.push(Src {
            path: path.to_vec(),
            data,
            body_off,
            text,
            post,
            layout,
            slug: Vec::new(),
            out_dir: Vec::new(),
            html: Vec::new(),
            toc: Vec::new(),
            excerpt: Vec::new(),
            date_long: Vec::new(),
            reading: 1,
            tag_refs: Vec::new(),
            newer: None,
            older: None,
        });
    }

    // permalinks
    let mut taken: HashMap<B, HashSet<B>> = HashMap::new();
    let mut by_source: HashMap<B, usize> = HashMap::new();
    let mut urls: Vec<B> = Vec::with_capacity(srcs.len());
    for (si, s) in srcs.iter_mut().enumerate() {
        let dir: B = if s.post { b"posts".to_vec() } else { dir_of(&s.path).to_vec() };
        let set = taken.entry(dir.clone()).or_insert_with(|| {
            let mut h = HashSet::new();
            if dir.is_empty() {
                h.insert(b"posts".to_vec());
                h.insert(b"tags".to_vec());
                h.insert(b"page".to_vec());
            }
            h
        });
        let want = match &s.data.slug {
            Some(x) => x.clone(),
            None => slugify(s.data.title.as_deref().unwrap()),
        };
        let slug = unique_slug(want, set);
        let mut od = Vec::new();
        if !dir.is_empty() {
            od.extend_from_slice(&dir);
            od.push(b'/');
        }
        od.extend_from_slice(&slug);
        let mut url = base.to_vec();
        url.push(b'/');
        url.extend_from_slice(&od);
        url.push(b'/');
        s.slug = slug;
        s.out_dir = od;
        urls.push(url);
        by_source.insert(s.path.clone(), si);
    }

    // markdown
    let static_paths: HashSet<B> = statics.iter().map(|x| x.0.to_vec()).collect();
    let mut broken: HashSet<B> = HashSet::new();
    for s in srcs.iter_mut() {
        let mut env = LinkEnv {
            dir: dir_of(&s.path),
            base,
            by_source: &by_source,
            urls: &urls,
            statics: &static_paths,
            src_path: &s.path,
            broken: &mut broken,
        };
        let (h, t, e) = render_md(&s.text[s.body_off..], &mut env);
        s.html = h;
        s.toc = t;
        s.excerpt = e;
        s.date_long = s.data.date.as_deref().map(long_date).unwrap_or_default();
        s.reading = ((words_of(&s.text[s.body_off..]) + 199) / 200).max(1) as i64;
    }

    // collections
    let mut posts: Vec<usize> = (0..srcs.len()).filter(|&i| srcs[i].post).collect();
    posts.sort_by(|&x, &y| {
        cmp16(srcs[y].data.date.as_deref().unwrap(), srcs[x].data.date.as_deref().unwrap()).then_with(|| cmp16(&srcs[x].slug, &srcs[y].slug))
    });
    for (k, &p) in posts.iter().enumerate() {
        srcs[p].newer = if k == 0 { None } else { Some(posts[k - 1]) };
        srcs[p].older = if k + 1 == posts.len() { None } else { Some(posts[k + 1]) };
    }

    // tags
    let mut tags: Vec<Tag> = Vec::new();
    let mut by_slug: HashMap<B, usize> = HashMap::new();
    let own_tags = |s: &Src| -> Vec<(B, B)> {
        let mut seen: Vec<B> = Vec::new();
        let mut v = Vec::new();
        if let Some(ts) = &s.data.tags {
            for name in ts {
                let sl = slugify(name);
                if !seen.contains(&sl) {
                    seen.push(sl.clone());
                    v.push((name.clone(), sl));
                }
            }
        }
        v
    };
    for &p in &posts {
        let mut refs = Vec::new();
        for (name, sl) in own_tags(&srcs[p]) {
            let ti = *by_slug.entry(sl.clone()).or_insert_with(|| {
                let mut url = base.to_vec();
                url.extend_from_slice(b"/tags/");
                url.extend_from_slice(&sl);
                url.push(b'/');
                tags.push(Tag { name, slug: sl.clone(), url, posts: Vec::new() });
                tags.len() - 1
            });
            tags[ti].posts.push(p);
            let t = &tags[ti];
            refs.push((t.name.clone(), t.slug.clone(), t.url.clone()));
        }
        srcs[p].tag_refs = refs;
    }
    for si in 0..srcs.len() {
        if srcs[si].post {
            continue;
        }
        let refs: Vec<(B, B, B)> = own_tags(&srcs[si])
            .into_iter()
            .map(|(name, sl)| match by_slug.get(&sl) {
                Some(&ti) => (tags[ti].name.clone(), tags[ti].slug.clone(), tags[ti].url.clone()),
                None => (name, sl, Vec::new()),
            })
            .collect();
        srcs[si].tag_refs = refs;
    }
    tags.sort_by(|x, y| y.posts.len().cmp(&x.posts.len()).then_with(|| cmp16(&x.slug, &y.slug)));

    // layouts
    let per_eff = per.max(1);
    let pages = ((posts.len() + per_eff - 1) / per_eff).max(1);
    let pager_urls: Vec<B> = (1..=pages)
        .map(|k| {
            let mut u = base.to_vec();
            if k == 1 {
                u.push(b'/');
            } else {
                u.extend_from_slice(format!("/page/{}/", k).as_bytes());
            }
            u
        })
        .collect();
    let ctx = Ctx {
        srcs: &srcs,
        urls: &urls,
        posts: &posts,
        tags: &tags,
        recent,
        per: per_eff,
        pages,
        pager_urls: &pager_urls,
        site_title,
        base,
        templates: &templates,
    };
    out.push(b'{');
    let mut first = true;
    let mut written: HashSet<B> = HashSet::new();
    let mut buf: B = Vec::with_capacity(1 << 16);
    let mut frames: Vec<LoopFrame> = Vec::new();
    for (si, s) in srcs.iter().enumerate() {
        buf.clear();
        ctx.render(&templates[&s.layout], Top::Page(si), &mut frames, 0, &mut buf);
        let mut key = s.out_dir.clone();
        key.extend_from_slice(b"/index.html");
        put(out, &mut first, &key, &buf);
        written.insert(key);
    }
    if let Some(t) = templates.get(b"index".as_slice()) {
        for k in 1..=pages {
            buf.clear();
            ctx.render(t, Top::Index(k), &mut frames, 0, &mut buf);
            let key = if k == 1 { b"index.html".to_vec() } else { format!("page/{}/index.html", k).into_bytes() };
            put(out, &mut first, &key, &buf);
            written.insert(key);
        }
    }
    if let Some(t) = templates.get(b"tag".as_slice()) {
        for (ti, tg) in tags.iter().enumerate() {
            buf.clear();
            ctx.render(t, Top::Tag(ti), &mut frames, 0, &mut buf);
            let mut key = b"tags/".to_vec();
            key.extend_from_slice(&tg.slug);
            key.extend_from_slice(b"/index.html");
            put(out, &mut first, &key, &buf);
            written.insert(key);
        }
    }
    if let Some(t) = templates.get(b"tags".as_slice()) {
        buf.clear();
        ctx.render(t, Top::Tags, &mut frames, 0, &mut buf);
        put(out, &mut first, b"tags/index.html", &buf);
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
                put(out, &mut first, p, c);
            }
        }
    }
    // report
    report.extend(broken.into_iter());
    report.sort_by(|a, b| cmp16(a, b));
    let npages = srcs.iter().filter(|s| !s.post).count();
    let mut rep: B = Vec::new();
    for l in &report {
        rep.extend_from_slice(l);
        rep.push(b'\n');
    }
    rep.extend_from_slice(
        format!("built {} pages, {} posts, {} tags and {} static files\n", npages, posts.len(), tags.len(), statics.len()).as_bytes(),
    );
    put(out, &mut first, b"build-report.txt", &rep);
    out.push(b'}');
}

fn main() {
    let ind = std::env::var("MAKEFUN_INPUT_DIR").unwrap_or_else(|_| "/input".to_string());
    let outd = std::env::var("MAKEFUN_OUTPUT_DIR").unwrap_or_else(|_| "/output".to_string());
    let data = std::fs::read(format!("{}/cases.json", ind)).expect("read cases.json");
    let mut p = P { s: &data, i: 0 };
    let root = p.val();
    let mut out: B = Vec::with_capacity(data.len() * 2 + (1 << 20));
    out.push(b'[');
    if let J::Arr(cases) = &root {
        for (i, c) in cases.iter().enumerate() {
            if i > 0 {
                out.push(b',');
            }
            match c.get(b"input") {
                Some(inp) => build(inp, &mut out),
                None => out.extend_from_slice(b"null"),
            }
        }
    }
    out.push(b']');
    std::fs::write(format!("{}/answers.json", outd), &out).expect("write answers.json");
    std::process::exit(0);
}
