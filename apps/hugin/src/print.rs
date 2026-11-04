//! `hugin -p`: hand Hugin one task and get a proposal back (I5).
//!
//! Claude Code runs `hugin -p "task" --check "cargo test -p x"`. Hugin does the work on the
//! local model, which costs nothing from the Claude plan, and hands back its answer, a patch
//! and the result of the check. Claude reviews the patch and applies it.
//!
//! **Nothing is written to the user's folder.** The turn runs in a scratch copy of it
//! ([`bifrost_core::scratch`]): HEAD plus the uncommitted files, in a folder of its own under
//! `%TEMP%`. Nobody is there to approve anything, so [`unattended::decide`] answers each card:
//! edits to the copy and the check are allowed, and nothing else is — Hugin offers no shell
//! command here. The check builds into a cache of its own, never the user's `target\`.
//!
//! **The check runs code Hugin wrote, before anyone has read it**, with the user's rights, in
//! the copy. That is the point of a check, and the risk: it is run unattended only when the
//! turn changed no file that makes a check run something (`Cargo.toml`, a `conftest.py`, a
//! makefile…; [`unattended::runs_code`]), and `--check` should be named only for a repository
//! the user trusts. A program named with a folder in it is the copy's, never the user's.
//!
//! **A failed check is not "done".** A model that ran the check, saw it fail and says the work is
//! done is pushed back to fix it (a few times, [`bifrost_core::agent::turn::MAX_PUSH_BACKS`]), and
//! whatever it says in the end, the report says how the run came out: [`Status`] `done`, `failed`
//! (the final check failed — no command to apply, only a draft patch to read), `review` (the
//! patch changes existing tests — likewise), `partial` (cut short, or a check that gave no
//! verdict) or `error` (no proposal). **The exit code is 0 for done, 3 for failed, review or
//! partial — not done, so the patch is not to be applied as it is — 1 when there is no proposal,
//! and 2 for a command line that could not be read.**
//!
//! **A passing check can still be wrong.** A model can make a check pass by changing the test that
//! failed. A patch that changes existing tests ([`bent`]) is put to the model once, in the
//! conversation it is in, while a sixth of the time is left. **If it still changes them, the run is
//! not done**: the status is `review` until someone confirms the change was asked for (unless the
//! check failed, which is the status then), and the files are in the report's `warnings`. A
//! `cargo test` that exits 0 with a test program cut short has not passed either
//! ([`bifrost_core::check::Outcome::passed`]).
//!
//! **It ends on time.** `--time` (nine minutes by default, under Claude Code's ten) is the
//! whole run, counted from the start of the process, and every git command is given no more
//! than what is left of it. The turn runs on a thread of its own, because a request to the
//! model cannot be cut short; at its deadline the main thread stops the turn, takes the lock
//! every edit takes so none lands afterwards, and goes on with what the copy holds. The last
//! stretch is kept back for the final check.
//!
//! **The report is printed before the copy is cleaned up**, so a run killed while it tidies
//! has already said what it did ([`Finished`]).
//!
//! - [`execute`] — the whole run, with the model and the runner passed in so a test can
//!   script both; [`run`] is the real one
//! - [`Report`] — what a run came to, and how it reads as text and as JSON
//! - [`parse_check`], [`parse_time`] — the two flags that take more than a word
//!
//! **Not saved**: no conversation goes to the store and `config.json` is never written.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use bifrost_core::agent::env::Env;
use bifrost_core::agent::round::{Round, round_in};
use bifrost_core::agent::run::{Ran, edits_lock, run_approved_in};
use bifrost_core::agent::send::Calibration;
use bifrost_core::agent::ticket::PendingCall;
use bifrost_core::agent::turn::{Card, Decision, Frontend, Patch, RoundNumbers, run_turn_with};
use bifrost_core::agent::unattended;
use bifrost_core::chat::Message;
use bifrost_core::check::{self, Ended};
use bifrost_core::config;
use bifrost_core::edit::{self, Workspace};
use bifrost_core::scratch::{self, Git, Lock, Scratch};
use serde::Serialize;

use crate::bent::{self, Bent};
use crate::session::HUGIN_IDENTITY;
use crate::start;
use crate::stop::Stopper;
use crate::trace::{self, Trace};

/// How long the whole run may take unless `--time` says: nine minutes, under the ten Claude
/// Code gives a command.
pub const DEFAULT_TIME: u64 = 540;

/// The most diff text printed. Claude Code reads what a command prints, so a patch past this is
/// not printed at all: the files and their counts are, and the patch file, which has all of it.
const MAX_SHOWN_DIFF: usize = 16 * 1024;

/// The most characters of a task echoed back.
const MAX_SHOWN_TASK: usize = 1000;

/// The most of a failed check's output that is kept.
const OUTPUT_TAIL: usize = 4000;

/// A patch that changes existing tests is put to the model once more, but only while a sixth of the
/// run's time is left for it to answer: a follow-up cut short by the deadline costs more than the
/// warning it was meant to spare.
const FOLLOW_UP_SHARE: u32 = 6;

/// The most rounds of that follow-up.
const FOLLOW_UP_ROUNDS: usize = 10;

// ── what was asked ──────────────────────────────────────────────────────────

/// Where the task comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Task {
    /// Given on the command line.
    Text(String),
    /// `-p -`: read from standard input, for a task too long for a command line.
    Stdin,
}

/// What `-p` was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    pub task: Task,
    /// `--check`: the one program that finds out whether the work is right, for this run only.
    /// Without it the folder's check in Mimir's settings is used, and without that, none.
    pub check: Option<Vec<String>>,
    /// `--json`: the result as one JSON object, for a program to read.
    pub json: bool,
    pub folder: PathBuf,
    /// `--time`: how long the whole run may take.
    pub time: Duration,
    /// `--model`: the model for this run only, instead of the config's coding model.
    pub model: Option<String>,
    /// `--trace`: a file to append a line to for every request sent to the model.
    pub trace: Option<PathBuf>,
}

/// `--check`'s text as the program and arguments it runs. **It is run directly, not by a
/// shell**, so quotes group words and nothing else is special — which is why a pipe, `&&`, a
/// redirect or a variable is refused rather than passed on as an argument that would mean
/// nothing. `npm` is spelled `npm.cmd`, which is what Windows runs.
pub fn parse_check(text: &str) -> Result<Vec<String>, String> {
    let mut words: Vec<String> = Vec::new();
    let (mut word, mut started, mut quote) = (String::new(), false, None::<char>);
    for c in text.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => word.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                started = true;
            }
            None if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            None if matches!(c, '|' | '&' | ';' | '<' | '>' | '`' | '$' | '%') => {
                return Err(format!(
                    "--check runs one program and its arguments, not a shell command line: `{c}` is not allowed outside quotes"
                ));
            }
            None => {
                word.push(c);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return Err("--check has a quote that is never closed".to_string());
    }
    if started {
        words.push(word);
    }
    if words.is_empty() {
        return Err("--check needs a command".to_string());
    }
    if words[0].eq_ignore_ascii_case("npm") {
        words[0] = "npm.cmd".to_string();
    }
    Ok(words)
}

