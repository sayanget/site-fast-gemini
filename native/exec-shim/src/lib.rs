// bin/sitegen.node: the cheapest way from `node main.ts` into the native engine. run.js names the engine in
// $SITEGEN_EXEC and process.dlopen()s this object; its ELF constructor execve()s the engine, which replaces node (and
// ends node's other threads) without node's own process.execve, whose JS side loads internal/worker (~1.4 ms) and
// whose failure aborts node. If the exec cannot happen the constructor just returns, dlopen throws ("did not
// self-register") and run.js falls back. No libc is linked in: getenv, execve and environ resolve against the libc
// node already has loaded (glibc or musl), so the object has no DT_NEEDED at all.
#![no_std]

use core::ffi::c_char;

extern "C" {
    static environ: *const *const c_char;
    fn getenv(name: *const c_char) -> *const c_char;
    fn execve(path: *const c_char, argv: *const *const c_char, envp: *const *const c_char) -> i32;
}

extern "C" fn exec_engine() {
    unsafe {
        let p = getenv(c"SITEGEN_EXEC".as_ptr());
        if !p.is_null() {
            let argv = [p, core::ptr::null()];
            execve(p, argv.as_ptr(), environ);
        }
    }
}

#[used]
#[link_section = ".init_array"]
static INIT: extern "C" fn() = exec_engine;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
