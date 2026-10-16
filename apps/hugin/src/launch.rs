//! Launching Hugin: open it in Windows Terminal, in the folder it was last used in.
//!
//! Shared by `hugin --launch` and the `hugin-launch` binary, which is a windowless program
//! (no console flash) for a shortcut key to start. The pure parts, the folder choice and the
//! `wt` argument list, are tested in `launch_tests.rs`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use bifrost_core::config;

/// `CREATE_NEW_CONSOLE`, for the plain console when Windows Terminal is not there.
const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

/// The folder to open in: the recorded one if it is a directory that exists, else the
/// user's profile folder, else the current one. The check is injected so a test needs no disk.
pub fn choose_folder(recorded: Option<String>, profile: Option<String>, is_dir: impl Fn(&str) -> bool) -> String {
    recorded
        .filter(|folder| is_dir(folder))
        .or(profile.filter(|p| !p.is_empty()))
        .unwrap_or_else(|| ".".to_string())
}

/// Windows Terminal splits its command line at `;`, so a `;` that is part of a path is `\;`.
fn escape(text: &str) -> String {
    text.replace(';', r"\;")
}

/// The arguments for `wt.exe`: always a new window, never a tab in one that is open.
pub fn wt_args(folder: &str, program: &str) -> Vec<String> {
    ["-w", "new", "new-tab", "--title", "Hugin", "-d"]
        .into_iter()
        .map(String::from)
        .chain([escape(folder), escape(program)])
        .collect()
}

/// Which way Hugin was started, for the splash that waits for its window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Started {
    /// In a new Windows Terminal window; `wt.exe` hands the work to a process that is not ours.
    Terminal,
    /// In a console of its own, by this process id.
    Console(u32),
}

/// What to start and where: the `hugin.exe` beside this program, and the folder to open in.
pub fn target() -> Result<(PathBuf, String), String> {
    let program = std::env::current_exe()
        .map_err(|e| format!("cannot find hugin.exe: {e}"))?
        .with_file_name("hugin.exe");
    let folder = choose_folder(config::load().hugin_last_folder, std::env::var("USERPROFILE").ok(), |f| {
        Path::new(f).is_dir()
    });
    Ok((program, folder))
}

/// Start Hugin in a terminal window, never waiting on it. `Ok` says which way it was started;
/// the `Err` says why not.
pub fn start(program: &Path, folder: &str) -> Result<Started, String> {
    let program_text = program.to_string_lossy().into_owned();

    let wt = Command::new("wt.exe")
        .args(wt_args(folder, &program_text))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let wt_error = match wt {
        Ok(_) => return Ok(Started::Terminal),
        Err(e) => e,
    };

    new_console(program, folder)
        .map(Started::Console)
        .map_err(|e| format!("could not start Windows Terminal ({wt_error}) or {program_text} ({e})"))
}

/// Start `program` in a console window of its own, in `folder` — what happens where Windows
/// Terminal is not installed. The `Ok` is the new process's id.
///
/// **Not through `std::process`**, which always hands the child its parent's standard handles
/// (`STARTF_USESTDHANDLES`). `hugin-launch` is windowless and has none, so Hugin would start
/// with nothing to read or write, take itself for a script, and end at once — a window that
/// flashes and closes. Without that flag, Windows gives the child its new console's own
/// handles, and Hugin sees a terminal.
fn new_console(program: &Path, folder: &str) -> Result<u32, String> {
    use std::ffi::{c_void, OsStr};
    use std::os::windows::ffi::OsStrExt;

    #[repr(C)]
    struct StartupInfo {
        cb: u32,
        reserved: *mut u16,
        desktop: *mut u16,
        title: *mut u16,
        x: u32,
        y: u32,
        x_size: u32,
        y_size: u32,
        x_chars: u32,
        y_chars: u32,
        fill: u32,
        flags: u32,
        show: u16,
        reserved2_len: u16,
        reserved2: *mut u8,
        stdin: *mut c_void,
        stdout: *mut c_void,
        stderr: *mut c_void,
    }
    #[repr(C)]
    struct ProcessInfo {
        process: *mut c_void,
        thread: *mut c_void,
        pid: u32,
        tid: u32,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateProcessW(
            application: *const u16,
            command_line: *mut u16,
            process_attributes: *const c_void,
            thread_attributes: *const c_void,
            inherit_handles: i32,
            flags: u32,
            environment: *const c_void,
            directory: *const u16,
            startup: *const StartupInfo,
            info: *mut ProcessInfo,
        ) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    let wide = |s: &OsStr| s.encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let application = wide(program.as_os_str());
    // CreateProcessW may write into the command line, so it is a buffer of our own.
    let mut command_line = wide(OsStr::new(&format!("\"{}\"", program.display())));
    let directory = wide(OsStr::new(folder));
    // SAFETY: both are plain C structs of integers and pointers, for which zero is valid.
    let mut startup: StartupInfo = unsafe { std::mem::zeroed() };
    startup.cb = size_of::<StartupInfo>() as u32;
    let mut info: ProcessInfo = unsafe { std::mem::zeroed() };
    // SAFETY: every pointer is to a live, NUL-terminated buffer or a struct of the size given,
    // and nothing is inherited.
    let started = unsafe {
        CreateProcessW(
            application.as_ptr(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            CREATE_NEW_CONSOLE,
            std::ptr::null(),
            directory.as_ptr(),
            &startup,
            &mut info,
        )
    };
    if started == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    // SAFETY: the two handles CreateProcessW returned, closed once; the process runs on.
    unsafe {
        CloseHandle(info.thread);
        CloseHandle(info.process);
    }
    Ok(info.pid)
}