/// `--time`'s seconds, or [`DEFAULT_TIME`] when it is not given.
pub fn parse_time(text: Option<&str>) -> Result<Duration, String> {
    parse_time_or(text, DEFAULT_TIME)
}

/// [`parse_time`] with another default, in seconds: `--ask` gives itself five minutes.
pub fn parse_time_or(text: Option<&str>, default: u64) -> Result<Duration, String> {
    let Some(text) = text else {
        return Ok(Duration::from_secs(default));
    };
    match text.trim().parse::<u64>() {
        Ok(secs) if secs > 0 => Ok(Duration::from_secs(secs)),
        _ => Err(format!("--time takes a whole number of seconds, 1 or more, not {text}")),
    }
}

/// `path` with `.` and `..` folded in by text alone, touching no filesystem.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir if matches!(out.components().next_back(), Some(Component::Normal(_))) => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Whether `path` is `root` or something under it, by text and without regard to case, as
/// Windows reads paths.
fn inside(path: &Path, root: &Path) -> bool {
    let plain = |p: &Path| {
        p.to_string_lossy()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
    };
    let (path, root) = (plain(path), plain(root));
    path == root || path.starts_with(&format!("{root}\\"))
}

/// `path` as the file system really has it, with no `\\?\` prefix and no short name; as it is if
/// it cannot be found.
fn real_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path)
        .map(|real| PathBuf::from(edit::plain(&real)))
        .unwrap_or_else(|_| path.to_path_buf())
}

/// The check as it runs in the copy. **A program named with a folder in it is the copy's**:
/// `.\scripts\verify.cmd` is a path from the folder the check runs in, but the system resolves a
/// program's path from the directory `hugin` was started in, which is the user's, so the check
/// would run — and write to — the user's own file. So a relative path is joined to the folder in
/// the copy, and must stay inside the copy; an absolute path is left as it is unless it points
/// into the user's repository, which is refused, since the copy has its own version of that
/// file. A bare name is looked up on `PATH`, as ever.
pub fn resolve_check(
    argv: &[String],
    folder: &Path,
    copy_root: &Path,
    repo_root: &Path,
) -> Result<Vec<String>, String> {
    let Some(program) = argv.first() else {
        return Ok(argv.to_vec());
    };
    let given = Path::new(program);
    let has_folder = program.contains(['/', '\\']) || program.as_bytes().get(1) == Some(&b':');
    if !has_folder {
        return Ok(argv.to_vec());
    }
    let use_instead = "name it relative to the folder (such as .\\verify.cmd), which runs the copy's version, \
                       or use a program on PATH";
    let resolved = if given.is_absolute() {
        let at = lexical(given);
        if inside(&at, repo_root) {
            return Err(format!(
                "--check names {program}, a file in your repository ({}); hugin runs the copy's, not yours: {use_instead}",
                repo_root.display()
            ));
        }
        return Ok(argv.to_vec());
    } else if given.has_root() || matches!(given.components().next(), Some(Component::Prefix(_))) {
        return Err(format!(
            "--check names {program} by a drive or a root alone; {use_instead}"
        ));
    } else {
        lexical(&folder.join(given))
    };
    if !inside(&resolved, copy_root) {
        return Err(format!(
            "--check names {program}, which leads out of the folder; {use_instead}"
        ));
    }
    let mut out = argv.to_vec();
    out[0] = resolved.to_string_lossy().into_owned();
    Ok(out)
}

// ── the time a run has ──────────────────────────────────────────────────────

/// How a run's time is shared out. The turn gets all but the last stretch, which the final
/// check and the patch need; the stretches shrink with a short `--time`, so that `--time 30`
/// still means thirty seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Budget {
    pub total: Duration,
    /// Kept back for the final check and what follows it: a quarter of the time, at most 90 s.
    pub reserve: Duration,
    /// How long a stopped turn is given to wind down before the run goes on without it.
    pub grace: Duration,
    /// Kept back from the final check for the patch and the output.
    pub tail: Duration,
}

impl Budget {
    pub fn of(total: Duration) -> Self {
        let reserve = (total / 4).min(Duration::from_secs(90));
        Self {
            total,
            reserve,
            grace: (reserve / 2).min(Duration::from_secs(5)),
            tail: (total / 10).min(Duration::from_secs(10)),
        }
    }
}

// ── the model and the runner ────────────────────────────────────────────────

/// One request to the model: the conversation, what the prompt is calibrated from, and
/// whether it is the closing request. [`round_in`] is the real one.
pub type Ask = dyn FnMut(&[Message], Option<Calibration>, bool) -> Result<Round, String> + Send;

/// The running of an approved call: its name, its path and its ticket. [`run_approved_in`]
/// is the real one.
pub type Run = dyn FnMut(&str, &str, &str) -> Result<Ran, String> + Send;

/// The model and the runner, which move to the thread the turn runs on.
pub struct Wired {
    pub ask: Box<Ask>,
    pub run: Box<Run>,
}

/// What the turn is set up with, for whatever wires the model and the runner.
pub struct Ctx {
    /// The folder the model works in: the user's folder, in the copy.
    pub root: String,
    pub env: Env,
    /// The check as it runs in the copy, if there is one.
    pub check: Option<Vec<String>>,
}

/// The real model and runner: the app's round and the app's run, in this setting. The config
/// is loaded fresh each round, as Hugin's session does, and set in memory only — the folder,
/// its check and its notes — and **never saved**; every call that needs no card runs without one.
fn real(model: &str, ctx: &Ctx) -> Wired {
    let (asked, root, check, env) = (model.to_string(), ctx.root.clone(), ctx.check.clone(), ctx.env.clone());
    let run_env = ctx.env.clone();
    let ask = move |messages: &[Message], calibration: Option<Calibration>, closing: bool| {
        let mut cfg = config::load();
        cfg.workspace = Some(root.clone());
        cfg.set_instructions(&root, true);
        cfg.set_check(&root, check.clone());
        cfg.ask_every_call = false;
        round_in(messages, &asked, &cfg, &env, calibration, closing)
    };
    let run = move |name: &str, path: &str, token: &str| run_approved_in(name, path, token, &run_env);
    Wired {
        ask: Box::new(ask),
        run: Box::new(run),
    }
}

// ── what the turn leaves ────────────────────────────────────────────────────

/// What the turn has done, kept where the main thread can read it however the turn ends.
#[derive(Default)]
struct Progress {
    /// The model's last words.
    answer: Option<String>,
    /// What the loop said that a person would have been told: a bound reached, a call it gave up on.
    notes: Vec<String>,
    /// How many requests to the model came back.
    rounds: usize,
    /// The undo handle of every change the model made, oldest first.
    undo: Vec<String>,
    /// The check the model last ran, and whether anything was written since.
    last_check: Option<check::Outcome>,
    edited_since: bool,
}

type Shared = Arc<Mutex<Progress>>;

/// A panic elsewhere must not hide what the turn had done.
fn held(shared: &Shared) -> std::sync::MutexGuard<'_, Progress> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

