//! Running the one check command you chose for the project folder.
//!
//! **This is the tool that lets the model find out it broke something.** An edit
//! can apply cleanly and still leave a call site pointing at a name that no longer
//! exists; nothing in G7 would ever tell the model. `cargo check` does.
//!
//! **The model never chooses the command.** Mimir detects [`candidates`] from the
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

/// The most memory any one process in a run may commit. Model-written code runs
/// here, and on 2026-11-01 a runaway loop in one test committed 81 GB and took the
/// whole machine down. Past this, an allocation fails and the process ends with
/// an error, which reads as a failed run. A debug link of this workspace needs
/// well under it.
pub const PROCESS_MEMORY_LIMIT: usize = 8 * 1024 * 1024 * 1024;

/// The most memory a whole run may commit, every process it starts together, so
/// code that starts many processes cannot add up past [`PROCESS_MEMORY_LIMIT`].
/// **Measured 2026-11-01:** a rebuild of this workspace's own crates at cargo's
/// default 16 parallel jobs peaked at 11.8 GB of commit across 17 build
/// processes, so 16 GB left too little room.
pub const RUN_MEMORY_LIMIT: usize = 24 * 1024 * 1024 * 1024;

/// A run is killed when the free space on its drive falls below this, **or below
/// half of what the drive had free when the run started, if that is less**
/// (`disk_floor_for`): a drive that begins under the floor would otherwise have
/// every run killed at its first look. A loop writing files fills a disk in
/// minutes, well inside a timeout, and a full system drive takes Windows down as
/// surely as running out of memory.
pub const DISK_FLOOR: u64 = 10 * 1024 * 1024 * 1024;

/// The free space below which a run that started with `start_free` bytes free is
/// killed: [`DISK_FLOOR`], or half of `start_free` when the drive began under twice
/// that. A run can then use up to half of what a nearly full drive has left, and
/// no more.
fn disk_floor_for(start_free: u64) -> u64 {
    DISK_FLOOR.min(start_free / 2)
}

/// How often the free space is looked at, in polls.
const DISK_EVERY: u32 = 10;

/// How often the wait looks at the child, the clock and the Stop button.
const POLL: Duration = Duration::from_millis(100);

/// How long to wait for the last output once the command has ended.
///
/// A grandchild that outlives its parent can hold the pipe open for ever, and
/// `run_approved` would hang with it. Past this, the output so far is what the
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
    if argv.iter().any(|a| a == "test") {
        TEST_TIMEOUT
    } else {
        CHECK_TIMEOUT
    }
}

// ── the shell a command runs in ─────────────────────────────────────────────

/// Where a shell command runs. Hugin offers the model a tool for any command, and
/// the shell decides what its text means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    PowerShell,
    Cmd,
    Bash,
}

impl Shell {
    /// Every shell, in the order they are listed.
    pub const ALL: [Shell; 3] = [Shell::PowerShell, Shell::Cmd, Shell::Bash];

    /// The word the config and `--shell` use.
    pub fn name(self) -> &'static str {
        match self {
            Shell::PowerShell => "powershell",
            Shell::Cmd => "cmd",
            Shell::Bash => "bash",
        }
    }

    /// The shell as a person reads it.
    pub fn label(self) -> &'static str {
        match self {
            Shell::PowerShell => "Windows PowerShell",
            Shell::Cmd => "cmd.exe",
            Shell::Bash => "Git Bash",
        }
    }

    /// The shell a word names, or `None`.
    pub fn parse(word: &str) -> Option<Shell> {
        match word.trim().to_ascii_lowercase().as_str() {
            "powershell" => Some(Shell::PowerShell),
            "cmd" => Some(Shell::Cmd),
            "bash" => Some(Shell::Bash),
            _ => None,
        }
    }

    /// Whether it can be started on this machine. Only Git Bash can be missing.
    pub fn available(self) -> bool {
        self != Shell::Bash || bash_path().is_some()
    }
}

/// Git for Windows' `bin\bash.exe`: under `%ProgramFiles%\Git`, or beside the
/// `git.exe` found on `PATH`. **Never `C:\Windows\System32\bash.exe`**, which is the
/// launcher for WSL and would run the command in a Linux distribution.
pub fn bash_path() -> Option<PathBuf> {
    if let Some(pf) = std::env::var_os("ProgramFiles") {
        let p = PathBuf::from(pf).join("Git").join("bin").join("bash.exe");
        if p.is_file() {
            return Some(p);
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        let git = dir.join("git.exe");
        if !git.is_file() {
            return None;
        }
        // `Git\cmd\git.exe` → `Git\bin\bash.exe`; `Git\bin\git.exe` → itself.
        let p = dir.join("..").join("bin").join("bash.exe");
        p.is_file().then(|| p.components().collect::<PathBuf>())
    })
}

/// What PowerShell is given to run: `command` on lines of its own, between a preamble —
/// no progress bars, UTF-8 out — and a close that **ends with the exit code of the last
/// program that failed**. Left to itself `powershell -Command` ends with 1 whenever the
/// last thing failed, so `cargo build` failing with 101 would read as 1, and the model
/// would be told a number cargo never said. A cmdlet that fails still ends it with 1.
fn powershell_script(command: &str) -> String {
    const START: &str = "$ProgressPreference='SilentlyContinue'; [Console]::OutputEncoding=[Text.Encoding]::UTF8; $global:LASTEXITCODE=0";
    const END: &str =
        "$bifrostOk=$?; if ($global:LASTEXITCODE) { exit $global:LASTEXITCODE } elseif (-not $bifrostOk) { exit 1 }";
    format!("{START}\n{command}\n{END}")
}

/// How long a command line Windows starts a process with, less a margin.
const MAX_COMMAND_LINE: usize = 30_000;

/// The process that runs `command` in `shell`, with the command passed **byte for
/// byte**: PowerShell takes it encoded, so no quoting reaches it; cmd takes it as
/// the raw tail of its command line, because std's quoting would break cmd's own
/// parsing; bash takes it as one argument.
///
/// `Err` for a command a shell cannot run as written — more than one line for cmd, which
/// stops at the first, and one too long for PowerShell's command line — so nothing runs
/// that differs from what was approved.
fn shell_command(shell: Shell, command: &str) -> Result<Command, String> {
    match shell {
        Shell::PowerShell => {
            let script = powershell_script(command);
            let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
            // Decoded by PowerShell itself and run as a script block, rather than handed to
            // `-EncodedCommand`: the same bytes arrive, but `-EncodedCommand` makes it write
            // its errors as CLIXML, and the model would be given XML instead of the error.
            let run = format!(
                "& ([scriptblock]::Create([Text.Encoding]::Unicode.GetString([Convert]::FromBase64String('{}'))))",
                base64(&utf16)
            );
            if run.len() > MAX_COMMAND_LINE {
                return Err(format!(
                    "this command is too long to hand to PowerShell ({} characters) — write it to a .ps1 file and run that",
                    command.chars().count()
                ));
            }
            let mut cmd = Command::new("powershell.exe");
            cmd.args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
            ])
            .arg(run);
            Ok(cmd)
        }
        Shell::Cmd => {
            if command.contains(['\n', '\r']) {
                return Err("cmd.exe runs one line, and stops at the first — join the commands with && instead".into());
            }
            let mut cmd = Command::new("cmd.exe");
            cmd.args(["/d", "/s", "/c"]);
            raw_tail(&mut cmd, &format!("\"{command}\""));
            Ok(cmd)
        }
        Shell::Bash => {
            let bash = bash_path().ok_or(r"Git Bash is not installed (no Git for Windows bin\bash.exe)")?;
            let mut cmd = Command::new(bash);
            cmd.args(["--noprofile", "--norc", "-c"]).arg(command);
            Ok(cmd)
        }
    }
}

