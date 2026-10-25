//! `hugin --ask "<question>"`: Hugin as a free local scout (I7).
//!
//! Claude Code spends a great many tokens reading files to find out where something is or how it
//! works. This does that reading on the local model, which costs nothing from the Claude plan, and
//! hands back an answer and the files it looked at. Claude checks those files if the answer matters:
//! the answer can be wrong, and `looked_at` is what makes it cheap to check.
//!
//! **It only looks, and that is built in rather than asked for.**
//!
//! - The model is offered the three tools that read — `read_file`, `list_files`, `search` — and
//!   no others ([`offered`]); not the task list, not a check, not a command.
//! - Whatever it asks for anyway is **denied** ([`decide`]): the folder's rules already turn away
//!   a call to a tool that is not there, and an edit or a command that is planned anyway is
//!   answered "no" here, however it is asked.
//! - The runner it is given refuses to run anything at all, so an approved call, if one could
//!   ever get through, ends the turn with an error rather than doing something.
//! - It works **in place**, since nothing is written there to protect: no copy, no git, no backups,
//!   no session saved, no config written. A test shows the folder, its `.git` included, as it was
//!   found, down to the times ([`scratch::fingerprints`]).
//!
//! **What it looked at comes from the calls it made, not from what it says.** Each read that ran
//! is a card the loop shows its frontend; [`Scout`] writes it down, with the lines the read really
//! returned ([`lines_of`]), and a read that failed is not counted. A search and a listing are not a
//! file looked at.
//!
//! **It ends on time**, like `-p` and with the same watchdog ([`print::await_turn`]): the turn is
//! stopped at the limit and the report says it is partial. It takes the same lock, so a `--ask`
//! and a `-p` do not both load the model at once.
//!
//! - [`execute`] — the whole run, with the model and the runner passed in so a test can script
//!   both; [`run`] is the real one
//! - [`Report`] — what a run came to, and how it reads as text and as JSON
//!
//! **Nothing is printed until the end**: one report on standard output, text or JSON.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use bifrost_core::agent::env::Env;
use bifrost_core::agent::round::round_in;
use bifrost_core::agent::send::Calibration;
use bifrost_core::agent::ticket::PendingCall;
use bifrost_core::agent::turn::{Card, Decision, Frontend, Patch, RoundNumbers, run_turn_with};
use bifrost_core::agent::unattended;
use bifrost_core::chat::{Message, Tool};
use bifrost_core::edit::Workspace;
use bifrost_core::scratch::{self, Lock};
use bifrost_core::{config, instructions, tools};
use serde::Serialize;

use crate::print::{self, Wired};
use crate::start;
use crate::stop::Stopper;
use crate::trace::{self, Trace};

/// How long the whole run may take unless `--time` says: five minutes. A question is a smaller
/// thing than a task.
pub const DEFAULT_TIME: u64 = 300;

// ── what was asked ──────────────────────────────────────────────────────────

/// Where the question comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Question {
    /// Given on the command line.
    Text(String),
    /// `--ask -`: read from standard input, for a question too long for a command line.
    Stdin,
}

/// What `--ask` was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    pub question: Question,
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

/// `--time`'s seconds, or [`DEFAULT_TIME`] when it is not given.
pub fn parse_time(text: Option<&str>) -> Result<Duration, String> {
    print::parse_time_or(text, DEFAULT_TIME)
}

// ── what the model is given ─────────────────────────────────────────────────

/// The tools that only look. Named here, and taken from the app's own list rather than worded
/// again, so the model reads the same descriptions the app ships; a tool added to that list later
/// is not offered here unless it is named.
const LOOKING: [&str; 3] = ["read_file", "list_files", "search"];

/// The tools a scout is offered: the three that read, and nothing that writes or runs.
pub fn offered() -> Vec<Tool> {
    tools::available(true, None).into_iter().filter(|tool| LOOKING.contains(&tool.function.name.as_str())).collect()
}