/// The turn's frontend with nobody there: [`unattended::decide`] answers every card, and the
/// loop's words are kept for the report.
struct Unattended {
    progress: Shared,
    stopper: Stopper,
    /// Each card shown: the path it names, and where a move goes.
    shown: Vec<(String, Option<String>)>,
    /// The files the turn has changed so far, as the cards named them: what the rule is told.
    edited: Vec<String>,
    /// What the next round is calibrated from: the last round that was measured, kept for a
    /// follow-up in the same conversation.
    calibration: Option<Calibration>,
}

impl Frontend for Unattended {
    fn stopped(&self) -> bool {
        self.stopper.stopped()
    }

    fn status(&mut self, _text: &str) {}

    fn round(&mut self, numbers: RoundNumbers) {
        held(&self.progress).rounds += 1;
        if numbers.prompt_eval_count > 0 {
            self.calibration = Some(Calibration::new(numbers.prompt_chars, numbers.prompt_eval_count));
        }
    }

    fn answer(&mut self, content: &str, _prompt_eval_count: u32, _eval_count: u32) {
        if !content.trim().is_empty() {
            held(&self.progress).answer = Some(content.trim().to_string());
        }
    }

    fn error(&mut self, message: &str) {
        held(&self.progress).notes.push(message.to_string());
    }

    fn card(&mut self, card: Card) -> usize {
        self.shown.push((card.path, card.to));
        self.shown.len() - 1
    }

    /// A change that was written has an undo handle: its file is one the turn has changed.
    fn update(&mut self, card: usize, patch: Patch) {
        if patch.undo.is_some()
            && let Some((path, to)) = self.shown.get(card)
        {
            self.edited.push(path.clone());
            self.edited.extend(to.clone());
        }
    }

    fn decide(&mut self, _card: usize, call: &PendingCall) -> Decision {
        unattended::decide(call, &self.edited)
    }

    fn save(&mut self) {}

    /// **A model that says it is done after its check failed is not believed**: the loop pushes
    /// it back to fix the code (a few times), and the report says what the check came to however
    /// the turn ends.
    fn insist_on_checks(&self) -> bool {
        true
    }
}

// ── a run ───────────────────────────────────────────────────────────────────

/// Everything a run is given.
pub struct Job {
    pub task: String,
    pub folder: PathBuf,
    /// The check to run, from `--check` or from Mimir's setting for the folder.
    pub check: Option<Vec<String>>,
    pub time: Duration,
    /// When the process began, which is when the time starts running.
    pub started: Instant,
    /// Where the run's folder, the kept patches and the lock are: `%TEMP%\hugin-p`.
    pub work: PathBuf,
    /// Where the check builds: a folder for each repository, so the next run starts warm.
    pub cache: PathBuf,
}

/// Where the check builds, under `%LOCALAPPDATA%\bifrost\hugin-p`.
fn cache_dir() -> PathBuf {
    match std::env::var_os("LOCALAPPDATA") {
        Some(local) => PathBuf::from(local).join("bifrost").join("hugin-p"),
        None => scratch::work_dir().join("cache"),
    }
}

/// One file the patch touches, and what made the change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub from: Option<String>,
    pub status: &'static str,
    /// `edit` when the model's own edit tools made it, `other` when something else did: the
    /// check writing a lockfile, say.
    pub by: &'static str,
    /// The lines it gained and lost; `None` for a file that is not text.
    pub added: Option<u64>,
    pub deleted: Option<u64>,
}

/// What the final check came to.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckReport {
    pub command: String,
    /// `None` when the check did not run.
    pub passed: Option<bool>,
    pub exit_code: Option<i32>,
    pub seconds: f64,
    pub timed_out: bool,
    /// What Smart App Control refused, when it stopped the check: a problem with Windows, not the code.
    pub blocked: Option<String>,
    /// The model's own last run, because nothing was edited after it.
    pub reused: bool,
    /// It timed out in the time the run had left, which was less than a check is usually given:
    /// the run's limit stopped it, not the check's own.
    pub cut_short: bool,
    /// The end of the output of a check that finished and failed.
    pub output_tail: String,
    /// Why a check that was asked for did not run.
    pub not_run: Option<String>,
}

/// How a run came out, which is the first thing the report says. **Only `Done` is done**: any
/// other status means the patch is not to be applied as it is — see [`Report::exit_code`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// The turn ended by itself, and the check passed — or none was asked for or set.
    Done,
    /// The check ran and failed. The model's answer may say the work is done; it is not.
    Failed,
    /// The check did not fail, but **the patch changes existing tests** even after the model was
    /// asked to put them back: a test may have been bent to fit the code. Not done until someone
    /// confirms the change was asked for.
    Review,
    /// Cut short, or not verified: stopped by the time limit or a bound, ended by an error after
    /// some work, no answer — or a check that gave no verdict (it timed out, Windows blocked it,
    /// or it was not run), so nothing says the work is right.
    Partial,
    /// There is no proposal at all: the run could not start, or could not make its patch.
    Error,
}

/// What a run came to.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub task: String,
    /// The user's folder.
    pub folder: String,
    /// The top of the repository, where the patch applies.
    pub repo_root: String,
    /// The commit the copy started from.
    pub base: String,
    pub model: String,
    pub answer: String,
    /// The patch, byte for byte; empty when nothing changed.
    pub patch: Vec<u8>,
    pub patch_file: Option<PathBuf>,
    pub files: Vec<FileChange>,
    pub check: Option<CheckReport>,
    /// What was not carried into the copy, each with why.
    pub skipped: Vec<String>,
    /// The turn was stopped by the time limit.
    pub stopped: bool,
    /// What the loop said that a person would have been told.
    pub notes: Vec<String>,
    pub seconds: f64,
    pub time: u64,
    pub error: Option<String>,
    /// There is no proposal at all: the run could not start, or could not make its patch.
    pub errored: bool,
    /// The existing tests the patch changes, file by file: a test may have been bent to fit the
    /// code. If there are any the run is not done — see [`Status::Review`].
    pub bent: Vec<Bent>,
}

/// The report as `--json` writes it. **The order of the fields is the point**: what Claude
/// needs first comes first — how it came out, what to be careful of, whether it is whole, the
/// check, the files, how to apply — then the answer, and the patch text, which can be long, last.
#[derive(Serialize)]
struct JsonReport<'a> {
    status: Status,
    warnings: Vec<String>,
    error: Option<&'a str>,
    partial: bool,
    stopped: bool,
    check: Option<JsonCheck<'a>>,
    files: Vec<JsonFile<'a>>,
    apply: Option<String>,
    patch_file: Option<String>,
    skipped: &'a [String],
    notes: &'a [String],
    answer: &'a str,
    seconds: f64,
    model: &'a str,
    folder: &'a str,
    repo_root: &'a str,
    base: &'a str,
    task: String,
    diff_lossy: bool,
    diff_omitted: bool,
    diff: String,
}

