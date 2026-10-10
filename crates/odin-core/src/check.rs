//! Running the one check command you chose for the project folder.
//!
//! **This is the tool that lets the model find out it broke something.** An edit
//! can apply cleanly and still leave a call site pointing at a name that no longer
//! exists; nothing in G7 would ever tell the model. `cargo check` does.
//!
//! **The model never chooses the command.** Mímir detects [`candidates`] from the
//! folder's own files, a person picks one by position, and the approval ticket
//! carries the exact argv that runs. The model's only choice is *when*.
//!
//! Three things make running a build from a GUI survivable, and each has a test:
//!
//! 1. **It can be stopped.** A timeout, and a `cancelled` callback wired to the
//!    app's Stop button. Stopping kills the whole process tree — `cargo` alone
//!    would leave its `rustc` children running.
//! 2. **Its output cannot eat the context.** Only the first [`HEAD`] bytes and the
//!    last [`TAIL`] are kept, with a count of what was cut. Compiler errors lead;
//!    test summaries trail.
//! 3. **A blocked build is not a broken build.** Smart App Control refuses freshly
//!    built proc-macro DLLs on this machine (PLAN.md §3). The model is told that
//!    plainly, so it does not "fix" code that was never wrong.
//!
//! Blocking, like the rest of this crate. Tauri callers go through
//! `spawn_blocking`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::edit::Workspace;

/// Output kept from the start of a run. Compiler errors come first, and the
/// first error is usually the one that matters.
pub const HEAD: usize = 6 * 1024;

/// Output kept from the end. Where a test run says how many passed.
pub const TAIL: usize = 2 * 1024;

/// How long a check may run. Generous for `cargo check`, which is seconds warm.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(180);

/// How long a test run may run. A cold `cargo test` builds every test binary.
pub const TEST_TIMEOUT: Duration = Duration::from_secs(600);

/// How often the wait looks at the child, the clock and the Stop button.
const POLL: Duration = Duration::from_millis(100);

/// How long to wait for the last output once the command has ended.
///
/// A grandchild that outlives its parent can hold the pipe open for ever, and
/// `run_tool` would hang with it. Past this, the output so far is what the
/// model gets.
const DRAIN: Duration = Duration::from_secs(2);

// ── what can be run ─────────────────────────────────────────────────────────

/// The commands worth offering for this folder, most useful first.
///
/// Read from the folder's own manifests, so what is offered is what the project
/// already uses. `npm` is spelled `npm.cmd` on Windows: `Command` looks for the
/// exact name on `PATH`, and npm is a batch file there.
pub fn candidates(ws: &Workspace) -> Vec<Vec<String>> {
    let root = ws.root();
    let mut out = Vec::new();

    if root.join("Cargo.toml").is_file() {
        out.push(argv(&["cargo", "check"]));
        out.push(argv(&["cargo", "test"]));
    }

    if let Some(scripts) = npm_scripts(&root.join("package.json")) {
        let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
        // `npm init` writes a test script that only fails. Offering it would be
        // offering a check that can never pass.
        if scripts
            .get("test")
            .and_then(|s| s.as_str())
            .is_some_and(|s| !s.contains("no test specified"))
        {
            out.push(argv(&[npm, "test"]));
        }
        if scripts.get("build").is_some_and(|s| s.is_string()) {
            out.push(argv(&[npm, "run", "build"]));
        }
    }
    out
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

fn npm_scripts(path: &Path) -> Option<serde_json::Map<String, serde_json::Value>> {
    let text = std::fs::read_to_string(path).ok()?;
    // PowerShell 5.1 and some editors write a byte-order mark, which is not a typo.
    let json: serde_json::Value = serde_json::from_str(text.strip_prefix('\u{feff}').unwrap_or(&text)).ok()?;
    json.get("scripts")?.as_object().cloned()
}

/// A command as a person reads it.
pub fn label(argv: &[String]) -> String {
    argv.join(" ")
}

/// How long this command may run: tests get longer than a check.
pub fn timeout_for(argv: &[String]) -> Duration {
    if argv.iter().any(|a| a == "test") { TEST_TIMEOUT } else { CHECK_TIMEOUT }
}

// ── running it ──────────────────────────────────────────────────────────────

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// It finished by itself. `None` when there was no exit code.
    Exited(Option<i32>),
    /// It ran past its timeout and was killed.
    TimedOut,
    /// The user pressed Stop and it was killed.
    Stopped,
}

