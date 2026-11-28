//! Console setup for a readable REPL: on Windows switch the attached console to UTF-8
//! (code page 65001) and enable ANSI/VT processing for the live status line; both are
//! restored on exit.

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    #[link(name = "kernel32")]
    extern "system" {
        fn GetConsoleOutputCP() -> u32;
        fn GetConsoleCP() -> u32;
        fn SetConsoleOutputCP(cp: u32) -> i32;
        fn SetConsoleCP(cp: u32) -> i32;
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn GetConsoleMode(handle: *mut c_void, mode: *mut u32) -> i32;
        fn SetConsoleMode(handle: *mut c_void, mode: u32) -> i32;
    }

    const UTF8_CP: u32 = 65001;
    const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5; // -11
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;

    static PREV_OUT: AtomicU32 = AtomicU32::new(0);
    static PREV_IN: AtomicU32 = AtomicU32::new(0);
    static VT_VALID: AtomicBool = AtomicBool::new(false);
    static VT_PREV_MODE: AtomicU32 = AtomicU32::new(0);

    pub fn init() {
        // A zero code page means there is no attached console (pipes, mintty, a service):
        // the bytes already travel as UTF-8 and there is nothing to switch.
        let out = unsafe { GetConsoleOutputCP() };
        let inp = unsafe { GetConsoleCP() };
        PREV_OUT.store(out, Ordering::SeqCst);
        PREV_IN.store(inp, Ordering::SeqCst);
        if out != 0 && out != UTF8_CP {
            unsafe {
                SetConsoleOutputCP(UTF8_CP);
            }
        }
        if inp != 0 && inp != UTF8_CP {
            unsafe {
                SetConsoleCP(UTF8_CP);
            }
        }

        // Let the REPL repaint one status line in place (\x1b[2K\r) instead of printing
        // literal escape sequences in cmd.exe / PowerShell windows.
        let handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        if !handle.is_null() {
            let mut mode = 0u32;
            if unsafe { GetConsoleMode(handle, &mut mode) } != 0 {
                VT_PREV_MODE.store(mode, Ordering::SeqCst);
                VT_VALID.store(true, Ordering::SeqCst);
                unsafe {
                    SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
                }
            }
        }
    }

    pub fn restore() {
        let out = PREV_OUT.load(Ordering::SeqCst);
        let inp = PREV_IN.load(Ordering::SeqCst);
        if out != 0 {
            unsafe {
                SetConsoleOutputCP(out);
            }
        }
        if inp != 0 {
            unsafe {
                SetConsoleCP(inp);
            }
        }
        if VT_VALID.swap(false, Ordering::SeqCst) {
            let handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
            if !handle.is_null() {
                unsafe {
                    SetConsoleMode(handle, VT_PREV_MODE.load(Ordering::SeqCst));
                }
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn init() {}
    pub fn restore() {}
}

pub use imp::{init, restore};
