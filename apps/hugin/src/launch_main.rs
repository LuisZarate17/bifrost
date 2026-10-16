//! `hugin-launch`: what the Ctrl+Alt+H shortcut starts. It opens Hugin in Windows Terminal, in
//! the folder it was last used in (see [`launch`]).
//!
//! A windows-subsystem program, so starting it never shows a console window: a console app
//! started from a shortcut key would flash one before the terminal opened. Hence also that a
//! failure is a message box, not text on a stderr nobody sees.

#![windows_subsystem = "windows"]

#[path = "launch.rs"]
mod launch;

use std::os::windows::ffi::OsStrExt;

#[link(name = "user32")]
unsafe extern "system" {
    fn MessageBoxW(hwnd: *mut core::ffi::c_void, text: *const u16, caption: *const u16, kind: u32) -> i32;
}

/// `MB_OK | MB_ICONERROR`.
const MB_ERROR: u32 = 0x10;

fn wide(text: &str) -> Vec<u16> {
    std::ffi::OsStr::new(text).encode_wide().chain(Some(0)).collect()
}

fn main() -> std::process::ExitCode {
    match launch::launch() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(reason) => {
            let (text, caption) = (wide(&reason), wide("Hugin"));
            // SAFETY: both strings are NUL-terminated and outlive the call; no owner window.
            unsafe { MessageBoxW(core::ptr::null_mut(), text.as_ptr(), caption.as_ptr(), MB_ERROR) };
            std::process::ExitCode::from(1)
        }
    }
}