#[derive(Serialize)]
struct JsonCheck<'a> {
    command: &'a str,
    passed: Option<bool>,
    exit_code: Option<i32>,
    timed_out: bool,
    blocked: bool,
    reused: bool,
    cut_short: bool,
    not_run: Option<&'a str>,
    seconds: f64,
    output_tail: &'a str,
}

#[derive(Serialize)]
struct JsonFile<'a> {
    path: &'a str,
    status: &'static str,
    by: &'static str,
    added: Option<u64>,
    deleted: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    from: Option<&'a str>,
}

impl Report {
    /// The final check ran, finished, and did not pass. A check that timed out, was blocked by
    /// Windows or was not run gave no verdict on the work — see [`Report::unverified`].
    fn check_failed(&self) -> bool {
        self.check
            .as_ref()
            .is_some_and(|c| c.passed == Some(false) && c.not_run.is_none() && !c.timed_out && c.blocked.is_none())
    }

    /// A check was asked for and gave no verdict: it was not run, ran out of time, or was blocked
    /// by Windows. Nothing then says the work is right.
    fn unverified(&self) -> bool {
        self.check
            .as_ref()
            .is_some_and(|c| c.not_run.is_some() || c.timed_out || c.blocked.is_some())
    }

    /// Short of a whole answer: stopped, ended by an error or a bound, no answer given, or a
    /// check that gave no verdict.
    pub fn partial(&self) -> bool {
        self.stopped || self.error.is_some() || !self.notes.is_empty() || self.answer.is_empty() || self.unverified()
    }

    /// How the run came out: no proposal at all, a check that failed (**whatever the answer
    /// says**), a patch that changes existing tests (**whatever the check says**, unless it
    /// failed), cut short or unverified, or done. The first of those that holds.
    pub fn status(&self) -> Status {
        if self.errored {
            Status::Error
        } else if self.check_failed() {
            Status::Failed
        } else if !self.bent.is_empty() {
            Status::Review
        } else if self.partial() {
            Status::Partial
        } else {
            Status::Done
        }
    }

    /// The final check ran, and passed.
    fn check_passed(&self) -> bool {
        self.check.as_ref().is_some_and(|c| c.passed == Some(true))
    }

    /// **0 when done; 3 when not (the check failed, the patch changes existing tests, or the run
    /// was cut short or unverified): the patch is not to be applied as it is; 1 when there is no
    /// proposal at all.** (2, a command line that could not be read, never gets as far as a report.)
    pub fn exit_code(&self) -> u8 {
        match self.status() {
            Status::Done => 0,
            Status::Failed | Status::Review | Status::Partial => 3,
            Status::Error => 1,
        }
    }

    /// The command that applies the patch, as a person would type it. **None for a patch whose
    /// check failed, or that changes existing tests**: its file is a draft to read, and no command
    /// is offered.
    pub fn apply_command(&self) -> Option<String> {
        if matches!(self.status(), Status::Failed | Status::Review) {
            return None;
        }
        let file = self.patch_file.as_ref()?;
        Some(format!(
            "git -C \"{}\" apply --whitespace=nowarn \"{}\"",
            self.repo_root,
            file.display()
        ))
    }

    /// This run has no proposal, and why.
    fn abort(mut self, why: impl Into<String>) -> Self {
        self.error = Some(why.into());
        self.errored = true;
        self
    }

    /// The patch as text if it is short enough to print, whether that is not the patch exactly
    /// (a byte that was not UTF-8), and whether it was left out for its length.
    fn diff(&self) -> (String, bool, bool) {
        if self.patch.len() > MAX_SHOWN_DIFF {
            return (String::new(), false, true);
        }
        let text = String::from_utf8_lossy(&self.patch);
        let lossy = matches!(text, std::borrow::Cow::Owned(_));
        (text.into_owned(), lossy, false)
    }

    /// The lines gained and lost over every file, and whether any file was not text.
    fn totals(&self) -> (u64, u64, bool) {
        let added = self.files.iter().filter_map(|f| f.added).sum();
        let deleted = self.files.iter().filter_map(|f| f.deleted).sum();
        (added, deleted, self.files.iter().any(|f| f.added.is_none()))
    }