#[cfg(windows)]
fn raw_tail(cmd: &mut Command, tail: &str) {
    use std::os::windows::process::CommandExt;
    cmd.raw_arg(tail);
}

#[cfg(not(windows))]
fn raw_tail(cmd: &mut Command, tail: &str) {
    cmd.arg(tail);
}

/// Standard base64 with padding — all `-EncodedCommand` needs.
fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n =
            (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
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
    /// Its drive fell below the run's floor ([`DISK_FLOOR`] at most) and it was killed.
    DiskFull,
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
    /// For a `cargo test` that exited 0: how many of its test programs began (`running N
    /// tests`) and never reported (`test result:`). **Counted over everything the command
    /// wrote**, not the part kept. A test program that calls `std::process::exit(0)` ends the
    /// run there with a good exit code, and cargo goes on to the next one; this is the only
    /// sign that most of the tests never ran. Always 0 for any other command.
    pub unfinished_tests: usize,
}

impl Outcome {
    /// It exited 0 **and no test program was cut short**: a `cargo test` that stopped early
    /// has not passed, whatever its exit code says.
    pub fn passed(&self) -> bool {
        self.ended == Ended::Exited(Some(0)) && self.unfinished_tests == 0
    }
}

/// What the model is told when a test program ended without reporting its result.
pub const UNFINISHED_NOTE: &str = "A test program ended without reporting its result — something in the code exited or \
     aborted the test run (std::process::exit, abort, a panic in a destructor…). This counts as a failure.";

/// Whether `argv` is `cargo test` (or `cargo t`), whose output says when a test program began
/// and when it reported. A run that only builds or lists the tests has no results to count.
fn counts_test_runs(argv: &[String]) -> bool {
    let Some((program, args)) = argv.split_first() else {
        return false;
    };
    let is_cargo = Path::new(program)
        .file_stem()
        .is_some_and(|stem| stem.eq_ignore_ascii_case("cargo"));
    // The subcommand is the first word that is not a flag or a `+toolchain`.
    let subcommand = args.iter().find(|a| !a.starts_with('-') && !a.starts_with('+'));
    is_cargo
        && matches!(subcommand.map(String::as_str), Some("test" | "t"))
        && !args
            .iter()
            .any(|a| matches!(a.as_str(), "--no-run" | "--list" | "--help" | "-h"))
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
    let mut cmd = Command::new(program);
    cmd.args(args);
    let io = Io {
        count_tests: counts_test_runs(argv),
        ..Io::default()
    };
    run_prepared(cmd, root, &label(argv), program, timeout, env, io, cancelled)
}

/// [`run`] with `input` written to the command's standard input, and room for
/// `keep` bytes of output instead of the usual head and tail — for a command whose
/// output is one document that has to arrive whole, such as `claude -p
/// --output-format json`. Everything else is [`run`]'s: one job for the tree,
/// no window, the timeout and Stop. The input is written on a thread of its own,
/// so a command that never reads it cannot stall the wait.
pub fn run_with_input(
    root: &Path,
    argv: &[String],
    input: &[u8],
    keep: usize,
    timeout: Duration,
    env: &[(String, String)],
    cancelled: impl Fn() -> bool,
) -> Result<Outcome, String> {
    let (program, args) = argv.split_first().ok_or("no command was given")?;
    let mut cmd = Command::new(program);
    cmd.args(args);
    let io = Io {
        input: Some(input),
        head: keep,
        tail: 0,
        count_tests: false,
    };
    run_prepared(cmd, root, &label(argv), program, timeout, env, io, cancelled)
}

/// What a run is fed, and how much of its output is kept.
struct Io<'a> {
    input: Option<&'a [u8]>,
    head: usize,
    tail: usize,
    /// The command is `cargo test`: count its test programs as the output goes by.
    count_tests: bool,
}

impl Default for Io<'_> {
    fn default() -> Self {
        Io {
            input: None,
            head: HEAD,
            tail: TAIL,
            count_tests: false,
        }
    }
}

/// Run `command` in `shell`, in `root`, until it ends, times out, or `cancelled()`
/// says stop. Everything [`run`] does — one job for the whole tree, the capped
/// output, Smart App Control — and **the command reaches the shell byte for byte**:
/// see [`shell_command`].
///
/// `Err` only when the shell could not be started (Git Bash not installed, say).
pub fn run_shell(
    root: &Path,
    shell: Shell,
    command: &str,
    timeout: Duration,
    env: &[(String, String)],
    cancelled: impl Fn() -> bool,
) -> Result<Outcome, String> {
    let cmd = shell_command(shell, command)?;
    let program = cmd.get_program().to_string_lossy().into_owned();
    run_prepared(cmd, root, command, &program, timeout, env, Io::default(), cancelled)
}