/// What a run produced.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub ended: Ended,
    pub elapsed: Duration,
    /// Stdout and stderr interleaved as they arrived, capped: [`HEAD`] bytes,
    /// a marker naming how much was cut, then the last [`TAIL`] bytes.
    pub output: String,
    /// Every byte the command wrote, including what was cut.
    pub bytes: u64,
    /// False when something the command started was still holding its output
    /// open after it ended, so the last of the output may be missing.
    pub complete: bool,
    /// Set when the run failed because Smart App Control refused a file the
    /// build needed — what was refused, in words. See
    /// [`blocked_by_smart_app_control`].
    pub blocked: Option<String>,
}

impl Outcome {
    pub fn passed(&self) -> bool {
        self.ended == Ended::Exited(Some(0))
    }
}

/// Run `argv` in `root` until it ends, times out, or `cancelled()` says stop.
///
/// `Err` only when the command could not be started at all — a program not on
/// `PATH`, say. Everything after that is an [`Outcome`], because a failing build
/// is a normal thing for a check to report.
///
/// `root` is used as the working directory **without** the `\\?\` prefix that
/// canonical Windows paths carry: cmd.exe, which runs `npm.cmd`, refuses a UNC
/// working directory and silently falls back to `C:\Windows`.
pub fn run(
    root: &Path,
    argv: &[String],
    timeout: Duration,
    env: &[(String, String)],
    cancelled: impl Fn() -> bool,
) -> Result<Outcome, String> {
    let (program, args) = argv.split_first().ok_or("no command was given")?;
    let dir = plain(root);

    // One pipe for both streams, so the model sees an error next to the line
    // that caused it rather than all of stdout and then all of stderr.
    let (mut reader, writer) = std::io::pipe().map_err(|e| format!("could not make a pipe: {e}"))?;
    let writer2 = writer.try_clone().map_err(|e| format!("could not make a pipe: {e}"))?;

    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(writer)
        .stderr(writer2)
        // Colour codes are noise to a model and cost tokens.
        .env("CARGO_TERM_COLOR", "never")
        .env("NO_COLOR", "1")
        // cmd.exe otherwise looks in the current folder before PATH for a bare
        // command name, so a `node.cmd` the model wrote into the project would run
        // in place of the real one.
        .env("NoDefaultCurrentDirectoryInExePath", "1");
    for (k, v) in env {
        cmd.env(k, v);
    }
    no_window(&mut cmd);

    // Everything the command starts goes in one job, so it can all be ended
    // together — when the command is stopped, when it ends but leaves something
    // running, and when Mímir itself exits mid-check.
    let job = Job::new();

    let started = Instant::now();
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        // Windows refuses to even start a program Smart App Control has blocked, so
        // there is no output to read the block from — the error is the evidence.
        // Answered as a blocked run, so the model is told what happened to it, rather
        // than as "could not start", which reads like a mistake in the command.
        Err(e) if spawn_blocked(&e) => {
            return Ok(Outcome {
                ended: Ended::Exited(None),
                elapsed: started.elapsed(),
                output: format!("could not start `{}`: {e}", label(argv)),
                bytes: 0,
                complete: true,
                blocked: Some(format!("the program `{program}`")),
            });
        }
        Err(e) => return Err(format!("could not start `{}` in {}: {e}", label(argv), dir.display())),
    };
    // The Command still holds this process's copies of the pipe's write end.
    // Until they are closed the reader never sees end-of-file.
    drop(cmd);
    // Assigned the moment it exists. A child that started a process of its own
    // in the microseconds before this would leave that one outside the job —
    // `cargo` and `npm` take milliseconds just to start, and `taskkill /T` still
    // backs the job up on Stop.
    let job = job.filter(|j| j.assign(&child));

    let captured = Arc::new(Mutex::new(Capture::default()));
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    {
        let captured = Arc::clone(&captured);
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => captured.lock().expect("capture lock").push(&buf[..n]),
                }
            }
            let _ = done_tx.send(());
        });
    }

    let ended = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ended::Exited(status.code()),
            Ok(None) => {}
            Err(_) => break Ended::Exited(None),
        }
        if cancelled() {
            kill_tree(&mut child, job.as_ref());
            break Ended::Stopped;
        }
        if started.elapsed() >= timeout {
            kill_tree(&mut child, job.as_ref());
            break Ended::TimedOut;
        }
        std::thread::sleep(POLL);
    };
    let elapsed = started.elapsed();
    // Anything the command left running when it ended by itself — a server a
    // test started, say — ends with it. The check is over.
    if let Some(job) = &job {
        job.kill();
    }

    // Whatever arrives in time. A reader still blocked past this is left behind:
    // it ends by itself when whatever holds the pipe exits.
    let complete = done_rx.recv_timeout(DRAIN).is_ok();
    let capture = captured.lock().expect("capture lock");
    let output = capture.render();
    let passed = ended == Ended::Exited(Some(0));
    let blocked = if passed { None } else { blocked_by_smart_app_control(&dir, &output) };
    Ok(Outcome { ended, elapsed, output, bytes: capture.total, complete, blocked })
}