    fn json(&self) -> JsonReport<'_> {
        let (diff, diff_lossy, diff_omitted) = self.diff();
        JsonReport {
            status: self.status(),
            warnings: self.bent.iter().map(Bent::warning).collect(),
            error: self.error.as_deref(),
            partial: self.partial(),
            stopped: self.stopped,
            check: self.check.as_ref().map(|c| JsonCheck {
                command: &c.command,
                passed: c.passed,
                exit_code: c.exit_code,
                timed_out: c.timed_out,
                blocked: c.blocked.is_some(),
                reused: c.reused,
                cut_short: c.cut_short,
                not_run: c.not_run.as_deref(),
                seconds: c.seconds,
                output_tail: &c.output_tail,
            }),
            files: self
                .files
                .iter()
                .map(|f| JsonFile {
                    path: &f.path,
                    status: f.status,
                    by: f.by,
                    added: f.added,
                    deleted: f.deleted,
                    from: f.from.as_deref(),
                })
                .collect(),
            apply: self.apply_command(),
            patch_file: self.patch_file.as_ref().map(|p| p.display().to_string()),
            skipped: &self.skipped,
            notes: &self.notes,
            answer: &self.answer,
            seconds: self.seconds,
            model: &self.model,
            folder: &self.folder,
            repo_root: &self.repo_root,
            base: &self.base,
            task: shown_task(&self.task),
            diff_lossy,
            diff_omitted,
            diff,
        }
    }

    /// The report as `--json` gives it: one object, whatever happened, its fields in the order
    /// [`JsonReport`] names them.
    pub fn to_json_string(&self) -> String {
        serde_json::to_string(&self.json())
            .unwrap_or_else(|e| format!("{{\"error\":\"could not write the report: {e}\"}}"))
    }

    /// The same as a value, which does not keep the order, for a test that looks fields up.
    #[cfg(test)]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self.json()).unwrap_or(serde_json::Value::Null)
    }

    /// What else cut the turn short, when its first line is about something else: the time limit,
    /// an error, a bound. Empty when nothing did.
    fn also_cut_short(&self, files: &str) -> String {
        let mut also: Vec<String> = Vec::new();
        if self.stopped {
            also.push(format!("stopped at the {} s time limit ({files} by then)", self.time));
        }
        also.extend(self.error.iter().map(|e| format!("ended with an error: {e}")));
        also.extend(self.notes.iter().cloned());
        if also.is_empty() {
            return String::new();
        }
        let lines: String = also.iter().map(|line| format!("  {line}\n")).collect();
        format!("\nthe turn was also cut short:\n{lines}")
    }

    /// Why a proposal is partial, as the clause its first line gives.
    fn partial_why(&self) -> String {
        if let Some(why) = self.error.clone().or_else(|| self.notes.first().cloned()) {
            return why;
        }
        match self
            .check
            .as_ref()
            .filter(|_| self.unverified() && !self.answer.is_empty())
        {
            Some(c) => unverified_why(c),
            None => "the model gave no answer".to_string(),
        }
    }

    /// The report as plain text: a first line that says how it came out, the answer, **then the
    /// check and how to apply — before the patch, so that a long patch never buries them** — and
    /// then the patch: its files with their counts, and its text if it is short enough to print,
    /// else where to read it.
    ///
    /// **A check that failed leads instead.** The first line says not to apply the patch, the check
    /// and the end of its output come next, and the model's own words follow, labelled: they can
    /// say the work is done when it is not. The patch file is offered as a draft to read, with no
    /// command to apply it.
    ///
    /// **A patch that changes existing tests is for review**: the first line says so
    /// ([`bent::review_line`]), the files and lines come next, and the patch file is a draft to
    /// read, as when the check failed. A check that failed is the status then, with a line above
    /// it ([`bent::warning_line`]) saying the tests were changed as well.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        let status = self.status();
        if status == Status::Failed && !self.bent.is_empty() {
            out.push_str(&bent::warning_line(&self.bent));
            out.push('\n');
        }
        let n = self.files.len();
        let files = format!("{n} file{} changed", if n == 1 { "" } else { "s" });
        let secs = self.seconds.round();
        out.push_str(&match status {
            Status::Error => format!("hugin -p could not make a proposal: {}\n", self.error.as_deref().unwrap_or_default()),
            Status::Failed => "hugin: FAILED — the check failed; do not apply this patch as it is.\n".to_string(),
            Status::Review => format!("{}\n", bent::review_line(&self.bent, self.check_passed())),
            Status::Partial if self.stopped => format!(
                "hugin: PARTIAL proposal. Stopped at the {} s time limit; this is what it had done by then ({files}).\n",
                self.time
            ),
            Status::Partial => format!("hugin: PARTIAL proposal. {} ({files}, {secs} s).\n", self.partial_why()),
            Status::Done => format!("hugin: done in {secs} s on {} ({files}).\n", self.model),
        });

        match status {
            Status::Error => {
                if !self.answer.is_empty() {
                    out.push_str(&format!("\n{}\n", self.answer));
                }
                return out;
            }
            Status::Failed => {
                out.push('\n');
                out.push_str(&self.check_line());
                if !self.answer.is_empty() {
                    out.push_str(&format!(
                        "\nThe model's own last words (it can say the work is done when its check says it is not):\n{}\n",
                        self.answer
                    ));
                }
                out.push_str(&self.also_cut_short(&files));
            }
            Status::Review => {
                out.push_str(&format!(
                    "\nthe existing tests the patch changes:\n{}\n",
                    bent::listing(&self.bent)
                ));
                if !self.answer.is_empty() {
                    out.push_str(&format!(
                        "\nThe model's own words (it may say the change was needed; confirm that it was):\n{}\n",
                        self.answer
                    ));
                }
                out.push('\n');
                out.push_str(&self.check_line());
                out.push_str(&self.also_cut_short(&files));
            }
            Status::Done | Status::Partial => {
                if !self.answer.is_empty() {
                    out.push_str(&format!("\n{}\n", self.answer));
                }
                out.push('\n');
                out.push_str(&self.check_line());
            }
        }
        if !self.skipped.is_empty() {
            out.push_str("\nnot carried into the copy:\n");
            for s in &self.skipped {
                out.push_str(&format!("  {s}\n"));
            }
        }
        if matches!(status, Status::Failed | Status::Review) {
            if let Some(file) = &self.patch_file {
                out.push_str(&format!("\ndraft patch (do not apply as is): {}\n", file.display()));
            }
        } else if let Some(apply) = self.apply_command() {
            out.push_str(&format!("\nto apply: {apply}\n"));
        }

        if self.patch.is_empty() {
            out.push_str("\nNo files were changed.\n");
            return out;
        }
        let (added, deleted, binary) = self.totals();
        let counts = format!(
            "+{added} -{deleted}{}",
            if binary { ", and a file that is not text" } else { "" }
        );
        out.push_str(&format!("\n--- patch: {files}, {counts} ---\n"));
        for f in &self.files {
            let from = f
                .from
                .as_ref()
                .map(|from| format!(" (from {from})"))
                .unwrap_or_default();
            let lines = match (f.added, f.deleted) {
                (Some(a), Some(d)) => format!("+{a} -{d}"),
                _ => "not text".to_string(),
            };
            let by = if f.by == "edit" {
                "by an edit"
            } else {
                "by something else: not one of the model's edits"
            };
            out.push_str(&format!("{} {}{from}  {lines}  [{by}]\n", f.status, f.path));
        }
        let (diff, _, omitted) = self.diff();
        if omitted {
            let kb = self.patch.len().div_ceil(1024);
            let whole = match &self.patch_file {
                Some(file) => format!("read it in {}, which has all of it", file.display()),
                None => "it could not be saved".to_string(),
            };
            out.push_str(&format!(
                "\nThe patch is {kb} KB, too long to print here ({} KB at most): {whole}.\n",
                MAX_SHOWN_DIFF / 1024
            ));
        } else {
            out.push('\n');
            out.push_str(&diff);
            if !diff.ends_with('\n') {
                out.push('\n');
            }
        }
        out
    }

    fn check_line(&self) -> String {
        let Some(c) = &self.check else {
            return "check: none was run (no --check, and no check is set for this folder in Mimir)\n".to_string();
        };
        let name = &c.command;
        if let Some(why) = &c.not_run {
            return format!("check: `{name}` was not run: {why}\n");
        }
        if let Some(what) = &c.blocked {
            return format!(
                "check: `{name}` could not finish: Windows Smart App Control blocked {what}. This is not a problem in the code; run it again.\n"
            );
        }
        let reused = if c.reused {
            ", the model's own last run, with nothing edited after it"
        } else {
            ""
        };
        let how = if c.timed_out && c.cut_short {
            format!("did not finish in the {} s the run had left", c.seconds)
        } else if c.timed_out {
            format!("timed out after {} s", c.seconds)
        } else {
            let code = c
                .exit_code
                .map_or("no exit code".to_string(), |code| format!("exit {code}"));
            format!(
                "{} ({code}, {} s{reused})",
                if c.passed == Some(true) { "passed" } else { "FAILED" },
                c.seconds
            )
        };
        let mut line = format!("check: `{name}` {how}\n");
        if c.passed != Some(true) && !c.output_tail.is_empty() {
            line.push_str(&format!("check output (the end):\n{}\n", c.output_tail.trim_end()));
        }
        line
    }
}

/// Why a check that was asked for gave no verdict, as the clause the first line of a partial
/// proposal gives.
fn unverified_why(c: &CheckReport) -> String {
    if let Some(why) = &c.not_run {
        format!("The check was not run: {why}")
    } else if let Some(what) = &c.blocked {
        format!("Windows Smart App Control blocked {what}, so the check could not finish")
    } else if c.cut_short {
        format!("The check did not finish in the {} s the run had left", c.seconds)
    } else {
        format!("The check timed out after {} s", c.seconds)
    }
}

