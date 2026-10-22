//! I3 — which shell should Hugin's `run_command` use?
//!
//!   cargo run -p bench --bin cmdprobe
//!   cargo run -p bench --bin cmdprobe -- --runs 1 --only c05 --shell bash
//!
//! Hugin gives the model one tool that runs any command, and the shell decides what the
//! command's text means: PowerShell, cmd.exe or Git Bash. They differ in exactly the ways
//! a small model trips over — quoting, pipelines, `%` and `$`, what `ls` is — so the shell
//! is chosen by measurement, not taste. Every available shell gets the same eight tasks.
//!
//! **Each run is a fresh fixture** in a temp folder: a git repository on `main` with three
//! commits, holding a tiny Cargo crate that does not build (`E0308`) and `notes/` with
//! twelve `.txt` files of known length, four of which mention a deadline and two of which
//! hold a `FIXME`. The model is told only the question; **this program** scores the final
//! answer, from facts it wrote into the fixture itself.
//!
//! **Offline, and nothing is destroyed.** Cargo runs with `CARGO_NET_OFFLINE=true` and one
//! shared target directory. A probe approves every call, so its frontend refuses any command
//! that matches a destructive pattern (`probe::destructive`) and counts the refusal.
//!
//! A run passes when the answer is right **and** it took no more rounds than the app
//! allows. Right but later is `late`, and counted apart.

#[path = "../probe.rs"]
mod probe;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use bifrost_core::chat::{Message, Options};
use bifrost_core::check::{self, Shell};
use bifrost_core::edit::Workspace;
use bifrost_core::{config, ollama, tools};

use probe::{Check, Setup, Transport};

/// Past the app's own bound, so a task that needs eleven rounds is seen as that.
const PROBE_ROUNDS: usize = 20;

// ── the fixture ─────────────────────────────────────────────────────────────

const CARGO_TOML: &str = "[package]
name = \"demo\"
version = \"0.1.0\"
edition = \"2021\"

[workspace]
";

/// `double` returns an `i32` and is handed a `&str`: `error[E0308]: mismatched types`.
const MAIN_RS: &str = "fn double(x: i32) -> i32 {
    x * 2
}

