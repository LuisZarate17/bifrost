//! J1 — can a local model answer questions about this code, by reading it?
//!
//!   cargo run -p bench --bin askprobe -- --selfcheck
//!   cargo run -p bench --bin askprobe -- --model qwen3-coder:30b
//!   cargo run -p bench --bin askprobe -- --model gpt-oss:20b --think high --runs 3 --only 07
//!
//! Puts the twelve questions of `evals/askprobe/questions.json` to a local model through
//! `hugin --ask`, in a worktree at the pinned commit, and scores each answer by the facts it must
//! contain and the files Hugin says it looked at. Every answer in the file is true of the code at
//! the pin, and the worktree is what the model reads.
//!
//! **A fact is present if ANY of its regexes matches the answer** (case-insensitive; `.` does not
//! cross a line, `[\s\S]` does). The score: *right* when every fact is present; *partly* when at
//! least half are, or fewer but at least one expected file is among those Hugin looked at; *wrong*
//! otherwise. A Hugin `status` of `partial` caps the score at partly and `error` at wrong: an
//! answer cut short by the time limit, or no answer at all, is not credited in full.
//!
//! Hugin runs in a sandbox as in `realprobe`: `APPDATA`, `LOCALAPPDATA`, `TEMP` and `TMP` point
//! into a folder of this invocation's own, with a `config.json` that names the model, so the
//! user's real settings are never read or written. Questions change no file; the worktree is
//! checked after every answer, and reset (and the answer noted) if anything in it changed.
//!
//! `--selfcheck` runs no model and no Hugin: it proves each question's two hand-written answers
//! score right and wrong, and that every file the questions name exists at the pin.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bifrost_core::config::Think;
use bifrost_core::{check, ollama};
use regex::{Regex, RegexBuilder};
use serde_json::Value;

// What this probe and realprobe share: the worktree, the sandbox, the installed-model check.
#[path = "../harness.rs"]
mod harness;

use harness::{Sandbox, Worktree, copy_hugin, first_line, git, require_installed, unix_now};

/// What Hugin is told it may take, and how long this program waits for it before it ends the
/// process tree and books a wrong answer.
const HUGIN_TIME: u64 = 600;
const HUGIN_GUARD: Duration = Duration::from_secs(700);
const BUILD_TIMEOUT: Duration = Duration::from_secs(1800);
/// The output kept of a command: all of it, for any run this program makes.
const KEEP: usize = 16 * 1024 * 1024;
/// A model tag no Ollama has: the selfcheck proves a run refuses it.
const NO_SUCH_MODEL: &str = "askprobe-no-such-model";

const USAGE: &str = "\
usage: askprobe --model <tag> [--runs N] [--only <id-prefix>] [--think <off|low|medium|high>] [--keep]
       askprobe --selfcheck [--only <id-prefix>]
       askprobe --help

  --model <tag>     the Ollama model to ask (required; the config's is never used)
  --runs N          times each question is asked (default 2)
  --only <prefix>   only the questions whose id starts with this
  --think <level>   the model's `think` setting for the whole run: off, low, medium or high
  --keep            keep the worktree and the sandbox (%TEMP%\\bifrost-askprobe-<pid>)
  --selfcheck       no model, no Hugin: prove every question scores as it claims to
  --help, -h        this text

Run it from the repository root. Results go to evals/out/J1-ask-<unix>/results.json.
";

// ── the questions ────────────────────────────────────────────────────────────

/// One thing an answer must say: present if any of the regexes matches.
#[derive(Debug, Clone)]
struct Fact {
    name: String,
    any: Vec<Regex>,
}

impl Fact {
    fn present(&self, answer: &str) -> bool {
        self.any.iter().any(|re| re.is_match(answer))
    }
}

/// Two hand-written answers, with the files Hugin would have looked at for each.
#[derive(Debug, Clone)]
struct Selfcheck {
    right: String,
    right_looked_at: Vec<String>,
    wrong: String,
    wrong_looked_at: Vec<String>,
}

#[derive(Debug, Clone)]
struct Question {
    id: String,
    question: String,
    required: Vec<Fact>,
    /// Repo-relative, forward slashes: what a good answer reads.
    files: Vec<String>,
    #[allow(dead_code)]
    note: String,
    selfcheck: Option<Selfcheck>,
}

#[derive(Debug, Clone)]
struct Questions {
    pin: String,
    list: Vec<Question>,
}

fn text_of(v: &Value, key: &str) -> Result<String, String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("`{key}` is missing or not text"))
}

fn texts_of(v: &Value, key: &str) -> Result<Vec<String>, String> {
    v.get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("`{key}` is missing or not a list"))?
        .iter()
        .map(|s| {
            s.as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("`{key}` holds something that is not text"))
        })
        .collect()
}

fn parse_fact(v: &Value) -> Result<Fact, String> {
    let name = text_of(v, "name")?;
    let any = texts_of(v, "any").map_err(|e| format!("fact `{name}`: {e}"))?;
    if any.is_empty() {
        return Err(format!("fact `{name}` has no regex"));
    }
    let any = any
        .iter()
        .map(|p| {
            RegexBuilder::new(p)
                .case_insensitive(true)
                .build()
                .map_err(|e| format!("fact `{name}`: bad regex `{p}`: {e}"))
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Fact { name, any })
}

fn parse_selfcheck(v: &Value) -> Result<Selfcheck, String> {
    Ok(Selfcheck {
        right: text_of(v, "right")?,
        right_looked_at: texts_of(v, "right_looked_at")?,
        wrong: text_of(v, "wrong")?,
        wrong_looked_at: texts_of(v, "wrong_looked_at")?,
    })
}

fn parse_question(v: &Value) -> Result<Question, String> {
    let id = text_of(v, "id")?;
    let inner = |e: String| format!("{id}: {e}");
    let required = v
        .get("required")
        .and_then(Value::as_array)
        .ok_or_else(|| inner("`required` is missing".to_string()))?
        .iter()
        .map(|f| parse_fact(f).map_err(inner))
        .collect::<Result<Vec<_>, String>>()?;
    if required.is_empty() {
        return Err(inner("`required` may not be empty".to_string()));
    }
    let files = texts_of(v, "files").map_err(inner)?;
    if files.is_empty() {
        return Err(inner("`files` may not be empty".to_string()));
    }
    let selfcheck = v.get("selfcheck").map(parse_selfcheck).transpose().map_err(inner)?;
    Ok(Question {
        question: text_of(v, "question").map_err(inner)?,
        required,
        files,
        note: text_of(v, "note").map_err(inner)?,
        selfcheck,
        id,
    })
}

/// `questions.json` as questions. The ids must be different from one another.
fn parse_questions(text: &str) -> Result<Questions, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    let pin = text_of(&v, "pin")?;
    let list = v
        .get("questions")
        .and_then(Value::as_array)
        .ok_or("`questions` is missing")?
        .iter()
        .map(parse_question)
        .collect::<Result<Vec<_>, String>>()?;
    for (i, q) in list.iter().enumerate() {
        if list[..i].iter().any(|p| p.id == q.id) {
            return Err(format!("the id {} is used twice", q.id));
        }
    }
    Ok(Questions { pin, list })
}

