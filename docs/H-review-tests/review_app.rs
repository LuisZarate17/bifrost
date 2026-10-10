//! Independent review of Stage H's app wiring, written by a reviewer who did not
//! write the code: `check` (H1), `config` (H1/H4b/H5/H10) and the conventions
//! `apps/mimir/src-tauri/src/lib.rs` leans on.
//!
//! Naming: `holds_*` tests pass and are evidence for a guarantee; `defect_*` tests
//! assert what the spec or the docs promise and FAIL today, each pointing at one
//! finding in the review report; `control_*` tests check a technique used by the
//! others. Everything touching `config` runs against a temporary `APPDATA`, never
//! the real one (see `sandbox`).
//!
//! Run only with: `cargo test -p odin-core --test review_app`
#![cfg(windows)]

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use odin_core::chat::{FunctionCall, Message, ToolCall};
use odin_core::check::{self, Ended, Outcome};
use odin_core::config::{self, Config};
use odin_core::edit::Workspace;
use odin_core::tools;

// ── helpers ─────────────────────────────────────────────────────────────────

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Where scratch folders go. `REVIEW_APP_TMP` lets the reviewer keep them in a
/// scratch area; otherwise the system temp folder, as the crate's own tests do.
fn base() -> PathBuf {
    std::env::var_os("REVIEW_APP_TMP").map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
}

fn unique(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let t = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
    base().join(format!("review-{tag}-{}-{t}-{n}", std::process::id()))
}

/// A throwaway folder, removed on drop.
struct Folder(PathBuf);