/// The task as echoed: whole unless it is very long.
fn shown_task(task: &str) -> String {
    if task.chars().count() <= MAX_SHOWN_TASK {
        return task.to_string();
    }
    let cut: String = task.chars().take(MAX_SHOWN_TASK).collect();
    format!("{cut}…")
}

/// The last `max` bytes of `text`, from a character boundary.
fn tail_of(text: &str, max: usize) -> String {
    let text = text.trim_end();
    if text.len() <= max {
        return text.to_string();
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &text[start..])
}

pub(crate) fn seconds(d: Duration) -> f64 {
    (d.as_secs_f64() * 10.0).round() / 10.0
}

/// A finished check as the report has it. **One killed because its drive was nearly full is
/// not a failed check**: it gave no verdict on the code, so it is reported as not run, with the
/// reason, like a timeout or a block.
pub(crate) fn report_of(argv: &[String], o: &check::Outcome, reused: bool) -> CheckReport {
    if o.ended == Ended::DiskFull {
        return not_run(argv, "stopped because the disk was nearly full");
    }
    CheckReport {
        command: check::label(argv),
        passed: Some(o.passed()),
        exit_code: match o.ended {
            Ended::Exited(code) => code,
            _ => None,
        },
        seconds: seconds(o.elapsed),
        timed_out: o.ended == Ended::TimedOut,
        blocked: o.blocked.clone(),
        reused,
        cut_short: false,
        // What a build had printed when it was cut off says nothing, and a block is not the code's.
        // A test run that exited 0 and still failed says why first: its output alone would not.
        output_tail: if o.passed() || o.blocked.is_some() || matches!(o.ended, Ended::TimedOut | Ended::Stopped) {
            String::new()
        } else if o.unfinished_tests > 0 {
            format!("{}\n{}", check::UNFINISHED_NOTE, tail_of(&o.output, OUTPUT_TAIL))
        } else {
            tail_of(&o.output, OUTPUT_TAIL)
        },
        not_run: None,
    }
}

/// A check that was asked for and did not run.
fn not_run(argv: &[String], why: impl Into<String>) -> CheckReport {
    CheckReport {
        command: check::label(argv),
        passed: None,
        exit_code: None,
        seconds: 0.0,
        timed_out: false,
        blocked: None,
        reused: false,
        cut_short: false,
        output_tail: String::new(),
        not_run: Some(why.into()),
    }
}

/// A run that is over: its report, and the copy and the lock it was made with, **held until the
/// caller lets go of them**. The copy's folder takes a moment to remove, and a run that is killed
/// meanwhile — Claude Code kills a command at its timeout — must already have said what it did.
pub struct Finished {
    pub report: Report,
    _copy: Option<Scratch>,
    _lock: Option<Lock>,
}

/// Wait for the turn to say it has ended, until `until`. At the deadline it is stopped and given
/// `grace` to wind down, and **the lock every edit takes is taken from then on**, so that a turn
/// still inside a request, which cannot be reached, writes nothing to the copy while the patch is
/// made: it is handed back for the caller to hold, and it is what says the turn was stopped.
pub(crate) fn await_turn(
    rx: &Receiver<Result<(), String>>,
    until: Instant,
    grace: Duration,
    stopper: &Stopper,
) -> (Result<(), String>, Option<MutexGuard<'static, ()>>) {
    match rx.recv_timeout(until.saturating_duration_since(Instant::now())) {
        Ok(result) => (result, None),
        Err(RecvTimeoutError::Disconnected) => (Err("the turn ended unexpectedly".to_string()), None),
        Err(RecvTimeoutError::Timeout) => {
            stopper.stop();
            let wound_down = rx.recv_timeout(grace);
            (wound_down.unwrap_or(Ok(())), Some(edits_lock()))
        }
    }
}

/// What to say to the model when the copy now holds a patch that changes existing tests
/// ([`bent::changed_tests`]) and a sixth of the run's time is left for it to answer. The patch is
/// made as it stands; nothing in the copy changes.
pub fn bent_follow_up(copy: &mut Scratch, until: Instant, total: Duration) -> Option<String> {
    if until.saturating_duration_since(Instant::now()) < total / FOLLOW_UP_SHARE {
        return None;
    }
    let end = copy.snapshot().ok()?;
    let patch = copy.patch(&end).ok()?;
    let bent = bent::changed_tests(&patch.bytes);
    (!bent.is_empty()).then(|| bent::follow_up(&bent))
}

