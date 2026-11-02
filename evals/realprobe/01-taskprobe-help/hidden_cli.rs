// Hidden tests for realprobe task 01-taskprobe-help. They run the built `taskprobe` binary in a
// sandbox: APPDATA, LOCALAPPDATA and TEMP are a fresh folder holding a config that names a model
// that does not exist, PATH holds nothing runnable, and a run still going after 20 s is killed.
// Every command line here must be turned away or answered before anything else happens.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

struct Ran {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    timed_out: bool,
}

fn sandbox(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("realprobe-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("bifrost")).unwrap();
    std::fs::write(
        dir.join("bifrost").join("config.json"),
        r#"{"model":"realprobe-no-such-model","coding_model":"realprobe-no-such-model"}"#,
    )
    .unwrap();
    dir
}

fn run_taskprobe(tag: &str, args: &[&str]) -> Ran {
    let dir = sandbox(tag);
    let mut child = Command::new(env!("CARGO_BIN_EXE_taskprobe"))
        .args(args)
        .env("APPDATA", &dir)
        .env("LOCALAPPDATA", &dir)
        .env("TEMP", &dir)
        .env("TMP", &dir)
        .env("PATH", &dir)
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let t_out = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = out.read_to_end(&mut b);
        String::from_utf8_lossy(&b).into_owned()
    });
    let t_err = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = err.read_to_end(&mut b);
        String::from_utf8_lossy(&b).into_owned()
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    let (code, timed_out);
    loop {
        match child.try_wait().unwrap() {
            Some(status) => {
                code = status.code();
                timed_out = false;
                break;
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                code = None;
                timed_out = true;
                break;
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    let ran = Ran { code, stdout: t_out.join().unwrap(), stderr: t_err.join().unwrap(), timed_out };
    let _ = std::fs::remove_dir_all(&dir);
    ran
}

const FLAGS: [&str; 12] = [
    "--model", "--runs", "--only", "--mode", "--compact", "--read", "--plan", "--ops", "--think", "--keep",
    "--selfcheck", "--help",
];

fn assert_lists_every_flag(text: &str) {
    for flag in FLAGS {
        assert!(text.contains(flag), "usage does not mention {flag}:\n{text}");
    }
}

#[test]
fn realprobe_hidden_help_prints_usage_with_every_flag_and_exits_0() {
    let r = run_taskprobe("tp-help", &["--help"]);
    assert!(!r.timed_out, "timed out");
    assert_eq!(r.code, Some(0), "stdout: {} stderr: {}", r.stdout, r.stderr);
    assert_lists_every_flag(&r.stdout);
    assert!(!r.stdout.contains("H3 — task probe"), "the probe started: {}", r.stdout);
}

#[test]
fn realprobe_hidden_short_h_is_help_too() {
    let r = run_taskprobe("tp-h", &["-h"]);
    assert!(!r.timed_out, "timed out");
    assert_eq!(r.code, Some(0), "stdout: {} stderr: {}", r.stdout, r.stderr);
    assert_lists_every_flag(&r.stdout);
}

#[test]
fn realprobe_hidden_an_unknown_flag_exits_2_and_is_named_on_stderr_with_the_usage() {
    let r = run_taskprobe("tp-bogus", &["--bogus"]);
    assert!(!r.timed_out, "timed out");
    assert_eq!(r.code, Some(2), "stdout: {} stderr: {}", r.stdout, r.stderr);
    assert!(r.stderr.contains("unknown flag: --bogus"), "{}", r.stderr);
    assert_lists_every_flag(&r.stderr);
    assert!(!r.stdout.contains("H3 — task probe"), "the probe started: {}", r.stdout);
}

#[test]
fn realprobe_hidden_an_unknown_flag_after_good_ones_is_still_refused_before_any_run() {
    let r = run_taskprobe("tp-late", &["--model", "x", "--runs", "1", "--bogus"]);
    assert!(!r.timed_out, "timed out");
    assert_eq!(r.code, Some(2), "stdout: {} stderr: {}", r.stdout, r.stderr);
    assert!(r.stderr.contains("unknown flag: --bogus"), "{}", r.stderr);
    assert!(!r.stdout.contains("H3 — task probe"), "the probe started: {}", r.stdout);
}

#[test]
fn realprobe_hidden_a_misspelt_flag_and_an_equals_form_are_refused_with_their_whole_text() {
    let r = run_taskprobe("tp-typo", &["--mdoel", "x"]);
    assert_eq!(r.code, Some(2), "stdout: {} stderr: {}", r.stdout, r.stderr);
    assert!(r.stderr.contains("unknown flag: --mdoel"), "{}", r.stderr);
    let r = run_taskprobe("tp-eq", &["--model=x"]);
    assert_eq!(r.code, Some(2), "stdout: {} stderr: {}", r.stdout, r.stderr);
    assert!(r.stderr.contains("unknown flag: --model=x"), "{}", r.stderr);
    let r = run_taskprobe("tp-short", &["-z"]);
    assert_eq!(r.code, Some(2), "stdout: {} stderr: {}", r.stdout, r.stderr);
    assert!(r.stderr.contains("unknown flag: -z"), "{}", r.stderr);
}