/// Who the model is, and what it does here.
const IDENTITY: &str = "You are Hugin, a code scout running on the user's own computer. If asked your name, say Hugin.";

/// How it answers. **Short on purpose**: the answer is read by another model that is paying for
/// every word of it, and the files it cites are what it will check.
const HOW: &str = "You can only look: read_file, list_files and search. You cannot change or run anything. Use paths \
     relative to that folder.\n\
     1. Find where it is with search or list_files, rather than guessing a path. Search for more than one \
     word — the words of the question, then the names you find — and look at what each search found.\n\
     2. Read the code with read_file. A search hit is one line of a file: read the code you will cite — never \
     answer from search hits alone, and never guess what a file says.\n\
     3. Answer the question from the code you read. Name the files and line ranges you relied on, like \
     src/main.rs:120-180. If the code does not settle it, say \"not sure\" and what you could not find, rather \
     than guess. Keep the answer short.";

/// The system prompt: the identity, the folder, how to answer, and the project's own notes when it
/// has them. Used as it is, in place of the app's, which tells the model it can change files.
pub fn prompt(ws: &Workspace) -> String {
    let prompt = format!("{IDENTITY} You answer one question about the code in the user's project folder: {}\n{HOW}", ws.root_display());
    match instructions::find(ws) {
        Some(notes) => tools::with_notes(prompt, &notes),
        None => prompt,
    }
}

/// What to answer a card that asks, which is **no, always**: a read outside the folder or of a
/// file that is held back, and anything that would change or run something, however it is asked.
/// The scout is offered nothing that asks, so a call that does has gone beyond what it was given.
pub fn decide(_call: &PendingCall) -> Decision {
    Decision::Deny
}

/// The model and the runner, once the folder is known.
pub struct Ctx {
    /// The folder, as the model is told it.
    pub root: String,
    pub env: Env,
}

/// The real model and runner: the app's round, offering the three tools that look, and a runner
/// that refuses. The config is loaded fresh each round and set in memory only, **never saved** —
/// the folder, and no check, so a check set for the folder in Mimir is not offered.
fn real(model: &str, ctx: &Ctx) -> Wired {
    let (asked, root, env) = (model.to_string(), ctx.root.clone(), ctx.env.clone());
    let ask = move |messages: &[Message], calibration: Option<Calibration>, closing: bool| {
        let mut cfg = config::load();
        cfg.workspace = Some(root.clone());
        cfg.set_check(&root, None);
        cfg.ask_every_call = false;
        round_in(messages, &asked, &cfg, &env, calibration, closing)
    };
    // Nothing is approved, so nothing should be run; if the loop ever asks, it is an error.
    let run = |name: &str, _path: &str, _token: &str| Err(format!("hugin --ask only looks: it cannot run {name}"));
    Wired { ask: Box::new(ask), run: Box::new(run) }
}

// ── what the turn leaves ────────────────────────────────────────────────────

/// What the turn has done, kept where the main thread can read it however the turn ends.
#[derive(Default)]
struct Progress {
    /// The model's last words.
    answer: Option<String>,
    /// What the loop said that a person would have been told: a bound reached, a call it gave up on.
    notes: Vec<String>,
    /// Every read that ran, in order: the file, and the lines it returned (`None` for the whole).
    reads: Vec<(String, Option<String>)>,
}

type Shared = Arc<Mutex<Progress>>;

/// A panic elsewhere must not hide what the turn had done.
fn held(shared: &Shared) -> MutexGuard<'_, Progress> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

/// The turn's frontend with nobody there: it writes down the reads that ran, keeps the words, and
/// says no to every card that asks.
struct Scout {
    progress: Shared,
    stopper: Stopper,
    /// How many cards have been shown: the position the next one is given.
    shown: usize,
}

impl Frontend for Scout {
    fn stopped(&self) -> bool {
        self.stopper.stopped()
    }

    fn status(&mut self, _text: &str) {}

    fn round(&mut self, _numbers: RoundNumbers) {}