/// The whole run: copy the folder, run the turn in the copy until it ends or its time is up,
/// run the check once on what is there, and make the patch. **The user's folder is only read.**
///
/// The clock started with the process ([`Job::started`]). `wire` makes the model and the runner
/// once the copy exists and says where it is; a test hands in a script.
pub fn execute(job: &Job, git: &Git, model: &str, wire: impl FnOnce(&Ctx) -> Wired) -> Finished {
    let started = job.started;
    let budget = Budget::of(job.time);
    // No git command is given more than the run has left (and a floor to finish the patch in).
    let git = git.until(started + budget.total);
    let mut report = Report {
        task: job.task.clone(),
        folder: Workspace::open(&job.folder).map_or_else(|_| job.folder.display().to_string(), |ws| ws.root_display()),
        model: model.to_string(),
        time: job.time.as_secs(),
        ..Report::default()
    };
    let finish = |report: Report, copy: Option<Scratch>, lock: Option<Lock>| Finished {
        report: Report {
            seconds: seconds(started.elapsed()),
            ..report
        },
        _copy: copy,
        _lock: lock,
    };

    // One run at a time: two would wait on each other's cargo lock and both load the model.
    let lock = match Lock::take(&job.work) {
        Ok(lock) => lock,
        Err(e) => return finish(report.abort(e), None, None),
    };
    scratch::sweep(&job.work);
    let mut copy = match Scratch::of_in(&git, &job.work, &job.folder) {
        Ok(copy) => copy,
        Err(e) => return finish(report.abort(e), None, Some(lock)),
    };
    report.repo_root = copy.repo_root.display().to_string();
    report.base = copy.base.clone();
    report.skipped = copy.skipped_lines();

    let ws = match Workspace::open(copy.folder()) {
        Ok(ws) => ws,
        Err(e) => return finish(report.abort(e), Some(copy), Some(lock)),
    };
    // The check as it runs in the copy; `job.check` stays what the report calls it. The three
    // places are compared as the file system has them, not as a short name or a prefix writes them.
    let (folder, copy_root, repo_root) = (
        real_path(ws.root()),
        real_path(copy.copy_root()),
        real_path(&copy.repo_root),
    );
    let check = match job
        .check
        .as_deref()
        .map(|argv| resolve_check(argv, &folder, &copy_root, &repo_root))
    {
        Some(Err(e)) => return finish(report.abort(e), Some(copy), Some(lock)),
        Some(Ok(argv)) => Some(argv),
        None => None,
    };
    let backups = copy.run_dir().join("backups");
    // One build folder for each repository, so the next run's check starts warm; never the
    // user's own `target\`, which holds the programs their shortcuts run.
    let hash = edit::fingerprint(copy.git_dir.to_string_lossy().to_lowercase().as_bytes());
    let target = job.cache.join(format!("target-{hash:016x}"));
    let env = Env {
        identity: Some(HUGIN_IDENTITY.into()),
        commands: None,
        backups: Some(backups.clone()),
        check_env: vec![
            ("CARGO_TARGET_DIR".to_string(), target.to_string_lossy().into_owned()),
            ("CARGO_NET_OFFLINE".to_string(), "true".to_string()),
        ],
        ..Env::default()
    };
    let ctx = Ctx {
        root: ws.root_display(),
        env,
        check: check.clone(),
    };
    let wired = wire(&ctx);

    // The turn, on a thread of its own: a request to the model cannot be cut short.
    let progress: Shared = Arc::default();
    let stopper = Stopper::default();
    let (mut ask, mut inner) = (wired.ask, wired.run);
    let recorded = progress.clone();
    let mut run = move |name: &str, path: &str, token: &str| {
        let ran = inner(name, path, token);
        if let Ok(ran) = &ran {
            let mut p = held(&recorded);
            if let Some(id) = &ran.undo {
                p.undo.push(id.clone());
                p.edited_since = true;
            }
            if name == "run_check"
                && let Some(outcome) = &ran.check
            {
                p.last_check = Some(outcome.clone());
                p.edited_since = false;
            }
        }
        ran
    };
    let (tx, rx) = std::sync::mpsc::channel();
    // The one follow-up the patch may earn, sent after the turn has ended; dropped unsent if not.
    let (follow_tx, follow_rx) = std::sync::mpsc::channel::<String>();
    let (task, turn_progress, turn_stopper) = (job.task.clone(), progress.clone(), stopper.clone());
    stopper.begin();
    let worker = std::thread::Builder::new().name("hugin-p-turn".into()).spawn(move || {
        let mut fe = Unattended {
            progress: turn_progress.clone(),
            stopper: turn_stopper,
            shown: Vec::new(),
            edited: Vec::new(),
            calibration: None,
        };
        let mut convo = vec![Message::user(&task)];
        let result = run_turn_with(&mut convo, None, unattended::ROUNDS, &mut *ask, &mut run, &mut fe);
        let ended_by_itself = result.is_ok();
        let _ = tx.send(result);
        // The patch is looked at now. If it changes existing tests the follow-up arrives, and the
        // turn goes on **in the same conversation**; if none comes, the sender is dropped and this ends.
        if ended_by_itself && let Ok(text) = follow_rx.recv() {
            // The loop leaves the model's last words out of the conversation, and the follow-up answers them.
            let said = held(&turn_progress).answer.clone();
            if let Some(content) = said {
                convo.push(Message {
                    role: "assistant".to_string(),
                    content,
                    images: None,
                    tool_calls: None,
                    tool_name: None,
                    thinking: None,
                });
            }
            convo.push(Message::user(text));
            let calibration = fe.calibration;
            let result = run_turn_with(&mut convo, calibration, FOLLOW_UP_ROUNDS, &mut *ask, &mut run, &mut fe);
            let _ = tx.send(result);
        }
    });
    if let Err(e) = worker {
        return finish(
            report.abort(format!("could not start the turn: {e}")),
            Some(copy),
            Some(lock),
        );
    }

    let turn_ends = started + budget.total.saturating_sub(budget.reserve);
    // Held from the deadline until the patch is made, once it has been taken.
    let (mut turn, mut writers) = await_turn(&rx, turn_ends, budget.grace, &stopper);
    stopper.end();
    // **A check that passes may only be passing because a test was changed.** A turn that ended by
    // itself, with time left, and whose patch changes existing tests is told so, once, and goes on.
    let mut follow_tx = Some(follow_tx);
    if turn.is_ok()
        && writers.is_none()
        && let Some(text) = bent_follow_up(&mut copy, turn_ends, budget.total)
    {
        stopper.begin();
        if follow_tx.take().is_some_and(|tx| tx.send(text).is_ok()) {
            (turn, writers) = await_turn(&rx, turn_ends, budget.grace, &stopper);
        }
        stopper.end();
    }
    drop(follow_tx);
    report.stopped = writers.is_some();

    let (answer, notes, rounds, undo, last_check, edited_since) = {
        let p = held(&progress);
        (
            p.answer.clone(),
            p.notes.clone(),
            p.rounds,
            p.undo.clone(),
            p.last_check.clone(),
            p.edited_since,
        )
    };
    report.answer = answer.unwrap_or_default();
    report.notes = notes;
    if let Err(e) = turn {
        report.error = Some(e);
        // A turn that never got a reply and wrote nothing did not run: there is nothing to propose.
        report.errored = rounds == 0 && undo.is_empty();
        if report.errored {
            return finish(report, Some(copy), Some(lock));
        }
    }

    // What the model's edits touched, as repository paths. `None` if the records cannot be read.
    let edited: Option<Vec<String>> = edit::turn_changes(&undo, &backups).ok().map(|nets| {
        nets.iter()
            .flat_map(|n| std::iter::once(n.display.clone()).chain(n.moved_to.clone()))
            .map(|p| format!("{}{p}", copy.prefix))
            .collect()
    });

    // The check, once, on what the copy holds: the model's own last run if nothing was written
    // after it, else a new one. **Not if the turn changed a file that makes a check run code**
    // (a build file, a runner's config): the card that warns about it, which a person would
    // read, is not here to read.
    if let (Some(shown), Some(argv)) = (&job.check, &check) {
        let runs_code: Vec<&str> = edited
            .iter()
            .flatten()
            .map(String::as_str)
            .filter(|p| unattended::runs_code(p))
            .collect();
        let reusable =
            last_check.filter(|o| !edited_since && o.blocked.is_none() && matches!(o.ended, Ended::Exited(Some(_))));
        report.check = Some(if !runs_code.is_empty() {
            let why = format!(
                "the turn changed {}, and a check runs those files; read the patch, then run the check yourself",
                runs_code.join(", ")
            );
            not_run(shown, why)
        } else if let Some(outcome) = reusable {
            report_of(shown, &outcome, true)
        } else {
            let end = started + budget.total;
            let left = end
                .saturating_duration_since(Instant::now())
                .saturating_sub(budget.tail);
            if left < Duration::from_secs(2).min(budget.total / 4) {
                not_run(shown, "no time was left for it")
            } else {
                let usual = check::timeout_for(argv);
                match check::run(ws.root(), argv, usual.min(left), &ctx.env.check_env, || false) {
                    // Cut short only if it ran out of the time the run had left, not out of its own.
                    Ok(outcome) => CheckReport {
                        cut_short: left < usual && outcome.ended == Ended::TimedOut,
                        ..report_of(shown, &outcome, false)
                    },
                    Err(e) => not_run(shown, e),
                }
            }
        });
    }

    // The patch: everything the copy holds, against what it started with (but for the links the
    // run made, which `finish` leaves out and names).
    let made = copy.finish();
    drop(writers);
    let patch = match made {
        Ok(patch) => patch,
        Err(e) => {
            return finish(
                report.abort(format!("could not make the patch: {e}")),
                Some(copy),
                Some(lock),
            );
        }
    };
    report.skipped = copy.skipped_lines();
    // A change is the model's own if one of its edits touched the file. Without the records to
    // say, every change is taken to be.
    let own = |path: &str| {
        edited
            .as_ref()
            .is_none_or(|e| e.iter().any(|p| p.eq_ignore_ascii_case(path)))
    };
    report.files = patch
        .files
        .iter()
        .map(|c| FileChange {
            path: c.path.clone(),
            from: c.from.clone(),
            status: c.status,
            by: if own(&c.path) || c.from.as_deref().is_some_and(own) {
                "edit"
            } else {
                "other"
            },
            added: c.added,
            deleted: c.deleted,
        })
        .collect();
    report.patch = patch.bytes;
    // Whatever the follow-up came to, the patch as it stands is what is warned about.
    report.bent = bent::changed_tests(&report.patch);
    report.patch_file = keep_patch(&job.work, &report);
    finish(report, Some(copy), Some(lock))
}