fn main() {
    let n: i32 = double(\"4\");
    println!(\"{n}\");
}
";

const NOTES_README: &str = "These notes belong to the demo project.
One idea to a file.
Nothing here is secret.
";

/// The subjects of the three commits, oldest first.
const SUBJECTS: [&str; 3] = ["Add the demo crate", "Add the project notes", "Flag two notes for follow-up"];

/// The notes the last commit touched, and the other facts the answers are checked against.
/// All of them are written into the fixture by [`build`], so they cannot drift from it.
const FLAGGED: [&str; 2] = ["n03.txt", "n07.txt"];
const DEADLINE_NOTES: [usize; 4] = [2, 5, 9, 11];
const NOTE_COUNT: usize = 12;

/// Note `n` (1-based): `n + 2` lines. Line 2 mentions a deadline in the notes that do, and
/// line 1 holds a FIXME in the flagged ones once the last commit has been made.
fn note(n: usize, flagged: bool) -> String {
    let mut text = String::new();
    for line in 1..=n + 2 {
        let body = if flagged && line == 1 {
            format!("FIXME: look at note {n} again")
        } else if line == 2 && DEADLINE_NOTES.contains(&n) {
            "the deadline moves to friday".to_string()
        } else {
            format!("note {n} line {line}")
        };
        text.push_str(&body);
        text.push('\n');
    }
    text
}

fn note_name(n: usize) -> String {
    format!("n{n:02}.txt")
}

/// What the answers are checked against.
#[derive(Debug, Clone, PartialEq)]
struct Facts {
    last_subject: &'static str,
    commits: usize,
    branch: &'static str,
    last_files: [&'static str; 2],
    /// Lines in all the `.txt` files under `notes/` (the README is not one).
    total_lines: usize,
    deadline_files: usize,
    /// What `rustc --version` prints, read when the probe starts.
    rustc: String,
}

fn facts(rustc: &str) -> Facts {
    Facts {
        last_subject: SUBJECTS[2],
        commits: SUBJECTS.len(),
        branch: "main",
        last_files: FLAGGED,
        total_lines: (1..=NOTE_COUNT).map(|n| n + 2).sum(),
        deadline_files: DEADLINE_NOTES.len(),
        rustc: rustc.trim().to_string(),
    }
}

fn git(dir: &Path, args: &[&str]) -> Result<(), String> {
    let out = Command::new("git").args(args).current_dir(dir).output().map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("git {args:?}: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn commit(dir: &Path, subject: &str) -> Result<(), String> {
    git(dir, &["add", "-A"])?;
    git(dir, &["commit", "-q", "-m", subject])
}

/// A fresh copy of the fixture in `dir`: the repository, its three commits and its files.
fn build(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    git(dir, &["init", "-q", "-b", "main"])?;
    // Local only, and nothing from the machine's own git settings that could stop a commit.
    for (k, v) in [
        ("user.name", "Probe"),
        ("user.email", "probe@example.com"),
        ("commit.gpgsign", "false"),
        ("core.autocrlf", "false"),
    ] {
        git(dir, &["config", k, v])?;
    }

    write(dir, "Cargo.toml", CARGO_TOML);
    write(dir, "src/main.rs", MAIN_RS);
    write(dir, ".gitignore", "target/\n");
    commit(dir, SUBJECTS[0])?;

    for n in 1..=NOTE_COUNT {
        write(dir, &format!("notes/{}", note_name(n)), &note(n, false));
    }
    write(dir, "notes/README.md", NOTES_README);
    commit(dir, SUBJECTS[1])?;

    for name in FLAGGED {
        let n: usize = name[1..3].parse().unwrap();
        write(dir, &format!("notes/{name}"), &note(n, true));
    }
    commit(dir, SUBJECTS[2])
}

// ── the tasks ───────────────────────────────────────────────────────────────

struct Task {
    id: &'static str,
    /// The question, and nothing else.
    prompt: &'static str,
    /// Whether the answer says what it should.
    right: fn(&str, &Facts) -> Result<(), String>,
}

/// The digits in `text`, as whole numbers: `"102 lines (3 files)"` holds 102 and 3.
fn numbers(text: &str) -> Vec<usize> {
    text.split(|c: char| !c.is_ascii_digit()).filter_map(|t| t.parse().ok()).collect()
}

fn says(answer: &str, wanted: &str) -> Result<(), String> {
    if answer.to_lowercase().contains(&wanted.to_lowercase()) {
        Ok(())
    } else {
        Err(format!("the answer does not say \"{wanted}\""))
    }
}

fn says_number(answer: &str, wanted: usize) -> Result<(), String> {
    if numbers(answer).contains(&wanted) {
        Ok(())
    } else {
        Err(format!("the answer does not say {wanted}"))
    }
}

fn tasks() -> Vec<Task> {
    vec![
        Task {
            id: "c01-last-subject",
            prompt: "What is the subject line of the most recent commit in this repository? Answer with just the subject.",
            right: |a, f| says(a, f.last_subject),
        },
        Task {
            id: "c02-commit-count",
            prompt: "How many commits does this repository have? Answer with just the number.",
            right: |a, f| says_number(a, f.commits),
        },
        Task {
            id: "c03-branch",
            prompt: "What is the name of the current git branch? Answer with just the name.",
            right: |a, f| says(a, f.branch),
        },
        Task {
            id: "c04-last-files",
            prompt: "Which two files did the most recent commit change? Answer with just the two file names.",
            right: |a, f| f.last_files.iter().try_for_each(|name| says(a, name)),
        },
        Task {
            id: "c05-build-error",
            prompt: "Run cargo build in the project. It fails with a compiler error: what is that error's code? (Rust error codes look like E0123.)",
            right: |a, _| says(a, "E0308"),
        },
        Task {
            id: "c06-rustc",
            prompt: "Which version of rustc is installed? Answer with just the version number.",
            right: |a, f| {
                // `rustc 1.99.0 (b940084d7 2026-09-28)`: the number is what is asked for.
                let version = f.rustc.split_whitespace().nth(1).unwrap_or(&f.rustc);
                says(a, version)
            },
        },
        Task {
            id: "c07-note-lines",
            prompt: "How many lines are there in total in all the .txt files under notes/? Answer with just the number.",
            right: |a, f| says_number(a, f.total_lines),
        },
        Task {
            id: "c08-deadline",
            prompt: "How many files under notes/ contain the word deadline? Answer with just the number.",
            right: |a, f| says_number(a, f.deadline_files),
        },
    ]
}

// ── running one task ────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Pass,
    Fail,
    Error,
}

impl Verdict {
    fn label(self) -> &'static str {
        match self {
            Verdict::Pass => "PASS",
            Verdict::Fail => "FAIL",
            Verdict::Error => "ERROR",
        }
    }
}

struct Outcome {
    verdict: Verdict,
    why: String,
    rounds: usize,
    /// Right, but later than the app would have allowed.
    late: bool,
    commands: usize,
    failed_commands: usize,
    denied_commands: usize,
    answer: String,
    secs: f64,
    log: Vec<String>,
}

/// The verdict on a finished run. Pure, so it can be tested without a model: the run's own
/// failures are errors, then the answer is judged, then the rounds it took.
fn judge(task: &Task, f: &Facts, r: &probe::Run) -> (Verdict, String, bool) {
    if let Some(e) = &r.error {
        return (Verdict::Error, e.clone(), false);
    }
    if r.blocked {
        return (Verdict::Error, "Smart App Control blocked a command".into(), false);
    }
    match (task.right)(&r.answer, f) {
        Ok(()) if r.hit_bound => (Verdict::Fail, format!("still calling tools after {PROBE_ROUNDS} rounds"), false),
        Ok(()) if r.rounds > tools::MAX_ROUNDS => (Verdict::Pass, String::new(), true),
        Ok(()) => (Verdict::Pass, String::new(), false),
        // A command that timed out told the model nothing: the environment, not the model.
        Err(e) if r.check_timeouts > 0 => {
            (Verdict::Error, format!("a command timed out — environment ({e})"), false)
        }
        Err(e) if r.stopped => (Verdict::Fail, format!("stopped after repeating a failing call — {e}"), false),
        Err(e) => (Verdict::Fail, e, false),
    }
}

fn run(model: &str, task: &Task, shell: Shell, opts: Options, scratch: &Path, env: &Check, f: &Facts) -> Outcome {
    let started = Instant::now();
    let dir = scratch.join("project");
    let backups = scratch.join("backups");
    let _ = std::fs::remove_dir_all(scratch);
    let mut out = Outcome {
        verdict: Verdict::Error,
        why: String::new(),
        rounds: 0,
        late: false,
        commands: 0,
        failed_commands: 0,
        denied_commands: 0,
        answer: String::new(),
        secs: 0.0,
        log: Vec::new(),
    };
    if let Err(e) = build(&dir) {
        out.why = format!("could not build the fixture: {e}");
        return out;
    }
    let ws = Workspace::open(&dir).expect("fixture folder");

    // What Hugin offers: the folder's tools and `run_command`, with the sentence it adds.
    let mut offered = tools::available(true, None);
    offered.push(tools::run_command_tool(shell));
    let system = format!("{} {}", tools::system_prompt(&ws, false), tools::command_sentence(shell));
    let mut messages = vec![Message::system(system), Message::user(task.prompt)];
    let setup =
        Setup { ws: &ws, backups: &backups, check: Some(env), confine_reads: false, compact: true, commands: Some(shell), unattended: false };
    let r = probe::run_loop(model, &mut messages, &offered, opts, Transport::Recover, PROBE_ROUNDS, &setup);

    let (verdict, why, late) = judge(task, f, &r);
    out.verdict = verdict;
    out.why = why;
    out.late = late;
    out.rounds = r.rounds;
    out.commands = r.commands;
    out.failed_commands = r.failed_commands;
    out.denied_commands = r.denied_commands;
    out.answer = r.answer;
    out.log = r.log;
    out.secs = started.elapsed().as_secs_f64();
    out
}

// ── main ────────────────────────────────────────────────────────────────────

fn arg(args: &[String], key: &str) -> Option<String> {
    args.iter().position(|a| a == key).and_then(|i| args.get(i + 1)).cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = config::load();
    let model = arg(&args, "--model").unwrap_or(cfg.model.clone());
    let runs: usize = arg(&args, "--runs").and_then(|s| s.parse().ok()).unwrap_or(3);
    let only = arg(&args, "--only");
    let shells: Vec<Shell> = match arg(&args, "--shell") {
        Some(name) => match Shell::parse(&name) {
            Some(shell) if shell.available() => vec![shell],
            Some(shell) => {
                eprintln!("{} is not available on this machine", shell.label());
                std::process::exit(1);
            }
            None => {
                eprintln!("unknown shell: {name} (powershell, cmd or bash)");
                std::process::exit(1);
            }
        },
        None => Shell::ALL.into_iter().filter(|s| s.available()).collect(),
    };
    let tasks: Vec<Task> =
        tasks().into_iter().filter(|t| only.as_deref().is_none_or(|o| t.id.starts_with(o))).collect();

    let rustc = match Command::new("rustc").arg("--version").output() {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => {
            eprintln!("could not run rustc --version");
            std::process::exit(1);
        }
    };
    let f = facts(&rustc);

    let scratch_root = std::env::temp_dir().join(format!("bifrost-cmdprobe-{}", std::process::id()));
    let env = Check {
        argv: Vec::new(),
        env: vec![
            ("CARGO_TARGET_DIR".into(), scratch_root.join("target").to_string_lossy().into_owned()),
            ("CARGO_NET_OFFLINE".into(), "true".into()),
        ],
    };

    println!("I3 — command probe");
    println!("model:  {model}");
    println!("ctx:    {}", cfg.num_ctx);
    println!("shells: {}", shells.iter().map(|s| s.label()).collect::<Vec<_>>().join(", "));
    println!("tasks:  {}", tasks.len());
    println!("runs:   {runs} at the app's own settings, plus 1 at temperature 0");
    println!("bound:  {PROBE_ROUNDS} rounds (the app stops at {})", tools::MAX_ROUNDS);
    println!("rustc:  {rustc}\n");

    // Find out now, not forty runs in, that the fixture is what the answers assume: it
    // builds into the one error, in the shared target directory.
    let warm = scratch_root.join("warm");
    if let Err(e) = build(&warm) {
        eprintln!("could not build the fixture: {e}");
        std::process::exit(1);
    }
    let cargo = vec!["cargo".to_string(), "build".to_string()];
    match check::run(&warm, &cargo, check::CHECK_TIMEOUT, &env.env, || false) {
        Ok(o) if !o.passed() && o.output.contains("E0308") => println!("fixture fails with E0308, as it should"),
        Ok(o) => {
            eprintln!("the fixture does not fail with E0308:\n{}", o.output);
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("could not run cargo: {e}");
            std::process::exit(1);
        }
    }

    match ollama::load(&model, cfg.num_ctx) {
        Ok(d) => println!("warmed in {:.1}s\n", d.as_secs_f64()),
        Err(e) => {
            eprintln!("could not load {model}: {e}");
            std::process::exit(1);
        }
    }

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let settings: Vec<(&str, Options, usize)> =
        vec![("app", Options::new(cfg.num_ctx), runs), ("t=0", Options::measured(cfg.num_ctx), 1)];

    let mut rows = Vec::new();
    let mut json = Vec::new();

    for &shell in &shells {
        for (label, opts, n) in &settings {
            let (mut pass, mut late, mut fail, mut error) = (0, 0, 0, 0);
            let (mut secs, mut commands, mut failed, mut denied) = (0.0, 0, 0, 0);
            println!("── {} · {} ──", shell.label(), label);
            for task in &tasks {
                let mut line = String::new();
                for i in 0..*n {
                    let scratch = scratch_root.join(format!("{}-{}-{i}", label.replace('=', ""), task.id));
                    let o = run(&model, task, shell, *opts, &scratch, &env, &f);
                    let _ = std::fs::remove_dir_all(&scratch);
                    match o.verdict {
                        Verdict::Pass if o.late => late += 1,
                        Verdict::Pass => pass += 1,
                        Verdict::Fail => fail += 1,
                        Verdict::Error => error += 1,
                    }
                    secs += o.secs;
                    commands += o.commands;
                    failed += o.failed_commands;
                    denied += o.denied_commands;
                    let shown = if o.verdict == Verdict::Pass && o.late { "LATE" } else { o.verdict.label() };
                    line.push_str(&format!("{shown:<5} "));
                    if o.verdict != Verdict::Pass {
                        println!("    {} run {}: {} — {}", task.id, i + 1, o.verdict.label(), o.why);
                        println!("      answer: {}", o.answer.replace('\n', " ⏎ "));
                        for l in &o.log {
                            println!("      {l}");
                        }
                    }
                    json.push(serde_json::json!({
                        "shell": shell.name(), "setting": label, "task": task.id, "run": i + 1,
                        "verdict": o.verdict.label(), "late": o.late, "why": o.why, "rounds": o.rounds,
                        "commands": o.commands, "failedCommands": o.failed_commands,
                        "deniedCommands": o.denied_commands, "answer": o.answer, "log": o.log,
                        "secs": (o.secs * 10.0).round() / 10.0,
                    }));
                }
                println!("  {:<18} {line}", task.id);
            }
            let total = pass + late + fail + error;
            let per = |x: f64| x / total.max(1) as f64;
            let pct = 100.0 * pass as f64 / total.max(1) as f64;
            println!(
                "  → {pass}/{total} pass within {} rounds ({pct:.0}%) · {late} only later · {fail} fail · {error} error · \
                 {:.1} commands/run · {:.1} failed/run · {denied} denied · {:.1}s avg\n",
                tools::MAX_ROUNDS,
                per(commands as f64),
                per(failed as f64),
                per(secs)
            );
            rows.push(format!(
                "| {} | {label} | {pass}/{total} ({pct:.0}%) | {late} | {fail} | {error} | {:.1} | {:.1} | {denied} | {:.1}s |",
                shell.label(),
                per(commands as f64),
                per(failed as f64),
                per(secs),
            ));
        }
    }

    println!("─── {model} ───");
    println!(
        "| shell | setting | pass (≤{} rounds) | pass later | fail | error | commands/run | failed/run | denied | time/run |",
        tools::MAX_ROUNDS
    );
    println!("|---|---|---|---|---|---|---|---|---|---|");
    for r in &rows {
        println!("{r}");
    }

    let out = PathBuf::from("evals/out").join(format!("I3-cmd-{stamp}"));
    if std::fs::create_dir_all(&out).is_ok() {
        let _ = std::fs::write(
            out.join("results.json"),
            serde_json::to_string_pretty(&serde_json::json!({ "model": model, "rustc": rustc, "runs": json })).unwrap(),
        );
        println!("\nsaved {}", out.join("results.json").display());
    }
    let _ = std::fs::remove_dir_all(&scratch_root);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("bifrost-cmdprobe-test-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn out(dir: &Path, args: &[&str]) -> String {
        let o = Command::new("git").args(args).current_dir(dir).output().unwrap();
        assert!(o.status.success(), "git {args:?}");
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    /// **The answers are true of the fixture**: what the tasks expect is what git and the
    /// files themselves say, read back from a fixture that was really built.
    #[test]
    fn the_fixture_is_what_the_answers_assume() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let dir = scratch("facts");
        build(&dir).unwrap();
        let f = facts("rustc 1.0.0 (abc 2026-01-01)");

        assert_eq!(out(&dir, &["rev-list", "--count", "HEAD"]), f.commits.to_string());
        assert_eq!(out(&dir, &["log", "-1", "--pretty=%s"]), f.last_subject);
        assert_eq!(out(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]), f.branch);
        let subjects = out(&dir, &["log", "--pretty=%s", "--reverse"]);
        assert_eq!(subjects.lines().collect::<Vec<_>>(), SUBJECTS);
        let changed = out(&dir, &["show", "--name-only", "--pretty=format:", "HEAD"]);
        let mut changed: Vec<&str> = changed.lines().collect();
        changed.sort();
        assert_eq!(changed, ["notes/n03.txt", "notes/n07.txt"]);

        let (mut lines, mut deadline, mut fixme) = (0, 0, 0);
        let mut txt = 0;
        for e in std::fs::read_dir(dir.join("notes")).unwrap() {
            let p = e.unwrap().path();
            let text = std::fs::read_to_string(&p).unwrap();
            if p.extension().is_some_and(|x| x == "txt") {
                txt += 1;
                lines += text.lines().count();
            }
            deadline += usize::from(text.to_lowercase().contains("deadline"));
            fixme += usize::from(text.contains("FIXME"));
        }
        assert_eq!((txt, lines, deadline, fixme), (12, f.total_lines, f.deadline_files, 2));
        assert_eq!(f.total_lines, 102);
        assert!(out(&dir, &["status", "--porcelain"]).is_empty(), "the fixture is committed whole");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// And the crate fails the way task 5 says. A target directory of its own, so this
    /// does not wait on the one the test run is holding.
    #[test]
    fn the_crate_fails_to_build_with_e0308() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let dir = scratch("build");
        build(&dir).unwrap();
        let env = vec![
            ("CARGO_TARGET_DIR".to_string(), dir.join("target-elsewhere").to_string_lossy().into_owned()),
            ("CARGO_NET_OFFLINE".to_string(), "true".to_string()),
        ];
        let argv = vec!["cargo".to_string(), "build".to_string()];
        let o = check::run(&dir, &argv, check::CHECK_TIMEOUT, &env, || false).unwrap();
        if o.blocked.is_some() {
            return; // Smart App Control: a fact about Windows, not the fixture.
        }
        assert!(!o.passed() && o.output.contains("E0308"), "{}", o.output);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn answered(task_id: &str, answer: &str) -> Result<(), String> {
        let task = tasks().into_iter().find(|t| t.id.starts_with(task_id)).unwrap();
        (task.right)(answer, &facts("rustc 1.99.0 (b940084d7 2026-09-28)"))
    }

    #[test]
    fn answers_are_scored_by_what_they_say_in_any_case() {
        assert!(answered("c01", "flag two NOTES for follow-up").is_ok());
        assert!(answered("c01", "Add the demo crate").is_err());
        assert!(answered("c02", "There are 3 commits.").is_ok());
        assert!(answered("c02", "13 commits").is_err(), "3 inside 13 is not 3");
        assert!(answered("c03", "Main").is_ok());
        assert!(answered("c03", "master").is_err());
        assert!(answered("c04", "notes/n03.txt and notes/n07.txt").is_ok());
        assert!(answered("c04", "notes/n03.txt").is_err());
        assert!(answered("c05", "error[e0308]: mismatched types").is_ok());
        assert!(answered("c05", "E0425").is_err());
        assert!(answered("c06", "1.99.0").is_ok());
        assert!(answered("c06", "rustc 1.98.0").is_err());
        assert!(answered("c07", "102").is_ok());
        assert!(answered("c07", "There are 102 lines (3 more in the README).").is_ok());
        assert!(answered("c07", "100").is_err());
        assert!(answered("c08", "4 files").is_ok());
        assert!(answered("c08", "2").is_err());
    }

    #[test]
    fn a_run_is_judged_by_its_failures_then_its_answer_then_its_rounds() {
        let task = tasks().into_iter().find(|t| t.id.starts_with("c03")).unwrap();
        let f = facts("rustc 1.0.0 (x 2026-01-01)");
        let run = |answer: &str, rounds: usize| probe::Run { answer: answer.into(), rounds, ..probe::Run::default() };

        assert!(matches!(judge(&task, &f, &run("main", 3)), (Verdict::Pass, _, false)));
        assert!(matches!(judge(&task, &f, &run("main", tools::MAX_ROUNDS + 1)), (Verdict::Pass, _, true)));
        assert!(matches!(judge(&task, &f, &run("dev", 3)), (Verdict::Fail, _, false)));
        let hit = probe::Run { hit_bound: true, ..run("main", 20) };
        assert!(matches!(judge(&task, &f, &hit), (Verdict::Fail, _, false)));
        let broke = probe::Run { error: Some("ollama".into()), ..run("main", 1) };
        assert!(matches!(judge(&task, &f, &broke), (Verdict::Error, _, _)));
        let blocked = probe::Run { blocked: true, ..run("main", 1) };
        assert!(matches!(judge(&task, &f, &blocked), (Verdict::Error, _, _)));
        // A wrong answer after a command timed out is the environment's.
        let timed = probe::Run { check_timeouts: 1, ..run("dev", 2) };
        assert!(matches!(judge(&task, &f, &timed), (Verdict::Error, _, _)));
    }

    #[test]
    fn there_are_eight_tasks_with_distinct_ids() {
        let ids: Vec<_> = tasks().iter().map(|t| t.id).collect();
        assert_eq!(ids.len(), 8);
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 8);
    }
}
