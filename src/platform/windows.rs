use std::ffi::c_void;

#[link(name = "kernel32")]
extern "system" {
    fn AttachConsole(process: u32) -> i32;
    fn GetConsoleWindow() -> *mut c_void;
    fn GetStdHandle(kind: u32) -> *mut c_void;
    fn GetFileType(handle: *mut c_void) -> u32;
}

#[link(name = "user32")]
extern "system" {
    fn MessageBoxW(window: *mut c_void, text: *const u16, title: *const u16, flags: u32) -> i32;
}

pub(super) fn initialize() {
    // SAFETY: ATTACH_PARENT_PROCESS attaches only an existing console. It
    // never allocates a window; failure is normal for desktop launches or
    // debug builds that already have their own console. Windows preserves
    // explicitly redirected standard handles when attaching.
    unsafe { AttachConsole(u32::MAX) };
}

pub(super) fn report_error(message: &str) {
    // SAFETY: These queries accept the standard Windows handle constants.
    let has_output =
        unsafe { !GetConsoleWindow().is_null() || GetFileType(GetStdHandle((-12i32) as u32)) != 0 };
    if has_output {
        return;
    }
    let text: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    let title: Vec<u16> = "aiterm".encode_utf16().chain(Some(0)).collect();
    // SAFETY: Both UTF-16 buffers are NUL-terminated and remain alive until
    // this synchronous dialog returns. No window handle is required.
    unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), 0x10) };
}