    fn answer(&mut self, content: &str, _prompt_eval_count: u32, _eval_count: u32) {
        if !content.trim().is_empty() {
            held(&self.progress).answer = Some(content.trim().to_string());
        }
    }

    fn error(&mut self, message: &str) {
        held(&self.progress).notes.push(message.to_string());
    }

    /// **A file looked at is a read that ran**, shown as an automatic card with its result: a read
    /// that failed, was refused or asked about is not one.
    fn card(&mut self, card: Card) -> usize {
        if card.name == "read_file" && card.auto && card.ok == Some(true) {
            let lines = lines_of(&card);
            held(&self.progress).reads.push((card.path.clone(), lines));
        }
        self.shown += 1;
        self.shown - 1
    }

    fn update(&mut self, _card: usize, _patch: Patch) {}

    fn decide(&mut self, _card: usize, call: &PendingCall) -> Decision {
        decide(call)
    }

    fn save(&mut self) {}
}

/// The lines a read returned, as `120-180`: from the header the read puts first
/// (`[lines 120–180 of 2140]`), which says what it really gave — the end asked for may have been
/// past the file's, or the read cut at its size limit — and failing that, what was asked for.
/// `None` for a read of the whole file, which has no range and no header.
fn lines_of(card: &Card) -> Option<String> {
    let asked = card.range?;
    let header = card.result.as_deref().and_then(|text| text.lines().next());
    let shown = header
        .and_then(|line| line.strip_prefix("[lines "))
        .and_then(|rest| rest.split_once(" of "))
        .and_then(|(range, _)| range.split_once('–'))
        .filter(|(from, to)| [from, to].iter().all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())))
        .map(|(from, to)| format!("{from}-{to}"));
    Some(shown.unwrap_or_else(|| match asked.end {
        Some(end) => format!("{}-{end}", asked.start),
        None => format!("{}-", asked.start),
    }))
}

// ── what a run came to ──────────────────────────────────────────────────────

/// How a run came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// The model answered, and nothing cut the turn short.
    Answered,
    /// Stopped by the time limit or a bound, or no answer was given: what there is is what it had.
    Partial,
    /// The run could not be made: the folder, the lock, the model.
    Error,
}

/// A file the scout read, and the lines it read of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LookedAt {
    /// Relative to the folder, with forward slashes.
    pub path: String,
    /// `120-180` for part of the file; null for all of it.
    pub lines: Option<String>,
}

/// The reads, once each: the files in the order they were first read, and the parts of a file
/// together. **A read of the whole file stands for the parts of it**, so a file read both ways is
/// named once, without lines.
fn looked_at(reads: &[(String, Option<String>)]) -> Vec<LookedAt> {
    let mut files: Vec<&str> = Vec::new();
    for (path, _) in reads {
        if !files.contains(&path.as_str()) {
            files.push(path);
        }
    }
    let mut out: Vec<LookedAt> = Vec::new();
    for file in files {
        let mut parts: Vec<&Option<String>> = Vec::new();
        for (path, lines) in reads {
            if path == file && !parts.contains(&lines) {
                parts.push(lines);
            }
        }
        if parts.iter().any(|lines| lines.is_none()) {
            out.push(LookedAt { path: file.to_string(), lines: None });
        } else {
            out.extend(parts.into_iter().map(|lines| LookedAt { path: file.to_string(), lines: lines.clone() }));
        }
    }
    out
}

/// What a run came to.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// The folder, as it was opened.
    pub folder: String,
    pub model: String,
    pub answer: String,
    pub looked_at: Vec<LookedAt>,
    /// The turn was stopped by the time limit.
    pub stopped: bool,
    /// What the loop said that a person would have been told.
    pub notes: Vec<String>,
    pub seconds: f64,
    pub time: u64,
    pub error: Option<String>,
}

/// The report as `--json` writes it, in this order: how it came out, the answer, what to check it
/// against, then the rest.
#[derive(Serialize)]
struct JsonReport<'a> {
    status: Status,
    answer: &'a str,
    looked_at: &'a [LookedAt],
    seconds: f64,
    model: &'a str,
    folder: &'a str,
    error: Option<&'a str>,
}