/// `\\?\C:\x` → `C:\x`, and `\\?\UNC\server\share\x` → `\\server\share\x`.
///
/// Stripping the prefix alone would turn a network share into
/// `UNC\server\share\x` — a relative path, and a check that cannot start.
fn plain(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s))
}

/// A Windows job object that ends everything in it when it is killed or
/// dropped — and, because the handle closes with the process, when Mímir exits.
#[cfg(windows)]
struct Job(windows::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Job {
    fn new() -> Option<Self> {
        use windows::Win32::System::JobObjects::{
            CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        // SAFETY: a new, unnamed job with default security; the handle is owned
        // by the returned value and closed in Drop. The information passed is a
        // correctly sized JOBOBJECT_EXTENDED_LIMIT_INFORMATION that outlives the
        // call.
        unsafe {
            let job = Job(CreateJobObjectW(None, windows::core::PCWSTR::null()).ok()?);
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .ok()?;
            Some(job)
        }
    }

    fn assign(&self, child: &std::process::Child) -> bool {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::System::JobObjects::AssignProcessToJobObject;
        // SAFETY: both handles are live — the job's is owned by self, and the
        // child's by `child`, which outlives this call.
        unsafe {
            AssignProcessToJobObject(self.0, windows::Win32::Foundation::HANDLE(child.as_raw_handle())).is_ok()
        }
    }

    fn kill(&self) {
        // SAFETY: the job handle is owned by self and still open.
        unsafe {
            let _ = windows::Win32::System::JobObjects::TerminateJobObject(self.0, 1);
        }
    }
}

#[cfg(windows)]
impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: closed exactly once, here. Closing it also ends anything still
        // in the job, which is what KILL_ON_JOB_CLOSE was set for.
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

/// Elsewhere there is no job; Stop falls back to killing the child alone.
#[cfg(not(windows))]
struct Job;

#[cfg(not(windows))]
impl Job {
    fn new() -> Option<Self> {
        None
    }
    fn assign(&self, _: &std::process::Child) -> bool {
        false
    }
    fn kill(&self) {}
}

#[cfg(windows)]
fn no_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    // Mímir has no console, so every console program it starts would otherwise
    // get a window of its own flashing up. CREATE_NO_WINDOW alone — see
    // `ollama::spawn_detached` for why not DETACHED_PROCESS as well.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn no_window(_: &mut Command) {}

/// Kill the command **and everything it started.** `Child::kill` ends only the
/// process itself; `cargo` would die and leave `rustc` compiling. The job does
/// it in one call; `taskkill /T` walks the tree too, for anything the job did not
/// catch.
fn kill_tree(child: &mut std::process::Child, job: Option<&Job>) {
    if let Some(job) = job {
        job.kill();
    }
    #[cfg(windows)]
    {
        let mut kill = Command::new("taskkill");
        kill.args(["/T", "/F", "/PID", &child.id().to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        no_window(&mut kill);
        let _ = kill.status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// The start and the end of the output, and how much there was.
#[derive(Default)]
struct Capture {
    head: Vec<u8>,
    tail: std::collections::VecDeque<u8>,
    total: u64,
}

impl Capture {
    fn push(&mut self, mut bytes: &[u8]) {
        self.total += bytes.len() as u64;
        let room = HEAD.saturating_sub(self.head.len());
        let take = room.min(bytes.len());
        self.head.extend_from_slice(&bytes[..take]);
        bytes = &bytes[take..];
        for &b in bytes {
            if self.tail.len() == TAIL {
                self.tail.pop_front();
            }
            self.tail.push_back(b);
        }
    }

    /// **Decoded together when nothing was cut**, because the head/tail boundary is
    /// then arbitrary and may fall inside a character. At a real cut, the head's
    /// last character and the tail's first may be split by the cut itself; the
    /// broken pieces are dropped rather than shown as U+FFFD beside the marker.
    fn render(&self) -> String {
        let kept = (self.head.len() + self.tail.len()) as u64;
        let tail: Vec<u8> = self.tail.iter().copied().collect();
        let cut = self.total - kept;
        if cut == 0 {
            let mut all = self.head.clone();
            all.extend_from_slice(&tail);
            return String::from_utf8_lossy(&all).into_owned();
        }
        let mut head = &self.head[..];
        if let Err(e) = std::str::from_utf8(head) {
            // `error_len() == None` means the bytes end in the start of a character.
            if e.error_len().is_none() {
                head = &head[..e.valid_up_to()];
            }
        }
        let skip = tail.iter().take(3).take_while(|b| (**b & 0xC0) == 0x80).count();
        let head = String::from_utf8_lossy(head);
        let tail = String::from_utf8_lossy(&tail[skip..]);
        format!("{head}\n[… {cut} bytes cut …]\n{tail}")
    }
}

// ── telling the model ───────────────────────────────────────────────────────

/// What the model is told about a run.
///
/// **Never starts with `error:`.** `tools::prior_failures` counts a result that
/// does as a failed call, and three identical failed calls end the turn. A check
/// that reports a broken build has *worked* — the model should read it and fix
/// the code, not be stopped for asking twice.
pub fn for_model(argv: &[String], outcome: &Outcome) -> String {
    let name = label(argv);
    let secs = outcome.elapsed.as_secs_f64();

    if let Some(what) = &outcome.blocked {
        return format!(
            "`{name}` could not finish: Windows Smart App Control blocked {what}. This is not a \
             problem in the code — do not change any files because of it. Tell the user; \
             rebuilding usually clears it."
        );
    }

    let head = match &outcome.ended {
        Ended::Exited(Some(0)) => format!("`{name}` passed (exit 0, {secs:.1} s)."),
        Ended::Exited(Some(code)) => format!("`{name}` failed (exit {code}, {secs:.1} s)."),
        Ended::Exited(None) => format!("`{name}` ended without an exit code after {secs:.1} s."),
        Ended::TimedOut => format!("`{name}` ran for {secs:.0} s, past its time limit, and was stopped."),
        Ended::Stopped => format!("`{name}` was stopped by the user after {secs:.1} s."),
    };
    let head = if outcome.complete {
        head
    } else {
        format!("{head} Something it started was still running, so the output may be incomplete.")
    };
    let output = outcome.output.trim_end();
    if output.is_empty() {
        head
    } else {
        format!("{head} Output:\n\n{output}")
    }
}

/// Whether a command that could not be started was refused by Smart App Control:
/// Windows error 4551, "An Application Control policy has blocked this file".
/// Pure, so it can be tested without a blocked file.
fn spawn_blocked(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(4551) || e.to_string().contains("Application Control policy has blocked")
}

/// The signs that Smart App Control, not the code, stopped a **failed** build
/// run in `dir`. Only ever asked of a run that failed: a passing build was not
/// blocked, whatever its output happens to quote.
///
/// Two shapes, both seen on this machine (PLAN.md §3, A4 and G7 results):
///
/// - **An executable refused** — a build script or a test binary. Windows says
///   so outright: `os error 4551`, "An Application Control policy has blocked
///   this file".
/// - **A library refused** — `rustc` cannot load a proc-macro DLL and reports
///   `E0463 can't find crate` for a crate that is **in the project's
///   `Cargo.lock`**: a dependency the project really has, that the compiler
///   could not open. Seen for `zerovec_derive` and for `tauri`. The same error
///   for a crate the lockfile does not list is the model's mistake — a `use` of
///   something the project does not depend on — and it must be told to fix it.
pub fn blocked_by_smart_app_control(dir: &Path, output: &str) -> Option<String> {
    if output.contains("os error 4551") || output.contains("Application Control policy has blocked") {
        return Some("a program the build needed".to_string());
    }
    let lock = cargo_lock(dir)?;
    for line in output.lines().filter(|l| l.contains("E0463")) {
        let Some(start) = line.find("can't find crate for `") else { continue };
        let rest = &line[start + "can't find crate for `".len()..];
        let Some(end) = rest.find('`') else { continue };
        let krate = &rest[..end];
        // rustc names a crate with underscores; Cargo.lock names its package,
        // usually with hyphens.
        let listed = [krate.to_string(), krate.replace('_', "-")]
            .iter()
            .any(|name| lock.contains(&format!("name = \"{name}\"")));
        if listed {
            return Some(format!("`{krate}`, a library the project depends on"));
        }
    }
    None
}

/// The `Cargo.lock` that governs `dir`: its own, or its workspace's, above it.
fn cargo_lock(dir: &Path) -> Option<String> {
    dir.ancestors().find_map(|d| std::fs::read_to_string(d.join("Cargo.lock")).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway folder, removed on drop — the same shape as `edit`'s Sandbox.
    struct Folder(PathBuf);

    impl Folder {
        fn new(tag: &str) -> Self {
            let n = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            let dir = std::env::temp_dir().join(format!("odin-check-{tag}-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn write(&self, name: &str, text: &str) {
            std::fs::write(self.0.join(name), text).unwrap();
        }
        fn ws(&self) -> Workspace {
            Workspace::open(&self.0).unwrap()
        }
    }

    impl Drop for Folder {
        /// **Retried, because a process that is a moment from ending can still have
        /// the folder as its working directory** — `ping` after the "linger" test's
        /// check — and Windows then refuses to remove it. One try, with the error
        /// ignored, is how an `odin-check-linger-…` folder was left in `%TEMP%`.
        fn drop(&mut self) {
            for _ in 0..50 {
                if std::fs::remove_dir_all(&self.0).is_ok() || !self.0.exists() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }

    fn cmd(parts: &[&str]) -> Vec<String> {
        argv(parts)
    }

    fn never() -> bool {
        false
    }

    // ── candidates ──

    #[test]
    fn a_cargo_project_offers_check_then_test() {
        let f = Folder::new("cargo");
        f.write("Cargo.toml", "[package]\nname = \"x\"\n");
        assert_eq!(candidates(&f.ws()), vec![cmd(&["cargo", "check"]), cmd(&["cargo", "test"])]);
    }

    #[test]
    fn npm_scripts_are_offered_only_when_they_exist() {
        let f = Folder::new("npm");
        f.write("package.json", r#"{"scripts":{"test":"vitest run","build":"vite build"}}"#);
        let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
        assert_eq!(candidates(&f.ws()), vec![cmd(&[npm, "test"]), cmd(&[npm, "run", "build"])]);
    }

    #[test]
    fn npm_inits_placeholder_test_is_not_a_check() {
        let f = Folder::new("npm-init");
        f.write(
            "package.json",
            r#"{"scripts":{"test":"echo \"Error: no test specified\" && exit 1"}}"#,
        );
        assert!(candidates(&f.ws()).is_empty());
    }

    #[test]
    fn a_folder_with_no_manifest_offers_nothing() {
        let f = Folder::new("empty");
        f.write("notes.txt", "hello");
        assert!(candidates(&f.ws()).is_empty());
    }

    #[test]
    fn tests_get_the_longer_timeout() {
        assert_eq!(timeout_for(&cmd(&["cargo", "check"])), CHECK_TIMEOUT);
        assert_eq!(timeout_for(&cmd(&["cargo", "test"])), TEST_TIMEOUT);
        assert_eq!(timeout_for(&cmd(&["npm.cmd", "test"])), TEST_TIMEOUT);
    }

    // ── running ──
    //
    // Driven through `cmd /c`, which every Windows machine has, so these need no
    // toolchain and no network.

    #[cfg(windows)]
    #[test]
    fn a_passing_command_is_reported_with_its_output() {
        let f = Folder::new("pass");
        let out = run(&f.0, &cmd(&["cmd", "/c", "echo all good"]), CHECK_TIMEOUT, &[], never).unwrap();
        assert!(out.passed());
        assert!(out.output.contains("all good"), "{:?}", out.output);
    }

    #[cfg(windows)]
    #[test]
    fn a_failing_command_keeps_its_exit_code_and_both_streams() {
        let f = Folder::new("fail");
        let out = run(
            &f.0,
            &cmd(&["cmd", "/c", "echo to-stdout & echo to-stderr 1>&2 & exit 3"]),
            CHECK_TIMEOUT,
            &[],
            never,
        )
        .unwrap();
        assert_eq!(out.ended, Ended::Exited(Some(3)));
        assert!(out.output.contains("to-stdout") && out.output.contains("to-stderr"), "{:?}", out.output);
    }

    #[cfg(windows)]
    #[test]
    fn it_runs_in_the_plain_folder_not_the_verbatim_one() {
        // Workspace::root() is the `\\?\C:\…` form. Handed to cmd.exe as a working
        // directory that would be refused and replaced with C:\Windows.
        let f = Folder::new("cwd");
        let ws = f.ws();
        assert!(ws.root().to_string_lossy().starts_with(r"\\?\"));
        let out = run(ws.root(), &cmd(&["cmd", "/c", "cd"]), CHECK_TIMEOUT, &[], never).unwrap();
        let printed = out.output.trim().to_lowercase();
        assert_eq!(printed, ws.root_display().to_lowercase());
    }

    #[cfg(windows)]
    #[test]
    fn a_command_past_its_timeout_is_killed() {
        let f = Folder::new("timeout");
        let started = Instant::now();
        let out = run(
            &f.0,
            &cmd(&["cmd", "/c", "ping -n 30 127.0.0.1 >nul"]),
            Duration::from_millis(500),
            &[],
            never,
        )
        .unwrap();
        assert_eq!(out.ended, Ended::TimedOut);
        assert!(out.complete, "the killed command's children kept the output open");
        assert!(started.elapsed() < Duration::from_secs(10), "took {:?}", started.elapsed());
    }

    /// **Stop kills the tree.** `cmd` starts `ping` as a child; killing only
    /// `cmd` would leave `ping` holding the pipe, and the reader would wait out
    /// the whole thirty seconds.
    #[cfg(windows)]
    #[test]
    fn stop_kills_the_command_and_its_children() {
        let f = Folder::new("stop");
        let started = Instant::now();
        let out = run(
            &f.0,
            &cmd(&["cmd", "/c", "ping -n 30 127.0.0.1"]),
            CHECK_TIMEOUT,
            &[],
            || started.elapsed() > Duration::from_millis(700),
        )
        .unwrap();
        assert_eq!(out.ended, Ended::Stopped);
        // A surviving `ping` would still hold the pipe, and the reader would be
        // abandoned after DRAIN rather than reaching end-of-file.
        assert!(out.complete, "a child outlived the stop and kept the output open");
    }

    #[test]
    fn a_program_that_does_not_exist_is_an_error() {
        let f = Folder::new("missing");
        let err = run(&f.0, &cmd(&["no-such-program-odin"]), CHECK_TIMEOUT, &[], never).unwrap_err();
        assert!(err.contains("could not start"), "{err}");
    }

    // ── output ──

    #[test]
    fn long_output_keeps_the_start_and_the_end() {
        let mut c = Capture::default();
        c.push(b"FIRST");
        c.push(&vec![b'x'; HEAD + TAIL + 5000]);
        c.push(b"LAST");
        let text = c.render();
        assert!(text.starts_with("FIRST"));
        assert!(text.ends_with("LAST"));
        assert!(text.contains("bytes cut"));
        assert!(text.len() < HEAD + TAIL + 100);
    }

    #[test]
    fn short_output_is_kept_whole() {
        let mut c = Capture::default();
        c.push(b"one\n");
        c.push(b"two\n");
        assert_eq!(c.render(), "one\ntwo\n");
    }

    fn outcome(ended: Ended, output: &str) -> Outcome {
        Outcome {
            ended,
            elapsed: Duration::from_millis(1500),
            output: output.into(),
            bytes: output.len() as u64,
            complete: true,
            blocked: None,
        }
    }

    /// A project whose lockfile lists these packages, for asking whether an
    /// E0463 names a real dependency.
    fn locked(tag: &str, packages: &[&str]) -> Folder {
        let f = Folder::new(tag);
        let body: String =
            packages.iter().map(|p| format!("[[package]]\nname = \"{p}\"\nversion = \"1.0.0\"\n\n")).collect();
        f.write("Cargo.lock", &format!("version = 4\n\n{body}"));
        f
    }

    #[test]
    fn what_the_model_is_told_never_reads_as_a_failed_call() {
        let check = cmd(&["cargo", "check"]);
        for ended in [Ended::Exited(Some(0)), Ended::Exited(Some(101)), Ended::TimedOut, Ended::Stopped] {
            let said = for_model(&check, &outcome(ended, "error[E0425]: cannot find function `greet`"));
            assert!(!said.starts_with("error:"), "{said}");
        }
    }

    #[test]
    fn a_failed_build_says_so_and_carries_the_error() {
        let said = for_model(
            &cmd(&["cargo", "check"]),
            &outcome(Ended::Exited(Some(101)), "error[E0425]: cannot find function `greet` in this scope"),
        );
        assert!(said.starts_with("`cargo check` failed (exit 101"), "{said}");
        assert!(said.contains("cannot find function `greet`"));
    }

    #[test]
    fn a_blocked_run_tells_the_model_not_to_touch_the_code() {
        let o = Outcome { blocked: Some("`tauri`, a library".into()), ..outcome(Ended::Exited(Some(101)), "") };
        let said = for_model(&cmd(&["cargo", "check"]), &o);
        assert!(said.contains("Smart App Control") && said.contains("do not change any files"), "{said}");
    }

    /// Both blocks seen on this machine: a proc-macro with the tell-tale name,
    /// and `tauri`, which has none — what they share is being in the lockfile.
    #[test]
    fn a_locked_crate_the_compiler_cannot_open_is_a_block() {
        let f = locked("lock", &["zerovec-derive", "tauri"]);
        let zerovec = "error[E0463]: can't find crate for `zerovec_derive`";
        let tauri = "error[E0463]: can't find crate for `tauri`";
        assert!(blocked_by_smart_app_control(&f.0, zerovec).is_some());
        assert!(blocked_by_smart_app_control(&f.0, tauri).is_some());
    }

    /// **A missing crate the project does not depend on is the model's
    /// mistake** — `extern crate serde_derive;` with no serde — and it must be
    /// told to fix it, not that Windows is to blame. Even with a macro's name.
    #[test]
    fn a_crate_the_lockfile_does_not_list_is_a_code_problem() {
        let f = locked("nolock", &["tauri"]);
        assert!(blocked_by_smart_app_control(&f.0, "error[E0463]: can't find crate for `serde_derive`").is_none());
        let bare = Folder::new("nolockfile");
        assert!(blocked_by_smart_app_control(&bare.0, "error[E0463]: can't find crate for `serde`").is_none());
    }

    #[test]
    fn a_blocked_executable_is_a_block_wherever_it_happens() {
        let f = Folder::new("exe");
        let out = "error: could not execute process `target\\debug\\deps\\x.exe` (never executed)\n\
                   Caused by: An Application Control policy has blocked this file. (os error 4551)";
        assert!(blocked_by_smart_app_control(&f.0, out).is_some());
    }

    /// Only a failed run is asked. A passing build whose output happens to quote
    /// the words — a test that prints them — was not blocked.
    #[cfg(windows)]
    #[test]
    fn a_passing_run_is_never_called_blocked() {
        let f = locked("passing", &["tauri"]);
        let out = run(
            &f.0,
            &cmd(&["cmd", "/c", "echo error[E0463]: can't find crate for `tauri` (os error 4551)"]),
            CHECK_TIMEOUT,
            &[],
            never,
        )
        .unwrap();
        assert!(out.passed());
        assert_eq!(out.blocked, None);
        assert!(for_model(&cmd(&["cargo", "check"]), &out).contains("passed"));
    }

    /// **What a check leaves running ends with it.** `start /b` runs `ping` in the
    /// background and `cmd` exits at once; without the job, `ping` would run on,
    /// holding the output open until the reader gave up on it.
    #[cfg(windows)]
    #[test]
    fn what_a_check_leaves_running_is_ended_when_it_finishes() {
        let f = Folder::new("linger");
        let started = Instant::now();
        let out = run(&f.0, &cmd(&["cmd", "/c", "start /b ping -n 30 127.0.0.1 >nul"]), CHECK_TIMEOUT, &[], never)
            .unwrap();
        assert!(out.passed());
        assert!(out.complete, "the background ping outlived the check");
        assert!(started.elapsed() < DRAIN, "took {:?}", started.elapsed());
    }

    #[test]
    fn a_character_on_the_head_boundary_survives_when_nothing_was_cut() {
        let mut c = Capture::default();
        let mut v = vec![b'a'; HEAD - 1];
        v.extend_from_slice("\u{e9}".as_bytes());
        v.extend_from_slice(&[b'b'; 100]);
        c.push(&v);
        let text = c.render();
        assert!(text.contains('\u{e9}') && !text.contains('\u{fffd}') && !text.contains("bytes cut"));
    }

    #[test]
    fn a_cut_drops_the_split_characters_at_both_ends_of_the_marker() {
        let mut c = Capture::default();
        let total = 20_000usize;
        let mut v = vec![b'a'; total];
        v[HEAD - 1..HEAD + 1].copy_from_slice("\u{e9}".as_bytes());
        let tail_start = total - TAIL;
        v[tail_start - 1..tail_start + 2].copy_from_slice("\u{2500}".as_bytes());
        c.push(&v);
        let text = c.render();
        assert!(text.contains("bytes cut") && !text.contains('\u{fffd}'), "{}", text.matches('\u{fffd}').count());
    }

    #[test]
    fn a_bom_in_package_json_does_not_hide_the_npm_checks() {
        let f = Folder::new("npm-bom");
        f.write("package.json", "\u{feff}{\"scripts\":{\"test\":\"vitest run\"}}");
        assert_eq!(candidates(&f.ws()).len(), 1);
    }

    /// Windows error 4551 is Smart App Control refusing to start the file, however
    /// the error text is worded; any other start failure is not.
    #[test]
    fn a_start_refused_by_smart_app_control_is_recognised() {
        assert!(spawn_blocked(&std::io::Error::from_raw_os_error(4551)));
        assert!(!spawn_blocked(&std::io::Error::from_raw_os_error(2)));
        assert!(!spawn_blocked(&std::io::Error::other("nothing to do with it")));
    }

    #[test]
    fn a_network_share_becomes_a_path_cmd_can_use() {
        assert_eq!(plain(Path::new(r"\\?\UNC\server\share\proj")), PathBuf::from(r"\\server\share\proj"));
        assert_eq!(plain(Path::new(r"\\?\C:\work\proj")), PathBuf::from(r"C:\work\proj"));
        assert_eq!(plain(Path::new(r"C:\work\proj")), PathBuf::from(r"C:\work\proj"));
    }
}