impl Folder {
    fn new(tag: &str) -> Self {
        let p = unique(tag);
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let p = self.0.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn cmd(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

fn never() -> bool {
    false
}

/// Point `APPDATA` at a fresh temporary folder (once per test binary) and return
/// the lock that serialises every test touching `config`. **Never call a `config`
/// function without holding this**, or the real `%APPDATA%\odin` would be used.
fn sandbox() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = DIR.get_or_init(|| {
        let d = unique("appdata");
        std::fs::create_dir_all(&d).unwrap();
        // SAFETY: Windows environment access is thread-safe at the OS level, and
        // this runs once, under the lock, before any config call in this binary.
        unsafe { std::env::set_var("APPDATA", &d) };
        d
    });
    let path = config::path().expect("APPDATA is set");
    assert!(path.starts_with(dir), "refusing to touch a config outside the sandbox: {}", path.display());
    let _ = std::fs::remove_dir_all(dir.join("odin"));
    guard
}

fn config_file() -> PathBuf {
    let p = config::path().unwrap();
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    p
}

/// Whether some process still has `path` open. A rename needs DELETE access, which
/// an open handle without FILE_SHARE_DELETE refuses; nothing is changed on success.
fn held(path: &Path) -> bool {
    let probe = path.with_extension("probe");
    match std::fs::rename(path, &probe) {
        Ok(()) => {
            std::fs::rename(&probe, path).expect("put the probe file back");
            false
        }
        Err(_) => true,
    }
}

fn wait_released(path: &Path, what: &str) {
    assert!(path.exists(), "setup: {} was never created", path.display());
    let deadline = Instant::now() + Duration::from_secs(3);
    while held(path) {
        assert!(
            Instant::now() < deadline,
            "{what}: {} is still open in a process the check started",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Run `type blob.bin` through `check::run`, so the bytes arrive through the pipe.
fn type_file(f: &Folder, bytes: &[u8]) -> Outcome {
    f.write("blob.bin", bytes);
    check::run(&f.0, &cmd(&["cmd", "/c", "type", "blob.bin"]), check::CHECK_TIMEOUT, &[], never).unwrap()
}

fn call(name: &str, args: serde_json::Value) -> ToolCall {
    ToolCall { id: None, function: FunctionCall { name: name.into(), arguments: args } }
}

fn asks(c: &ToolCall) -> Message {
    Message { role: "assistant".into(), content: String::new(), images: None, tool_calls: Some(vec![c.clone()]), tool_name: None }
}

// ── check: the technique used below ─────────────────────────────────────────

/// The "is anything of the tree still alive" probe must not be vacuous: a running
/// `ping` really keeps the file its output is redirected to open.
#[test]
fn control_a_live_ping_holds_its_redirect_file_and_a_dead_one_does_not() {
    let f = Folder::new("control");
    let mut child = std::process::Command::new("cmd")
        .args(["/c", "ping -n 5 127.0.0.1 > held.txt"])
        .current_dir(&f.0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(900));
    let file = f.0.join("held.txt");
    assert!(file.exists());
    assert!(held(&file), "premise: a running ping should keep held.txt open");
    let _ = child.wait();
    wait_released(&file, "after the control command ended by itself");
}

// ── check: killing the tree ─────────────────────────────────────────────────

/// **A timeout ends the whole tree**, not only `cmd`: nothing the command started
/// is left holding the file its output goes to, and the run says TimedOut, never
/// an exit code.
#[test]
fn holds_a_timeout_ends_the_whole_tree_and_is_not_reported_as_an_exit() {
    let f = Folder::new("timeout");
    let started = Instant::now();
    let argv = cmd(&["cmd", "/c", "ping -n 40 127.0.0.1 > held.txt"]);
    let out = check::run(&f.0, &argv, Duration::from_millis(1200), &[], never).unwrap();
    assert_eq!(out.ended, Ended::TimedOut, "{out:?}");
    assert!(!matches!(out.ended, Ended::Exited(_)) && !out.passed());
    assert!(out.complete, "the pipe did not close: something survived the kill");
    assert!(started.elapsed() < Duration::from_secs(10), "took {:?}", started.elapsed());
    wait_released(&f.0.join("held.txt"), "after a timeout");
    let said = check::for_model(&argv, &out);
    assert!(said.contains("time limit") && !said.starts_with("error:"), "{said}");
}

/// **Stop ends the whole tree** the same way, and says Stopped.
#[test]
fn holds_stop_ends_the_whole_tree_and_is_not_reported_as_an_exit() {
    let f = Folder::new("stop");
    let started = Instant::now();
    let argv = cmd(&["cmd", "/c", "ping -n 40 127.0.0.1 > held.txt"]);
    let out = check::run(&f.0, &argv, check::CHECK_TIMEOUT, &[], || started.elapsed() > Duration::from_millis(1200))
        .unwrap();
    assert_eq!(out.ended, Ended::Stopped, "{out:?}");
    assert!(!matches!(out.ended, Ended::Exited(_)) && !out.passed());
    assert!(out.complete);
    assert!(started.elapsed() < Duration::from_secs(10), "took {:?}", started.elapsed());
    wait_released(&f.0.join("held.txt"), "after Stop");
    let said = check::for_model(&argv, &out);
    assert!(said.contains("stopped by the user") && !said.starts_with("error:"), "{said}");
}

/// A command that exits and leaves a background child running: the child ends with
/// it, and the run does not wait out the reader's deadline.
#[test]
fn holds_a_background_child_is_ended_when_the_command_exits() {
    let f = Folder::new("linger");
    let started = Instant::now();
    let out = check::run(
        &f.0,
        &cmd(&["cmd", "/c", "start /b ping -n 40 127.0.0.1 > held.txt"]),
        check::CHECK_TIMEOUT,
        &[],
        never,
    )
    .unwrap();
    assert!(out.passed(), "{out:?}");
    assert!(out.complete, "the background ping outlived the check");
    assert!(started.elapsed() < Duration::from_millis(1900), "took {:?}", started.elapsed());
    wait_released(&f.0.join("held.txt"), "after the command exited");
}

// ── check: output caps ──────────────────────────────────────────────────────

#[test]
fn holds_the_pipe_is_byte_exact_for_utf8_text() {
    let f = Folder::new("utf8-small");
    let text = "caf\u{e9} \u{2500} \u{f1}\n";
    let out = type_file(&f, text.as_bytes());
    assert!(out.output.contains(text.trim_end()), "{:?}", out.output);
    assert_eq!(out.bytes as usize, text.len());
}

#[test]
fn holds_the_byte_count_and_the_cut_marker_are_exact() {
    let f = Folder::new("caps");
    let total = 30_000usize;
    let out = type_file(&f, &vec![b'x'; total]);
    assert_eq!(out.bytes as usize, total);
    assert!(out.output.contains(&format!("{} bytes cut", total - (check::HEAD + check::TAIL))), "marker missing");
    assert!(out.output.starts_with("xxxx") && out.output.trim_end().ends_with("xxxx"));
    assert!(out.output.len() < check::HEAD + check::TAIL + 100, "{} bytes kept", out.output.len());
}

/// **Finding F6.** Head and tail are decoded separately, so a character on the 6 KB
/// boundary is garbled even when nothing at all was cut (6,245 bytes in total).
#[test]
fn defect_a_character_on_the_head_boundary_is_garbled_even_when_nothing_was_cut() {
    let f = Folder::new("utf8-edge");
    let mut v = vec![b'a'; check::HEAD - 1];
    v.extend_from_slice("\u{e9}".as_bytes());
    v.extend_from_slice(&[b'b'; 100]);
    let out = type_file(&f, &v);
    assert_eq!(out.bytes as usize, v.len(), "premise: byte-exact pipe");
    assert!(!out.output.contains("bytes cut"), "premise: nothing was cut");
    let around: String = out.output.chars().skip(check::HEAD - 3).take(8).collect();
    assert!(out.output.contains('\u{e9}'), "the e-acute on the boundary came back as {around:?}");
}

/// **Finding F6 (cut case).** At a real cut the partial sequences at both ends of
/// the marker become U+FFFD.
#[test]
fn defect_a_cut_leaves_replacement_characters_at_both_ends_of_the_marker() {
    let f = Folder::new("utf8-cut");
    let total = 20_000usize;
    let mut v = vec![b'a'; total];
    v[check::HEAD - 1..check::HEAD + 1].copy_from_slice("\u{e9}".as_bytes());
    let tail_start = total - check::TAIL;
    v[tail_start - 1..tail_start + 2].copy_from_slice("\u{2500}".as_bytes());
    let out = type_file(&f, &v);
    assert_eq!(out.bytes as usize, total, "premise: byte-exact pipe");
    assert!(out.output.contains("bytes cut"));
    let n = out.output.matches('\u{FFFD}').count();
    assert_eq!(n, 0, "{n} replacement characters around the cut marker");
}

// ── check: environment, working directory, errors ───────────────────────────

#[test]
fn holds_colour_is_off_and_the_cwd_search_is_off_in_the_child() {
    let f = Folder::new("env");
    let out = check::run(
        &f.0,
        &cmd(&["cmd", "/c", "echo %NO_COLOR%/%CARGO_TERM_COLOR%/%NoDefaultCurrentDirectoryInExePath%"]),
        check::CHECK_TIMEOUT,
        &[],
        never,
    )
    .unwrap();
    assert!(out.output.contains("1/never/1"), "{:?}", out.output);
}

/// **`NoDefaultCurrentDirectoryInExePath` works**: a `hello.cmd` the model wrote
/// into the project is not run by a bare `hello`, while plain cmd.exe would.
#[test]
fn holds_a_script_in_the_project_folder_is_not_run_by_a_bare_name() {
    let f = Folder::new("cwd-search");
    // The harness that runs these tests may already set the variable; the premise and the test need it unset here.
    // SAFETY: Windows environment access is thread-safe at the OS level; no test depends on this variable being set.
    unsafe { std::env::remove_var("NoDefaultCurrentDirectoryInExePath") };
    f.write("hello.cmd", b"@echo PWNED\r\n");
    let plain = std::process::Command::new("cmd")
        .args(["/c", "hello"])
        .current_dir(&f.0)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&plain.stdout).contains("PWNED"), "premise: plain cmd.exe searches the cwd");
    let out = check::run(&f.0, &cmd(&["cmd", "/c", "hello"]), check::CHECK_TIMEOUT, &[], never).unwrap();
    assert!(!out.output.contains("PWNED"), "{:?}", out.output);
    assert!(!out.passed());
}

#[test]
fn holds_a_missing_program_and_a_missing_folder_are_errors_naming_the_command() {
    let f = Folder::new("missing");
    let e = check::run(&f.0, &cmd(&["no-such-program-odin"]), check::CHECK_TIMEOUT, &[], never).unwrap_err();
    assert!(e.contains("could not start") && e.contains("no-such-program-odin"), "{e}");
    let gone = f.0.join("nope");
    let e = check::run(&gone, &cmd(&["cmd", "/c", "echo hi"]), check::CHECK_TIMEOUT, &[], never).unwrap_err();
    assert!(e.contains("could not start"), "{e}");
    assert_eq!(check::run(&f.0, &[], check::CHECK_TIMEOUT, &[], never).unwrap_err(), "no command was given");
}

/// A failing build whose own output begins `error:` is still not an `error:`
/// result, so three identical failing checks do not end the turn as "stuck".
#[test]
fn holds_a_failing_check_whose_output_starts_with_error_is_not_a_failed_call() {
    let f = Folder::new("failing");
    let argv = cmd(&["cmd", "/c", "echo error: boom & exit 2"]);
    let out = check::run(&f.0, &argv, check::CHECK_TIMEOUT, &[], never).unwrap();
    assert_eq!(out.ended, Ended::Exited(Some(2)));
    let said = check::for_model(&argv, &out);
    assert!(said.contains("failed (exit 2") && !said.starts_with("error:"), "{said}");
    let c = call("run_check", serde_json::json!({}));
    let mut history = vec![Message::user("fix it")];
    for _ in 0..3 {
        history.push(asks(&c));
        history.push(Message::tool_result("run_check", said.clone()));
    }
    assert_eq!(tools::prior_failures(&history, &c), 0);
}

/// The signature itself is recognised in a spawn error's text; see finding F5: the
/// app never asks, because `run` returns before it can look.
#[test]
fn holds_the_4551_signature_is_recognised_in_a_spawn_error_text() {
    let f = Folder::new("sac");
    let text = "could not start `target/debug/tool.exe --check` in C:/p: An Application Control policy has \
                blocked this file. (os error 4551)";
    assert!(check::blocked_by_smart_app_control(&f.0, text).is_some());
}

// ── check: candidates ───────────────────────────────────────────────────────

#[test]
fn holds_npm_candidates_need_real_scripts_and_the_placeholder_is_not_one() {
    let npm = "npm.cmd";
    let f = Folder::new("npm");
    f.write("package.json", br#"{"scripts":{"test":"vitest run","build":"vite build","lint":"x"}}"#);
    assert_eq!(
        check::candidates(&Workspace::open(&f.0).unwrap()),
        vec![cmd(&[npm, "test"]), cmd(&[npm, "run", "build"])]
    );
    let g = Folder::new("npm-placeholder");
    g.write("package.json", br#"{"scripts":{"test":"echo \"Error: no test specified\" && exit 1","build":5}}"#);
    assert!(check::candidates(&Workspace::open(&g.0).unwrap()).is_empty());
}

/// **Finding F7.** npm reads a `package.json` with a UTF-8 byte-order mark (Windows
/// editors and PowerShell 5.1's `-Encoding UTF8` write one); `candidates` parses it
/// with `serde_json`, which refuses the BOM, so no npm check is offered at all.
#[test]
fn defect_a_utf8_bom_in_package_json_hides_every_npm_check() {
    let f = Folder::new("npm-bom");
    let mut text = vec![0xEF, 0xBB, 0xBF];
    text.extend_from_slice(br#"{"scripts":{"test":"vitest run"}}"#);
    f.write("package.json", &text);
    let offered = check::candidates(&Workspace::open(&f.0).unwrap());
    assert_eq!(offered, vec![cmd(&["npm.cmd", "test"])], "a BOM'd package.json offers {offered:?}");
}

// ── config ──────────────────────────────────────────────────────────────────

#[test]
fn holds_config_round_trips_through_disk_with_the_h4b_h5_h10_fields() {
    let _g = sandbox();
    let mut c = Config::default();
    c.set_check(r"C:\Work\Demo", Some(vec!["cargo".into(), "check".into()]));
    c.set_instructions(r"C:\Work\Demo", true);
    c.model_before_coding = Some("qwen3-vl:8b-instruct-q4_K_M".into());
    c.coding_model = "qwen3-coder:30b".into();
    config::save(&c).unwrap();
    let back = config::load();
    assert_eq!(back, c);
    assert_eq!(back.check_for(r"c:\work\demo").map(|v| v.join(" ")), Some("cargo check".into()));
    assert!(back.instructions_for(r"C:\WORK\DEMO"));
}

/// **Finding F2a.** A syntax slip while hand-editing `config.json` (the documented
/// way to set a custom check) makes `load()` answer with defaults (by design), and
/// the very next save from any Mimir command writes those defaults over the file.
/// Nothing keeps the malformed original, so the hand-typed content is gone.
#[test]
fn defect_a_syntax_slip_in_config_json_is_erased_by_the_next_save_with_no_backup() {
    let _g = sandbox();
    let p = config_file();
    std::fs::write(&p, r#"{"model":"mine","checks":{"C:/proj":["cargo","clippy","--all"]},}"#).unwrap();
    assert_eq!(config::load(), Config::default(), "premise: a malformed file loads as defaults");
    // What any Mimir command does: load, change one field, save.
    let mut c = config::load();
    c.num_ctx = 16384;
    config::save(&c).unwrap();
    let survives = std::fs::read_dir(p.parent().unwrap())
        .unwrap()
        .flatten()
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .any(|t| t.contains("clippy"));
    assert!(survives, "the hand-typed check exists nowhere after one unrelated save");
}

/// **Finding F2b.** Only `checks`, `ask_every_call` and `instructions` are read
/// leniently. A `null` where a string is expected (the natural way to write "no
/// coding model") fails the whole file and resets every other setting.
#[test]
fn defect_a_null_in_a_strict_field_resets_every_other_setting() {
    let _g = sandbox();
    let p = config_file();
    std::fs::write(&p, r#"{"model":"mine","coding_model":null,"checks":{"C:/proj":["cargo","check"]}}"#).unwrap();
    let c = config::load();
    assert_eq!(c.model, "mine", "everything else was reset: {c:?}");
}

/// **Finding F2c.** A field this build does not know is dropped by the next save.
#[test]
fn defect_unknown_fields_are_dropped_by_the_next_save() {
    let _g = sandbox();
    let p = config_file();
    std::fs::write(&p, r#"{"model":"mine","future_setting":{"a":1}}"#).unwrap();
    let c = config::load();
    assert_eq!(c.model, "mine");
    config::save(&c).unwrap();
    let after = std::fs::read_to_string(&p).unwrap();
    assert!(after.contains("future_setting"), "dropped:\n{after}");
}

/// **Finding F3.** Every config command in `lib.rs` is `load`, change, `save` with
/// no lock; two that overlap lose one update. H6's results call this race "real"
/// and defer a lock; H10 added another writer (`set_notes`) and
/// `accept_model_offer` holds a stale copy across a network call.
#[test]
fn defect_two_config_commands_that_read_before_either_writes_lose_one_update() {
    let _g = sandbox();
    let folder = r"C:\review\proj";
    let mut set_check = config::load();
    let mut set_notes = config::load();
    set_check.set_check(folder, Some(vec!["cargo".into(), "check".into()]));
    config::save(&set_check).unwrap();
    set_notes.set_instructions(folder, true);
    config::save(&set_notes).unwrap();
    let now = config::load();
    assert!(
        now.check_for(folder).is_some() && now.instructions_for(folder),
        "check kept: {}, notes kept: {}",
        now.check_for(folder).is_some(),
        now.instructions_for(folder)
    );
}

/// **Finding F2d (probabilistic).** `save` is `fs::write`: it truncates in place,
/// unlike `store::save` in the same crate (temp file and rename). A reader (Heimdall
/// reads this file on every summon) can catch it empty or half written, and
/// `load()` then answers with defaults. A large file makes the window observable.
#[test]
fn defect_a_reader_can_catch_config_json_half_written() {
    let _g = sandbox();
    let mut big = Config::default();
    big.recent_workspaces =
        (0..3000).map(|i| format!(r"C:\review\recent\folder-{i:05}-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx")).collect();
    config::save(&big).unwrap();
    let p = config::path().unwrap();
    let full = std::fs::metadata(&p).unwrap().len();
    let done = Arc::new(AtomicBool::new(false));
    let writer = {
        let (done, big) = (done.clone(), big.clone());
        std::thread::spawn(move || {
            for _ in 0..600 {
                config::save(&big).unwrap();
            }
            done.store(true, Ordering::SeqCst);
        })
    };
    let (mut torn, mut reads) = (0u32, 0u32);
    while !done.load(Ordering::SeqCst) {
        if let Ok(bytes) = std::fs::read(&p) {
            reads += 1;
            if bytes.len() as u64 != full {
                torn += 1;
            }
        }
    }
    writer.join().unwrap();
    eprintln!("torn reads: {torn} of {reads}");
    assert_eq!(torn, 0, "{torn} of {reads} reads saw a config.json of the wrong length");
}

// ── tools: conventions the wiring relies on ─────────────────────────────────

/// **Finding F10.** Success is inferred from the text: `Ran::text` (lib.rs:1164) marks
/// anything starting `error:` as failed, and `prior_failures` counts it the same
/// way. A successful read of a file that itself begins `error:` (a captured
/// compiler log, say) is indistinguishable from a failed read.
#[test]
fn defect_a_file_that_starts_with_error_colon_reads_like_a_failed_call() {
    let f = Folder::new("errfile");
    let p = f.write("build.log", b"error: could not compile `demo`\nmore output\n");
    let said = tools::run_read_file(&p, None);
    assert!(said.contains("could not compile"), "premise: the read worked: {said}");
    assert!(!said.starts_with("error:"), "a successful read looks like a failure: {said:?}");
}

/// The consequence of the above: three successful reads of such a file end the turn.
#[test]
fn defect_three_successful_reads_of_such_a_file_end_the_turn_as_stuck() {
    let f = Folder::new("errfile-stuck");
    let p = f.write("build.log", b"error: could not compile `demo`\nmore output\n");
    let c = call("read_file", serde_json::json!({ "path": "build.log" }));
    let result = tools::run_read_file(&p, None);
    let mut history = vec![Message::user("what does the log say")];
    for _ in 0..2 {
        history.push(asks(&c));
        history.push(Message::tool_result("read_file", result.clone()));
    }
    let prior = tools::prior_failures(&history, &c);
    assert!(prior + 1 < tools::MAX_IDENTICAL_FAILURES, "prior failures {prior}: the third read is refused as stuck");
}

/// The 7th `update_plan` in a turn is the first refused, whether or not the earlier
/// ones were accepted, and an earlier turn's updates do not count.
#[test]
fn holds_the_seventh_plan_update_is_the_first_refused() {
    let plan = call("update_plan", serde_json::json!({ "plan": "[ ] one" }));
    let mut history = vec![Message::user("an earlier turn")];
    for _ in 0..tools::MAX_PLAN_UPDATES + 3 {
        history.push(asks(&plan));
        history.push(Message::tool_result("update_plan", "Plan updated: 0 of 1 done."));
    }
    history.push(Message::user("this turn"));
    for n in 1..=tools::MAX_PLAN_UPDATES + 1 {
        let refused = tools::plan_updates(&history) >= tools::MAX_PLAN_UPDATES;
        assert_eq!(refused, n == tools::MAX_PLAN_UPDATES + 1, "update {n}");
        history.push(asks(&plan));
        history.push(Message::tool_result("update_plan", "Plan updated: 0 of 1 done."));
    }
}

#[test]
fn holds_a_plan_never_asks_even_with_ask_every_call_and_no_folder() {
    let req = tools::interpret(&call("update_plan", serde_json::json!({ "plan": "[x] a\n[~] b\n[ ] c" }))).unwrap();
    for ask_every_call in [false, true] {
        assert!(
            matches!(tools::approval(None, &req, ask_every_call), tools::Approval::Auto(tools::Auto::Plan { .. })),
            "ask_every_call={ask_every_call}"
        );
    }
}

/// A move onto a build file, and the delete or move of one, are named on the next
/// Run-check card, and only when the change was actually written.
#[test]
fn holds_build_files_changed_sees_moves_onto_and_deletes_of_build_files() {
    let mv = call("move_file", serde_json::json!({ "path": "tools/gen.rs", "to": "BUILD.RS" }));
    let del = call("delete_file", serde_json::json!({ "path": "sub/Cargo.toml" }));
    let refused = call("edit_file", serde_json::json!({ "path": "package.json", "old_text": "a", "new_text": "b" }));
    let history = vec![
        Message::user("go"),
        asks(&mv),
        Message::tool_result("move_file", "Moved tools/gen.rs to BUILD.RS."),
        asks(&del),
        Message::tool_result("delete_file", "Deleted sub/Cargo.toml."),
        asks(&refused),
        Message::tool_result("edit_file", "error: the text was not found"),
    ];
    assert_eq!(tools::build_files_changed(&history), vec!["BUILD.RS".to_string(), "sub/Cargo.toml".to_string()]);
}

/// **Finding F2e (the safety switch fails open).** tools.rs promises that
/// `"ask_every_call"` set but unreadable counts as `true`, and config.rs says "a
/// typo can only make Mimir stricter". That holds for a bad *value*; a syntax
/// slip anywhere else in the file makes `load()` return every default, and the
/// default is `false`: reads, listings and searches inside the folder run unasked
/// with no message, although the user asked to be asked every time.
#[test]
fn defect_a_syntax_slip_turns_ask_every_call_off() {
    let _g = sandbox();
    let p = config_file();
    std::fs::write(&p, r#"{"ask_every_call":true,"model":"mine",}"#).unwrap();
    let c = config::load();
    assert!(c.ask_every_call, "the user's 'ask every time' came back as {}", c.ask_every_call);
}

// ── a few more guarantees, checked exhaustively ─────────────────────────────

/// **`for_model` never starts with `error:`**, whatever ended the run, whether the
/// output was complete, blocked or not, and whatever the output itself says.
#[test]
fn holds_for_model_never_starts_with_error_for_any_outcome() {
    let argv = cmd(&["cargo", "check"]);
    let ends = [Ended::Exited(Some(0)), Ended::Exited(Some(101)), Ended::Exited(Some(-1)), Ended::Exited(None), Ended::TimedOut, Ended::Stopped];
    for ended in ends {
        for complete in [true, false] {
            for blocked in [None, Some("a program the build needed".to_string())] {
                for output in ["", "error: boom", "\n\n  error[E0425]: cannot find function", "error:"] {
                    let o = Outcome {
                        ended: ended.clone(),
                        elapsed: Duration::from_millis(1500),
                        output: output.to_string(),
                        bytes: output.len() as u64,
                        complete,
                        blocked: blocked.clone(),
                    };
                    let said = check::for_model(&argv, &o);
                    assert!(!said.trim_start().starts_with("error:"), "{ended:?} complete={complete} blocked={blocked:?}: {said}");
                }
            }
        }
    }
}

#[test]
fn holds_a_folder_with_both_manifests_offers_cargo_first_then_npm() {
    let f = Folder::new("both");
    f.write("Cargo.toml", b"[package]\nname = \"x\"\n");
    f.write("package.json", br#"{"scripts":{"test":"jest","build":"tsc"}}"#);
    let offered: Vec<String> = check::candidates(&Workspace::open(&f.0).unwrap()).iter().map(|a| check::label(a)).collect();
    assert_eq!(offered, ["cargo check", "cargo test", "npm.cmd test", "npm.cmd run build"]);
}

#[test]
fn holds_candidates_survive_garbage_manifests() {
    for text in [&b"not json"[..], b"[]", b"{\"scripts\": []}", b"{\"scripts\": {\"test\": null, \"build\": 7}}", b""] {
        let f = Folder::new("garbage");
        f.write("package.json", text);
        assert!(check::candidates(&Workspace::open(&f.0).unwrap()).is_empty(), "{:?}", String::from_utf8_lossy(text));
    }
}

/// **Finding F2f.** The same applies to a UTF-8 byte-order mark, which Windows
/// editors and PowerShell 5.1 (`Set-Content -Encoding UTF8`) put in the file: the
/// whole config reads as defaults, and the next save writes them over it.
#[test]
fn defect_a_utf8_bom_in_config_json_makes_the_whole_file_read_as_defaults() {
    let _g = sandbox();
    let p = config_file();
    let mut text = vec![0xEF, 0xBB, 0xBF];
    text.extend_from_slice(br#"{"model":"mine","ask_every_call":true}"#);
    std::fs::write(&p, text).unwrap();
    let c = config::load();
    assert_eq!((c.model.as_str(), c.ask_every_call), ("mine", true), "a BOM'd config.json loaded as {c:?}");
}