fn load_questions(repo: &Path) -> Result<Questions, String> {
    let file = repo.join("evals").join("askprobe").join("questions.json");
    let text = std::fs::read_to_string(&file)
        .map_err(|e| format!("could not read {}: {e} — run from the repository root", file.display()))?;
    parse_questions(&text).map_err(|e| format!("{}: {e}", file.display()))
}

// ── scoring ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Score {
    Wrong,
    Partly,
    Right,
}

impl Score {
    const ALL: [Score; 3] = [Score::Right, Score::Partly, Score::Wrong];

    fn label(self) -> &'static str {
        match self {
            Score::Right => "RIGHT",
            Score::Partly => "PARTLY",
            Score::Wrong => "WRONG",
        }
    }
}

/// A path as Hugin reports it, or as the questions name it, for comparing: forward slashes, no
/// leading `./`.
fn normalize_path(path: &str) -> String {
    let mut s = path.trim().replace('\\', "/");
    while let Some(rest) = s.strip_prefix("./") {
        s = rest.to_string();
    }
    s
}

/// What an answer came to.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Scored {
    score: Score,
    /// The names of the facts the answer lacks.
    missing: Vec<String>,
    /// The expected files among those Hugin looked at.
    files_hit: Vec<String>,
    why: String,
}

/// The score for `answer`, given the paths Hugin looked at and its `status`.
///
/// Right: every fact. Partly: at least half the facts, or fewer than half but at least one expected
/// file looked at. Wrong: otherwise. Then the status caps it: `partial` at partly, `error` (and a run
/// that made no report, `-`) at wrong.
fn score_answer(q: &Question, answer: &str, looked_at: &[String], status: &str) -> Scored {
    let missing: Vec<String> = q
        .required
        .iter()
        .filter(|f| !f.present(answer))
        .map(|f| f.name.clone())
        .collect();
    let (total, present) = (q.required.len(), q.required.len() - missing.len());
    let seen: Vec<String> = looked_at.iter().map(|p| normalize_path(p)).collect();
    let files_hit: Vec<String> = q
        .files
        .iter()
        .filter(|f| seen.iter().any(|s| s.eq_ignore_ascii_case(&normalize_path(f))))
        .cloned()
        .collect();

    let (mut score, mut why) = if missing.is_empty() {
        (Score::Right, format!("all {total} facts"))
    } else if present * 2 >= total {
        (Score::Partly, format!("{present} of {total} facts"))
    } else if !files_hit.is_empty() {
        (
            Score::Partly,
            format!("{present} of {total} facts, but an expected file was read"),
        )
    } else {
        (
            Score::Wrong,
            format!("{present} of {total} facts, and no expected file was read"),
        )
    };
    let cap = match status {
        "error" | "-" => Score::Wrong,
        "partial" => Score::Partly,
        _ => Score::Right,
    };
    if score > cap {
        why = format!(
            "{why}; capped at {} because Hugin's status was {status}",
            cap.label().to_ascii_lowercase()
        );
        score = cap;
    }
    Scored {
        score,
        missing,
        files_hit,
        why,
    }
}

// ── Hugin ────────────────────────────────────────────────────────────────────

/// What a `hugin --ask --json` run said.
#[derive(Debug, Default, PartialEq)]
struct HuginReport {
    status: String,
    answer: String,
    looked_at: Vec<String>,
    seconds: f64,
    error: Option<String>,
}

/// The report in Hugin's output: the last line that is a JSON object with a `status`. Hugin's
/// stdout and stderr arrive interleaved, so it is looked for, not assumed to be the whole.
fn parse_report(output: &str) -> Result<HuginReport, String> {
    for line in output.lines().rev() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(status) = v.get("status").and_then(Value::as_str) else {
            continue;
        };
        let looked_at = v
            .get("looked_at")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|l| l.get("path").and_then(Value::as_str).or_else(|| l.as_str()))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        return Ok(HuginReport {
            status: status.to_string(),
            answer: v.get("answer").and_then(Value::as_str).unwrap_or("").to_string(),
            looked_at,
            seconds: v.get("seconds").and_then(Value::as_f64).unwrap_or(0.0),
            error: v.get("error").and_then(Value::as_str).map(str::to_string),
        });
    }
    Err("Hugin printed no JSON report".to_string())
}