impl Report {
    /// This run could not be made, and why.
    fn fail(mut self, why: impl Into<String>) -> Self {
        self.error = Some(why.into());
        self
    }

    pub fn status(&self) -> Status {
        if self.error.is_some() {
            Status::Error
        } else if self.stopped || !self.notes.is_empty() || self.answer.is_empty() {
            Status::Partial
        } else {
            Status::Answered
        }
    }

    /// **0 answered; 3 partial; 1 it could not be made.** (2, a command line that could not be
    /// read, never gets as far as a report.)
    pub fn exit_code(&self) -> u8 {
        match self.status() {
            Status::Answered => 0,
            Status::Partial => 3,
            Status::Error => 1,
        }
    }

    fn json(&self) -> JsonReport<'_> {
        JsonReport {
            status: self.status(),
            answer: &self.answer,
            looked_at: &self.looked_at,
            seconds: self.seconds,
            model: &self.model,
            folder: &self.folder,
            error: self.error.as_deref(),
        }
    }

    /// The report as `--json` gives it: one object, whatever happened.
    pub fn to_json_string(&self) -> String {
        serde_json::to_string(&self.json()).unwrap_or_else(|e| format!("{{\"error\":\"could not write the report: {e}\"}}"))
    }

    /// The same as a value, which does not keep the order, for a test that looks fields up.
    #[cfg(test)]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self.json()).unwrap_or(serde_json::Value::Null)
    }

    /// The report as plain text: a first line that says how it came out, the answer, and the files
    /// it looked at.
    pub fn to_text(&self) -> String {
        let secs = self.seconds.round();
        let mut out = match self.status() {
            Status::Error => {
                return format!("hugin --ask could not answer: {}\n", self.error.as_deref().unwrap_or_default());
            }
            Status::Answered => format!("hugin: answered in {secs} s on {}\n", self.model),
            Status::Partial if self.stopped => {
                format!("hugin: PARTIAL — stopped at the {} s time limit; this is what it had by then.\n", self.time)
            }
            Status::Partial => {
                let why = self.notes.first().cloned().unwrap_or_else(|| "the model gave no answer".to_string());
                format!("hugin: PARTIAL — {why} ({secs} s).\n")
            }
        };
        if !self.answer.is_empty() {
            out.push_str(&format!("\n{}\n", self.answer));
        }
        out.push('\n');
        if self.looked_at.is_empty() {
            out.push_str("looked at: no file was read\n");
        } else {
            out.push_str("looked at:\n");
            for l in &self.looked_at {
                match &l.lines {
                    Some(lines) => out.push_str(&format!("  {} (lines {lines})\n", l.path)),
                    None => out.push_str(&format!("  {}\n", l.path)),
                }
            }
        }
        out
    }
}

// ── a run ───────────────────────────────────────────────────────────────────

/// Everything a run is given.
pub struct Job {
    pub question: String,
    pub folder: PathBuf,
    pub time: Duration,
    /// When the process began, which is when the time starts running.
    pub started: Instant,
    /// Where the lock is, `-p`'s: `%TEMP%\hugin-p`. Nothing else is put there.
    pub work: PathBuf,
}