/// The one runner: `cmd` is the program and its arguments, and `shown` is what an
/// error names it as.
#[allow(clippy::too_many_arguments)]
fn run_prepared(
    mut cmd: Command,
    root: &Path,
    shown: &str,
    program: &str,
    timeout: Duration,
    env: &[(String, String)],
    io: Io,
    cancelled: impl Fn() -> bool,
) -> Result<Outcome, String> {
    let dir = plain(root);

    // One pipe for both streams, so the model sees an error next to the line
    // that caused it rather than all of stdout and then all of stderr.
    let (mut reader, writer) = std::io::pipe().map_err(|e| format!("could not make a pipe: {e}"))?;
    let writer2 = writer.try_clone().map_err(|e| format!("could not make a pipe: {e}"))?;

    cmd.current_dir(&dir)
        .stdin(if io.input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
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
    // running, and when Mimir itself exits mid-check. Made before anything starts:
    // without the job there are no memory limits, and model-written code runs
    // here, so it does not run at all rather than run unbounded.
    let job = Job::new();
    #[cfg(windows)]
    if job.is_none() {
        return Err(format!(
            "could not put `{shown}` under its memory limits, so it was not run"
        ));
    }

    let started = Instant::now();
    // Read once, as the run starts: the floor is a share of what this drive had then.
    let disk_floor = free_space(&dir).map(disk_floor_for);
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
                output: format!("could not start `{shown}`: {e}"),
                bytes: 0,
                complete: true,
                blocked: Some(format!("the program `{program}`")),
                unfinished_tests: 0,
            });
        }
        Err(e) => return Err(format!("could not start `{shown}` in {}: {e}", dir.display())),
    };
    // The Command still holds this process's copies of the pipe's write end.
    // Until they are closed the reader never sees end-of-file.
    drop(cmd);
    // Assigned the moment it exists. A child that started a process of its own
    // in the microseconds before this would leave that one outside the job —
    // `cargo` and `npm` take milliseconds just to start, and `taskkill /T` still
    // backs the job up on Stop.
    let job = match job {
        Some(job) => confine(job, &mut child, shown)?,
        None => None,
    };

    if let (Some(input), Some(mut stdin)) = (io.input, child.stdin.take()) {
        let input = input.to_vec();
        std::thread::spawn(move || {
            use std::io::Write;
            // A command that exits without reading it all closes the pipe; that is
            // its answer, not an error here. Dropping `stdin` is the end of input.
            let _ = stdin.write_all(&input);
        });
    }

    let captured = Arc::new(Mutex::new(
        Capture::new(io.head, io.tail).counting_tests(io.count_tests),
    ));
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

    let mut polls = 0u32;
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
        polls += 1;
        // No reading at the start, no guard for this run.
        if polls.is_multiple_of(DISK_EVERY)
            && disk_floor.is_some_and(|floor| free_space(&dir).is_some_and(|free| free < floor))
        {
            kill_tree(&mut child, job.as_ref());
            break Ended::DiskFull;
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
    let mut capture = captured.lock().expect("capture lock");
    let output = capture.render();
    let passed = ended == Ended::Exited(Some(0));
    let blocked = if passed {
        None
    } else {
        blocked_by_smart_app_control(&dir, &output)
    };
    // Only a run that ended by itself with a good exit code can have stopped early without
    // saying so, and only a stream read to its end can be counted: a test program whose result
    // line is still on its way is not one that never reported.
    let unfinished_tests = if passed && complete {
        capture.unfinished_tests()
    } else {
        0
    };
    Ok(Outcome {
        ended,
        elapsed,
        output,
        bytes: capture.total,
        complete,
        blocked,
        unfinished_tests,
    })
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
/// dropped — and, because the handle closes with the process, when Mimir exits.
/// No process in it may commit more than [`PROCESS_MEMORY_LIMIT`], nor all of
/// them together more than [`RUN_MEMORY_LIMIT`].
#[cfg(windows)]
struct Job(windows::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Job {
    fn new() -> Option<Self> {
        use windows::Win32::System::JobObjects::{
            CreateJobObjectW, JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };
        // SAFETY: a new, unnamed job with default security; the handle is owned
        // by the returned value and closed in Drop. The information passed is a
        // correctly sized JOBOBJECT_EXTENDED_LIMIT_INFORMATION that outlives the
        // call.
        unsafe {
            let job = Job(CreateJobObjectW(None, windows::core::PCWSTR::null()).ok()?);
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags =
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_PROCESS_MEMORY | JOB_OBJECT_LIMIT_JOB_MEMORY;
            info.ProcessMemoryLimit = PROCESS_MEMORY_LIMIT;
            info.JobMemoryLimit = RUN_MEMORY_LIMIT;
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
        unsafe { AssignProcessToJobObject(self.0, windows::Win32::Foundation::HANDLE(child.as_raw_handle())).is_ok() }
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

/// The free space on the drive `dir` is on, or `None` when it can't be read.
#[cfg(windows)]
fn free_space(dir: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut free = 0u64;
    // SAFETY: `wide` is NUL-terminated and outlives the call; `free` is a live u64.
    unsafe {
        windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
            windows::core::PCWSTR(wide.as_ptr()),
            Some(&mut free),
            None,
            None,
        )
        .ok()?;
    }
    Some(free)
}

#[cfg(not(windows))]
fn free_space(_dir: &Path) -> Option<u64> {
    None
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
pub(crate) fn no_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    // Mimir has no console, so every console program it starts would otherwise
    // get a window of its own flashing up. CREATE_NO_WINDOW alone — see
    // `ollama::spawn_detached` for why not DETACHED_PROCESS as well.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub(crate) fn no_window(_: &mut Command) {}

/// Put `child`, just started, in `job`: `Some(job)` when it is there.
///
/// **A child that cannot be put there is killed and the run refused**, since it would
/// run with no memory limits. The exception is a child that has already exited, which
/// Windows will not assign: nothing is left to limit, so the run goes on without the job
/// and reads what it wrote.
fn confine(job: Job, child: &mut std::process::Child, shown: &str) -> Result<Option<Job>, String> {
    if job.assign(child) {
        return Ok(Some(job));
    }
    if matches!(child.try_wait(), Ok(Some(_))) {
        return Ok(None);
    }
    kill_tree(child, None);
    Err(format!(
        "could not put `{shown}` under its memory limits, so it was not run"
    ))
}

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

/// How many bytes of a line are looked at when the output is counted for test programs: a
/// `running N tests` line and the start of a `test result:` line are far shorter.
const LINE_LOOKED_AT: usize = 128;

/// Counts the test programs of a `cargo test` as its output goes by: how many began, with the
/// line libtest writes first (`running 12 tests`), and how many reported, with the line it
/// writes last (`test result: ok. 12 passed; …`). **Every program that runs tests writes both**, a
/// doc-test run and a filtered run that found nothing included, so a difference between the two
/// counts is a program that was ended in between. Output that has no `running` line — a build,
/// a compile error, `--no-run`, a program with its own `main` — adds nothing to either.
#[derive(Default)]
struct TestRuns {
    /// The start of the line being read.
    line: Vec<u8>,
    began: usize,
    reported: usize,
}

impl TestRuns {
    fn push(&mut self, bytes: &[u8]) {
        for &b in bytes {
            if b == b'\n' {
                self.end_line();
            } else if self.line.len() < LINE_LOOKED_AT {
                self.line.push(b);
            }
        }
    }

    fn end_line(&mut self) {
        let text = String::from_utf8_lossy(&self.line);
        let line = text.trim_end_matches('\r');
        if is_libtest_header(line) {
            self.began += 1;
        } else if line.starts_with("test result: ") {
            self.reported += 1;
        }
        self.line.clear();
    }

    /// The programs that began and did not report, once the last line is counted.
    fn unfinished(&mut self) -> usize {
        if !self.line.is_empty() {
            self.end_line();
        }
        self.began.saturating_sub(self.reported)
    }
}

/// `running 12 tests` or `running 1 test`: the line libtest, and rustdoc's doc-test runner,
/// write before the first test.
fn is_libtest_header(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("running ") else {
        return false;
    };
    let count = rest.strip_suffix(" tests").or_else(|| rest.strip_suffix(" test"));
    count.is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The start and the end of the output, and how much there was.
struct Capture {
    head: Vec<u8>,
    tail: std::collections::VecDeque<u8>,
    total: u64,
    head_cap: usize,
    tail_cap: usize,
    /// Counts the test programs over the whole stream, not only the part kept: set for `cargo test`.
    runs: Option<TestRuns>,
}

impl Default for Capture {
    fn default() -> Self {
        Capture::new(HEAD, TAIL)
    }
}

impl Capture {
    fn new(head_cap: usize, tail_cap: usize) -> Self {
        Capture {
            head: Vec::new(),
            tail: Default::default(),
            total: 0,
            head_cap,
            tail_cap,
            runs: None,
        }
    }

    /// Count the test programs in what is pushed, if `on`.
    fn counting_tests(mut self, on: bool) -> Self {
        self.runs = on.then(TestRuns::default);
        self
    }

    /// The test programs that began and did not report, when they are being counted.
    fn unfinished_tests(&mut self) -> usize {
        self.runs.as_mut().map_or(0, TestRuns::unfinished)
    }

    fn push(&mut self, mut bytes: &[u8]) {
        if let Some(runs) = &mut self.runs {
            runs.push(bytes);
        }
        self.total += bytes.len() as u64;
        let room = self.head_cap.saturating_sub(self.head.len());
        let take = room.min(bytes.len());
        self.head.extend_from_slice(&bytes[..take]);
        bytes = &bytes[take..];
        if self.tail_cap == 0 {
            return;
        }
        for &b in bytes {
            if self.tail.len() == self.tail_cap {
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
        // Only a character left unfinished at the very end of the head is dropped —
        // whatever invalid bytes came earlier (a program writing cp1252, say) stay
        // as U+FFFD, which is honest, where trimming keyed on the *first* error
        // would leave the split character behind whenever there was one.
        let mut head = &self.head[..];
        let tail_from = tail.iter().take(3).take_while(|b| (**b & 0xC0) == 0x80).count();
        let unfinished = incomplete_tail(head);
        head = &head[..head.len() - unfinished];
        // The bytes dropped at the split were cut too, so they are counted.
        let cut = cut + (unfinished + tail_from) as u64;
        let head = String::from_utf8_lossy(head);
        let tail = String::from_utf8_lossy(&tail[tail_from..]);
        format!("{head}\n[… {cut} bytes cut …]\n{tail}")
    }
}

/// How many bytes at the end of `bytes` are the start of a UTF-8 character whose
/// rest is missing (0 if it ends on a whole character or on bytes that are simply
/// invalid). Looks back at most three bytes, the longest a start can be.
fn incomplete_tail(bytes: &[u8]) -> usize {
    for back in 1..=bytes.len().min(3) {
        let b = bytes[bytes.len() - back];
        if (b & 0xC0) == 0x80 {
            continue; // a continuation byte: keep looking for its start
        }
        let needs = match b {
            0xC2..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF4 => 4,
            _ => return 0,
        };
        return if needs > back { back } else { 0 };
    }
    0
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
        // An exit code of 0 that is not a pass: a test program ended the run early.
        Ended::Exited(Some(0)) if outcome.unfinished_tests > 0 => {
            format!("`{name}` failed (exit 0, {secs:.1} s). {UNFINISHED_NOTE}")
        }
        Ended::Exited(Some(0)) => format!("`{name}` passed (exit 0, {secs:.1} s)."),
        Ended::Exited(Some(code)) => format!("`{name}` failed (exit {code}, {secs:.1} s)."),
        Ended::Exited(None) => format!("`{name}` ended without an exit code after {secs:.1} s."),
        Ended::TimedOut => format!("`{name}` ran for {secs:.0} s, past its time limit, and was stopped."),
        Ended::Stopped => format!("`{name}` was stopped by the user after {secs:.1} s."),
        Ended::DiskFull => format!("`{name}` was stopped after {secs:.1} s because the disk was nearly full."),
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

/// What the model is told about a shell command: `` `ls` exited 0 (1.2 s). Output: ``.
///
/// **Never starts with `error:`**, for the reason [`for_model`] gives. Only a
/// command that could not be started at all is an `error:`, and that is the `Err`
/// of [`run_shell`], not an outcome.
pub fn for_command(shell: Shell, command: &str, outcome: &Outcome) -> String {
    let secs = outcome.elapsed.as_secs_f64();

    if let Some(what) = &outcome.blocked {
        return format!(
            "`{command}` could not finish: Windows Smart App Control blocked {what}. This is not a \
             problem with the command itself. Tell the user; rebuilding usually clears it."
        );
    }

    let head = match &outcome.ended {
        Ended::Exited(Some(code)) => format!("`{command}` exited {code} ({secs:.1} s)."),
        Ended::Exited(None) => format!("`{command}` ended without an exit code after {secs:.1} s."),
        Ended::TimedOut => format!("`{command}` ran for {secs:.0} s, past its time limit, and was stopped."),
        Ended::Stopped => format!("`{command}` was stopped by the user after {secs:.1} s."),
        Ended::DiskFull => format!("`{command}` was stopped after {secs:.1} s because the disk was nearly full."),
    };
    let head = if outcome.complete {
        head
    } else {
        format!("{head} Something it started was still running, so the output may be incomplete.")
    };
    let _ = shell;
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
        let Some(start) = line.find("can't find crate for `") else {
            continue;
        };
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
    dir.ancestors()
        .find_map(|d| std::fs::read_to_string(d.join("Cargo.lock")).ok())
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
            let dir = std::env::temp_dir().join(format!("bifrost-check-{tag}-{}-{n}", std::process::id()));
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
        /// ignored, is how an `bifrost-check-linger-…` folder was left in `%TEMP%`.
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
        assert_eq!(
            candidates(&f.ws()),
            vec![cmd(&["cargo", "check"]), cmd(&["cargo", "test"])]
        );
    }

    #[test]
    fn npm_scripts_are_offered_only_when_they_exist() {
        let f = Folder::new("npm");
        f.write(
            "package.json",
            r#"{"scripts":{"test":"vitest run","build":"vite build"}}"#,
        );
        let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
        assert_eq!(
            candidates(&f.ws()),
            vec![cmd(&[npm, "test"]), cmd(&[npm, "run", "build"])]
        );
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

    /// What a run is given on standard input reaches the command, and `keep` bytes of
    /// output arrive whole where a plain run would cut them.
    #[cfg(windows)]
    #[test]
    fn input_reaches_the_command_and_its_output_is_kept_whole() {
        let f = Folder::new("input");
        let lines = "x".repeat(99) + "\n";
        let input = lines.repeat(500);
        let argv = cmd(&["findstr", "^"]);
        let out = run_with_input(&f.0, &argv, input.as_bytes(), 1024 * 1024, CHECK_TIMEOUT, &[], never).unwrap();
        assert!(out.passed());
        assert!(
            out.output.len() > HEAD + TAIL && !out.output.contains("bytes cut"),
            "{}",
            out.output.len()
        );
        assert_eq!(out.output.lines().count(), 500);
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
        assert!(
            out.output.contains("to-stdout") && out.output.contains("to-stderr"),
            "{:?}",
            out.output
        );
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
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "took {:?}",
            started.elapsed()
        );
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
        let err = run(&f.0, &cmd(&["no-such-program-bifrost"]), CHECK_TIMEOUT, &[], never).unwrap_err();
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
            unfinished_tests: 0,
        }
    }

    /// A project whose lockfile lists these packages, for asking whether an
    /// E0463 names a real dependency.
    fn locked(tag: &str, packages: &[&str]) -> Folder {
        let f = Folder::new(tag);
        let body: String = packages
            .iter()
            .map(|p| format!("[[package]]\nname = \"{p}\"\nversion = \"1.0.0\"\n\n"))
            .collect();
        f.write("Cargo.lock", &format!("version = 4\n\n{body}"));
        f
    }

    #[test]
    fn what_the_model_is_told_never_reads_as_a_failed_call() {
        let check = cmd(&["cargo", "check"]);
        for ended in [
            Ended::Exited(Some(0)),
            Ended::Exited(Some(101)),
            Ended::TimedOut,
            Ended::Stopped,
        ] {
            let said = for_model(&check, &outcome(ended, "error[E0425]: cannot find function `greet`"));
            assert!(!said.starts_with("error:"), "{said}");
        }
    }

    #[test]
    fn a_failed_build_says_so_and_carries_the_error() {
        let said = for_model(
            &cmd(&["cargo", "check"]),
            &outcome(
                Ended::Exited(Some(101)),
                "error[E0425]: cannot find function `greet` in this scope",
            ),
        );
        assert!(said.starts_with("`cargo check` failed (exit 101"), "{said}");
        assert!(said.contains("cannot find function `greet`"));
    }

    #[test]
    fn a_blocked_run_tells_the_model_not_to_touch_the_code() {
        let o = Outcome {
            blocked: Some("`tauri`, a library".into()),
            ..outcome(Ended::Exited(Some(101)), "")
        };
        let said = for_model(&cmd(&["cargo", "check"]), &o);
        assert!(
            said.contains("Smart App Control") && said.contains("do not change any files"),
            "{said}"
        );
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
            &cmd(&[
                "cmd",
                "/c",
                "echo error[E0463]: can't find crate for `tauri` (os error 4551)",
            ]),
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
        let out = run(
            &f.0,
            &cmd(&["cmd", "/c", "start /b ping -n 30 127.0.0.1 >nul"]),
            CHECK_TIMEOUT,
            &[],
            never,
        )
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
        assert!(
            text.contains("bytes cut") && !text.contains('\u{fffd}'),
            "{}",
            text.matches('\u{fffd}').count()
        );
        // The one byte of the head's split character and the two of the tail's were
        // cut too, and are in the count.
        assert!(text.contains(&format!("[… {} bytes cut …]", total - HEAD - TAIL + 3)));
    }

    /// An invalid byte earlier in the head (cp1252 output) must not stop the split
    /// character at its end from being trimmed; the earlier byte stays as U+FFFD.
    #[test]
    fn an_earlier_invalid_byte_does_not_keep_the_split_character_in_the_head() {
        let mut c = Capture::default();
        let total = 20_000usize;
        let mut v = vec![b'a'; total];
        v[10] = 0xE9;
        v[HEAD - 1..HEAD + 1].copy_from_slice("\u{e9}".as_bytes());
        c.push(&v);
        let text = c.render();
        assert_eq!(text.matches('\u{fffd}').count(), 1, "only the earlier byte");
        assert!(text.contains(&format!("[… {} bytes cut …]", total - HEAD - TAIL + 1)));
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
        assert_eq!(
            plain(Path::new(r"\\?\UNC\server\share\proj")),
            PathBuf::from(r"\\server\share\proj")
        );
        assert_eq!(plain(Path::new(r"\\?\C:\work\proj")), PathBuf::from(r"C:\work\proj"));
        assert_eq!(plain(Path::new(r"C:\work\proj")), PathBuf::from(r"C:\work\proj"));
    }

    // ── test programs that stop early ──
    //
    // What `cargo test` wrote, recorded from a crate with a library, a binary, an integration
    // test and a doc-test (the paths shortened). The early exits are a test that calls
    // `std::process::exit(0)`: cargo sees a good exit code and goes on to the next program.

    /// Four test programs, four results.
    const CARGO_NORMAL: &str = r#"   Compiling sample v0.1.0 (C:\work\sample)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.32s
     Running unittests src\lib.rs (target\debug\deps\sample-0c397b863d7a06b2.exe)

running 2 tests
test tests::adds ... ok
test tests::adds_more ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src\main.rs (target\debug\deps\sample-2c8e0ac1960a88c0.exe)

running 1 test
test tests::main_runs ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests\it.rs (target\debug\deps\it-5374f0f9f2da8122.exe)

running 2 tests
test integration_adds_again ... ok
test integration_adds ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests sample

running 1 test
test src\lib.rs - add (line 3) ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.14s

"#;

    /// The library's third test exits. Its line is cut off, and cargo's next line lands on it.
    const CARGO_EARLY_EXIT: &str = r#"   Compiling sample v0.1.0 (C:\work\sample)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.38s
     Running unittests src\lib.rs (target\debug\deps\sample-0c397b863d7a06b2.exe)

running 3 tests
test tests::adds ... ok
test tests::adds_more ... ok
test tests::zz_exits ...      Running unittests src\main.rs (target\debug\deps\sample-2c8e0ac1960a88c0.exe)

running 1 test
test tests::main_runs ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests\it.rs (target\debug\deps\it-5374f0f9f2da8122.exe)

running 2 tests
test integration_adds ... ok
test integration_adds_again ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests sample

running 1 test
test src\lib.rs - add (line 3) ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.12s

"#;

    /// The same, with the tests running side by side: it exits before any of them reports.
    const CARGO_EARLY_EXIT_PARALLEL: &str = r#"    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.01s
     Running unittests src\lib.rs (target\debug\deps\sample-0c397b863d7a06b2.exe)

running 3 tests
     Running unittests src\main.rs (target\debug\deps\sample-2c8e0ac1960a88c0.exe)

running 1 test
test tests::main_runs ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests\it.rs (target\debug\deps\it-5374f0f9f2da8122.exe)

running 2 tests
test integration_adds ... ok
test integration_adds_again ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests sample

running 1 test
test src\lib.rs - add (line 3) ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.12s

"#;

    /// A filter that matches nothing: every program still begins and reports, with 0 tests.
    const CARGO_FILTERED: &str = r#"    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.01s
     Running unittests src\lib.rs (target\debug\deps\sample-0c397b863d7a06b2.exe)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 2 filtered out; finished in 0.00s

     Running unittests src\main.rs (target\debug\deps\sample-2c8e0ac1960a88c0.exe)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.00s

     Running tests\it.rs (target\debug\deps\it-5374f0f9f2da8122.exe)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 2 filtered out; finished in 0.00s

"#;

    /// `--no-run` builds the programs and runs none.
    const CARGO_NO_RUN: &str = r#"    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.00s
  Executable unittests src\lib.rs (target\debug\deps\sample-0c397b863d7a06b2.exe)
  Executable unittests src\main.rs (target\debug\deps\sample-2c8e0ac1960a88c0.exe)
  Executable tests\it.rs (target\debug\deps\it-5374f0f9f2da8122.exe)
"#;

    /// `-- --list` runs each program to list its tests: a `Running` line, and no `running` line.
    const CARGO_LIST: &str = r#"    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.00s
     Running unittests src\lib.rs (target\debug\deps\sample-0c397b863d7a06b2.exe)
tests::adds: test
tests::adds_more: test

2 tests, 0 benchmarks
     Running unittests src\main.rs (target\debug\deps\sample-2c8e0ac1960a88c0.exe)
tests::main_runs: test

1 test, 0 benchmarks
   Doc-tests sample
src\lib.rs - add (line 3): test

1 test, 0 benchmarks
"#;

    /// `-q`: no `Running` lines, only the programs' own words.
    const CARGO_QUIET: &str = r#"
running 2 tests
..
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s


running 1 test
.
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

"#;

    /// Nothing was built, so nothing ran; cargo exits 101 for it.
    const CARGO_COMPILE_ERROR: &str = r#"   Compiling sample v0.1.0 (C:\work\sample)
error[E0308]: mismatched types
 --> src\main.rs:1:26
  |
1 | fn main() { let x: i32 = "not a number"; println!("{x}"); }
  |                    ---   ^^^^^^^^^^^^^^ expected `i32`, found `&str`
  |                    |
  |                    expected due to this

For more information about this error, try `rustc --explain E0308`.
error: could not compile `sample` (bin "sample") due to 1 previous error
warning: build failed, waiting for other jobs to finish...
error: could not compile `sample` (bin "sample" test) due to 1 previous error
"#;

    /// The test programs of `output` that began and did not report, counted as the runner counts
    /// them: the stream comes in pieces, and a piece can end anywhere in a line.
    fn unfinished_in(output: &str) -> usize {
        let mut runs = TestRuns::default();
        for piece in output.as_bytes().chunks(37) {
            runs.push(piece);
        }
        runs.unfinished()
    }

    #[test]
    fn a_normal_run_of_several_test_programs_and_the_doc_tests_reports_every_one() {
        assert_eq!(unfinished_in(CARGO_NORMAL), 0);
        let mut runs = TestRuns::default();
        runs.push(CARGO_NORMAL.as_bytes());
        runs.unfinished();
        assert_eq!(
            (runs.began, runs.reported),
            (4, 4),
            "the library, the binary, the integration test, the doc-tests"
        );
        // One byte at a time, and with Windows line ends: the same.
        let mut slow = TestRuns::default();
        CARGO_NORMAL.as_bytes().iter().for_each(|b| slow.push(&[*b]));
        assert_eq!(slow.unfinished(), 0);
        assert_eq!(unfinished_in(&CARGO_NORMAL.replace('\n', "\r\n")), 0);
    }

    #[test]
    fn a_test_program_that_exits_in_the_middle_is_one_that_began_and_never_reported() {
        assert_eq!(unfinished_in(CARGO_EARLY_EXIT), 1);
        assert_eq!(unfinished_in(CARGO_EARLY_EXIT_PARALLEL), 1);
        assert_eq!(unfinished_in(&CARGO_EARLY_EXIT.replace('\n', "\r\n")), 1);
    }

    #[test]
    fn the_last_program_cut_short_and_the_last_line_without_its_newline_are_counted() {
        // The doc-tests begin and nothing follows.
        let at = CARGO_NORMAL.rfind("test result:").unwrap();
        assert_eq!(unfinished_in(&CARGO_NORMAL[..at]), 1);
        // The last result is there, only its newline is not.
        assert_eq!(unfinished_in(CARGO_NORMAL.trim_end()), 0);
        // Two cut short are two.
        assert_eq!(unfinished_in(&format!("{CARGO_EARLY_EXIT}{CARGO_EARLY_EXIT}")), 2);
    }

    /// The five outputs the counting must leave alone.
    #[test]
    fn output_that_is_not_a_run_that_stopped_early_adds_nothing() {
        let cases = [
            ("a compile error", CARGO_COMPILE_ERROR),
            ("--no-run", CARGO_NO_RUN),
            ("-- --list", CARGO_LIST),
            ("a filter that matches no test", CARGO_FILTERED),
            ("-q", CARGO_QUIET),
            ("nothing at all", ""),
            (
                "a program with a main of its own",
                "     Running tests\\custom.rs (target\\debug\\deps\\custom-1.exe)\nall checks done\n",
            ),
        ];
        for (what, output) in cases {
            assert_eq!(unfinished_in(output), 0, "{what}");
        }
    }

    /// The runner keeps the start and the end of what a command writes; the count is of all of it.
    #[test]
    fn the_count_is_of_the_whole_output_not_of_the_part_that_is_kept() {
        let mut capture = Capture::default().counting_tests(true);
        let filler = "test some::module::a_passing_test ... ok\n".repeat(20_000);
        capture.push(filler.as_bytes());
        capture.push(CARGO_EARLY_EXIT.as_bytes());
        capture.push(filler.as_bytes());
        let kept = capture.render();
        assert!(kept.contains("bytes cut"), "most of it was cut");
        assert!(
            !kept.contains("zz_exits") && !kept.contains("running 3 tests"),
            "and what was cut held the early exit"
        );
        assert_eq!(capture.unfinished_tests(), 1);
        // A command that is not `cargo test` is not counted at all.
        let mut other = Capture::default();
        other.push(CARGO_EARLY_EXIT.as_bytes());
        assert_eq!(other.unfinished_tests(), 0);
    }

    #[test]
    fn only_cargo_test_is_counted_and_not_when_it_builds_or_lists() {
        let counted = |parts: &[&str]| counts_test_runs(&cmd(parts));
        for yes in [
            &["cargo", "test"][..],
            &["cargo", "t", "-p", "hugin"],
            &["cargo.exe", "test", "--workspace"],
            &[r"C:\Users\me\.cargo\bin\cargo.exe", "test"],
            &["CARGO.EXE", "test"],
            &["cargo", "+nightly", "test"],
            &["cargo", "--locked", "test", "-p", "x", "--", "--nocapture"],
            &["cargo", "test", "--doc"],
            &["cargo", "test", "-q"],
        ] {
            assert!(counted(yes), "{yes:?}");
        }
        for no in [
            &["cargo", "check"][..],
            &["cargo", "build", "--tests"],
            &["cargo", "run", "--", "test"],
            &["cargo", "TEST"],
            &["cargo", "test", "--no-run"],
            &["cargo", "test", "--", "--list"],
            &["cargo", "test", "--help"],
            &["cargo", "nextest", "run"],
            &["npm.cmd", "test"],
            &["node", "test.js"],
            &["cargo"],
            &[],
        ] {
            assert!(!counted(no), "{no:?}");
        }
    }

    /// Exit code 0 is not a pass when a test program was cut short, and the model is told why.
    #[test]
    fn a_test_run_that_exited_0_but_stopped_early_has_not_passed_and_the_model_is_told_why() {
        let early = Outcome {
            unfinished_tests: 1,
            ..outcome(Ended::Exited(Some(0)), "running 3 tests\n")
        };
        assert!(!early.passed());
        let said = for_model(&cmd(&["cargo", "test", "-p", "hugin"]), &early);
        assert!(
            said.starts_with(
                "`cargo test -p hugin` failed (exit 0, 1.5 s). A test program ended without reporting its result"
            ),
            "{said}"
        );
        assert!(
            said.contains("std::process::exit") && said.ends_with("running 3 tests") && !said.starts_with("error:"),
            "{said}"
        );
        assert!(said.contains("This counts as a failure."), "{said}");
        // A pass is still a pass, and a failure that is a failure for other reasons says what it did.
        assert!(outcome(Ended::Exited(Some(0)), "").passed());
        assert!(for_model(&cmd(&["cargo", "test"]), &outcome(Ended::Exited(Some(0)), "")).contains("passed (exit 0"));
        assert!(
            for_model(&cmd(&["cargo", "test"]), &outcome(Ended::Exited(Some(101)), ""))
                .starts_with("`cargo test` failed (exit 101")
        );
    }

    /// The whole way through the runner, on output a command writes: what `type` prints is what
    /// `cargo test` printed.
    #[cfg(windows)]
    #[test]
    fn a_run_that_exits_0_with_a_test_program_cut_short_is_not_a_pass() {
        let f = Folder::new("early");
        f.write("early.txt", CARGO_EARLY_EXIT);
        f.write("normal.txt", CARGO_NORMAL);
        let print = |file: &str, count_tests: bool, then: &str| {
            let mut c = Command::new("cmd");
            c.args(["/c", &format!("type {file}{then}")]);
            run_prepared(
                c,
                &f.0,
                "type",
                "cmd",
                CHECK_TIMEOUT,
                &[],
                Io {
                    count_tests,
                    ..Io::default()
                },
                never,
            )
            .unwrap()
        };
        let early = print("early.txt", true, "");
        assert_eq!(
            (early.ended.clone(), early.unfinished_tests, early.passed()),
            (Ended::Exited(Some(0)), 1, false),
            "{early:?}"
        );
        assert!(early.complete && for_model(&cmd(&["cargo", "test"]), &early).contains("failed (exit 0"));
        // The programs that did report, and the same text when the command is not `cargo test`.
        assert!(print("normal.txt", true, "").passed());
        let plain = print("early.txt", false, "");
        assert!(plain.passed() && plain.unfinished_tests == 0, "{plain:?}");
        // A run that failed says it failed by its exit code, and is not counted twice over.
        let failed = print("early.txt", true, " & exit 3");
        assert_eq!((failed.ended, failed.unfinished_tests), (Ended::Exited(Some(3)), 0));
    }

    // ── shells ──

    fn args_of(cmd: &Command) -> Vec<String> {
        cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    fn unbase64(text: &str) -> Vec<u8> {
        let mut out = Vec::new();
        let mut acc = 0u32;
        let mut bits = 0;
        for c in text.bytes().filter(|c| *c != b'=') {
            let v = match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' => 62,
                _ => 63,
            } as u32;
            acc = acc << 6 | v;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits & 0xFF) as u8);
            }
        }
        out
    }

    #[test]
    fn base64_matches_the_standard_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn shell_names_round_trip_and_unknown_is_none() {
        for s in Shell::ALL {
            assert_eq!(Shell::parse(s.name()), Some(s));
        }
        assert_eq!(Shell::parse(" PowerShell "), Some(Shell::PowerShell));
        assert_eq!(Shell::parse("zsh"), None);
        assert_eq!(
            [Shell::PowerShell.label(), Shell::Cmd.label(), Shell::Bash.label()],
            ["Windows PowerShell", "cmd.exe", "Git Bash"]
        );
    }

    /// **The command reaches PowerShell as it was written**: encoded, so no quoting
    /// can touch it, and it decodes back to the preamble and the exact text.
    #[test]
    fn powershell_gets_the_command_encoded() {
        let command = "Write-Output \"it's\" | % { $_ }  # \u{e9}\nGet-Date";
        let cmd = shell_command(Shell::PowerShell, command).unwrap();
        assert_eq!(cmd.get_program(), "powershell.exe");
        let args = args_of(&cmd);
        assert_eq!(
            args[..6],
            [
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command"
            ]
        );
        assert_eq!(args.len(), 7);
        let (head, tail) = (
            "& ([scriptblock]::Create([Text.Encoding]::Unicode.GetString([Convert]::FromBase64String('",
            "'))))",
        );
        let encoded = args[6]
            .strip_prefix(head)
            .and_then(|r| r.strip_suffix(tail))
            .expect("the decoding wrapper");
        let units: Vec<u16> = unbase64(encoded)
            .chunks(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        assert_eq!(String::from_utf16(&units).unwrap(), powershell_script(command));
        assert!(
            powershell_script(command).contains(&format!("\n{command}\n")),
            "the command itself, untouched"
        );
    }

    /// cmd gets `/d /s /c "<command>"` with the command untouched — including its
    /// own quotes, which std's quoting would have escaped.
    #[test]
    fn cmd_gets_the_command_as_a_raw_tail() {
        let command = r#"echo "a b" & dir "C:\Program Files""#;
        let cmd = shell_command(Shell::Cmd, command).unwrap();
        assert_eq!(cmd.get_program(), "cmd.exe");
        assert_eq!(args_of(&cmd), ["/d", "/s", "/c", &format!("\"{command}\"")]);
    }

    #[test]
    fn bash_gets_the_command_as_one_argument() {
        let Some(bash) = bash_path() else { return };
        let command = "echo \"a b\" && ls 'x y'";
        let cmd = shell_command(Shell::Bash, command).unwrap();
        assert_eq!(cmd.get_program(), bash.as_os_str());
        assert_eq!(args_of(&cmd), ["--noprofile", "--norc", "-c", command]);
    }

    #[test]
    fn bash_is_never_the_wsl_launcher() {
        if let Some(p) = bash_path() {
            let lower = p.to_string_lossy().to_lowercase();
            assert!(
                !lower.contains("system32") && lower.ends_with(r"bin\bash.exe"),
                "{lower}"
            );
        }
    }

    #[test]
    fn what_a_command_is_told_never_reads_as_a_failed_call() {
        for ended in [
            Ended::Exited(Some(0)),
            Ended::Exited(Some(1)),
            Ended::Exited(None),
            Ended::TimedOut,
            Ended::Stopped,
        ] {
            let said = for_command(Shell::Cmd, "dir", &outcome(ended, "error: nothing"));
            assert!(!said.starts_with("error:"), "{said}");
        }
        let o = Outcome {
            blocked: Some("a program".into()),
            ..outcome(Ended::Exited(Some(1)), "")
        };
        assert!(!for_command(Shell::Cmd, "dir", &o).starts_with("error:"));
    }

    #[test]
    fn for_command_words_each_ending() {
        let say = |ended, output: &str| for_command(Shell::Bash, "git log", &outcome(ended, output));
        assert_eq!(
            say(Ended::Exited(Some(0)), "abc\n"),
            "`git log` exited 0 (1.5 s). Output:\n\nabc"
        );
        assert_eq!(say(Ended::Exited(Some(0)), ""), "`git log` exited 0 (1.5 s).");
        assert!(say(Ended::Exited(Some(2)), "x").starts_with("`git log` exited 2 (1.5 s)."));
        assert!(say(Ended::TimedOut, "").contains("past its time limit"));
        assert!(say(Ended::Stopped, "").contains("stopped by the user"));
        let o = Outcome {
            blocked: Some("a program".into()),
            ..outcome(Ended::Exited(Some(1)), "")
        };
        assert!(for_command(Shell::Bash, "git log", &o).contains("Smart App Control"));
    }

    fn shells_here() -> Vec<Shell> {
        Shell::ALL.into_iter().filter(|s| s.available()).collect()
    }

    #[cfg(windows)]
    #[test]
    fn echo_hello_runs_in_every_available_shell() {
        let f = Folder::new("shells");
        for shell in shells_here() {
            let out = run_shell(&f.0, shell, "echo hello", CHECK_TIMEOUT, &[], never).unwrap();
            assert!(out.passed(), "{shell:?}: {:?}", out);
            assert_eq!(out.output.trim(), "hello", "{shell:?}");
        }
    }

    /// A PowerShell error reaches the model as the error's words, not as CLIXML — which is
    /// what `-EncodedCommand` would have made of it (review I3, finding 1).
    #[cfg(windows)]
    #[test]
    fn a_powershell_error_comes_back_as_plain_text() {
        let f = Folder::new("ps-error");
        let out = run_shell(
            &f.0,
            Shell::PowerShell,
            "Get-Item no-such-file-here",
            CHECK_TIMEOUT,
            &[],
            never,
        )
        .unwrap();
        assert!(
            !out.output.contains("CLIXML") && !out.output.contains("<Objs"),
            "{}",
            out.output
        );
        assert!(out.output.contains("no-such-file-here"), "{}", out.output);
    }

    /// What a shell cannot run as written is refused before anything starts: a second line
    /// for cmd, which would run only the first, and a command past PowerShell's command
    /// line (review I3, finding 5).
    #[test]
    fn a_command_a_shell_cannot_run_as_written_is_refused() {
        assert!(
            shell_command(Shell::Cmd, "echo one\necho two")
                .unwrap_err()
                .contains("one line")
        );
        assert!(shell_command(Shell::Cmd, "echo one && echo two").is_ok());
        let long = "Write-Output x; ".repeat(1_500);
        assert!(
            shell_command(Shell::PowerShell, &long)
                .unwrap_err()
                .contains("too long")
        );
        assert!(shell_command(Shell::Bash, &long).is_ok() || bash_path().is_none());
    }

    /// PowerShell ends with the code of a program that failed, as cargo said it — not the
    /// 1 `powershell -Command` would give — and still ends with 1 for a failed cmdlet and
    /// 0 for what worked.
    #[cfg(windows)]
    #[test]
    fn powershell_passes_a_programs_exit_code_through() {
        let f = Folder::new("ps-exit");
        let code = |command: &str| {
            run_shell(&f.0, Shell::PowerShell, command, CHECK_TIMEOUT, &[], never)
                .unwrap()
                .ended
        };
        assert_eq!(code("cmd /c exit 7"), Ended::Exited(Some(7)));
        assert_eq!(code("Get-Item no-such-file-here"), Ended::Exited(Some(1)));
        assert_eq!(code("Write-Output fine"), Ended::Exited(Some(0)));
        // A program that failed earlier in the line still counts, after a cmdlet that worked.
        assert_eq!(code("cmd /c exit 4; Write-Output after"), Ended::Exited(Some(4)));
    }

    #[cfg(windows)]
    #[test]
    fn a_shell_command_keeps_its_exit_code() {
        let f = Folder::new("shell-exit");
        for shell in shells_here() {
            let out = run_shell(&f.0, shell, "exit 3", CHECK_TIMEOUT, &[], never).unwrap();
            assert_eq!(out.ended, Ended::Exited(Some(3)), "{shell:?}");
        }
    }

    /// **Stop kills a shell command's tree** in each shell, with nothing left
    /// holding the output.
    #[cfg(windows)]
    #[test]
    fn stop_kills_a_shell_command_and_its_children() {
        let f = Folder::new("shell-stop");
        for shell in shells_here() {
            let started = Instant::now();
            let out = run_shell(&f.0, shell, "ping -n 30 127.0.0.1", CHECK_TIMEOUT, &[], || {
                started.elapsed() > Duration::from_millis(1500)
            })
            .unwrap();
            assert_eq!(out.ended, Ended::Stopped, "{shell:?}");
            assert!(out.complete, "{shell:?}: a child outlived the stop");
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "{shell:?}: {:?}",
                started.elapsed()
            );
        }
    }

    /// **No process in a run commits past [`PROCESS_MEMORY_LIMIT`]:** a command that
    /// asks for 9 GB, a gigabyte at a time, is refused the last of it and fails.
    /// Ignored because proving it commits 8 GB, too much beside a loaded model.
    #[cfg(windows)]
    #[test]
    #[ignore = "commits 8 GB of memory"]
    fn a_process_cannot_commit_past_the_memory_limit() {
        let f = Folder::new("memory-cap");
        let grab = "$ErrorActionPreference = 'Stop'; $held = [System.Collections.Generic.List[byte[]]]::new(); \
                    1..9 | ForEach-Object { $held.Add([byte[]]::new(1GB)) }; Write-Output 'got all 9'";
        let out = run_shell(&f.0, Shell::PowerShell, grab, CHECK_TIMEOUT, &[], never).unwrap();
        assert!(!out.passed(), "{:?}", out);
        assert!(!out.output.contains("got all 9"), "{}", out.output);
    }

    /// **A whole run cannot commit past [`RUN_MEMORY_LIMIT`]:** four processes that
    /// each hold 7 GB, each under the per-process limit, cannot all get it (28 GB
    /// against 24).
    #[cfg(windows)]
    #[test]
    #[ignore = "commits 24 GB of memory"]
    fn a_run_cannot_commit_past_its_memory_limit_across_processes() {
        let f = Folder::new("run-memory-cap");
        let grab = "$ErrorActionPreference = 'Stop'; $jobs = 1..4 | ForEach-Object { Start-Job { \
                    $held = [System.Collections.Generic.List[byte[]]]::new(); \
                    1..7 | ForEach-Object { $held.Add([byte[]]::new(1GB)) }; Start-Sleep 10; 'got 7' } }; \
                    $jobs | Wait-Job | Receive-Job";
        let out = run_shell(&f.0, Shell::PowerShell, grab, CHECK_TIMEOUT, &[], never).unwrap();
        // Whole lines: an error quotes the script, and the script says `got 7` too.
        assert!(
            out.output.lines().filter(|l| l.trim() == "got 7").count() < 4,
            "{}",
            out.output
        );
    }

    /// **A run's disk floor is [`DISK_FLOOR`], or half of what its drive started with when
    /// that is less:** a drive that begins low is not one every run is killed on.
    #[test]
    fn a_run_on_a_drive_that_starts_low_gets_a_floor_it_can_work_above() {
        const GB: u64 = 1024 * 1024 * 1024;
        assert_eq!(disk_floor_for(500 * GB), DISK_FLOOR);
        assert_eq!(disk_floor_for(2 * DISK_FLOOR), DISK_FLOOR);
        assert_eq!(disk_floor_for(6 * GB), 3 * GB);
        // Nothing free to begin with: nothing to guard.
        assert_eq!(disk_floor_for(0), 0);
    }

    /// A child that was started is put in the job and stays under its limits.
    #[cfg(windows)]
    #[test]
    fn a_running_child_is_put_in_the_job() {
        let mut cmd = Command::new("ping");
        cmd.args(["-n", "30", "127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        no_window(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        let job = confine(Job::new().unwrap(), &mut child, "ping").unwrap();
        assert!(job.is_some());
        kill_tree(&mut child, job.as_ref());
    }

    /// **A child that has already exited cannot be put in a job, and that is not a
    /// refusal:** there is nothing left to limit, so the run carries on without one.
    #[cfg(windows)]
    #[test]
    fn a_child_that_exited_before_the_job_took_it_does_not_fail_the_run() {
        let mut cmd = Command::new("cmd");
        cmd.args(["/c", "exit", "0"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        no_window(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        child.wait().unwrap();
        let job = confine(Job::new().unwrap(), &mut child, "cmd").unwrap();
        assert!(job.is_none());
    }

    /// The disk guard can read the free space where runs happen.
    #[cfg(windows)]
    #[test]
    fn the_free_space_of_a_runs_drive_can_be_read() {
        let f = Folder::new("free-space");
        assert!(free_space(&f.0).is_some_and(|free| free > 0));
    }

    #[test]
    fn a_run_stopped_for_a_full_disk_says_so() {
        let out = Outcome {
            ended: Ended::DiskFull,
            elapsed: Duration::from_secs(3),
            output: String::new(),
            bytes: 0,
            complete: true,
            blocked: None,
            unfinished_tests: 0,
        };
        assert!(for_command(Shell::PowerShell, "x", &out).contains("disk was nearly full"));
        assert!(!out.passed());
    }
}