/// `hugin --ask -` on `question` in `tree`. `Err` is a run that made no report: it could not start,
/// was killed past the guard, was stopped, or printed nothing to read.
fn ask_hugin(hugin: &Path, sandbox: &Sandbox, tree: &Path, question: &str, model: &str) -> Result<HuginReport, String> {
    let argv: Vec<String> = [
        &hugin.to_string_lossy(),
        "--ask",
        "-",
        "--json",
        "--folder",
        &tree.to_string_lossy(),
        "--time",
        &HUGIN_TIME.to_string(),
        "--model",
        model,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let o = check::run_with_input(
        tree,
        &argv,
        question.as_bytes(),
        KEEP,
        HUGIN_GUARD,
        &sandbox.env,
        || false,
    )?;
    match &o.ended {
        check::Ended::TimedOut => {
            return Err(format!(
                "Hugin ran past {}s and its process tree was killed",
                HUGIN_GUARD.as_secs()
            ));
        }
        check::Ended::Stopped => return Err("Hugin was stopped".to_string()),
        check::Ended::DiskFull => return Err("Hugin was stopped for a nearly full disk".to_string()),
        check::Ended::Exited(_) => {}
    }
    parse_report(&o.output).map_err(|e| {
        if let Some(what) = &o.blocked {
            return format!("Windows blocked {what}");
        }
        let tail: String = o
            .output
            .chars()
            .rev()
            .take(300)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("{e} (exit {:?}): {}", o.ended, tail.trim())
    })
}

/// `git status --porcelain` in the worktree: the first changed line, or `None` when it is clean. A
/// status that cannot be read counts as a change.
fn dirty(tree: &Path) -> Option<String> {
    match git(Some(tree), &["status", "--porcelain"]) {
        Ok(out) if out.status.success() => {
            let line = first_line(&out.stdout);
            (!line.is_empty()).then_some(line)
        }
        Ok(out) => Some(format!("git status failed: {}", first_line(&out.stderr))),
        Err(e) => Some(e),
    }
}

// ── one run ──────────────────────────────────────────────────────────────────

struct Ctx {
    repo: PathBuf,
    scratch: PathBuf,
    keep: bool,
}

#[derive(Debug, Clone)]
struct RunResult {
    id: String,
    run: usize,
    score: Score,
    missing: Vec<String>,
    files_hit: Vec<String>,
    files_expected: usize,
    seconds: f64,
    status: String,
    why: String,
    answer: String,
    /// The worktree was not clean after this answer and was reset.
    reset: bool,
}

fn run_one(tree: &Worktree, hugin: &Path, sandbox: &Sandbox, q: &Question, run: usize, model: &str) -> RunResult {
    let started = Instant::now();
    let mut r = match ask_hugin(hugin, sandbox, &tree.path, &q.question, model) {
        Err(why) => RunResult {
            id: q.id.clone(),
            run,
            score: Score::Wrong,
            missing: q.required.iter().map(|f| f.name.clone()).collect(),
            files_hit: Vec::new(),
            files_expected: q.files.len(),
            seconds: started.elapsed().as_secs_f64(),
            status: "-".to_string(),
            why: format!("no report: {why}"),
            answer: String::new(),
            reset: false,
        },
        Ok(report) => {
            let s = score_answer(q, &report.answer, &report.looked_at, &report.status);
            let mut why = s.why;
            if report.status == "error" {
                why = format!("{why} — {}", report.error.as_deref().unwrap_or("no reason given"));
            }
            RunResult {
                id: q.id.clone(),
                run,
                score: s.score,
                missing: s.missing,
                files_hit: s.files_hit,
                files_expected: q.files.len(),
                seconds: report.seconds,
                status: report.status,
                why,
                answer: report.answer,
                reset: false,
            }
        }
    };
    if let Some(changed) = dirty(&tree.path) {
        let reset = tree.reset();
        r.reset = true;
        r.why = format!(
            "{}; the worktree was changed ({changed}) and {}",
            r.why,
            if reset.is_ok() {
                "was reset"
            } else {
                "COULD NOT BE RESET"
            }
        );
    }
    r
}

// ── reports ──────────────────────────────────────────────────────────────────

fn count(results: &[RunResult], score: Score) -> usize {
    results.iter().filter(|r| r.score == score).count()
}

fn mean_seconds(results: &[RunResult]) -> f64 {
    if results.is_empty() {
        0.0
    } else {
        results.iter().map(|r| r.seconds).sum::<f64>() / results.len() as f64
    }
}

/// The table at the end: answers by score, then the mean time.
fn summary(results: &[RunResult], questions: usize, runs: usize) -> Vec<String> {
    let mut lines = vec!["| score | answers |".to_string(), "|---|---|".to_string()];
    for s in Score::ALL {
        lines.push(format!("| {} | {} |", s.label(), count(results, s)));
    }
    lines.push(format!("| total | {} |", results.len()));
    lines.push(String::new());
    lines.push(format!(
        "right {} / partly {} / wrong {} out of {} answers ({questions} questions × {runs} runs)",
        count(results, Score::Right),
        count(results, Score::Partly),
        count(results, Score::Wrong),
        results.len()
    ));
    lines.push(format!("mean time: {:.1}s per answer", mean_seconds(results)));
    let unreported = results.iter().filter(|r| r.status == "-").count();
    if unreported > 0 {
        lines.push(format!(
            "{unreported} answer(s) had no Hugin report at all (counted wrong; see why)"
        ));
    }
    lines
}

fn results_json(
    model: &str,
    think: Option<Think>,
    pin: &str,
    results: &[RunResult],
    questions: usize,
    runs: usize,
) -> Value {
    let answers: Vec<Value> = results
        .iter()
        .map(|r| {
            serde_json::json!({
                "question": r.id, "run": r.run, "score": r.score.label().to_ascii_lowercase(), "missing": r.missing,
                "files_hit": r.files_hit, "files_expected": r.files_expected, "seconds": (r.seconds * 10.0).round() / 10.0,
                "status": r.status, "why": r.why, "worktree_reset": r.reset, "answer": r.answer,
            })
        })
        .collect();
    serde_json::json!({
        "model": model, "think": think.map(Think::name), "pin": pin, "questions": questions, "runs_each": runs,
        "summary": {
            "right": count(results, Score::Right), "partly": count(results, Score::Partly), "wrong": count(results, Score::Wrong),
            "total": results.len(), "mean_seconds": (mean_seconds(results) * 10.0).round() / 10.0,
        },
        "answers": answers,
    })
}

// ── selfcheck ────────────────────────────────────────────────────────────────

/// Whether `path` is in the tree of `pin`.
fn exists_at(pin: &str, path: &str) -> Result<bool, String> {
    Ok(git(None, &["cat-file", "-e", &format!("{pin}:{path}")])?
        .status
        .success())
}

fn selfcheck(qs: &Questions, list: &[Question]) -> bool {
    println!("J1 — askprobe selfcheck (no model, no Hugin)\n");
    println!("| question | right answer scores right | wrong answer scores wrong | files exist at the pin |");
    println!("|---|---|---|---|");
    let mut all_ok = true;

    let rev = format!("{}^{{commit}}", qs.pin);
    let pin_ok = git(None, &["rev-parse", "--verify", &rev]).is_ok_and(|o| o.status.success());
    if !pin_ok {
        println!("the pin {} is not a commit here", qs.pin);
        all_ok = false;
    }

    for q in list {
        let mut notes: Vec<String> = Vec::new();
        let (mut right, mut wrong) = ("NO".to_string(), "NO".to_string());
        match &q.selfcheck {
            None => notes.push("no selfcheck answers".to_string()),
            Some(sc) => {
                let r = score_answer(q, &sc.right, &sc.right_looked_at, "answered");
                if r.score == Score::Right {
                    right = "yes".to_string();
                } else {
                    notes.push(format!(
                        "right answer scored {}: {} (missing: {})",
                        r.score.label(),
                        r.why,
                        r.missing.join(", ")
                    ));
                }
                let w = score_answer(q, &sc.wrong, &sc.wrong_looked_at, "answered");
                if w.score == Score::Wrong {
                    wrong = "yes".to_string();
                } else {
                    let hit: Vec<&str> = q
                        .required
                        .iter()
                        .filter(|f| f.present(&sc.wrong))
                        .map(|f| f.name.as_str())
                        .collect();
                    notes.push(format!(
                        "wrong answer scored {}: {} (facts it hit: {})",
                        w.score.label(),
                        w.why,
                        hit.join(", ")
                    ));
                }
            }
        }
        let (mut gone, mut files_ok): (Vec<&str>, bool) = (Vec::new(), true);
        for f in &q.files {
            match exists_at(&qs.pin, f) {
                Ok(true) => {}
                Ok(false) => gone.push(f),
                Err(e) => {
                    files_ok = false;
                    notes.push(e);
                }
            }
        }
        let files = if files_ok && gone.is_empty() { "yes" } else { "NO" };
        if !gone.is_empty() {
            notes.push(format!("not in {}: {}", qs.pin, gone.join(", ")));
        }
        all_ok &= right == "yes" && wrong == "yes" && files == "yes";
        println!("| {} | {right} | {wrong} | {files} |", q.id);
        for n in notes {
            println!("    {}: {n}", q.id);
        }
    }

    let refused = require_installed(&["qwen3:8b".to_string()], NO_SUCH_MODEL).is_err()
        && require_installed(&[], NO_SUCH_MODEL).is_err()
        && require_installed(&["qwen3:8b".to_string()], "qwen3:8b").is_ok();
    all_ok &= refused;
    println!(
        "\n| a model Ollama does not list is refused before any work | {} |",
        if refused { "yes" } else { "NO" }
    );
    println!("\nselfcheck: {}", if all_ok { "all hold" } else { "FAILED" });
    all_ok
}

// ── main ─────────────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
struct Opts {
    model: Option<String>,
    runs: usize,
    only: Option<String>,
    think: Option<Think>,
    keep: bool,
    selfcheck: bool,
}

#[derive(Debug, PartialEq)]
enum Cmd {
    Help,
    Run(Opts),
}

fn parse_args(args: &[String]) -> Result<Cmd, String> {
    let mut o = Opts {
        model: None,
        runs: 2,
        only: None,
        think: None,
        keep: false,
        selfcheck: false,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = |flag: &str, what: &str| it.next().cloned().ok_or_else(|| format!("{flag} needs {what}"));
        match arg.as_str() {
            "--help" | "-h" => return Ok(Cmd::Help),
            "--keep" => o.keep = true,
            "--selfcheck" => o.selfcheck = true,
            "--model" => {
                let tag = value("--model", "a model tag")?;
                if tag.trim().is_empty() {
                    return Err("--model needs a model tag".to_string());
                }
                o.model = Some(tag.trim().to_string());
            }
            "--runs" => {
                let n = value("--runs", "a number")?;
                o.runs = n
                    .parse()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| format!("--runs takes a number above 0, not {n}"))?;
            }
            "--only" => o.only = Some(value("--only", "an id prefix")?),
            "--think" => {
                let word = value("--think", "off, low, medium or high")?;
                o.think = Some(
                    Think::parse(&word).ok_or_else(|| format!("--think takes off, low, medium or high, not {word}"))?,
                );
            }
            other if other.starts_with('-') => return Err(format!("unknown flag: {other}")),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    if o.model.is_none() && !o.selfcheck {
        return Err("--model <tag> is required".to_string());
    }
    Ok(Cmd::Run(o))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(real_main(&args));
}

/// Everything, so that the worktree and the scratch folder are dropped before the process exits.
fn real_main(args: &[String]) -> i32 {
    let opts = match parse_args(args) {
        Ok(Cmd::Help) => {
            print!("{USAGE}");
            return 0;
        }
        Ok(Cmd::Run(o)) => o,
        Err(e) => {
            eprintln!("{e}\n\n{USAGE}");
            return 2;
        }
    };
    let repo = std::env::current_dir().unwrap_or_default();
    let qs = match load_questions(&repo) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let list: Vec<Question> = qs
        .list
        .iter()
        .filter(|q| opts.only.as_deref().is_none_or(|o| q.id.starts_with(o)))
        .cloned()
        .collect();
    if list.is_empty() {
        eprintln!("no question matches --only {}", opts.only.as_deref().unwrap_or(""));
        return 2;
    }

    if opts.selfcheck {
        return i32::from(!selfcheck(&qs, &list));
    }
    let scratch = std::env::temp_dir().join(format!("bifrost-askprobe-{}", std::process::id()));
    let ctx = Ctx {
        repo,
        scratch,
        keep: opts.keep,
    };
    let code = real_run(&ctx, &opts, &qs.pin, &list);
    if opts.keep {
        println!("kept {}", ctx.scratch.display());
    } else {
        let _ = std::fs::remove_dir_all(&ctx.scratch);
        let _ = git(None, &["worktree", "prune"]);
    }
    code
}

/// `cargo build -p hugin` in the repo's own `target/` (cargo itself says whether the copy there is
/// stale), and the copy of the binary in `scratch`.
fn build_hugin(ctx: &Ctx) -> Result<PathBuf, String> {
    let target = ctx.repo.join("target");
    let env = vec![("CARGO_TARGET_DIR".to_string(), target.to_string_lossy().into_owned())];
    let argv = ["cargo", "build", "-p", "hugin"].map(String::from);
    let o = check::run_with_input(&ctx.repo, &argv, &[], KEEP, BUILD_TIMEOUT, &env, || false)?;
    if let Some(what) = &o.blocked {
        return Err(format!("cargo build -p hugin: Windows blocked {what}"));
    }
    if !o.passed() {
        let tail: String = o
            .output
            .chars()
            .rev()
            .take(400)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        return Err(format!("cargo build -p hugin failed ({:?}): {}", o.ended, tail.trim()));
    }
    copy_hugin(&target, &ctx.scratch)
}

fn real_run(ctx: &Ctx, opts: &Opts, pin: &str, list: &[Question]) -> i32 {
    let model = opts.model.clone().unwrap_or_default();

    // Started if it is not running, as the apps and the CLI do. Then the model must be one Ollama
    // has: a run against a tag it does not list would spend its hours failing at the first request.
    if let Err(e) = ollama::ensure_running() {
        eprintln!("could not start Ollama: {e}");
        return 1;
    }
    let installed: Vec<String> = match ollama::tags() {
        Ok(models) => models.into_iter().map(|m| m.name).collect(),
        Err(e) => {
            eprintln!("could not ask Ollama what is installed: {e}");
            return 1;
        }
    };
    if let Err(e) = require_installed(&installed, &model) {
        eprintln!("{e}");
        return 1;
    }

    let hugin = match build_hugin(ctx) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let sandbox = match Sandbox::make(&ctx.scratch.join("sandbox"), &model, opts.think) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    // One worktree for every question: they change no file, and it is checked after each answer.
    let tree = match Worktree::add(&ctx.scratch.join("work"), pin, ctx.keep) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };

    println!("J1 — askprobe");
    println!("model:  {model}");
    println!("think:  {}", opts.think.map_or("unset (the model's own)", Think::name));
    println!("pin:    {pin}");
    println!("asks:   {} questions × {} runs", list.len(), opts.runs);
    println!(
        "hugin:  {} (--time {HUGIN_TIME}, killed past {}s)\n",
        hugin.display(),
        HUGIN_GUARD.as_secs()
    );

    let mut results: Vec<RunResult> = Vec::new();
    for q in list {
        for run in 1..=opts.runs {
            let r = run_one(&tree, &hugin, &sandbox, q, run, &model);
            let missing = if r.missing.is_empty() {
                "none".to_string()
            } else {
                r.missing.join("; ")
            };
            println!(
                "{:<6} {} run {}: status {}, {:.0}s, files {}/{}, missing: {missing} — {}",
                r.score.label(),
                r.id,
                r.run,
                r.status,
                r.seconds,
                r.files_hit.len(),
                r.files_expected,
                r.why
            );
            results.push(r);
        }
    }

    println!("\n─── {model} ───");
    for line in summary(&results, list.len(), opts.runs) {
        println!("{line}");
    }
    let out = ctx
        .repo
        .join("evals")
        .join("out")
        .join(format!("J1-ask-{}", unix_now()));
    if std::fs::create_dir_all(&out).is_ok() {
        let file = out.join("results.json");
        let json = results_json(&model, opts.think, pin, &results, list.len(), opts.runs);
        let _ = std::fs::write(&file, serde_json::to_string_pretty(&json).unwrap_or_default());
        println!("\nsaved {}", file.display());
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::sandbox_config;

    fn strings(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    fn repo() -> PathBuf {
        // Cargo runs a package's tests with the package folder as the current directory.
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    /// A question with `n` facts, each present when the answer holds `fact<i>`, and two expected files.
    fn question(n: usize) -> Question {
        let text = format!(
            r#"{{"pin": "0940923", "questions": [{{"id": "t", "question": "q?", "note": "n", "files": ["a/b.rs", "c.rs"], "required": [{}]}}]}}"#,
            (0..n)
                .map(|i| format!(r#"{{"name": "f{i}", "any": ["fact{i}"]}}"#))
                .collect::<Vec<_>>()
                .join(",")
        );
        parse_questions(&text).unwrap().list.remove(0)
    }

    #[test]
    fn every_question_in_the_repository_parses() {
        let qs = load_questions(&repo()).unwrap();
        assert_eq!(qs.pin, "0940923");
        assert_eq!(qs.list.len(), 12);
        for q in &qs.list {
            assert!(
                (2..=4).contains(&q.required.len()),
                "{}: {} facts",
                q.id,
                q.required.len()
            );
            assert!(
                !q.files.is_empty() && q.files.iter().all(|f| !f.contains('\\') && !f.starts_with("./")),
                "{}: files",
                q.id
            );
            assert!(q.selfcheck.is_some(), "{}: no selfcheck answers", q.id);
            assert!(
                q.id.len() > 3 && q.id.as_bytes()[..2].iter().all(u8::is_ascii_digit),
                "{}: id starts with its number",
                q.id
            );
        }
        let ids: Vec<&str> = qs.list.iter().map(|q| q.id.as_str()).collect();
        assert_eq!(ids[0], "01-check-lookup");
        assert_eq!(ids[11], "12-malformed-tool-call");
    }

    #[test]
    fn a_questions_file_that_is_wrong_is_refused() {
        assert!(parse_questions("nope").is_err());
        let one = r#"{"pin": "p", "questions": [{"id": "a", "question": "q", "note": "n", "files": ["f"], "required": [{"name": "x", "any": ["y"]}]}]}"#;
        assert!(parse_questions(one).is_ok());
        assert!(
            parse_questions(&one.replace(r#""pin": "p", "#, ""))
                .unwrap_err()
                .contains("pin")
        );
        assert!(
            parse_questions(&one.replace(r#"["y"]"#, "[]"))
                .unwrap_err()
                .contains("no regex")
        );
        assert!(
            parse_questions(&one.replace(r#"["y"]"#, r#"["(open"]"#))
                .unwrap_err()
                .contains("bad regex")
        );
        assert!(
            parse_questions(&one.replace(r#"["f"]"#, "[]"))
                .unwrap_err()
                .contains("`files` may not be empty")
        );
        let entry =
            r#"{"id": "a", "question": "q", "note": "n", "files": ["f"], "required": [{"name": "x", "any": ["y"]}]}"#;
        let twice = format!(r#"{{"pin": "p", "questions": [{entry}, {entry}]}}"#);
        assert!(parse_questions(&twice).unwrap_err().contains("used twice"));
    }

    #[test]
    fn a_fact_is_present_if_any_regex_matches_and_case_does_not_matter() {
        let qs = parse_questions(
            r#"{"pin": "p", "questions": [{"id": "a", "question": "q", "note": "n", "files": ["f"], "required": [
                {"name": "six", "any": ["\\b6\\s*(KB|KiB|k|kilobytes|\\*\\s*1024)\\b", "\\b6,?144"]}]}]}"#,
        )
        .unwrap();
        let fact = &qs.list[0].required[0];
        for yes in [
            "the first 6 KB",
            "the first 6kb",
            "6 kib",
            "6 kilobytes",
            "6*1024 bytes",
            "6144 bytes",
            "6,144 bytes",
        ] {
            assert!(fact.present(yes), "{yes}");
        }
        for no in ["the first 16 KB", "8 KB", "6 keys", "6 bytes", "", "six kilobytes"] {
            assert!(!fact.present(no), "{no}");
        }
    }

    #[test]
    fn the_shipped_facts_accept_phrasings_and_reject_wrong_values() {
        let qs = load_questions(&repo()).unwrap();
        let fact = |id: &str, n: usize| qs.list.iter().find(|q| q.id == id).unwrap().required[n].clone();
        let head = fact("02-check-output-cap", 0);
        assert!(
            head.present("it keeps the first 6 KB") && head.present("HEAD = 6 * 1024") && head.present("6 KiB of head")
        );
        assert!(!head.present("it keeps the first 8 KB") && !head.present("it keeps the first 16 KB"));
        let process = fact("08-memory-limits", 0);
        assert!(
            process.present("PROCESS_MEMORY_LIMIT is 8 GiB")
                && process.present("8 GB per process")
                && process.present("each process may use 8 gigabytes")
        );
        assert!(
            !process.present("each process may commit 24 GiB, and the whole run 8 GiB together")
                && !process.present("16 GB per process")
        );
        let run = fact("08-memory-limits", 1);
        assert!(
            run.present("RUN_MEMORY_LIMIT: 24 GiB")
                && run.present("24 GB in total")
                && run.present("24 GiB for the whole run")
        );
        assert!(
            !run.present("each process may commit 24 GiB, and the whole run 8 GiB together")
                && !run.present("12 GB in total")
        );
    }

    /// What a person (or a model) writes for a fact, and whether the shipped regexes must accept it:
    /// `(question id, fact index, sentence, accepted)`. From the review of the regexes: the right
    /// phrasings they once refused, and the wrong or vague ones they once let through. A window
    /// between two words runs across a dot (`check.rs`, `e.g.`) but not across a line.
    const SENTENCES: &[(&str, usize, &str, bool)] = &[
        (
            "08-memory-limits",
            0,
            "Each process is limited to 8 GiB and the whole run to 24 GiB.",
            true,
        ),
        ("08-memory-limits", 0, "There is an 8 GiB limit per process.", true),
        (
            "08-memory-limits",
            0,
            "An 8 GiB limit per process, and a 24 GiB limit across all processes.",
            true,
        ),
        ("08-memory-limits", 0, "A process may commit at most 8 GiB.", true),
        ("08-memory-limits", 0, "Every process may use up to 8 GiB.", true),
        ("08-memory-limits", 0, "Processes are limited to 8 GiB each.", true),
        (
            "08-memory-limits",
            0,
            "The cap is 8 GiB, applied to each child process.",
            true,
        ),
        ("08-memory-limits", 0, "PROCESS_MEMORY_LIMIT is 16 GiB", false),
        (
            "08-memory-limits",
            0,
            "each process may commit 24 GiB, and the whole run 8 GiB together",
            false,
        ),
        (
            "08-memory-limits",
            0,
            "The whole run may use 8 GiB, shared by all processes.",
            false,
        ),
        (
            "08-memory-limits",
            0,
            "The processes of a run share 8 GiB in total.",
            false,
        ),
        (
            "08-memory-limits",
            1,
            "A 24 GiB limit applies across all processes in the run.",
            true,
        ),
        ("08-memory-limits", 1, "A 24 GiB limit across all processes.", true),
        ("08-memory-limits", 1, "The job as a whole may use 24 GiB.", true),
        ("08-memory-limits", 1, "24 GiB limit for the whole run", true),
        (
            "08-memory-limits",
            1,
            "RUN_MEMORY_LIMIT (24 * 1024 * 1024 * 1024)",
            true,
        ),
        ("08-memory-limits", 1, "the entire run is capped at 8 GiB", false),
        (
            "08-memory-limits",
            1,
            "each process may commit 24 GiB, and the whole run 8 GiB together",
            false,
        ),
        (
            "02-check-output-cap",
            0,
            "It keeps up to 6 KB of the output's beginning.",
            true,
        ),
        ("02-check-output-cap", 0, "The head is HEAD (6 * 1024 bytes).", true),
        ("02-check-output-cap", 0, "It retains the first 16 KB", false),
        (
            "02-check-output-cap",
            0,
            "It keeps the first part of the output (see check.rs), 6 KB",
            true,
        ),
        ("02-check-output-cap", 1, "The last 2 KiB are kept", true),
        ("02-check-output-cap", 1, "TAIL = 2 * 1024", true),
        (
            "02-check-output-cap",
            2,
            "A marker `[… N bytes cut …]` is inserted",
            true,
        ),
        (
            "02-check-output-cap",
            2,
            "The middle is replaced by a notice saying how many bytes were omitted",
            true,
        ),
        (
            "10-compaction",
            2,
            "It stops when the estimate is within the budget",
            false,
        ),
        ("10-compaction", 2, "The BUDGET is checked on every pass", false),
        ("10-compaction", 2, "BUDGET = 0.75", true),
        ("10-compaction", 2, "until the prompt fits in 75% of num_ctx", true),
        ("10-compaction", 2, "up to three quarters of the context window", true),
        ("10-compaction", 2, "it targets 0.75 of the context", true),
        (
            "10-compaction",
            3,
            "the system prompt and user messages are never touched",
            true,
        ),
        (
            "10-compaction",
            3,
            "Never touched: system prompt, user messages, last message",
            true,
        ),
        (
            "10-compaction",
            3,
            "It never modifies the system prompt, the user's messages or the last message",
            true,
        ),
        (
            "12-malformed-tool-call",
            0,
            "The request is retried with the same parameters",
            false,
        ),
        (
            "12-malformed-tool-call",
            0,
            "It asks again through the raw /api/generate endpoint",
            true,
        ),
        (
            "12-malformed-tool-call",
            2,
            "it returns an invalid_tool_call result to the model",
            true,
        ),
        (
            "12-malformed-tool-call",
            2,
            "the error message is sent back to the model so it can retry",
            true,
        ),
        ("05-undo-backups", 0, r"Backups go in %APPDATA%\bifrost\backups", true),
        (
            "05-undo-backups",
            0,
            "They are stored in the backups directory under the data directory",
            true,
        ),
        (
            "05-undo-backups",
            0,
            "Backups are in a folder called backups next to the config",
            false,
        ),
        (
            "04-claude-check-scope",
            3,
            "The prompt is piped to claude's standard input",
            true,
        ),
        ("04-claude-check-scope", 3, "the prompt is passed via stdin", true),
        ("04-claude-check-scope", 2, "It runs in a temporary directory", true),
        ("04-claude-check-scope", 2, "Run in an empty temp folder", true),
        (
            "07-unattended-policy",
            0,
            "The policy is unattended::decide in agent/unattended.rs",
            true,
        ),
        ("07-unattended-policy", 0, "decide() in unattended.rs", true),
        ("07-unattended-policy", 0, "Hugin runs unattended, asking nobody", false),
        (
            "07-unattended-policy",
            3,
            "Shell commands such as run_command are always denied.",
            true,
        ),
        ("07-unattended-policy", 3, "run_command is denied", true),
        (
            "07-unattended-policy",
            3,
            "Denied: anything else, e.g. a shell command",
            true,
        ),
        ("06-scratch-copy", 0, "It makes a shallow clone", true),
        (
            "06-scratch-copy",
            0,
            "It runs git clone --depth 1 on a file:// URL",
            true,
        ),
        ("06-scratch-copy", 0, "It clones the repository at HEAD", true),
        (
            "06-scratch-copy",
            0,
            "Clones are slow, so it copies the folder with robocopy",
            false,
        ),
        (
            "06-scratch-copy",
            2,
            "Because worktrees register themselves inside the real repository's .git folder",
            true,
        ),
        (
            "06-scratch-copy",
            2,
            "A worktree would add metadata to the user's repository",
            false,
        ),
        (
            "11-coding-model-offer",
            2,
            "the coding model has to be pulled in Ollama",
            true,
        ),
        ("11-coding-model-offer", 2, "offer_for checks tags available", false),
        (
            "11-coding-model-offer",
            3,
            "it offers going back to the previous model",
            true,
        ),
        (
            "11-coding-model-offer",
            3,
            "switches to the model that was in use earlier",
            true,
        ),
        ("11-coding-model-offer", 3, "switches to the model in use earlier", true),
        ("03-tray-menu", 2, "Open Hugin and Open Mimir are present", true),
        ("03-tray-menu", 2, "Open Vidar, Open Mimir, Open Hugin", true),
        ("03-tray-menu", 3, "Launch at login", true),
        (
            "09-think-setting",
            2,
            "it only forwards think if /api/show lists thinking among the model's capabilities",
            true,
        ),
        (
            "09-think-setting",
            2,
            r#"If the model's capabilities contain "thinking" the value is kept"#,
            true,
        ),
        (
            "09-think-setting",
            2,
            "only when detail.supports_thinking is true",
            false,
        ),
        (
            "09-think-setting",
            3,
            "chat.rs builds the body: think false for Off",
            true,
        ),
        (
            "09-think-setting",
            3,
            r#"the request body gets "think": false when off"#,
            true,
        ),
        (
            "09-think-setting",
            3,
            "the request JSON includes the think field",
            false,
        ),
        ("01-check-lookup", 1, "It is keyed by the folder path", true),
        ("01-check-lookup", 1, "stored by workspace directory", false),
        ("01-check-lookup", 1, "The map is keyed on the project's root", true),
        (
            "01-check-lookup",
            1,
            "The `checks` map is keyed by the workspace root_display()",
            true,
        ),
        ("01-check-lookup", 2, "The key comparison is not case sensitive", true),
        ("01-check-lookup", 2, "case-insensitively matched", true),
        ("01-check-lookup", 2, "compared with eq_ignore_ascii_case", true),
        ("01-check-lookup", 2, "The key is compared as it is written", false),
    ];

    /// **The regexes keep telling right from wrong:** every sentence of [`SENTENCES`].
    #[test]
    fn the_shipped_facts_take_the_right_sentences_and_refuse_the_wrong_ones() {
        let qs = load_questions(&repo()).unwrap();
        let mut bad = Vec::new();
        for (id, n, text, accepted) in SENTENCES {
            let q = qs
                .list
                .iter()
                .find(|q| q.id == *id)
                .unwrap_or_else(|| panic!("no question {id}"));
            if q.required[*n].present(text) != *accepted {
                bad.push(format!(
                    "{id} fact {n} ({}) should be {}: {text}",
                    q.required[*n].name,
                    if *accepted { "accepted" } else { "refused" }
                ));
            }
        }
        assert!(
            bad.is_empty(),
            "{} sentence(s) read wrongly:\n{}",
            bad.len(),
            bad.join("\n")
        );
    }

    #[test]
    fn right_partly_and_wrong() {
        let q = question(4);
        let want =
            |answer: &str, looked: &[&str], status: &str| score_answer(&q, answer, &strings(looked), status).score;
        // Every fact is right, with or without a file.
        assert_eq!(want("fact0 fact1 fact2 fact3", &[], "answered"), Score::Right);
        // At least half is partly (2 of 4 is exactly half).
        assert_eq!(want("fact0 fact3", &[], "answered"), Score::Partly);
        assert_eq!(want("fact0 fact1 fact2", &[], "answered"), Score::Partly);
        // Short of half: wrong, unless an expected file was read.
        assert_eq!(want("fact2", &[], "answered"), Score::Wrong);
        assert_eq!(want("nothing", &["README.md"], "answered"), Score::Wrong);
        assert_eq!(want("fact2", &["c.rs"], "answered"), Score::Partly);
        assert_eq!(want("nothing", &["a/b.rs"], "answered"), Score::Partly);
        // Three facts: half is 1.5, so one fact is not half.
        let q3 = question(3);
        assert_eq!(score_answer(&q3, "fact0", &[], "answered").score, Score::Wrong);
        assert_eq!(score_answer(&q3, "fact0 fact1", &[], "answered").score, Score::Partly);
    }

    #[test]
    fn a_partial_or_error_status_caps_the_score() {
        let q = question(2);
        let all = "fact0 fact1";
        let s = score_answer(&q, all, &[], "partial");
        assert_eq!(s.score, Score::Partly);
        assert!(
            s.why.contains("capped at partly") && s.why.contains("partial"),
            "{}",
            s.why
        );
        let s = score_answer(&q, all, &strings(&["a/b.rs"]), "error");
        assert_eq!(s.score, Score::Wrong);
        assert!(s.why.contains("capped at wrong"), "{}", s.why);
        assert_eq!(score_answer(&q, all, &[], "-").score, Score::Wrong);
        // A cap never raises a score.
        assert_eq!(score_answer(&q, "nothing", &[], "partial").score, Score::Wrong);
        assert_eq!(score_answer(&q, "fact0", &[], "partial").score, Score::Partly);
        assert!(!score_answer(&q, "fact0", &[], "partial").why.contains("capped"));
        // Any other status is not a cap.
        assert_eq!(score_answer(&q, all, &[], "answered").score, Score::Right);
    }

    #[test]
    fn what_was_missing_and_which_files_were_hit_are_recorded() {
        let q = question(3);
        let s = score_answer(&q, "fact1", &strings(&["c.rs", "other.rs"]), "answered");
        assert_eq!(s.missing, ["f0", "f2"]);
        assert_eq!(s.files_hit, ["c.rs"]);
    }

    #[test]
    fn looked_at_paths_are_normalised() {
        assert_eq!(normalize_path(r"a\b.rs"), "a/b.rs");
        assert_eq!(normalize_path("./a/b.rs"), "a/b.rs");
        assert_eq!(normalize_path(r".\a\b.rs"), "a/b.rs");
        assert_eq!(normalize_path("././a.rs"), "a.rs");
        assert_eq!(normalize_path("  a/b.rs  "), "a/b.rs");
        assert_eq!(normalize_path("a/b.rs"), "a/b.rs");
        let q = question(2);
        for looked in [r"a\b.rs", "./a/b.rs", r".\a\b.rs", "A/B.RS"] {
            assert_eq!(
                score_answer(&q, "nothing", &strings(&[looked]), "answered").files_hit,
                ["a/b.rs"],
                "{looked}"
            );
        }
        assert!(
            score_answer(&q, "nothing", &strings(&["xa/b.rs", "a/b.rs.bak"]), "answered")
                .files_hit
                .is_empty()
        );
    }

    #[test]
    fn hugins_report_is_found_among_other_output() {
        let json = r#"{"status":"answered","answer":"It is in a.rs.","looked_at":[{"path":"a/b.rs","lines":"1-9"},{"path":"c.rs","lines":null}],"seconds":12.5,"model":"m","folder":"f","error":null}"#;
        let r = parse_report(&format!("noise\n{json}\n")).unwrap();
        assert_eq!(
            (r.status.as_str(), r.answer.as_str(), r.seconds),
            ("answered", "It is in a.rs.", 12.5)
        );
        assert_eq!(r.looked_at, ["a/b.rs", "c.rs"]);
        assert_eq!(r.error, None);
        let err = parse_report(r#"{"status":"error","answer":"","looked_at":[],"seconds":1.0,"model":"m","folder":"f","error":"no such folder"}"#).unwrap();
        assert_eq!(
            (err.status.as_str(), err.error.as_deref()),
            ("error", Some("no such folder"))
        );
        assert!(parse_report("no json here\n{not json}\n").is_err());
        assert!(parse_report(r#"{"answer":"no status"}"#).is_err());
    }

    fn args(words: &[&str]) -> Result<Cmd, String> {
        parse_args(&strings(words))
    }

    #[test]
    fn flags_parse() {
        let Ok(Cmd::Run(o)) = args(&[
            "--model", "m:1", "--runs", "3", "--only", "07", "--think", "HIGH", "--keep",
        ]) else {
            panic!()
        };
        assert_eq!(
            (
                o.model.as_deref(),
                o.runs,
                o.only.as_deref(),
                o.think,
                o.keep,
                o.selfcheck
            ),
            (Some("m:1"), 3, Some("07"), Some(Think::High), true, false)
        );
        let Ok(Cmd::Run(o)) = args(&["--model", "m"]) else {
            panic!()
        };
        assert_eq!(
            (o.runs, o.think, o.keep),
            (2, None, false),
            "two runs unless told otherwise"
        );
        let Ok(Cmd::Run(o)) = args(&["--selfcheck"]) else {
            panic!()
        };
        assert!(o.selfcheck && o.model.is_none());
        assert_eq!(args(&["--help"]), Ok(Cmd::Help));
        assert_eq!(args(&["-h"]), Ok(Cmd::Help));
        // A value is the next word, even one that starts with a dash.
        let Ok(Cmd::Run(o)) = args(&["--model", "m", "--only", "-x"]) else {
            panic!()
        };
        assert_eq!(o.only.as_deref(), Some("-x"));
    }

    #[test]
    fn a_missing_model_and_a_bad_flag_are_refused() {
        assert!(args(&[]).unwrap_err().contains("--model"));
        assert!(args(&["--runs", "2"]).unwrap_err().contains("--model"));
        assert!(args(&["--model"]).unwrap_err().contains("needs a model tag"));
        assert_eq!(args(&["--model", "m", "--bogus"]).unwrap_err(), "unknown flag: --bogus");
        assert_eq!(args(&["-x"]).unwrap_err(), "unknown flag: -x");
        assert!(
            args(&["--model", "m", "stray"])
                .unwrap_err()
                .contains("unexpected argument: stray")
        );
        assert!(args(&["--model", "m", "--runs", "0"]).is_err());
        assert!(args(&["--model", "m", "--runs", "many"]).is_err());
        assert!(
            args(&["--model", "m", "--think", "max"])
                .unwrap_err()
                .contains("--think takes")
        );
        assert!(args(&["--model", " "]).is_err());
    }

    #[test]
    fn a_model_ollama_does_not_list_is_refused() {
        let installed = strings(&["qwen3-coder:30b", "Gemma3:latest"]);
        assert!(
            require_installed(&installed, NO_SUCH_MODEL)
                .unwrap_err()
                .contains("not installed")
        );
        assert!(require_installed(&installed, "QWEN3-coder:30B").is_ok());
        assert!(require_installed(&installed, "gemma3").is_ok());
        assert!(require_installed(&installed, "qwen3-coder").is_err());
        let high: Value = serde_json::from_str(&sandbox_config("m:1", Some(Think::High))).unwrap();
        assert_eq!(
            (
                high["model_settings"]["m:1"]["think"].as_str(),
                high["coding_model"].as_str()
            ),
            (Some("high"), Some("m:1"))
        );
    }

    #[test]
    fn the_summary_counts_each_score_and_the_results_name_what_was_missed() {
        let run = |score, seconds, status: &str| RunResult {
            id: "q".into(),
            run: 1,
            score,
            missing: vec!["a fact".into()],
            files_hit: vec!["a.rs".into()],
            files_expected: 2,
            seconds,
            status: status.into(),
            why: String::new(),
            answer: "x".into(),
            reset: false,
        };
        let results = [
            run(Score::Right, 10.0, "answered"),
            run(Score::Partly, 20.0, "partial"),
            run(Score::Wrong, 30.0, "-"),
            run(Score::Wrong, 0.0, "error"),
        ];
        let text = summary(&results, 2, 2).join("\n");
        assert!(
            text.contains("| RIGHT | 1 |")
                && text.contains("| PARTLY | 1 |")
                && text.contains("| WRONG | 2 |")
                && text.contains("| total | 4 |")
        );
        assert!(text.contains("right 1 / partly 1 / wrong 2 out of 4 answers (2 questions × 2 runs)"));
        assert!(text.contains("mean time: 15.0s") && text.contains("1 answer(s) had no Hugin report"));
        let json = results_json("m", Some(Think::Low), "0940923", &results, 2, 2);
        assert_eq!(
            (
                json["summary"]["right"].as_u64(),
                json["summary"]["mean_seconds"].as_f64(),
                json["think"].as_str()
            ),
            (Some(1), Some(15.0), Some("low"))
        );
        let first = &json["answers"][0];
        assert_eq!(
            (
                first["score"].as_str(),
                first["files_expected"].as_u64(),
                first["missing"][0].as_str()
            ),
            (Some("right"), Some(2), Some("a fact"))
        );
        assert_eq!(json["answers"][2]["status"], "-");
    }
}
