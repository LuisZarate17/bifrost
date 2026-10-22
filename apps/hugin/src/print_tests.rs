//! `hugin -p`, run whole over a scripted model: what it hands back, what it leaves alone, and
//! how it ends. The model and the runner are a script; everything else is the real thing — the
//! copy, the edit tools, the check, the patch and the report — against a repository made for
//! each test, with a git configuration of its own so that nothing depends on the machine's.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bifrost_core::agent::round::{Reply, Round};
use bifrost_core::agent::run::Ran;
use bifrost_core::agent::send::{Calibration, compact_for_send};
use bifrost_core::agent::ticket::PendingCall;
use bifrost_core::chat::{self, Message};
use bifrost_core::scratch::{self, Git};
use bifrost_core::{check, edit, tools};
use serde_json::json;

use crate::print::*;

// ── a place to work ─────────────────────────────────────────────────────────

struct World {
    dir: PathBuf,
    git: Git,
    env: Vec<(String, String)>,
}

impl World {
    fn new(tag: &str) -> Self {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
        let dir = std::env::temp_dir().join(format!("hugin-print-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("global.gitconfig");
        std::fs::write(&config, "[user]\n\tname = t\n\temail = t@t\n").unwrap();
        let env = vec![
            ("GIT_CONFIG_GLOBAL".to_string(), config.to_string_lossy().into_owned()),
            ("GIT_CONFIG_NOSYSTEM".to_string(), "1".to_string()),
        ];
        World { dir, git: Git::with_env(env.clone()), env }
    }

    /// A repository whose files are checked out with CRLF, as this machine's are.
    fn repo(&self, name: &str) -> Repo {
        let root = self.dir.join(name);
        std::fs::create_dir_all(&root).unwrap();
        let repo = Repo { root, env: self.env.clone() };
        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&["config", "core.autocrlf", "true"]);
        repo
    }

    fn work(&self) -> PathBuf {
        self.dir.join("work")
    }

    fn job(&self, folder: &Path, check: Option<Vec<String>>, secs: u64) -> Job {
        Job {
            task: "the task".to_string(),
            folder: folder.to_path_buf(),
            check,
            time: Duration::from_secs(secs),
            started: Instant::now(),
            work: self.work(),
            cache: self.dir.join("cache"),
        }
    }

    /// Run a scripted model over `repo`; the script's log says which calls reached the runner.
    /// The copy is gone when this returns.
    fn run(&self, repo: &Repo, check: Option<Vec<String>>, secs: u64, steps: Vec<Step>) -> (Report, Log) {
        let (wire, log) = script(steps);
        (execute(&self.job(&repo.root, check, secs), &self.git, "scripted-model", wire).report, log)
    }

    /// Run folders left in the work folder: a finished run leaves none.
    fn runs_left(&self) -> usize {
        std::fs::read_dir(self.work())
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("run-"))
            .count()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        for _ in 0..20 {
            if std::fs::remove_dir_all(&self.dir).is_ok() || !self.dir.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

struct Repo {
    root: PathBuf,
    env: Vec<(String, String)>,
}

impl Repo {
    fn git(&self, args: &[&str]) -> String {
        let mut cmd = Command::new("git");
        cmd.args(args).current_dir(&self.root);
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("git runs");
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn commit(&self) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", "x"]);
    }
}

fn have_git() -> bool {
    Command::new("git").arg("--version").output().is_ok()
}

/// `dir` is exactly what `before` says, times included, and a failure names what differs.
fn unchanged(dir: &Path, before: &BTreeMap<String, scratch::Print>) {
    let now = scratch::fingerprints(dir).unwrap();
    let differ: Vec<&String> = before.keys().chain(now.keys()).filter(|k| before.get(*k) != now.get(*k)).take(10).collect();
    assert!(differ.is_empty(), "the user's folder changed: {differ:?}");
}

// ── the model, scripted ─────────────────────────────────────────────────────

/// What the scripted model does on each request, in order.
enum Step {
    /// Ask to replace `.1` with `.2` in the file `.0`.
    Edit(&'static str, &'static str, &'static str),
    /// Ask to run the check.
    Check,
    /// Ask to read a file.
    Read(&'static str),
    Answer(&'static str),
    /// Take this long to reply, as a request that cannot be interrupted does.
    Sleep(u64),
    /// The request fails.
    Fail(&'static str),
}

/// The names of the calls that reached the runner, in order.
type Log = Arc<Mutex<Vec<String>>>;

fn round(reply: Reply) -> Result<Round, String> {
    Ok(Round { reply, prompt_eval_count: 0, recovered: false, sent: compact_for_send(&mut [], &[], 8192, None, 0) })
}

fn asks(name: &str, path: &str, token: &str, arguments: serde_json::Value) -> Result<Round, String> {
    round(Reply::Call {
        call: PendingCall {
            token: token.into(),
            name: name.into(),
            path: path.into(),
            diff: None,
            range: None,
            changed_build_files: Vec::new(),
            to: None,
            folders: Vec::new(),
            size: None,
        },
        raw_call: chat::ToolCall { id: None, function: chat::FunctionCall { name: name.into(), arguments } },
    })
}

/// A model that does `steps` and a runner that does what an approved call does: an edit is
/// planned when it is asked for, against the file as it is then, and applied with the real edit
/// code into the run's own backups; the check really runs. (The real runner redeems a ticket
/// from one slot every test in the process would share.)
fn script(steps: Vec<Step>) -> (impl FnOnce(&Ctx) -> Wired, Log) {
    let log = Log::default();
    let seen = log.clone();
    let wire = move |ctx: &Ctx| {
        let plans: Arc<Mutex<HashMap<String, edit::Plan>>> = Arc::default();
        let (planned, applying) = (plans.clone(), plans);
        let (root, backups) = (ctx.root.clone(), ctx.env.backups.clone().expect("backups are in the run's folder"));
        let (argv, check_env) = (ctx.check.clone().unwrap_or_default(), ctx.env.check_env.clone());
        let (mut steps, mut asked) = (VecDeque::from(steps), 0);
        let (asking_root, asking_argv) = (root.clone(), argv.clone());
        let ask = move |_: &[Message], _: Option<Calibration>, _: bool| {
            let (root, argv) = (&asking_root, &asking_argv);
            let step = steps.pop_front().expect("the loop asked the model more often than the script allows");
            asked += 1;
            let token = format!("t{asked}");
            match step {
                Step::Answer(text) => round(Reply::Answer { content: text.into(), eval_count: 7 }),
                Step::Fail(why) => Err(why.to_string()),
                Step::Sleep(secs) => {
                    std::thread::sleep(Duration::from_secs(secs));
                    round(Reply::Answer { content: "too late".into(), eval_count: 7 })
                }
                Step::Edit(path, old, new) => {
                    let ws = edit::Workspace::open(root).unwrap();
                    let plan = edit::plan_replace(&ws, path, old, new).unwrap();
                    planned.lock().unwrap().insert(token.clone(), plan.clone());
                    asks("edit_file", &plan.display, &token, json!({ "path": path, "old_text": old, "new_text": new }))
                }
                Step::Check => asks("run_check", &check::label(argv), &token, json!({})),
                Step::Read(path) => asks("read_file", path, &token, json!({ "path": path })),
            }
        };
        let run = move |name: &str, _path: &str, token: &str| {
            seen.lock().unwrap().push(name.to_string());
            match name {
                "edit_file" => {
                    let plan = applying.lock().unwrap().remove(token).expect("an edit that was asked for");
                    let done = edit::apply(&plan, &backups)?;
                    Ok(Ran { undo: Some(done.id.clone()), ..Ran::text(tools::applied_message(&done)) })
                }
                "run_check" => {
                    let o = check::run(Path::new(&root), &argv, check::timeout_for(&argv), &check_env, || false)?;
                    Ok(Ran { ok: o.passed(), check: Some(o.clone()), ..Ran::text(check::for_model(&argv, &o)) })
                }
                other => Err(format!("the script did not expect a call to {other}")),
            }
        };
        Wired { ask: Box::new(ask), run: Box::new(run) }
    };
    (wire, log)
}

fn names(log: &Log) -> Vec<String> {
    log.lock().unwrap().clone()
}

/// A check that passes and writes a line to `file` each time it runs, so that a test can count the runs.
fn counting(file: &Path) -> Vec<String> {
    vec!["cmd".into(), "/c".into(), format!("echo x>>{}", file.display())]
}

fn runs_of(file: &Path) -> usize {
    std::fs::read_to_string(file).map(|t| t.lines().count()).unwrap_or(0)
}

// ── what was asked ──────────────────────────────────────────────────────────

fn words(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_check_is_one_program_and_its_arguments_and_quotes_group_words() {
    assert_eq!(parse_check("cargo test -p hugin"), Ok(words(&["cargo", "test", "-p", "hugin"])));
    assert_eq!(parse_check("  cargo   test  "), Ok(words(&["cargo", "test"])));
    assert_eq!(parse_check(r#"cargo test -- "two words" 'and one'"#), Ok(words(&["cargo", "test", "--", "two words", "and one"])));
    assert_eq!(parse_check(r#"prog "" x"#), Ok(words(&["prog", "", "x"])), "an empty argument is an argument");
    assert_eq!(parse_check(r"C:\tools\run.exe --flag"), Ok(words(&[r"C:\tools\run.exe", "--flag"])));
    // Inside quotes, shell syntax is only text.
    assert_eq!(parse_check(r#"node -e "a | b && c > d""#), Ok(words(&["node", "-e", "a | b && c > d"])));
    // npm is a batch file on Windows.
    assert_eq!(parse_check("npm test"), Ok(words(&["npm.cmd", "test"])));
    assert_eq!(parse_check("NPM run build"), Ok(words(&["npm.cmd", "run", "build"])));
}

#[test]
fn a_check_that_is_shell_syntax_is_refused_where_it_would_mean_something() {
    for bad in ["a | b", "a && b", "a & b", "a ; b", "a > out", "a < in", "a `b`", "echo $HOME", "echo %PATH%", "a 2>&1"] {
        let e = parse_check(bad).unwrap_err();
        assert!(e.contains("not a shell command line"), "{bad}: {e}");
    }
    assert!(parse_check("").unwrap_err().contains("needs a command"));
    assert!(parse_check("   ").unwrap_err().contains("needs a command"));
    assert!(parse_check(r#"cargo "test"#).unwrap_err().contains("never closed"));
}

#[test]
fn the_time_is_whole_seconds_from_one_and_defaults_to_nine_minutes() {
    assert_eq!(parse_time(None), Ok(Duration::from_secs(540)));
    assert_eq!(parse_time(Some("30")), Ok(Duration::from_secs(30)));
    for bad in ["0", "-5", "1.5", "soon", ""] {
        assert!(parse_time(Some(bad)).is_err(), "{bad}");
    }
}

#[test]
fn the_time_is_shared_out_and_shrinks_with_a_short_limit() {
    let at = |secs| Budget::of(Duration::from_secs(secs));
    let long = at(540);
    assert_eq!((long.reserve, long.grace, long.tail), (Duration::from_secs(90), Duration::from_secs(5), Duration::from_secs(10)));
    let short = at(30);
    assert_eq!(short.reserve, Duration::from_millis(7500));
    assert!(short.reserve + short.tail < short.total, "{short:?}");
    let tiny = at(5);
    assert!(tiny.reserve + tiny.grace < tiny.total && tiny.grace < tiny.reserve, "{tiny:?}");
}

// ── a whole run ─────────────────────────────────────────────────────────────

/// **An edit, then an answer**: the patch, the files and the answer come back; the user's
/// folder and its `.git/index` are as they were; the run's folder is gone; and the patch,
/// kept in a file with a record of the run, applies.
#[test]
fn an_edit_and_an_answer_make_a_patch_and_the_users_folder_never_sees_it() {
    if !have_git() {
        return;
    }
    let w = World::new("whole");
    let repo = w.repo("proj");
    repo.write("a.txt", "one\r\ntwo\r\nthree\r\n");
    repo.write("b.txt", "keep\n");
    repo.commit();
    // Work that is not committed, which the copy starts from and the patch must not repeat.
    repo.write("a.txt", "one\r\ntwo\r\nthree\r\nfour\r\n");
    let before = scratch::fingerprints(&repo.root).unwrap();

    let (report, log) = w.run(&repo, None, 60, vec![Step::Edit("a.txt", "two", "2"), Step::Answer("Changed two to 2.")]);

    assert_eq!(report.answer, "Changed two to 2.");
    assert!(report.error.is_none() && !report.stopped && !report.failed && !report.partial(), "{report:?}");
    assert_eq!(report.exit_code(), 0);
    assert_eq!(names(&log), ["edit_file"]);
    assert_eq!(report.model, "scripted-model");
    let patch = String::from_utf8(report.patch.clone()).unwrap();
    assert!(patch.contains("--- a/a.txt\n+++ b/a.txt\n") && patch.contains("-two\n+2\n"), "{patch}");
    assert!(!patch.contains("+four"), "the uncommitted line was there before the run: {patch}");
    assert_eq!(
        report.files,
        [FileChange { path: "a.txt".into(), from: None, status: "modified", by: "edit", added: Some(1), deleted: Some(1) }]
    );
    assert_eq!(report.base, repo.git(&["rev-parse", "HEAD"]).trim());
    assert!(report.check.is_none(), "no check was named");

    // Kept as bytes, beside a record, and said how to apply.
    let file = report.patch_file.clone().expect("the patch is kept");
    assert_eq!(std::fs::read(&file).unwrap(), report.patch);
    assert!(file.starts_with(w.work().join("patches")));
    let record: serde_json::Value = serde_json::from_slice(&std::fs::read(file.with_extension("json")).unwrap()).unwrap();
    assert_eq!(record["answer"], "Changed two to 2.");
    assert_eq!(
        report.apply_command().unwrap(),
        format!("git -C \"{}\" apply --whitespace=nowarn \"{}\"", report.repo_root, file.display())
    );

    unchanged(&repo.root, &before);
    assert_eq!(w.runs_left(), 0, "the run's folder is removed");
    w.git.apply(&repo.root, &file, true).expect("the patch applies to the folder it came from");
    unchanged(&repo.root, &before);
    w.git.apply(&repo.root, &file, false).unwrap();
    let applied = std::fs::read_to_string(repo.root.join("a.txt")).unwrap().replace("\r\n", "\n");
    assert_eq!(applied, "one\n2\nthree\nfour\n");
}

#[test]
fn a_task_that_changes_nothing_has_an_empty_patch_and_no_file() {
    if !have_git() {
        return;
    }
    let w = World::new("nothing");
    let repo = w.repo("proj");
    repo.write("a.txt", "one\n");
    repo.commit();
    let (report, log) = w.run(&repo, None, 60, vec![Step::Answer("There is nothing to change.")]);
    assert!(report.patch.is_empty() && report.files.is_empty() && report.patch_file.is_none(), "{report:?}");
    assert_eq!((report.answer.as_str(), names(&log).len(), report.exit_code()), ("There is nothing to change.", 0, 0));
    assert!(report.apply_command().is_none());
    assert!(report.to_text().contains("No files were changed."));
    assert!(!w.work().join("patches").exists());
}

/// A folder inside the repository is the folder the model works in, and the patch applies
/// from the repository's top.
#[test]
fn a_subfolder_is_worked_in_and_the_patch_is_from_the_top() {
    if !have_git() {
        return;
    }
    let w = World::new("sub");
    let repo = w.repo("proj");
    repo.write("top.txt", "top\n");
    repo.write("crates/x/lib.rs", "pub fn x() {}\n");
    repo.commit();
    let folder = Repo { root: repo.root.join("crates").join("x"), env: repo.env.clone() };
    let (report, _) = w.run(&folder, None, 60, vec![Step::Edit("lib.rs", "x() {}", "x() { 1; }"), Step::Answer("Done.")]);
    assert!(report.error.is_none(), "{report:?}");
    assert_eq!(report.files[0].path, "crates/x/lib.rs");
    assert_eq!(report.files[0].by, "edit");
    assert!(report.folder.ends_with("x"), "{}", report.folder);
    assert!(Path::new(&report.repo_root).join("top.txt").exists());
    w.git.apply(&repo.root, report.patch_file.as_ref().unwrap(), true).unwrap();
}

// ── the check ───────────────────────────────────────────────────────────────

/// The model's own last check is the final one if nothing was written after it; a write after
/// it means one more run, on what the copy holds then.
#[test]
fn the_final_check_is_the_models_last_run_unless_something_was_edited_after_it() {
    if !cfg!(windows) || !have_git() {
        return;
    }
    let w = World::new("reuse");
    let repo = w.repo("proj");
    repo.write("a.txt", "one\ntwo\nthree\n");
    repo.commit();
    let counter = w.dir.join("count.txt");

    let (report, log) = w.run(
        &repo,
        Some(counting(&counter)),
        60,
        vec![Step::Edit("a.txt", "two", "2"), Step::Check, Step::Answer("Done.")],
    );
    assert_eq!(names(&log), ["edit_file", "run_check"]);
    assert_eq!(runs_of(&counter), 1, "the model's run was reused");
    let c = report.check.unwrap();
    assert!((c.reused, c.passed, c.exit_code, c.timed_out) == (true, Some(true), Some(0), false), "{c:?}");
    assert!(c.output_tail.is_empty(), "nothing of a passing check's output is kept");

    let _ = std::fs::remove_file(&counter);
    let (report, _) = w.run(
        &repo,
        Some(counting(&counter)),
        60,
        vec![Step::Edit("a.txt", "two", "2"), Step::Check, Step::Edit("a.txt", "three", "3"), Step::Answer("Done.")],
    );
    assert_eq!(runs_of(&counter), 2, "a change after the model's check means a check of the final state");
    assert!(!report.check.unwrap().reused);

    let _ = std::fs::remove_file(&counter);
    let (report, _) = w.run(&repo, Some(counting(&counter)), 60, vec![Step::Edit("a.txt", "two", "2"), Step::Answer("Done.")]);
    assert_eq!(runs_of(&counter), 1, "a model that never ran the check gets it run once");
    assert!(!report.check.unwrap().reused);
}

#[test]
fn a_failing_check_is_reported_with_its_code_and_the_end_of_its_output() {
    if !cfg!(windows) || !have_git() {
        return;
    }
    let w = World::new("failing");
    let repo = w.repo("proj");
    repo.write("a.txt", "one\n");
    repo.commit();
    let failing = vec!["cmd".to_string(), "/c".to_string(), "echo boom: it broke & exit 3".to_string()];
    let (report, _) = w.run(&repo, Some(failing), 60, vec![Step::Edit("a.txt", "one", "1"), Step::Answer("Done.")]);
    assert_eq!(report.exit_code(), 0, "a check that fails is a result, not a failed run");
    let c = report.check.clone().unwrap();
    assert!((c.passed, c.exit_code) == (Some(false), Some(3)), "{c:?}");
    assert!(c.output_tail.contains("boom: it broke"), "{c:?}");
    let text = report.to_text();
    assert!(text.contains("FAILED (exit 3,") && text.contains("check output (the end):\nboom: it broke"), "{text}");
    assert_eq!(report.to_json()["check"]["passed"], false);
}

/// A check runs the build files, and the warning about it is for a person: with a build file
/// changed it is not run, by the model or at the end.
#[test]
fn a_changed_build_file_means_the_check_is_not_run_at_the_end() {
    if !cfg!(windows) || !have_git() {
        return;
    }
    let w = World::new("buildfile");
    let repo = w.repo("proj");
    repo.write("Cargo.toml", "[package]\nname = \"x\"\nversion = \"0.1.0\"\n");
    repo.commit();
    let counter = w.dir.join("count.txt");
    let (report, _) = w.run(
        &repo,
        Some(counting(&counter)),
        60,
        vec![Step::Edit("Cargo.toml", "0.1.0", "0.2.0"), Step::Answer("Bumped the version.")],
    );
    assert_eq!(runs_of(&counter), 0);
    let c = report.check.clone().unwrap();
    assert!(c.passed.is_none() && c.not_run.as_deref().is_some_and(|w| w.contains("Cargo.toml")), "{c:?}");
    assert!(report.to_text().contains("was not run: the turn changed Cargo.toml"));
    assert_eq!(report.to_json()["check"]["not_run"], c.not_run.unwrap());
    assert_eq!(report.exit_code(), 0);
}

/// What the check writes is in the patch, and said not to be the model's.
#[test]
fn a_file_the_check_wrote_is_in_the_patch_and_flagged_as_not_an_edit() {
    if !cfg!(windows) || !have_git() {
        return;
    }
    let w = World::new("other");
    let repo = w.repo("proj");
    repo.write("a.txt", "one\n");
    repo.commit();
    let writes = vec!["cmd".to_string(), "/c".to_string(), "echo locked> Cargo.lock".to_string()];
    let (report, _) = w.run(&repo, Some(writes), 60, vec![Step::Edit("a.txt", "one", "1"), Step::Answer("Done.")]);
    let by: Vec<(&str, &str)> = report.files.iter().map(|f| (f.path.as_str(), f.by)).collect();
    assert_eq!(by, [("Cargo.lock", "other"), ("a.txt", "edit")]);
    assert!(report.to_text().contains("added Cargo.lock  +1 -0  [by something else: not one of the model's edits]"));
}

// ── nobody to ask ───────────────────────────────────────────────────────────

#[test]
fn a_card_the_rule_refuses_never_reaches_the_runner_and_the_run_goes_on() {
    if !have_git() {
        return;
    }
    let w = World::new("refused");
    let repo = w.repo("proj");
    repo.write("a.txt", "one\n");
    repo.commit();
    let (report, log) = w.run(
        &repo,
        None,
        60,
        vec![Step::Read(r"C:\Windows\win.ini"), Step::Edit("a.txt", "one", "1"), Step::Answer("Done.")],
    );
    assert_eq!(names(&log), ["edit_file"], "the read outside the folder was refused, and nothing ran for it");
    assert_eq!(report.files.len(), 1);
    assert!(report.error.is_none() && !report.partial());
}

// ── how a run ends ──────────────────────────────────────────────────────────

/// **On time.** A model that takes longer than the whole limit to reply is stopped at the
/// deadline, the run goes on with what the copy holds, and it is marked stopped.
#[test]
fn a_slow_model_is_stopped_on_time_and_the_run_says_so() {
    if !have_git() {
        return;
    }
    let w = World::new("slow");
    let repo = w.repo("proj");
    repo.write("a.txt", "one\ntwo\n");
    repo.commit();
    let started = Instant::now();
    let (report, _) = w.run(&repo, None, 5, vec![Step::Edit("a.txt", "one", "1"), Step::Sleep(30)]);
    let took = started.elapsed();
    assert!(took < Duration::from_secs(8), "took {took:?}");
    assert!(report.seconds <= 5.2, "the whole run had 5 seconds, and took {} s", report.seconds);
    assert!(report.stopped && report.partial() && report.exit_code() == 0, "{report:?}");
    assert!(report.answer.is_empty());
    // What was done before the stop is not lost.
    assert_eq!(report.files.len(), 1, "{report:?}");
    assert!(report.to_text().starts_with("hugin: PARTIAL proposal. Stopped at the 5 s time limit"), "{}", report.to_text());
    assert_eq!(report.to_json()["stopped"], true);
    assert_eq!(w.runs_left(), 0);
}

#[test]
fn a_request_that_fails_before_any_reply_is_a_run_that_could_not_run() {
    if !have_git() {
        return;
    }
    let w = World::new("nomodel");
    let repo = w.repo("proj");
    repo.write("a.txt", "one\n");
    repo.commit();
    let (report, _) = w.run(&repo, None, 60, vec![Step::Fail("could not start Ollama: not installed")]);
    assert!(report.failed && report.exit_code() == 1, "{report:?}");
    assert_eq!(report.error.as_deref(), Some("could not start Ollama: not installed"));
    let json = report.to_json();
    assert_eq!((json["error"].as_str(), json["partial"].as_bool()), (Some("could not start Ollama: not installed"), Some(true)));
    assert!(report.to_text().starts_with("hugin -p could not make a proposal: could not start Ollama"));
    assert_eq!(w.runs_left(), 0);
}

#[test]
fn a_request_that_fails_after_an_edit_is_a_partial_proposal_with_its_patch() {
    if !have_git() {
        return;
    }
    let w = World::new("midway");
    let repo = w.repo("proj");
    repo.write("a.txt", "one\n");
    repo.commit();
    let (report, _) = w.run(&repo, None, 60, vec![Step::Edit("a.txt", "one", "1"), Step::Fail("POST /api/chat: connection refused")]);
    assert!(!report.failed && report.partial() && report.exit_code() == 0, "{report:?}");
    assert_eq!(report.error.as_deref(), Some("POST /api/chat: connection refused"));
    assert_eq!(report.files.len(), 1);
    assert!(report.to_text().starts_with("hugin: PARTIAL proposal. POST /api/chat: connection refused"));
}

#[test]
fn a_folder_that_is_not_in_git_is_said_in_words_and_nothing_is_made() {
    if !have_git() {
        return;
    }
    let w = World::new("notgit");
    let plain = w.dir.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let (wire, log) = script(vec![]);
    let report = execute(&w.job(&plain, None, 60), &w.git, "m", wire).report;
    assert!(report.failed && report.exit_code() == 1);
    assert!(report.error.as_deref().is_some_and(|e| e.contains("not in a git repository")), "{report:?}");
    assert!(names(&log).is_empty());
    assert_eq!(w.runs_left(), 0);
    let json = report.to_json();
    assert_eq!((json["diff"].as_str(), json["files"].as_array().map(Vec::len)), (Some(""), Some(0)));
}

#[test]
fn one_run_at_a_time() {
    if !cfg!(windows) || !have_git() {
        return;
    }
    let w = World::new("busy");
    let repo = w.repo("proj");
    repo.write("a.txt", "one\n");
    repo.commit();
    let held = scratch::Lock::take(&w.work()).unwrap();
    let (report, _) = w.run(&repo, None, 60, vec![Step::Answer("Done.")]);
    assert!(report.failed && report.error.as_deref().is_some_and(|e| e.contains("already running")), "{report:?}");
    drop(held);
    let (report, _) = w.run(&repo, None, 60, vec![Step::Answer("Done.")]);
    assert!(!report.failed, "{report:?}");
}

// ── the check's program ─────────────────────────────────────────────────────

fn argv(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// **A program named with a folder is the copy's**, and a bare name is left to `PATH`. The system
/// resolves `.\verify.cmd` from the directory `hugin` was started in, which is the user's.
#[test]
fn a_program_named_with_a_folder_is_resolved_in_the_copy_and_a_bare_name_is_left_to_path() {
    let (folder, copy, repo) = (Path::new(r"C:\t\run\copy\sub"), Path::new(r"C:\t\run\copy"), Path::new(r"C:\Users\me\repo"));
    let resolve = |list: &[&str]| resolve_check(&argv(list), folder, copy, repo);
    assert_eq!(resolve(&["cargo", "test"]).unwrap(), argv(&["cargo", "test"]));
    // Relative: joined to the folder in the copy, and the arguments untouched.
    assert_eq!(resolve(&[r".\verify.cmd", "--all"]).unwrap(), argv(&[r"C:\t\run\copy\sub\verify.cmd", "--all"]));
    assert_eq!(resolve(&["scripts/run.cmd"]).unwrap(), argv(&[r"C:\t\run\copy\sub\scripts\run.cmd"]));
    assert_eq!(resolve(&[r"..\tools\check.cmd"]).unwrap(), argv(&[r"C:\t\run\copy\tools\check.cmd"]));
    // ...but it must stay inside the copy.
    let e = resolve(&[r"..\..\x.cmd"]).unwrap_err();
    assert!(e.contains("leads out of the folder"), "{e}");
    // Absolute and elsewhere: as it is. In the user's repository, in any spelling: refused.
    assert_eq!(resolve(&[r"C:\Windows\System32\whoami.exe"]).unwrap(), argv(&[r"C:\Windows\System32\whoami.exe"]));
    for inside in [r"C:\Users\me\repo\check.cmd", r"c:\users\ME\repo\sub\..\check.cmd", "C:/Users/me/repo/x/check.cmd"] {
        let e = resolve(&[inside]).unwrap_err();
        assert!(e.contains("a file in your repository"), "{inside}: {e}");
    }
    assert!(resolve(&[r"C:\Users\me\repo-tools\check.cmd"]).is_ok(), "a different folder that starts with the same letters");
    // A drive or a root alone could mean anything.
    assert!(resolve(&[r"\tools\check.cmd"]).is_err());
    assert!(resolve(&["C:check.cmd"]).is_err());
}

/// **`.\verify.cmd` runs the copy's own file.** The user's folder is the one place a script
/// that writes beside itself must not write.
#[test]
fn a_program_named_with_a_folder_runs_the_copys_version_and_writes_nothing_to_the_users() {
    if !cfg!(windows) || !have_git() {
        return;
    }
    let w = World::new("relative");
    let repo = w.repo("proj");
    repo.write("verify.cmd", "@echo off\r\necho ran in %~dp0 > \"%~dp0ran.txt\"\r\nexit /b 0\r\n");
    repo.write("a.txt", "one\n");
    repo.commit();
    let before = scratch::fingerprints(&repo.root).unwrap();

    let (report, _) = w.run(&repo, Some(argv(&[r".\verify.cmd"])), 60, vec![Step::Edit("a.txt", "one", "1"), Step::Answer("Done.")]);
    let c = report.check.clone().unwrap();
    assert_eq!((c.command.as_str(), c.passed, c.exit_code), (r".\verify.cmd", Some(true), Some(0)), "{c:?}");
    // Its note is in the patch as the copy's file, and the user's folder has none.
    let by: Vec<(&str, &str)> = report.files.iter().map(|f| (f.path.as_str(), f.by)).collect();
    assert_eq!(by, [("a.txt", "edit"), ("ran.txt", "other")]);
    let patch = String::from_utf8_lossy(&report.patch).into_owned();
    assert!(
        patch.contains("ran in ") && patch.contains("\\work\\run-") && !patch.contains("\\proj\\"),
        "it ran in the copy, not in proj: {patch}"
    );
    assert!(!repo.root.join("ran.txt").exists());
    unchanged(&repo.root, &before);
}

#[test]
fn a_program_in_the_users_repository_is_refused_before_anything_is_run() {
    if !cfg!(windows) || !have_git() {
        return;
    }
    let w = World::new("absolute");
    let repo = w.repo("proj");
    repo.write("verify.cmd", "@echo off\r\necho ran > \"%~dp0ran.txt\"\r\n");
    repo.write("a.txt", "one\n");
    repo.commit();
    let before = scratch::fingerprints(&repo.root).unwrap();
    let theirs = repo.root.join("verify.cmd").to_string_lossy().into_owned();
    let (report, log) = w.run(&repo, Some(vec![theirs]), 60, vec![Step::Answer("Done.")]);
    assert!(report.failed && report.exit_code() == 1, "{report:?}");
    assert!(report.error.as_deref().is_some_and(|e| e.contains("a file in your repository")), "{report:?}");
    assert!(names(&log).is_empty());
    assert_eq!(w.runs_left(), 0);
    unchanged(&repo.root, &before);
}

/// The wider list, end to end: after a `conftest.py` was edited neither the model's check nor
/// the final one runs, and the report says why.
#[test]
fn a_changed_runner_file_stops_the_models_check_and_the_final_one() {
    if !cfg!(windows) || !have_git() {
        return;
    }
    let w = World::new("runner");
    let repo = w.repo("proj");
    repo.write("tests/conftest.py", "# conftest\n");
    repo.write("a.txt", "one\n");
    repo.commit();
    let counter = w.dir.join("count.txt");
    let (report, log) = w.run(
        &repo,
        Some(counting(&counter)),
        60,
        vec![Step::Edit("tests/conftest.py", "# conftest", "# changed"), Step::Check, Step::Answer("Done.")],
    );
    assert_eq!(names(&log), ["edit_file"], "the model's own check was turned away");
    assert_eq!(runs_of(&counter), 0, "and nothing ran the check at the end");
    let c = report.check.clone().unwrap();
    assert!(c.passed.is_none() && c.not_run.as_deref().is_some_and(|w| w.contains("tests/conftest.py")), "{c:?}");
}

// ── what it says ────────────────────────────────────────────────────────────

fn complete() -> Report {
    Report {
        task: "do it".into(),
        folder: r"C:\work\demo".into(),
        repo_root: r"C:\work\demo".into(),
        base: "abc123".into(),
        model: "qwen3-coder:30b".into(),
        answer: "Done.".into(),
        patch: b"diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n@@ -1 +1 @@\n-a\n+b\n".to_vec(),
        patch_file: Some(PathBuf::from(r"C:\t\hugin-p\patches\p.patch")),
        files: vec![FileChange { path: "x.rs".into(), from: None, status: "modified", by: "edit", added: Some(1), deleted: Some(1) }],
        check: Some(CheckReport {
            command: "cargo test -p x".into(),
            passed: Some(true),
            exit_code: Some(0),
            seconds: 12.5,
            timed_out: false,
            blocked: None,
            reused: true,
            cut_short: false,
            output_tail: String::new(),
            not_run: None,
        }),
        skipped: vec!["big.bin (over 10 MB)".into()],
        stopped: false,
        notes: Vec::new(),
        seconds: 41.2,
        time: 540,
        error: None,
        failed: false,
    }
}

/// **The JSON, field for field and in order**: Claude Code reads these names, and reads the
/// start of the output first. What matters is at the front — whether it is whole, the check, the
/// files, how to apply — then the answer, and the patch text, which can be long, last.
#[test]
fn the_json_has_exactly_these_fields_in_this_order() {
    let want = r##"{"error":null,"partial":false,"stopped":false,"check":{"command":"cargo test -p x","passed":true,"exit_code":0,"timed_out":false,"blocked":false,"reused":true,"cut_short":false,"not_run":null,"seconds":12.5,"output_tail":""},"files":[{"path":"x.rs","status":"modified","by":"edit","added":1,"deleted":1}],"apply":"git -C \"C:\\work\\demo\" apply --whitespace=nowarn \"C:\\t\\hugin-p\\patches\\p.patch\"","patch_file":"C:\\t\\hugin-p\\patches\\p.patch","skipped":["big.bin (over 10 MB)"],"notes":[],"answer":"Done.","seconds":41.2,"model":"qwen3-coder:30b","folder":"C:\\work\\demo","repo_root":"C:\\work\\demo","base":"abc123","task":"do it","diff_lossy":false,"diff_omitted":false,"diff":"diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n@@ -1 +1 @@\n-a\n+b\n"}"##;
    assert_eq!(complete().to_json_string(), want);

    // With no check, no patch and an error: the same fields, null or empty where there is nothing.
    let bare = Report { check: None, patch: Vec::new(), patch_file: None, files: Vec::new(), error: Some("x".into()), failed: true, ..complete() };
    let text = bare.to_json_string();
    let at = |key: &str| text.find(&format!("\"{key}\":")).unwrap_or_else(|| panic!("{key} in {text}"));
    let keys = ["error", "partial", "stopped", "check", "files", "apply", "patch_file", "skipped", "notes", "answer", "seconds", "model", "folder", "repo_root", "base", "task", "diff_lossy", "diff_omitted", "diff"];
    for pair in keys.windows(2) {
        assert!(at(pair[0]) < at(pair[1]), "{} comes before {}", pair[0], pair[1]);
    }
    assert!(text.ends_with("\"diff\":\"\"}"), "the patch text is the last field: {text}");
    let json = bare.to_json();
    assert!(json["check"].is_null() && json["patch_file"].is_null() && json["apply"].is_null());
    assert_eq!((json["error"].as_str(), json["partial"].as_bool()), (Some("x"), Some(true)));
}

#[test]
fn what_the_check_did_with_its_time_is_in_the_json() {
    let cut = CheckReport { passed: Some(false), exit_code: None, timed_out: true, cut_short: true, seconds: 4.2, ..complete().check.unwrap() };
    let json = Report { check: Some(cut), ..complete() }.to_json();
    assert_eq!(json["check"]["cut_short"], true);
    assert_eq!(json["check"]["timed_out"], true);
    assert_eq!(complete().to_json()["check"]["cut_short"], false);
}

#[test]
fn a_rename_names_where_it_came_from() {
    let mut r = complete();
    r.files = vec![FileChange { path: "new.rs".into(), from: Some("old.rs".into()), status: "renamed", by: "edit", added: Some(0), deleted: Some(0) }];
    assert_eq!(
        r.to_json()["files"][0],
        json!({ "path": "new.rs", "status": "renamed", "by": "edit", "added": 0, "deleted": 0, "from": "old.rs" })
    );
    assert!(r.to_text().contains("renamed new.rs (from old.rs)  +0 -0  [by an edit]"));
}

/// **The check and how to apply come before the patch**, so that a long patch never buries them.
#[test]
fn the_text_leads_with_whether_it_is_partial_then_answer_check_skipped_apply_and_last_the_patch() {
    let text = complete().to_text();
    assert_eq!(
        text,
        "hugin: done in 41 s on qwen3-coder:30b (1 file changed).\n\
         \n\
         Done.\n\
         \n\
         check: `cargo test -p x` passed (exit 0, 12.5 s, the model's own last run, with nothing edited after it)\n\
         \n\
         not carried into the copy:\n  big.bin (over 10 MB)\n\
         \n\
         to apply: git -C \"C:\\work\\demo\" apply --whitespace=nowarn \"C:\\t\\hugin-p\\patches\\p.patch\"\n\
         \n\
         --- patch: 1 file changed, +1 -1 ---\n\
         modified x.rs  +1 -1  [by an edit]\n\
         \n\
         diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n@@ -1 +1 @@\n-a\n+b\n"
    );

    let stopped = Report { stopped: true, answer: String::new(), ..complete() }.to_text();
    assert!(stopped.starts_with("hugin: PARTIAL proposal. Stopped at the 540 s time limit"), "{stopped}");
    let bound = Report { notes: vec!["Stopped after 20 tool rounds.".into()], ..complete() }.to_text();
    assert!(bound.starts_with("hugin: PARTIAL proposal. Stopped after 20 tool rounds."), "{bound}");
    let none = Report { check: None, ..complete() }.to_text();
    assert!(none.contains("check: none was run (no --check, and no check is set for this folder in Mimir)"), "{none}");
    let blocked = Report {
        check: Some(CheckReport { passed: Some(false), blocked: Some("a program the build needed".into()), ..complete().check.unwrap() }),
        ..complete()
    }
    .to_text();
    assert!(blocked.contains("Smart App Control blocked a program the build needed"), "{blocked}");
    let timed_out = Report {
        check: Some(CheckReport { passed: Some(false), exit_code: None, timed_out: true, seconds: 85.0, ..complete().check.unwrap() }),
        ..complete()
    }
    .to_text();
    assert!(timed_out.contains("check: `cargo test -p x` timed out after 85 s"), "{timed_out}");
    let cut_short = Report {
        check: Some(CheckReport {
            passed: Some(false),
            exit_code: None,
            timed_out: true,
            cut_short: true,
            seconds: 4.2,
            output_tail: String::new(),
            ..complete().check.unwrap()
        }),
        ..complete()
    }
    .to_text();
    assert!(cut_short.contains("check: `cargo test -p x` did not finish in the 4.2 s the run had left"), "{cut_short}");
}

/// **A long patch is not printed.** Past the cap the output has the files with their counts and
/// the place to read the whole patch — and the check and the apply command are still at the front.
#[test]
fn a_patch_past_the_cap_is_not_printed_but_its_files_the_check_and_where_to_read_it_are() {
    let mut long = complete();
    long.patch = format!("diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n@@ -1 +1,3000 @@\n-a\n{}", "+a line of the new text\n".repeat(3000)).into_bytes();
    long.files = vec![
        FileChange { path: "x.rs".into(), from: None, status: "modified", by: "edit", added: Some(3000), deleted: Some(1) },
        FileChange { path: "logo.png".into(), from: None, status: "added", by: "other", added: None, deleted: None },
    ];
    assert!(long.patch.len() > 60 * 1024);
    let text = long.to_text();
    assert!(!text.contains("a line of the new text") && !text.contains("diff --git"), "no patch text");
    assert!(text.len() < 2000, "{} bytes of output: {text}", text.len());
    assert!(text.contains("--- patch: 2 files changed, +3000 -1, and a file that is not text ---"), "{text}");
    assert!(text.contains("modified x.rs  +3000 -1  [by an edit]") && text.contains("added logo.png  not text  [by something else"), "{text}");
    assert!(text.contains("The patch is 71 KB, too long to print here (16 KB at most): read it in C:\\t\\hugin-p\\patches\\p.patch, which has all of it."), "{text}");
    let (check, apply, patch) = (text.find("check: `cargo test").unwrap(), text.find("to apply: git -C").unwrap(), text.find("--- patch:").unwrap());
    assert!(check < apply && apply < patch, "the check and the apply command come first: {text}");

    let json = long.to_json();
    assert_eq!((json["diff"].as_str(), json["diff_omitted"].as_bool(), json["diff_lossy"].as_bool()), (Some(""), Some(true), Some(false)));
    assert_eq!(json["patch_file"], r"C:\t\hugin-p\patches\p.patch");
    assert!(long.to_json_string().len() < 2500);

    // Just under the cap is printed whole; a byte that is not UTF-8 is said to be a stand-in.
    let mut fits = complete();
    fits.patch = vec![b'+'; 16 * 1024];
    assert_eq!(fits.to_json()["diff_omitted"], false);
    fits.patch.push(b'+');
    assert_eq!(fits.to_json()["diff_omitted"], true);
    let mut odd = complete();
    odd.patch = b"diff --git a/x b/x\n+caf\xe9\n".to_vec();
    assert_eq!(odd.to_json()["diff_lossy"], true);
    assert_eq!(complete().to_json()["diff_lossy"], false);
}

/// **The report is out before the copy is cleaned up**: the copy and the lock are held by the
/// value that holds the report, and go when it does.
#[test]
fn the_report_is_there_while_the_copy_still_is_and_the_copy_goes_when_it_is_let_go() {
    if !have_git() {
        return;
    }
    let w = World::new("finished");
    let repo = w.repo("proj");
    repo.write("a.txt", "one\n");
    repo.commit();
    let (wire, _) = script(vec![Step::Edit("a.txt", "one", "1"), Step::Answer("Done.")]);
    let finished = execute(&w.job(&repo.root, None, 60), &w.git, "m", wire);
    assert_eq!(finished.report.files.len(), 1);
    assert_eq!(w.runs_left(), 1, "the copy is still there, to be removed after the report is printed");
    assert!(scratch::Lock::take(&w.work()).is_err() || !cfg!(windows), "and so is the lock");
    drop(finished);
    assert_eq!(w.runs_left(), 0);
    assert!(scratch::Lock::take(&w.work()).is_ok());
}

#[test]
fn a_long_task_is_cut_in_the_echo() {
    let r = Report { task: "x".repeat(5000), ..complete() };
    let shown = r.to_json()["task"].as_str().unwrap().to_string();
    assert_eq!(shown.chars().count(), 1001);
    assert!(shown.ends_with('…'));
}

#[test]
fn the_note_for_claude_says_when_to_use_hugin_how_and_that_the_check_runs_what_hugin_wrote() {
    for must in ["hugin -p", "--check", "--json", "600000", "Good for", "Not for", "apply --whitespace=nowarn", "runs code Hugin wrote", "a repository you trust"] {
        assert!(CLAUDE_NOTE.contains(must), "{must}");
    }
    assert!(CLAUDE_NOTE.starts_with("## Hugin"));
}
