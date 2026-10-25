//! `hugin --ask`, run whole over a scripted model: what it is offered, what it looked at, what it
//! is refused, and what it leaves alone. The model and the runner are a script; everything else is
//! the real thing — the folder, the tools that read, the rule that says what asks, the report —
//! against a folder made for each test.
//!
//! **The scripted model reads for real.** A step that reads, lists or searches is run by the same
//! functions the app's round runs an automatic call with, and comes back as the same reply, so
//! what the scout "looked at" here is what a read really returned.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bifrost_core::agent::round::{Reply, Round};
use bifrost_core::agent::run::run_auto;
use bifrost_core::agent::send::{Calibration, compact_for_send};
use bifrost_core::agent::stop::next_stop;
use bifrost_core::agent::ticket::PendingCall;
use bifrost_core::agent::turn::Decision;
use bifrost_core::chat::{self, Message};
use bifrost_core::edit::Workspace;
use bifrost_core::scratch::{self, Lock};
use bifrost_core::tools::{self, Approval};
use serde_json::json;

use crate::ask::*;
use crate::print::Wired;

// ── a place to work ─────────────────────────────────────────────────────────

/// A folder for one test, with a work folder beside it for the lock. Removed on drop.
struct Dir {
    root: PathBuf,
}

impl Dir {
    fn new(tag: &str) -> Self {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
        let root = std::env::temp_dir().join(format!("hugin-ask-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(root.join("proj")).unwrap();
        Dir { root }
    }

    /// The folder that is asked about.
    fn folder(&self) -> PathBuf {
        self.root.join("proj")
    }

    fn work(&self) -> PathBuf {
        self.root.join("work")
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.folder().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn job(&self, secs: u64) -> Job {
        Job {
            question: "where is the thing?".to_string(),
            folder: self.folder(),
            time: Duration::from_secs(secs),
            started: Instant::now(),
            work: self.work(),
        }
    }

    /// Make the folder a git repository, with its files committed and a global configuration of
    /// its own so that nothing depends on the machine's.
    fn git_init(&self) {
        let config = self.root.join("global.gitconfig");
        std::fs::write(&config, "[user]\n\tname = t\n\temail = t@t\n").unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["add", "-A"], &["commit", "-q", "-m", "x"]] {
            let out = Command::new("git")
                .args(args)
                .current_dir(self.folder())
                .env("GIT_CONFIG_GLOBAL", &config)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .expect("git runs");
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        }
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        for _ in 0..20 {
            if std::fs::remove_dir_all(&self.root).is_ok() || !self.root.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn have_git() -> bool {
    Command::new("git").arg("--version").output().is_ok()
}

/// A source file of `n` lines, each `line N`, so a range can be told from the whole.
fn lines(n: usize) -> String {
    (1..=n).map(|i| format!("line {i}\n")).collect()
}

/// A folder with something in it to look at.
fn project(dir: &Dir) {
    dir.write("src/lib.rs", &lines(30));
    dir.write("src/other.rs", "fn other() {}\nfn more() {}\nfn third() {}\nfn fourth() {}\n");
    dir.write("README.md", "# demo\n\nA tiny project.\n");
}

// ── the model, scripted ─────────────────────────────────────────────────────

/// What the scripted model does on each request, in order.
enum Step {
    /// `read_file`: the file, and the lines asked for (start, end).
    Read(&'static str, Option<(usize, Option<usize>)>),
    List(Option<&'static str>),
    Search(&'static str),
    /// A call that arrives to be asked about, as one the scout was not given would: the tool and the path.
    Asks(&'static str, &'static str),
    Answer(&'static str),
    /// Take this long to reply, as a request that cannot be interrupted does.
    Sleep(u64),
    /// The request fails.
    Fail(&'static str),
}

/// The names of the calls that reached the runner: there should be none.
type Reached = Arc<Mutex<Vec<String>>>;

/// What each request was sent last: the role and the words.
type Heard = Arc<Mutex<Vec<(String, String)>>>;

/// The setting the run was set up with, as the script saw it.
type Seen = Arc<Mutex<Option<Ctx>>>;

fn round(reply: Reply) -> Result<Round, String> {
    Ok(Round { reply, prompt_eval_count: 0, recovered: false, sent: compact_for_send(&mut [], &[], 8192, None, 0) })
}

fn raw(name: &str, arguments: serde_json::Value) -> chat::ToolCall {
    chat::ToolCall { id: None, function: chat::FunctionCall { name: name.into(), arguments } }
}

/// A call the folder's rule runs without asking, run: what `round` does with a read, a listing or a
/// search, with the same functions. A call the rule turns away comes back rejected, and one that
/// asks is not for this.
fn auto(ws: &Workspace, name: &str, arguments: serde_json::Value) -> Result<Round, String> {
    let call = raw(name, arguments);
    let request = tools::interpret(&call).expect("a call the script means");
    match tools::approval(Some(ws), &request, false) {
        Approval::Auto(auto) => {
            let (name, path, range, ran) = run_auto(ws, auto, next_stop()).expect("a read that could be made");
            round(Reply::Auto {
                name: name.to_string(),
                path,
                range,
                result: ran.result,
                ok: ran.ok,
                stopped: ran.stopped,
                plan: None,
                raw_call: call,
            })
        }
        Approval::Reject(why) => round(Reply::Rejected {
            name: name.to_string(),
            path: request.path().to_string(),
            error: format!("error: {why}"),
            raw_call: call,
        }),
        Approval::Ask => panic!("{name} asks, and the script means a call that does not"),
    }
}

/// A call that asks, whatever it is.
fn asks(name: &str, path: &str) -> Result<Round, String> {
    round(Reply::Call {
        call: PendingCall {
            token: format!("t-{name}"),
            name: name.into(),
            path: path.into(),
            diff: None,
            range: None,
            changed_build_files: Vec::new(),
            to: None,
            folders: Vec::new(),
            size: None,
        },
        raw_call: raw(name, json!({ "path": path })),
    })
}

/// A model that does `steps` and a runner that must never be used. Also: what the model was sent
/// last on each request, and the env the run was set up with.
fn script(steps: Vec<Step>) -> (impl FnOnce(&Ctx) -> Wired, Reached, Heard, Seen) {
    let (ran, heard, seen) = (Reached::default(), Heard::default(), Seen::default());
    let (ran_by, heard_by, seen_by) = (ran.clone(), heard.clone(), seen.clone());
    let wire = move |ctx: &Ctx| {
        *seen_by.lock().unwrap() = Some(Ctx { root: ctx.root.clone(), env: ctx.env.clone() });
        let ws = Workspace::open(&ctx.root).unwrap();
        let mut steps = VecDeque::from(steps);
        let ask = move |messages: &[Message], _: Option<Calibration>, _: bool| {
            heard_by.lock().unwrap().push(messages.last().map(|m| (m.role.clone(), m.content.clone())).unwrap_or_default());
            match steps.pop_front().expect("the loop asked the model more often than the script allows") {
                Step::Answer(text) => round(Reply::Answer { content: text.into(), eval_count: 7 }),
                Step::Fail(why) => Err(why.to_string()),
                Step::Sleep(secs) => {
                    std::thread::sleep(Duration::from_secs(secs));
                    round(Reply::Answer { content: "too late".into(), eval_count: 7 })
                }
                Step::Read(path, range) => {
                    let args = json!({ "path": path, "start_line": range.map(|r| r.0), "end_line": range.and_then(|r| r.1) });
                    auto(&ws, "read_file", args)
                }
                Step::List(pattern) => auto(&ws, "list_files", json!({ "pattern": pattern })),
                Step::Search(text) => auto(&ws, "search", json!({ "text": text })),
                Step::Asks(name, path) => asks(name, path),
            }
        };
        let run = move |name: &str, _: &str, _: &str| {
            ran_by.lock().unwrap().push(name.to_string());
            Err(format!("the script did not expect a call to {name}"))
        };
        Wired { ask: Box::new(ask), run: Box::new(run) }
    };
    (wire, ran, heard, seen)
}

/// Run a script over `dir`.
fn ask(dir: &Dir, secs: u64, steps: Vec<Step>) -> (Report, Reached, Heard) {
    let (wire, ran, heard, _) = script(steps);
    (execute(&dir.job(secs), "scripted-model", wire), ran, heard)
}

fn looked(report: &Report) -> Vec<(&str, Option<&str>)> {
    report.looked_at.iter().map(|l| (l.path.as_str(), l.lines.as_deref())).collect()
}

// ── what it was asked ───────────────────────────────────────────────────────

#[test]
fn the_time_is_five_minutes_unless_it_is_given() {
    assert_eq!(parse_time(None), Ok(Duration::from_secs(300)));
    assert_eq!(parse_time(Some("45")), Ok(Duration::from_secs(45)));
    for bad in ["0", "-5", "1.5", "soon", ""] {
        assert!(parse_time(Some(bad)).is_err(), "{bad}");
    }
}

// ── what the model is given ─────────────────────────────────────────────────

/// **Read-only by construction**: the three tools that look, and none that writes or runs.
#[test]
fn the_scout_is_offered_the_tools_that_read_and_none_that_write() {
    let names: Vec<String> = offered().into_iter().map(|t| t.function.name).collect();
    assert_eq!(names, ["read_file", "list_files", "search"]);
    // Every other tool the app has exists, and is not among them.
    let all: Vec<String> = tools::available(true, Some("cargo check")).into_iter().map(|t| t.function.name).collect();
    for writer in ["edit_file", "create_file", "delete_file", "move_file", "run_check", "update_plan"] {
        assert!(all.iter().any(|n| n == writer), "{writer} is a tool the app has");
        assert!(!names.iter().any(|n| n == writer), "{writer} is offered");
    }
    // The words are the app's own.
    let app = tools::available(true, None);
    for tool in offered() {
        let same = app.iter().find(|t| t.function.name == tool.function.name).unwrap();
        assert_eq!(tool.function.description, same.function.description);
    }
}

#[test]
fn the_run_is_set_up_with_those_tools_a_prompt_of_its_own_and_no_commands() {
    let dir = Dir::new("setup");
    project(&dir);
    dir.write("AGENTS.md", "This project is about tiny things.\n");
    let (wire, _, _, seen) = script(vec![Step::Answer("Nowhere.")]);
    let report = execute(&dir.job(30), "scripted-model", wire);
    assert_eq!(report.status(), Status::Answered);
    let ctx = seen.lock().unwrap().take().expect("the run was set up");
    let env = ctx.env;
    let names: Vec<String> = env.offered.unwrap().into_iter().map(|t| t.function.name).collect();
    assert_eq!(names, ["read_file", "list_files", "search"]);
    assert!(env.commands.is_none() && env.identity.is_none(), "no shell, and no sentence for the app's own prompt");
    assert!(env.backups.is_none() && env.check_env.is_empty());
    let prompt = env.system.expect("a prompt of its own, used as it is");
    assert!(prompt.contains(&ctx.root), "{prompt}");
    for must in [
        "Hugin",
        "You can only look: read_file, list_files and search. You cannot change or run anything.",
        "Search for more than one word — the words of the question, then the names you find — and look at what each search found.",
        "Read the code with read_file. A search hit is one line of a file: read the code you will cite — never answer from search hits alone, and never guess what a file says.",
        "Answer the question from the code you read. Name the files and line ranges you relied on, like src/main.rs:120-180.",
        "say \"not sure\" and what you could not find, rather than guess. Keep the answer short.",
        "The project's own notes, from AGENTS.md",
        "This project is about tiny things.",
    ] {
        assert!(prompt.contains(must), "{must}\n{prompt}");
    }
    // The app's prompt tells the model it can change files; this one must not.
    assert!(!prompt.contains("edit_file") && !prompt.contains("change files") && !prompt.contains("read and change"), "{prompt}");
}

/// Every card that asks is answered no.
#[test]
fn whatever_asks_is_denied() {
    let call = |name: &str, path: &str| PendingCall {
        token: "t".into(),
        name: name.into(),
        path: path.into(),
        diff: None,
        range: None,
        changed_build_files: Vec::new(),
        to: None,
        folders: Vec::new(),
        size: None,
    };
    for name in [
        "edit_file", "create_file", "delete_file", "move_file", "write_file", "run_check", "run_command", "read_file", "list_files",
        "search", "update_plan", "no_such_tool", "",
    ] {
        for path in ["src/lib.rs", "../outside.txt", r"C:\Windows\win.ini", ".git/config", ""] {
            assert_eq!(decide(&call(name, path)), Decision::Deny, "{name} {path}");
        }
    }
}

// ── what it looked at ───────────────────────────────────────────────────────

/// **The files looked at are the reads that ran**, with the lines each really gave — not the files
/// the answer names. A read that failed, was turned away or never happened is not one; a search and
/// a listing are not a file looked at.
#[test]
fn what_it_looked_at_comes_from_the_reads_that_ran_and_not_from_the_answer() {
    let dir = Dir::new("looked");
    project(&dir);
    let steps = vec![
        Step::List(None),
        Step::Search("fn other"),
        Step::Read("src/lib.rs", None),
        Step::Read("src/other.rs", Some((2, Some(3)))),
        // Past the end of the file: the lines it really gave are 1–3, and the header says so.
        Step::Read("README.md", Some((1, Some(999)))),
        // A file that is not there, and a line past the end of one that is: neither was looked at.
        Step::Read("src/missing.rs", None),
        Step::Read("src/other.rs", Some((900, None))),
        Step::Answer("It is in src/never_read.rs, lines 1-5, and in src/lib.rs."),
    ];
    let (report, ran, _) = ask(&dir, 60, steps);
    assert_eq!(
        looked(&report),
        [("src/lib.rs", None), ("src/other.rs", Some("2-3")), ("README.md", Some("1-3"))],
        "{report:?}"
    );
    assert_eq!(report.answer, "It is in src/never_read.rs, lines 1-5, and in src/lib.rs.");
    assert_eq!((report.status(), report.exit_code()), (Status::Answered, 0));
    assert!(ran.lock().unwrap().is_empty());
}

#[test]
fn a_file_read_again_is_named_once_and_a_whole_read_stands_for_the_parts() {
    let dir = Dir::new("again");
    project(&dir);
    let steps = vec![
        Step::Read("src/lib.rs", Some((1, Some(5)))),
        Step::Read("src/lib.rs", Some((10, Some(12)))),
        Step::Read("src/lib.rs", Some((1, Some(5)))),
        Step::Read("src/other.rs", Some((1, Some(2)))),
        Step::Read("src/lib.rs", None),
        Step::Read("src/other.rs", Some((3, Some(4)))),
        Step::Answer("Done."),
    ];
    let (report, _, _) = ask(&dir, 60, steps);
    // Files in the order they were first read; the parts of one together.
    assert_eq!(looked(&report), [("src/lib.rs", None), ("src/other.rs", Some("1-2")), ("src/other.rs", Some("3-4"))]);
    let parts = vec![
        Step::Read("src/other.rs", Some((3, Some(4)))),
        Step::Read("src/lib.rs", Some((1, Some(5)))),
        Step::Read("src/other.rs", Some((1, Some(2)))),
        Step::Answer("Done."),
    ];
    let (report, _, _) = ask(&dir, 60, parts);
    assert_eq!(looked(&report), [("src/other.rs", Some("3-4")), ("src/other.rs", Some("1-2")), ("src/lib.rs", Some("1-5"))]);
}

/// A read that the size limit cut says what it gave, not what was asked for.
#[test]
fn a_read_cut_at_the_size_limit_names_the_lines_it_gave() {
    let dir = Dir::new("cut");
    // 3000 lines of 40 bytes: 120 KB, which a read gives 64 KB of at most.
    dir.write("big.txt", &format!("{}\n", "x".repeat(39)).repeat(3000));
    let (report, _, heard) = ask(&dir, 60, vec![Step::Read("big.txt", Some((1, Some(3000)))), Step::Answer("Read.")]);
    let (path, lines) = (report.looked_at[0].path.as_str(), report.looked_at[0].lines.clone().unwrap());
    assert_eq!(path, "big.txt");
    let (from, to) = lines.split_once('-').unwrap();
    assert_eq!(from, "1");
    let to: usize = to.parse().unwrap();
    assert!((1000..3000).contains(&to), "cut well before the 3000 asked for: {lines}");
    assert!(heard.lock().unwrap()[1].1.contains("cut at 65536 bytes"), "{:?}", heard.lock().unwrap()[1]);
}

#[test]
fn any_folder_will_do_git_or_not() {
    let dir = Dir::new("plain");
    project(&dir);
    assert!(!dir.folder().join(".git").exists());
    let (report, _, _) = ask(&dir, 60, vec![Step::Read("README.md", None), Step::Answer("A tiny project.")]);
    assert_eq!((report.status(), report.exit_code(), looked(&report)), (Status::Answered, 0, vec![("README.md", None)]));
}

// ── what it is refused ──────────────────────────────────────────────────────

/// **A call it was not given is denied, and nothing is run.** The scout is told no for an edit, a
/// create, a delete, a move, a command and a check, and for a read the folder's rule would ask
/// about; the model is told, and goes on; the runner is never reached; the files are as they were.
#[test]
fn a_call_it_was_not_given_is_denied_and_nothing_runs() {
    let dir = Dir::new("denied");
    project(&dir);
    let before = scratch::fingerprints(&dir.folder()).unwrap();
    let steps = vec![
        Step::Asks("edit_file", "src/lib.rs"),
        Step::Asks("create_file", "src/new.rs"),
        Step::Asks("delete_file", "src/lib.rs"),
        Step::Asks("move_file", "src/lib.rs"),
        Step::Asks("run_command", "del src\\lib.rs"),
        Step::Asks("run_check", "cargo test"),
        Step::Asks("read_file", "../outside.txt"),
        Step::Answer("I could not change anything, and that is all I know."),
    ];
    let (report, ran, heard) = ask(&dir, 60, steps);

    assert!(ran.lock().unwrap().is_empty(), "the runner was reached: {:?}", ran.lock().unwrap());
    // Each of them was answered no, and the model was told so before its next request.
    let told: Vec<String> = heard.lock().unwrap().iter().skip(1).map(|(role, said)| format!("{role}: {said}")).collect();
    assert_eq!(told.len(), 7);
    for (said, want) in told.iter().zip([
        "tool: The user declined this change to src/lib.rs. Nothing was written.",
        "tool: The user declined this change to src/new.rs. Nothing was written.",
        "tool: The user declined to delete src/lib.rs. Nothing was deleted.",
        "tool: The user declined to move src/lib.rs. Nothing was moved.",
        "tool: The user declined to run `del src\\lib.rs`. Nothing was run.",
        "tool: The user declined to run `cargo test`. Nothing was run.",
        "tool: The user declined to let you read ../outside.txt.",
    ]) {
        assert_eq!(said, want);
    }
    assert_eq!(report.answer, "I could not change anything, and that is all I know.");
    assert_eq!((report.status(), report.exit_code()), (Status::Answered, 0));
    assert!(report.looked_at.is_empty());
    assert_eq!(scratch::fingerprints(&dir.folder()).unwrap(), before, "the folder changed");
}

/// **Nothing is written anywhere in the folder, its `.git` included, down to the times**, by a run
/// that reads, lists, searches and is asked, over and over, to write.
#[test]
fn a_run_leaves_the_folder_and_its_git_as_it_found_them_down_to_the_times() {
    if !have_git() {
        return;
    }
    let dir = Dir::new("fingerprint");
    project(&dir);
    dir.write(".gitignore", "target/\n");
    dir.write("target/debug/out.bin", "built\n");
    dir.git_init();
    // Work that is not committed, and a file git has never seen.
    dir.write("src/lib.rs", &lines(31));
    dir.write("notes.txt", "untracked\n");
    let before = scratch::fingerprints(&dir.folder()).unwrap();
    assert!(before.keys().any(|k| k.starts_with(".git/objects/")), "the .git is in the fingerprint");

    let steps = vec![
        Step::List(None),
        Step::Search("line 3"),
        Step::Read("src/lib.rs", None),
        Step::Read("src/lib.rs", Some((5, Some(9)))),
        Step::Read("notes.txt", None),
        // A file of git's own is held back from a read: it arrives to be asked about, and is denied.
        Step::Asks("read_file", ".git/config"),
        Step::Asks("edit_file", "src/lib.rs"),
        Step::Asks("create_file", ".git/hooks/pre-commit"),
        Step::Asks("delete_file", "notes.txt"),
        Step::Asks("run_command", "git commit -am x"),
        Step::Answer("Done."),
    ];
    let (report, ran, _) = ask(&dir, 60, steps);
    assert_eq!(report.status(), Status::Answered, "{report:?}");
    assert!(ran.lock().unwrap().is_empty());
    assert_eq!(looked(&report), [("src/lib.rs", None), ("notes.txt", None)]);

    let after = scratch::fingerprints(&dir.folder()).unwrap();
    let differ: Vec<&String> = before.keys().chain(after.keys()).filter(|k| before.get(*k) != after.get(*k)).take(10).collect();
    assert!(differ.is_empty(), "the folder changed: {differ:?}");
    // And no file was left in the work folder but the lock.
    let left: Vec<String> = std::fs::read_dir(dir.work()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(left, ["lock"]);
}

// ── what the report says ────────────────────────────────────────────────────

fn answered() -> Report {
    Report {
        folder: r"C:\work\demo".into(),
        model: "qwen3-coder:30b".into(),
        answer: "It is in src/main.rs (lines 120-180).".into(),
        looked_at: vec![
            LookedAt { path: "src/main.rs".into(), lines: Some("120-180".into()) },
            LookedAt { path: "src/lib.rs".into(), lines: None },
        ],
        seconds: 41.2,
        time: 300,
        ..Report::default()
    }
}

#[test]
fn the_text_is_a_first_line_the_answer_and_the_files_looked_at() {
    assert_eq!(
        answered().to_text(),
        "hugin: answered in 41 s on qwen3-coder:30b\n\
         \n\
         It is in src/main.rs (lines 120-180).\n\
         \n\
         looked at:\n  src/main.rs (lines 120-180)\n  src/lib.rs\n"
    );
    let none = Report { looked_at: Vec::new(), ..answered() };
    assert!(none.to_text().ends_with("\n\nlooked at: no file was read\n"), "{}", none.to_text());
}

#[test]
fn a_partial_answer_says_why_and_still_lists_what_was_looked_at() {
    let stopped = Report { stopped: true, answer: String::new(), ..answered() };
    assert_eq!(
        stopped.to_text(),
        "hugin: PARTIAL — stopped at the 300 s time limit; this is what it had by then.\n\
         \n\
         looked at:\n  src/main.rs (lines 120-180)\n  src/lib.rs\n"
    );
    let bound = Report { notes: vec!["Stopped after 20 tool rounds.".into()], ..answered() };
    assert!(bound.to_text().starts_with("hugin: PARTIAL — Stopped after 20 tool rounds. (41 s).\n\nIt is in"), "{}", bound.to_text());
    let silent = Report { answer: String::new(), ..answered() };
    assert!(silent.to_text().starts_with("hugin: PARTIAL — the model gave no answer (41 s).\n"), "{}", silent.to_text());
}

#[test]
fn an_error_is_one_line() {
    let report = Report { error: Some("could not start Ollama: not installed".into()), ..answered() };
    assert_eq!(report.to_text(), "hugin --ask could not answer: could not start Ollama: not installed\n");
}

/// Answered is 0, partial 3, and a run that could not be made 1.
#[test]
fn each_way_a_run_ends_has_a_status_and_an_exit_code() {
    let cases = [
        ("answered", answered(), Status::Answered, 0),
        ("stopped by the time limit", Report { stopped: true, ..answered() }, Status::Partial, 3),
        ("stopped at a bound", Report { notes: vec!["Stopped after 20 tool rounds.".into()], ..answered() }, Status::Partial, 3),
        ("no answer", Report { answer: String::new(), ..answered() }, Status::Partial, 3),
        ("an error", Report { error: Some("no model".into()), ..answered() }, Status::Error, 1),
        ("an error, and nothing else", Report { error: Some("no model".into()), answer: String::new(), ..answered() }, Status::Error, 1),
    ];
    for (what, report, status, code) in cases {
        assert_eq!((report.status(), report.exit_code()), (status, code), "{what}");
        assert_eq!(report.to_json()["status"], serde_json::to_value(status).unwrap(), "{what}");
    }
    let words: Vec<_> = [Status::Answered, Status::Partial, Status::Error].iter().map(|s| serde_json::to_value(s).unwrap()).collect();
    assert_eq!(words, ["answered", "partial", "error"]);
}

/// **The JSON, field for field and in order.**
#[test]
fn the_json_has_exactly_these_fields_in_this_order() {
    let want = r##"{"status":"answered","answer":"It is in src/main.rs (lines 120-180).","looked_at":[{"path":"src/main.rs","lines":"120-180"},{"path":"src/lib.rs","lines":null}],"seconds":41.2,"model":"qwen3-coder:30b","folder":"C:\\work\\demo","error":null}"##;
    assert_eq!(answered().to_json_string(), want);
    let failed = Report { error: Some("no model".into()), answer: String::new(), looked_at: Vec::new(), ..answered() };
    assert_eq!(
        failed.to_json_string(),
        r##"{"status":"error","answer":"","looked_at":[],"seconds":41.2,"model":"qwen3-coder:30b","folder":"C:\\work\\demo","error":"no model"}"##
    );
}

// ── how a run ends ──────────────────────────────────────────────────────────

/// **On time.** A model that takes longer than the whole limit to reply is stopped at the
/// deadline, and the run is partial: what it had read is kept, and there is no answer.
#[test]
fn a_slow_model_is_stopped_on_time_and_the_run_is_partial() {
    let dir = Dir::new("slow");
    project(&dir);
    let started = Instant::now();
    let (report, ran, _) = ask(&dir, 4, vec![Step::Read("src/lib.rs", Some((1, Some(4)))), Step::Sleep(30)]);
    let took = started.elapsed();
    assert!(took < Duration::from_secs(7), "took {took:?}");
    assert!(report.seconds <= 4.2, "the whole run had 4 seconds, and took {} s", report.seconds);
    assert!(report.stopped && report.answer.is_empty(), "{report:?}");
    assert_eq!((report.status(), report.exit_code()), (Status::Partial, 3));
    assert_eq!(looked(&report), [("src/lib.rs", Some("1-4"))]);
    assert!(report.to_text().starts_with("hugin: PARTIAL — stopped at the 4 s time limit"), "{}", report.to_text());
    assert_eq!(report.to_json()["status"], "partial");
    assert!(ran.lock().unwrap().is_empty());
}

#[test]
fn a_request_that_fails_is_a_run_that_could_not_be_made() {
    let dir = Dir::new("nomodel");
    project(&dir);
    let (report, _, _) = ask(&dir, 60, vec![Step::Fail("could not start Ollama: not installed")]);
    assert_eq!((report.status(), report.exit_code()), (Status::Error, 1));
    assert_eq!(report.error.as_deref(), Some("could not start Ollama: not installed"));
    assert!(report.to_text().starts_with("hugin --ask could not answer: could not start Ollama"));
    assert_eq!(report.to_json()["error"], "could not start Ollama: not installed");
}

/// The same lock as `-p`, so a `--ask` and a `-p` do not both load the model.
#[test]
fn one_run_at_a_time_with_p() {
    if !cfg!(windows) {
        return;
    }
    let dir = Dir::new("busy");
    project(&dir);
    let held = Lock::take(&dir.work()).unwrap();
    let (wire, _, _, _) = script(vec![]);
    let report = execute(&dir.job(60), "m", wire);
    assert_eq!(report.status(), Status::Error);
    assert!(report.error.as_deref().is_some_and(|e| e.contains("already running")), "{report:?}");
    drop(held);
    let (report, _, _) = ask(&dir, 60, vec![Step::Answer("Free now.")]);
    assert_eq!(report.status(), Status::Answered, "{report:?}");
}

/// A folder that cannot be asked about is said in words before anything is made, and the model is
/// never set up.
#[test]
fn a_folder_that_cannot_be_asked_about_is_an_error_before_anything_starts() {
    let dir = Dir::new("nofolder");
    let wire = |_: &Ctx| -> Wired { panic!("the model was set up") };
    let mut job = dir.job(60);
    job.folder = dir.root.join("not-there");
    let report = execute(&job, "m", wire);
    assert_eq!((report.status(), report.exit_code()), (Status::Error, 1));
    assert!(report.error.as_deref().is_some_and(|e| e.contains("cannot use")), "{report:?}");

    if cfg!(windows) {
        let mut job = dir.job(60);
        job.folder = PathBuf::from(r"C:\");
        let report = execute(&job, "m", |_: &Ctx| -> Wired { panic!("the model was set up") });
        assert_eq!(report.status(), Status::Error);
        assert!(report.error.as_deref().is_some_and(|e| e.contains("too broad")), "{report:?}");
    }
    // Nothing was made for any of them, not even the work folder.
    assert!(!dir.work().exists());
}

#[test]
fn the_claude_note_tells_claude_when_to_ask_hugin_a_question_and_to_check_the_answer() {
    let note: String = crate::print::CLAUDE_NOTE.split_whitespace().collect::<Vec<_>>().join(" ");
    for must in [
        "**Questions about a codebase** (where is X, how does Y work, what calls Z)",
        "`hugin --ask \"<the question>\" --json`",
        "in place and read-only",
        "Check its `looked_at`, and open those files yourself if the answer matters: it can be wrong.",
    ] {
        assert!(note.contains(must), "{must}\n{note}");
    }
}

#[test]
fn nothing_but_the_lock_is_made_outside_the_folder() {
    let dir = Dir::new("lockonly");
    project(&dir);
    let (report, _, _) = ask(&dir, 60, vec![Step::Read("README.md", None), Step::Answer("A tiny project.")]);
    assert_eq!(report.status(), Status::Answered);
    let left: Vec<String> = std::fs::read_dir(dir.work()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(left, ["lock"]);
    // ... and nothing in the folder: the same files, as they were written.
    let mut files: Vec<String> = scratch::fingerprints(&dir.folder()).unwrap().into_keys().collect();
    files.sort();
    assert_eq!(files, ["README.md", "src/", "src/lib.rs", "src/other.rs"]);
}
