// Hidden tests for realprobe task 02-hugin-version. They run the built `hugin` binary in a sandbox:
// APPDATA, LOCALAPPDATA and TEMP are a fresh folder holding a config that names a model that does
// not exist, PATH holds nothing runnable, and a run that is still going after 20 s is killed.

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

fn run_hugin(tag: &str, args: &[&str]) -> Ran {
    let dir = sandbox(tag);
    let mut child = Command::new(env!("CARGO_BIN_EXE_hugin"))
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
    let (mut code, mut timed_out) = (None, false);
    loop {
        match child.try_wait().unwrap() {
            Some(status) => {
                code = status.code();
                break;
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
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

fn expected() -> String {
    format!("hugin {}", env!("CARGO_PKG_VERSION"))
}

#[test]
fn realprobe_hidden_version_prints_the_name_and_the_package_version() {
    let r = run_hugin("version-long", &["--version"]);
    assert!(!r.timed_out, "timed out");
    assert_eq!(r.code, Some(0), "stderr: {}", r.stderr);
    assert_eq!(r.stdout.trim_end_matches(['\r', '\n']), expected(), "stdout: {:?}", r.stdout);
}

#[test]
fn realprobe_hidden_capital_v_is_the_same_as_version() {
    let r = run_hugin("version-short", &["-V"]);
    assert!(!r.timed_out, "timed out");
    assert_eq!(r.code, Some(0), "stderr: {}", r.stderr);
    assert_eq!(r.stdout.trim_end_matches(['\r', '\n']), expected(), "stdout: {:?}", r.stdout);
}

#[test]
fn realprobe_hidden_the_help_lists_version_and_still_lists_help() {
    let r = run_hugin("version-help", &["--help"]);
    assert!(!r.timed_out, "timed out");
    assert_eq!(r.code, Some(0), "stderr: {}", r.stderr);
    assert!(r.stdout.contains("--version"), "{}", r.stdout);
    assert!(r.stdout.contains("--help"), "{}", r.stdout);
    let line = r.stdout.lines().find(|l| l.contains("--version")).unwrap();
    assert!(line.contains("version"), "{line}");
}

#[test]
fn realprobe_hidden_a_flag_that_only_starts_like_version_is_still_unknown() {
    let r = run_hugin("version-unknown", &["--versions"]);
    assert!(!r.timed_out, "timed out");
    assert_eq!(r.code, Some(2), "stdout: {} stderr: {}", r.stdout, r.stderr);
    assert!(r.stderr.contains("unknown argument: --versions"), "{}", r.stderr);
}