/// Save the patch, byte for byte, and a record of the run beside it, in `work\patches`. They
/// are kept for seven days (see [`scratch::sweep`]). `None` when there is nothing to save or it
/// could not be saved.
fn keep_patch(work: &Path, report: &Report) -> Option<PathBuf> {
    if report.patch.is_empty() {
        return None;
    }
    let dir = work.join("patches");
    std::fs::create_dir_all(&dir).ok()?;
    let name = format!("{}-{}", scratch::file_stamp(), std::process::id());
    let patch = dir.join(format!("{name}.patch"));
    std::fs::write(&patch, &report.patch).ok()?;
    let mut saved = report.clone();
    saved.patch_file = Some(patch.clone());
    let _ = std::fs::write(dir.join(format!("{name}.json")), saved.to_json_string());
    Some(patch)
}

// ── the real run ────────────────────────────────────────────────────────────

/// `hugin -p`: read the task, run it, print the report, and exit with its code.
pub fn run(args: Args) -> ExitCode {
    let started = Instant::now();
    let json = args.json;
    let task = match args.task {
        Task::Text(text) => text,
        Task::Stdin => {
            let mut text = String::new();
            if let Err(e) = std::io::stdin().read_to_string(&mut text) {
                eprintln!("hugin: could not read the task from standard input: {e}");
                return ExitCode::from(2);
            }
            text
        }
    };
    let task = task.trim().to_string();
    if task.is_empty() {
        eprintln!("hugin: the task is empty");
        return ExitCode::from(2);
    }

    let cfg = config::load();
    // `--check`, else the folder's check in Mimir's settings.
    let check = args.check.or_else(|| {
        let root = Workspace::open(&args.folder).ok()?.root_display();
        cfg.check_for(&root).cloned()
    });
    // `--model` for this run only: the config is read, never written.
    let model = args.model.unwrap_or_else(|| cfg.coding_model.clone());
    let trace = args.trace.as_deref().map(Trace::open);
    // The model loads while the copy is made. A failure is not news here: the first request meets it.
    let _ = start::warm(model.clone(), cfg.num_ctx);

    let job = Job {
        task,
        folder: args.folder,
        check,
        time: args.time,
        started,
        work: scratch::work_dir(),
        cache: cache_dir(),
    };
    let finished = execute(&job, &Git::new(), &model, |ctx| {
        trace::wired(trace, &model, real(&model, ctx))
    });
    let text = if json {
        format!("{}\n", finished.report.to_json_string())
    } else {
        finished.report.to_text()
    };
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
    drop(stdout);
    let code = finished.report.exit_code();
    // Only now does the copy go: the report is out, so a kill while it is removed loses nothing.
    // A turn still inside a request to the model is left behind: the process ends it.
    drop(finished);
    std::process::exit(i32::from(code))
}

/// The CLAUDE.md section that tells Claude when to hand work to Hugin: printed by
/// `hugin --claude-note`, never written to any file.
pub const CLAUDE_NOTE: &str = "\
## Hugin, the local coding agent

Hugin runs a coding model on this machine, so work handed to it costs nothing from the Claude
plan. Hand it a task that is small, well described and checkable; review what it sends back.

**Questions about a codebase** (where is X, how does Y work, what calls Z): ask Hugin first, with
`hugin --ask \"<the question>\" --json`. It reads the code on this machine, in place and
read-only, so it costs nothing from the Claude plan and writes nothing. Check its `looked_at`,
and open those files yourself if the answer matters: it can be wrong. (`--folder <path>` names
the folder. Exit code 0 is answered, 3 partial, 1 an error.)

To hand it a task:

    hugin -p \"<the task, with the file names>\" --check \"<one test or build command>\" --json

- **Good for:** a failing test to fix, a function or a module to write from a clear description,
  a mechanical change across a few files, boilerplate, a first draft to review.
- **Not for:** design decisions, work with no check, anything that needs the network or a
  shell, or a task so vague that you would have to see the code to describe it.
- **The check runs code Hugin wrote, before you review it**, with your rights. Name a check
  only in a repository you trust. If Hugin changed a file that makes a check run something
  (Cargo.toml, package.json, a makefile, conftest.py and the like), it does not run the check
  at the end: read the patch, then run the check yourself.
- `--check` is one program and its arguments, run in the copy (for example
  `cargo test -p hugin` or `npm test`); no pipes, `&&` or redirects. A program named with a
  folder (`.\\verify.cmd`) is the copy's own. Without `--check` the folder's check from Mimir is
  used if one is set, and otherwise no check runs.
- It works in a throwaway copy of the folder, so your files are never touched. It returns its
  answer, a patch and the check result. Read the patch before you apply it; a long one is not
  printed, only its files and the file to read it in.
- **Trust the check, not the answer.** Hugin's answer can say the work is done when its check
  failed. A failed or missing check means: don't apply it as it is — fix the patch or do the
  task yourself.
- **A passing check can still be wrong.** If the patch changes existing tests, which may have
  been bent to fit the code, Hugin asks the model once to put them back; if they are still
  changed the status is `review` (the files are in `warnings`), and it is not done until you
  confirm the change was asked for. Read the patch either way. A `cargo test` run in which a
  test program stops early (a `process::exit`, an abort) counts as failed, whatever its exit code.
- **Read the status first** (`status` in the JSON, the `hugin:` status line in plain text): `done`;
  `failed`, the check failed; `review`, the check did not fail but the patch changes existing
  tests; for both, no apply command is given, only a draft patch to read. `partial`, cut short
  or the check gave no verdict; `error`, no proposal. **Exit code 3, or status failed, review or
  partial, means not done: don't apply it as it is — fix the patch or do the task yourself.**
  Exit code 0 is done, and 1 is a run that could not happen.
- To apply: `git -C \"<repo>\" apply --whitespace=nowarn \"<patch file>\"`, the exact command is
  in the output. A patch goes stale if the same files change first.
- It stops itself at 9 minutes (`--time` changes that). Call it with a Bash timeout of
  600000 ms, or run it in the background.
- The copy has no ignored files, so a check that needs `node_modules` will not find them.
";