/// The whole run: open the folder, take the lock, and run the turn in place until it ends or its
/// time is up. **Nothing in the folder is written**, and no git is run.
///
/// The clock started with the process ([`Job::started`]). `wire` makes the model and the runner once
/// the folder is known; a test hands in a script.
pub fn execute(job: &Job, model: &str, wire: impl FnOnce(&Ctx) -> Wired) -> Report {
    let started = job.started;
    let mut report = Report {
        folder: job.folder.display().to_string(),
        model: model.to_string(),
        time: job.time.as_secs(),
        ..Report::default()
    };
    let finish = |report: Report| Report { seconds: print::seconds(started.elapsed()), ..report };

    let ws = match Workspace::open(&job.folder) {
        Ok(ws) => ws,
        Err(e) => return finish(report.fail(e)),
    };
    report.folder = ws.root_display();
    // The same rule that makes every read in such a folder ask: nothing could be read from it here.
    if tools::broad_root(ws.root(), std::env::var_os("USERPROFILE").map(PathBuf::from).as_deref()) {
        let why = format!("{} holds a whole drive or profile, which is too broad to read from; name the project folder with --folder", report.folder);
        return finish(report.fail(why));
    }
    // One run at a time, with `-p`: two would both load the model.
    let _lock = match Lock::take(&job.work) {
        Ok(lock) => lock,
        Err(e) => return finish(report.fail(e)),
    };
    let env = Env { system: Some(prompt(&ws)), offered: Some(offered()), ..Env::default() };
    let ctx = Ctx { root: ws.root_display(), env };
    let wired = wire(&ctx);

    // The turn, on a thread of its own: a request to the model cannot be cut short.
    let progress: Shared = Arc::default();
    let stopper = Stopper::default();
    let (mut ask, mut run) = (wired.ask, wired.run);
    let (tx, rx) = mpsc::channel();
    let (question, turn_progress, turn_stopper) = (job.question.clone(), progress.clone(), stopper.clone());
    stopper.begin();
    let worker = std::thread::Builder::new().name("hugin-ask-turn".into()).spawn(move || {
        let mut fe = Scout { progress: turn_progress, stopper: turn_stopper, shown: 0 };
        let mut convo = vec![Message::user(&question)];
        let result = run_turn_with(&mut convo, None, unattended::ROUNDS, &mut *ask, &mut *run, &mut fe);
        let _ = tx.send(result);
    });
    if let Err(e) = worker {
        return finish(report.fail(format!("could not start the turn: {e}")));
    }

    // The limit is the whole run: the last stretch is what the turn is given to wind down in.
    let grace = (job.time / 10).min(Duration::from_secs(5));
    let (turn, stopped) = print::await_turn(&rx, started + job.time.saturating_sub(grace), grace, &stopper);
    stopper.end();
    report.stopped = stopped.is_some();
    drop(stopped);

    let (answer, notes, reads) = {
        let p = held(&progress);
        (p.answer.clone(), p.notes.clone(), p.reads.clone())
    };
    report.answer = answer.unwrap_or_default();
    report.notes = notes;
    report.looked_at = looked_at(&reads);
    if let Err(e) = turn {
        report.error = Some(e);
    }
    finish(report)
}

// ── the real run ────────────────────────────────────────────────────────────

/// `hugin --ask`: read the question, answer it, print the report, and exit with its code.
pub fn run(args: Args) -> ExitCode {
    let started = Instant::now();
    let json = args.json;
    let question = match args.question {
        Question::Text(text) => text,
        Question::Stdin => {
            let mut text = String::new();
            if let Err(e) = std::io::stdin().read_to_string(&mut text) {
                eprintln!("hugin: could not read the question from standard input: {e}");
                return ExitCode::from(2);
            }
            text
        }
    };
    let question = question.trim().to_string();
    if question.is_empty() {
        eprintln!("hugin: the question is empty");
        return ExitCode::from(2);
    }

    let cfg = config::load();
    // `--model` for this run only: the config is read, never written.
    let model = args.model.unwrap_or_else(|| cfg.coding_model.clone());
    let trace = args.trace.as_deref().map(Trace::open);
    // The model loads while the folder is opened. A failure is not news here: the first request meets it.
    let _ = start::warm(model.clone(), cfg.num_ctx);

    let job = Job { question, folder: args.folder, time: args.time, started, work: scratch::work_dir() };
    let report = execute(&job, &model, |ctx| trace::wired(trace, &model, real(&model, ctx)));
    let text = if json { format!("{}\n", report.to_json_string()) } else { report.to_text() };
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
    drop(stdout);
    // A turn still inside a request to the model is left behind: the process ends it.
    std::process::exit(i32::from(report.exit_code()))
}
