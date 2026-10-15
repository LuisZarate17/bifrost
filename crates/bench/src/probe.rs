//! The tool loop every probe runs: the app's own, with the person taken out.
//!
//! Included by each probe as `#[path = "../probe.rs"] mod probe;` rather than
//! through a library target. A lib in this crate would be named `bench`, and on
//! Windows a lib and a bin cannot share a name (rust-lang/cargo#8519) — the same
//! reason Mimir's own lib is `mimir_lib`.
//!
//! **It drives the app's loop, not a copy of it (I1, 2c):**
//! `bifrost_core::agent::turn::run_turn_with`, with `round_in` deciding each reply
//! and `run_approved_in` running the calls that are approved. A probe that drove a
//! different loop would measure a different product, and a copy has to be kept
//! copying: only the first call in a reply runs, the asking turn goes back before its
//! result, three identical failures end the turn and a repeat carries the app's note,
//! successful automatic calls are free, the closing request has no tools, old results
//! are set aside — all of that is the app's code, and changes with it.
//!
//! **What it adds** is what a probe is and the app is not:
//!
//! - the person is taken out, so every call is approved — except that a read outside
//!   the folder is declined when [`Setup::confine_reads`] is set ([`read_allowed`])
//! - the little a probe varies about a round is [`Env`]'s: its sampling, its transport,
//!   the tools it offers, its own system prompt, where its backups go, and what a
//!   check's environment holds
//! - the counts ([`Run`]), written down from what the loop says as it says it: the
//!   rounds and the free ones, the calls the app would have stopped to ask about
//!   (`Run::asks`), the checks and how they ended, the size of each prompt

// Each probe is its own binary and uses only part of this — editprobe compares
// transports, fixprobe runs checks — so what one leaves unused is not dead.
#![allow(dead_code)]

use std::cell::RefCell;
use std::path::Path;
use std::time::{Duration, Instant};

use bifrost_core::agent::env::Env;
use bifrost_core::agent::round::{round_in, Reply, Round};
use bifrost_core::agent::run::{run_approved_in, Ran};
use bifrost_core::agent::ticket::PendingCall;
use bifrost_core::agent::turn::{self, Card, Decision, Frontend, Patch, RoundNumbers};
use bifrost_core::chat::{Message, Options, Tool, ToolCall};
use bifrost_core::config::Config;
use bifrost_core::edit::Workspace;
use bifrost_core::{check, tools};

/// How tool calls travel. **native**: Ollama parses them, and a call it cannot
/// parse fails the turn (G6, round 1). **recover**: `chat::chat_tools` — native
/// first, and when Ollama rejects a malformed call, the turn is re-asked raw and
/// the call repaired. What the app ships.
#[derive(Clone, Copy, PartialEq)]
pub enum Transport {
    Native,
    Recover,
}

impl Transport {
    pub fn name(self) -> &'static str {
        match self {
            Transport::Native => "native",
            Transport::Recover => "recover",
        }
    }
}

/// The check a probe offers, and the environment it runs in.
pub struct Check {
    pub argv: Vec<String>,
    /// A shared `CARGO_TARGET_DIR` so every run does not rebuild from nothing,
    /// and `CARGO_NET_OFFLINE` so no run can reach the network.
    pub env: Vec<(String, String)>,
}

/// Where the tools act.
pub struct Setup<'a> {
    pub ws: &'a Workspace,
    pub backups: &'a Path,
    pub check: Option<&'a Check>,
    /// Refuse reads outside the folder, as edits are. The app lets a person
    /// approve a read of anything; a probe approves every call, so where the
    /// answer exists elsewhere on disk — the task probe's worktree sits beside
    /// the repo holding the reference solution — a read must not reach it. The
    /// card is declined, so the model is told what the app tells it for a read the
    /// person said no to; see [`read_allowed`].
    pub confine_reads: bool,
    /// Set earlier results aside before each request, as the app does from H7
    /// (`bifrost_core::compact`). Off measures the loop as it was: the whole
    /// conversation sent every round, and Ollama's cut when it overflows.
    pub compact: bool,
}

/// Whether a probe that confines reads lets this one through: the file must exist
/// and resolve inside the folder, by the rule an edit's path is held to. A path that
/// leaves the folder, a link that leads out of it and `.git` are all refused.
pub fn read_allowed(ws: &Workspace, path: &str) -> bool {
    ws.resolve_existing(path).is_ok()
}

/// What one run of the loop did.
#[derive(Default)]
pub struct Run {
    /// The model's final words, if it stopped calling tools.
    pub answer: String,
    pub calls: Vec<String>,
    /// The undo id of every change the run made, oldest first — what `Undo all`
    /// would be given for this run.
    pub undo_ids: Vec<String>,
    /// `delete_file` and `move_file` calls, whether or not they worked — what H9's
    /// tools are used for — and how many times a file the run deleted was created
    /// again at the same path. That last is a rewrite done the long way round, which
    /// skips the diff an edit would have shown; the delete tool's description argues
    /// against it, and this counts whether the argument works.
    pub delete_calls: usize,
    pub move_calls: usize,
    pub delete_then_create: usize,
    /// Calls other than reads that came back as errors.
    pub edit_errors: usize,
    /// Reads that named a line range. In taskprobe every file is under 64 KB, so
    /// one means the model chose to page a file it could have read whole.
    pub ranged_reads: usize,
    /// Rounds that count against the bound: **a free round does not**, as in the
    /// app — an accepted `update_plan` (H10) and an automatic read, listing or
    /// search that succeeded (R-03).
    pub rounds: usize,
    /// `update_plan` calls the loop accepted — the model's task list updates.
    pub plan_updates: usize,
    /// Automatic reads, listings and searches that succeeded and so cost no round.
    /// Kept apart from `plan_updates`: this is the count that shows how much of a
    /// task the free rule carries.
    pub auto_calls: usize,
    /// Still calling tools when the bound ran out. The model then got one more
    /// request with no tools, and its words are in `answer`.
    pub hit_bound: bool,
    /// Ended by the app's loop-breaking: the same call failed three times.
    pub stopped: bool,
    /// Each call and what came back, for reading a failure rather than guessing.
    pub log: Vec<String>,
    pub recovered: usize,
    /// Each `run_check`, passed or not, in order.
    pub checks: Vec<bool>,
    /// How many of those never finished by themselves with an exit code — they
    /// timed out, were stopped, or ended with none (R-10). The model's check
    /// then told it nothing about its code, so the environment failed, not the
    /// model; a probe books a run that would fail with one of these as ERROR.
    pub check_timeouts: usize,
    /// Calls that would have shown a card under the app's default policy, and
    /// under the one before H5 (every call). Neither counts an edit rejected
    /// before its card, an invalid call, or one the loop-breaking stopped. A
    /// read of a folder or of a missing file counts in `asks_every_call` only:
    /// before H5 it got a card and failed after Allow; now it is rejected unasked.
    pub asks: usize,
    pub asks_every_call: usize,
    /// Smart App Control refused something a check needed: the run measured
    /// Windows, not the model.
    pub blocked: bool,
    /// Ollama failed — or an approved call could not be run at all. The run measured
    /// nothing — unless `runaway` is set.
    pub error: Option<String>,
    /// The failure was the model's: a reply that ran to the request's
    /// five-minute limit without finishing. Seen at H3 after reading a long,
    /// escape-heavy file — the model wrote one edit for ever. A result about
    /// the model, not about Ollama, so a probe scores it as a fail.
    pub runaway: bool,
    /// Ollama's `prompt_eval_count` for each request, in order.
    pub prompt_tokens: Vec<u32>,
    /// Results and long arguments set aside over the run (`Setup::compact`).
    pub set_aside: usize,
    /// Rounds where the prompt came back **smaller** than the round before while
    /// nothing had been set aside: inside a turn the prompt only grows, so a drop
    /// means Ollama cut it to fit `num_ctx` — measured at H7 to keep the system
    /// message and the tail and lose the user's request.
    pub truncated: usize,
}

/// Did a command end without an exit code of its own: timed out, stopped, or
/// exited with none? Its output then says nothing about the code it checked.
pub fn never_finished(ended: &check::Ended) -> bool {
    matches!(ended, check::Ended::TimedOut | check::Ended::Stopped | check::Ended::Exited(None))
}

/// A request that failed this close to `chat`'s five-minute limit was cut off
/// by it, not refused.
const RUNAWAY: Duration = Duration::from_secs(290);

fn short(s: &str, n: usize) -> String {
    let one = s.replace('\n', "⏎");
    if one.chars().count() > n { format!("{}…", one.chars().take(n).collect::<String>()) } else { one }
}

/// A call's path as the model wrote it, tidied — one file is one entry however it was
/// spelled.
fn tidy(arguments: &serde_json::Value) -> String {
    arguments["path"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .trim_matches(['"', '\'', '`'])
        .trim()
        .replace('\\', "/")
        .trim_start_matches("./")
        .to_lowercase()
}

/// What the loop's three hooks — the request, the run and the frontend — write down
/// between them. They are separate borrows of one turn, so it is shared through a
/// `RefCell`; each takes it for the length of one note and never across a call.
#[derive(Default)]
struct Book {
    run: Run,
    /// The call that asked and has not been answered yet: what [`Book::ran`] or
    /// [`Book::declined`] books once its outcome is known.
    waiting: Option<ToolCall>,
    /// The files this run deleted, as [`tidy`] spells them, to see which a later create
    /// puts back.
    deleted: Vec<String>,
    /// How long the last request that failed took, to tell a model that wrote until
    /// the limit from an Ollama that failed.
    failed_after: Option<Duration>,
}

impl Book {
    /// A request to the model has come back. `closing` is the last one of a turn that
    /// hit its bound. **A free round is given back**, as the loop gives it: an
    /// automatic call that worked. The round that answers counts, like any other.
    fn asked(&mut self, closing: bool, got: &Result<Round, String>, took: Duration) {
        if closing {
            self.run.hit_bound = true;
        } else {
            self.run.rounds += 1;
        }
        let round = match got {
            Ok(round) => round,
            Err(_) => {
                self.failed_after = Some(took);
                return;
            }
        };
        if round.recovered {
            self.run.recovered += 1;
            self.run.log.push("(ollama rejected a malformed call; recovered through the raw retry)".into());
        }
        match &round.reply {
            // Its words arrive through the frontend.
            Reply::Answer { .. } => {}
            Reply::Stuck { error } => {
                self.run.log.push(format!("stopped: {error}"));
                self.run.stopped = true;
            }
            Reply::Rejected { error, raw_call, .. } => {
                self.made(raw_call);
                // A read the rule turned away (a folder, a file that is not there) got a
                // card before H5, and failed after Allow.
                if raw_call.function.name == "read_file" && tools::interpret(raw_call).is_ok() {
                    self.run.asks_every_call += 1;
                }
                self.finished(raw_call, error, true);
            }
            Reply::Auto { name, result, ok, raw_call, .. } => {
                self.made(raw_call);
                if *ok {
                    if name == "update_plan" {
                        self.run.plan_updates += 1;
                    } else {
                        self.run.auto_calls += 1;
                    }
                    self.run.rounds = self.run.rounds.saturating_sub(1);
                }
                // Every call that reached a card asked before H5, a read, a listing or a
                // search that runs unasked now among them. The task list never had a card.
                if name != "update_plan" {
                    self.run.asks_every_call += 1;
                }
                self.finished(raw_call, result, !ok);
            }
            Reply::Call { raw_call, .. } => {
                self.made(raw_call);
                self.run.asks += 1;
                self.run.asks_every_call += 1;
                self.waiting = Some(raw_call.clone());
            }
        }
    }

    /// The model made this call, whatever becomes of it.
    fn made(&mut self, call: &ToolCall) {
        self.run.calls.push(call.function.name.clone());
        if call.function.name == "read_file" && tools::read_range(&call.function.arguments).ok().flatten().is_some() {
            self.run.ranged_reads += 1;
        }
    }

    /// What a call came to is known: log it, and count what it says. `failed` is what
    /// the app marks failed — a result that starts `error:` — so a check that finds
    /// errors, which ran, is not.
    fn finished(&mut self, call: &ToolCall, result: &str, failed: bool) {
        let name = call.function.name.as_str();
        self.run.log.push(format!(
            "{name}({}) → {}",
            short(&call.function.arguments.to_string(), 140),
            short(result, 140)
        ));
        if failed && !matches!(name, "read_file" | "update_plan") {
            self.run.edit_errors += 1;
        }
        match name {
            "delete_file" => {
                self.run.delete_calls += 1;
                if !failed {
                    self.deleted.push(tidy(&call.function.arguments));
                }
            }
            "move_file" => self.run.move_calls += 1,
            "create_file" if !failed && self.deleted.contains(&tidy(&call.function.arguments)) => {
                self.run.delete_then_create += 1;
            }
            _ => {}
        }
    }

    /// The call that asked was approved and run.
    fn ran(&mut self, ran: &Ran) {
        let Some(call) = self.waiting.take() else { return };
        self.run.undo_ids.extend(ran.undo.clone());
        if let Some(o) = &ran.check {
            self.run.checks.push(o.passed());
            if never_finished(&o.ended) && o.blocked.is_none() {
                self.run.check_timeouts += 1;
            }
            if o.blocked.is_some() {
                self.run.blocked = true;
            }
        }
        // **Not `!ran.ok`**: a check that finds errors worked, and what it found is the news.
        self.finished(&call, &ran.result, ran.result.trim_start().starts_with("error:"));
    }

    /// The call that asked was declined, and the model is told so.
    fn declined(&mut self) {
        let Some(call) = self.waiting.take() else { return };
        self.finished(&call, "declined: read outside the folder", true);
    }

    /// What a round cost. A count of 0 is a reply that did not report one — not a
    /// prompt of no tokens — so it neither counts as a cut nor is kept.
    fn counted(&mut self, numbers: RoundNumbers) {
        self.run.set_aside += numbers.set_aside;
        if numbers.prompt_eval_count == 0 {
            return;
        }
        if let Some(&before) = self.run.prompt_tokens.last()
            && numbers.prompt_eval_count < before
            && numbers.set_aside == 0
        {
            self.run.truncated += 1;
        }
        self.run.prompt_tokens.push(numbers.prompt_eval_count);
    }

    /// The model's words: its answer, or what it said at the bound.
    fn answered(&mut self, content: &str) {
        let label = if self.run.hit_bound { "closing answer" } else { "answer" };
        self.run.log.push(format!("{label}: {}", short(content, 300)));
        self.run.answer = content.to_string();
    }

    /// Something the person would have been told: the bound that was hit, say.
    fn said(&mut self, message: &str) {
        self.run.log.push(format!("said: {message}"));
    }

    /// The run, once the turn is over; `done` is how the loop ended.
    fn finish(mut self, done: Result<(), String>) -> Run {
        if let Err(e) = done {
            self.run.runaway = self.failed_after.is_some_and(|took| took >= RUNAWAY);
            self.run.error = Some(e);
        }
        self.run
    }
}

/// The person, taken out: every card is approved, and what the loop says is written
/// down. The one exception is a read outside the folder when the probe confines reads.
struct Fe<'a> {
    book: &'a RefCell<Book>,
    ws: &'a Workspace,
    confine_reads: bool,
}

impl Frontend for Fe<'_> {
    fn stopped(&self) -> bool {
        false
    }

    fn status(&mut self, _text: &str) {}

    fn round(&mut self, numbers: RoundNumbers) {
        self.book.borrow_mut().counted(numbers);
    }

    fn answer(&mut self, content: &str, _prompt_eval_count: u32, _eval_count: u32) {
        self.book.borrow_mut().answered(content);
    }

    fn error(&mut self, message: &str) {
        self.book.borrow_mut().said(message);
    }

    fn card(&mut self, _card: Card) -> usize {
        0
    }

    fn update(&mut self, _card: usize, _patch: Patch) {}

    fn decide(&mut self, _card: usize, call: &PendingCall) -> Decision {
        // A read's card names the file that would really be opened.
        if self.confine_reads && call.name == "read_file" && !read_allowed(self.ws, &call.path) {
            self.book.borrow_mut().declined();
            return Decision::Deny;
        }
        Decision::Allow
    }

    fn save(&mut self) {}
}

/// The app's turn, run to its end with the person taken out, and what it came to.
/// `ask` is one request to the model and `run` runs an approved call — the real ones
/// are [`run_loop`]'s; a test scripts them.
fn drive(
    convo: &mut Vec<Message>,
    bound: usize,
    s: &Setup,
    ask: &mut turn::Ask<'_>,
    run: &mut turn::Run<'_>,
) -> Run {
    let book = RefCell::new(Book::default());
    let done = turn::run_turn_with(
        convo,
        // Nothing to calibrate the first round from; each round after it learns this
        // model's characters per token from the one before, as the app's does.
        None,
        bound,
        &mut |messages, calibration, closing| {
            let started = Instant::now();
            let got = ask(messages, calibration, closing);
            book.borrow_mut().asked(closing, &got, started.elapsed());
            got
        },
        &mut |name, path, token| {
            let got = run(name, path, token);
            if let Ok(ran) = &got {
                book.borrow_mut().ran(ran);
            }
            got
        },
        &mut Fe { book: &book, ws: s.ws, confine_reads: s.confine_reads },
    );
    book.into_inner().finish(done)
}

/// The config a round reads, for the folder and the check a probe set up: the folder
/// is the workspace, its check (if it has one) is keyed by it as the app's is, and
/// every call that needs no card runs without one, as it does by default.
fn round_config(s: &Setup, opts: Options) -> Config {
    let root = s.ws.root_display();
    let checks = s.check.map(|c| (root.clone(), c.argv.clone())).into_iter().collect();
    Config { workspace: Some(root), checks, num_ctx: opts.num_ctx, ask_every_call: false, ..Config::default() }
}

/// The setting a probe runs the app's round in: what it varies, and the rest the
/// app's. `system` is the prompt it worded, which the turn puts in front itself.
fn round_env(system: Option<String>, offered: &[Tool], opts: Options, transport: Transport, s: &Setup) -> Env {
    Env {
        options: Some(opts),
        recover: transport == Transport::Recover,
        offered: Some(offered.to_vec()),
        system,
        compact: s.compact,
        backups: Some(s.backups.to_path_buf()),
        check_env: s.check.map(|c| c.env.clone()).unwrap_or_default(),
    }
}

/// The system prompt a probe's conversation starts with, taken out of it: the turn
/// puts it in front of every request itself, from `Env::system`.
fn take_system(messages: &mut Vec<Message>) -> Option<Message> {
    messages.first().is_some_and(|m| m.role == "system").then(|| messages.remove(0))
}

/// Drive the conversation until the model answers, the bound runs out, the
/// loop-breaking stops it, or Ollama fails.
///
/// **This is the app's turn** (`turn::run_turn_with`), asking through `round_in` and
/// running through `run_approved_in`, in the setting `opts`, `transport`, `offered` and
/// `s` make ([`Env`]). `messages` starts with the system prompt, as every probe's does:
/// it is lifted out for the turn and put back in front after, so `messages` ends as the
/// conversation the model has seen.
///
/// `bound` is the probe's own. The app stops at `tools::MAX_ROUNDS`; a probe can
/// run past it to record how many rounds a task really takes (H7 sets the app's
/// bound from that).
///
/// **A free call costs no round** (H10, R-03): an accepted `update_plan` and an
/// automatic read, listing or search that succeeded are not counted against
/// `bound` or in `Run::rounds`, and `tools::plan_updates` — the app's own rule —
/// rejects the 7th plan update in a turn, which does count. So the loop runs at most
/// `bound + MAX_FREE_CALLS` requests.
///
/// **A turn that hits the bound gets one closing request with no tools** and
/// `CLOSING_NOTE` after the conversation, as the app's does, so the model ends in
/// its own words (`Run::answer`). `hit_bound` is set by that request. If it fails it
/// is only logged: the task's work is judged elsewhere.
pub fn run_loop(
    model: &str,
    messages: &mut Vec<Message>,
    offered: &[Tool],
    opts: Options,
    transport: Transport,
    bound: usize,
    s: &Setup,
) -> Run {
    let cfg = round_config(s, opts);
    let system = take_system(messages);
    let env = round_env(system.as_ref().map(|m| m.content.clone()), offered, opts, transport, s);
    let run = drive(
        messages,
        bound,
        s,
        &mut |messages, calibration, closing| round_in(messages, model, &cfg, &env, calibration, closing),
        &mut |name, path, token| run_approved_in(name, path, token, &env),
    );
    if let Some(system) = system {
        messages.insert(0, system);
    }
    run
}

#[cfg(test)]
mod tests {
    use super::*;
    use bifrost_core::agent::send::{compact_for_send, Sent};
    use bifrost_core::chat;
    use serde_json::json;
    use std::collections::VecDeque;
    use std::path::PathBuf;

    /// A throwaway project at `base/tree`, with `base/backups` beside it, removed on drop.
    struct Sandbox {
        base: PathBuf,
        ws: Workspace,
        backups: PathBuf,
    }

    impl Sandbox {
        fn new(tag: &str) -> Self {
            let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let base = std::env::temp_dir().join(format!("bifrost-probe-{tag}-{}-{n}", std::process::id()));
            let tree = base.join("tree");
            std::fs::create_dir_all(&tree).unwrap();
            let ws = Workspace::open(&tree).unwrap();
            let backups = base.join("backups");
            Self { base, ws, backups }
        }

        fn setup(&self, confine_reads: bool) -> Setup<'_> {
            Setup { ws: &self.ws, backups: &self.backups, check: None, confine_reads, compact: true }
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    // ── a script for the model, and for what runs ───────────────────────────

    fn sent() -> Sent {
        compact_for_send(&mut [], &[], 8192, None, 0)
    }

    fn round_of(reply: Reply) -> Result<Round, String> {
        Ok(Round { reply, prompt_eval_count: 0, recovered: false, sent: sent() })
    }

    fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall { id: None, function: chat::FunctionCall { name: name.into(), arguments } }
    }

    fn answer(text: &str) -> Result<Round, String> {
        round_of(Reply::Answer { content: text.into(), eval_count: 7 })
    }

    /// A call that needs no card, already run.
    fn auto(name: &str, arguments: serde_json::Value, result: &str, ok: bool) -> Result<Round, String> {
        round_of(Reply::Auto {
            name: name.into(),
            path: String::new(),
            range: None,
            result: result.into(),
            ok,
            stopped: false,
            plan: None,
            raw_call: call(name, arguments),
        })
    }

    /// A call turned away without asking anyone.
    fn rejected(name: &str, arguments: serde_json::Value, error: &str) -> Result<Round, String> {
        round_of(Reply::Rejected {
            name: name.into(),
            path: String::new(),
            error: error.into(),
            raw_call: call(name, arguments),
        })
    }

    /// A call that asks. `shown` is the path its card names, and its ticket is `t-<shown>`.
    fn asks_about(name: &str, arguments: serde_json::Value, shown: &str) -> Result<Round, String> {
        round_of(Reply::Call {
            call: PendingCall {
                token: format!("t-{shown}"),
                name: name.into(),
                path: shown.into(),
                diff: None,
                range: None,
                changed_build_files: Vec::new(),
                to: None,
                folders: Vec::new(),
                size: None,
            },
            raw_call: call(name, arguments),
        })
    }

    fn ran(result: &str) -> Result<Ran, String> {
        Ok(Ran::text(result.into()))
    }

    /// What a script came to.
    struct Played {
        run: Run,
        /// The conversation the turn grew.
        convo: Vec<Message>,
        /// For each request to the model, whether it was the closing one.
        closing: Vec<bool>,
        /// For each request to the model, how many messages it was sent.
        sent: Vec<usize>,
        /// Each call that was run: `name:path:ticket`.
        ran: Vec<String>,
    }

    /// Run the loop on a script: the model's replies in order, and what each approved
    /// call comes to. Panics if the loop asks for more than the script holds, so a
    /// request that should not have been made is a failure in itself.
    fn play(s: &Setup, bound: usize, asks: Vec<Result<Round, String>>, runs: Vec<Result<Ran, String>>) -> Played {
        let mut convo = vec![Message::user("go")];
        let (mut asks, mut runs) = (VecDeque::from(asks), VecDeque::from(runs));
        let (mut closing, mut sent_len, mut handed) = (Vec::new(), Vec::new(), Vec::new());
        let run = drive(
            &mut convo,
            bound,
            s,
            &mut |messages, _, was_closing| {
                closing.push(was_closing);
                sent_len.push(messages.len());
                asks.pop_front().expect("the loop asked the model more often than the script allows")
            },
            &mut |name, path, token| {
                handed.push(format!("{name}:{path}:{token}"));
                runs.pop_front().expect("the loop ran a call the script did not expect")
            },
        );
        Played { run, convo, closing, sent: sent_len, ran: handed }
    }

    fn outcome(ended: check::Ended, blocked: Option<&str>) -> check::Outcome {
        check::Outcome {
            ended,
            elapsed: Duration::ZERO,
            output: String::new(),
            bytes: 0,
            complete: true,
            blocked: blocked.map(str::to_string),
        }
    }

    /// A check's result, as `run_approved` makes one.
    fn checked(o: check::Outcome) -> Result<Ran, String> {
        Ok(Ran { ok: o.passed(), check: Some(o), ..Ran::text("cargo check: the output".into()) })
    }

    // ── rounds ──────────────────────────────────────────────────────────────

    /// **A free round does not count, and the answering round does.** An automatic read
    /// that worked gives its round back; an approved edit keeps its own.
    #[test]
    fn a_free_automatic_read_costs_no_round_and_an_approved_edit_does() {
        let dir = Sandbox::new("free");
        let edit = json!({ "path": "a.txt", "old_text": "one", "new_text": "1" });
        let done = "Edited a.txt: 1 line removed, 1 added.";
        let played = play(
            &dir.setup(false),
            10,
            vec![
                auto("read_file", json!({ "path": "a.txt" }), "one\n", true),
                asks_about("edit_file", edit, "a.txt"),
                answer("Done."),
            ],
            vec![Ok(Ran { undo: Some("u1".into()), ..Ran::text(done.into()) })],
        );
        let r = played.run;
        assert_eq!((r.rounds, r.auto_calls, r.plan_updates), (2, 1, 0));
        assert_eq!(r.calls, ["read_file", "edit_file"]);
        assert_eq!((r.asks, r.asks_every_call, r.edit_errors), (1, 2, 0));
        assert_eq!(r.undo_ids, ["u1"]);
        assert_eq!(r.answer, "Done.");
        assert!(!r.hit_bound && !r.stopped && r.error.is_none() && !r.runaway);
        assert_eq!(played.ran, ["edit_file:a.txt:t-a.txt"]);
        assert_eq!(played.closing, [false, false, false]);
        // One line per call, then the answer.
        assert_eq!(r.log.len(), 3);
        assert_eq!(r.log[0], "read_file({\"path\":\"a.txt\"}) → one⏎");
        assert!(r.log[1].starts_with("edit_file({") && r.log[1].ends_with(&format!(") → {done}")), "{}", r.log[1]);
        assert_eq!(r.log[2], "answer: Done.");
        // The conversation is the app's: each call then its result.
        assert_eq!(played.convo.len(), 1 + 2 * 2);
        assert_eq!(played.convo[2], Message::tool_result("read_file", "one\n"));
        assert_eq!(played.convo[4], Message::tool_result("edit_file", done));
    }

    #[test]
    fn an_accepted_task_list_update_is_free_and_a_rejected_one_costs_a_round() {
        let dir = Sandbox::new("plan");
        let bad = "error: update_plan was called without any steps";
        let played = play(
            &dir.setup(false),
            10,
            vec![
                auto("update_plan", json!({ "plan": "[ ] a" }), "Plan updated: 0 of 1 done.", true),
                // A malformed list, and the 7th: rejected, and each costs a round.
                rejected("update_plan", json!({}), bad),
                rejected("update_plan", json!({ "plan": "[ ] again" }), tools::PLAN_LIMIT_NOTE),
                answer("Done."),
            ],
            vec![],
        );
        let r = played.run;
        assert_eq!((r.plan_updates, r.auto_calls), (1, 0));
        assert_eq!(r.rounds, 3, "the two rejections and the answer");
        // The task list is never a card, and never an edit error.
        assert_eq!((r.asks, r.asks_every_call, r.edit_errors), (0, 0, 0));
        assert_eq!(r.log[2], format!("update_plan({{\"plan\":\"[ ] again\"}}) → {}", short(tools::PLAN_LIMIT_NOTE, 140)));
    }

    /// **What counts as an ask is the app's own rule**, carried by the kind of reply: a
    /// call that reached a card asks; an automatic one never does but asked before H5; a
    /// read turned away (a folder, a missing file) asked before H5 only; an edit turned
    /// away, a malformed call and the task list never did.
    #[test]
    fn asks_follow_the_apps_rule_and_asks_every_call_the_one_before_h5() {
        let dir = Sandbox::new("asks");
        let edit = json!({ "path": "a.txt", "old_text": "hello", "new_text": "bye" });
        let cases = [
            ("an automatic search", auto("search", json!({ "text": "hello" }), "a.txt:1: hello", true), (0, 1)),
            ("an automatic read", auto("read_file", json!({ "path": "a.txt" }), "hello\n", true), (0, 1)),
            ("a listing that failed", auto("list_files", json!({}), "error: could not read", false), (0, 1)),
            ("the task list", auto("update_plan", json!({ "plan": "[ ] a" }), "Plan updated.", true), (0, 0)),
            ("an edit that asks", asks_about("edit_file", edit, "a.txt"), (1, 1)),
            ("a check that asks", asks_about("run_check", json!({}), "cargo check"), (1, 1)),
            (
                "a read of a folder",
                rejected("read_file", json!({ "path": "src" }), "error: src is a folder; use list_files"),
                (0, 1),
            ),
            (
                "a read the tools could not read",
                rejected("read_file", json!({ "path": "a.txt", "start_line": "abc" }), "error: start_line"),
                (0, 0),
            ),
            (
                "an edit that could never apply",
                rejected("edit_file", json!({ "path": "a.txt", "old_text": "nope" }), "error: not found"),
                (0, 0),
            ),
            (
                "a call that could not be read",
                rejected(chat::INVALID_TOOL_CALL, json!({ "error": "bad" }), "error: bad. Send the call again."),
                (0, 0),
            ),
        ];
        for (what, reply, want) in cases {
            let played = play(&dir.setup(false), 10, vec![reply, answer("ok")], vec![ran("ok")]);
            assert_eq!((played.run.asks, played.run.asks_every_call), want, "{what}");
        }
    }

    // ── what failed ─────────────────────────────────────────────────────────

    /// **A check that finds errors worked**, and is not an edit error; an edit refused
    /// at the last moment is. What fails is what the app marks failed: a result that
    /// starts `error:` — not `Ran::ok`, which a failing check also clears.
    #[test]
    fn a_failed_check_result_is_not_an_edit_error_but_a_refused_edit_is() {
        let dir = Sandbox::new("failed");
        let broken = outcome(check::Ended::Exited(Some(101)), None);
        let played = play(
            &dir.setup(false),
            10,
            vec![
                asks_about("run_check", json!({}), "cargo check"),
                asks_about("edit_file", json!({ "path": "a.txt" }), "a.txt"),
                asks_about("read_file", json!({ "path": "gone.txt" }), "gone.txt"),
                answer("Hm."),
            ],
            vec![checked(broken), ran("error: a.txt changed since the diff was shown"), ran("error: cannot read gone.txt")],
        );
        let r = played.run;
        // The refused edit only: the check ran, and a failed read is not an edit error.
        assert_eq!(r.edit_errors, 1);
        assert_eq!(r.checks, [false]);
        assert_eq!(r.rounds, 4);
    }

    #[test]
    fn a_run_that_could_not_be_run_at_all_is_the_runs_error() {
        let dir = Sandbox::new("unrun");
        let played = play(
            &dir.setup(false),
            10,
            vec![asks_about("edit_file", json!({ "path": "a.txt" }), "a.txt")],
            vec![Err("this call was not approved".into())],
        );
        let r = played.run;
        assert_eq!(r.error.as_deref(), Some("this call was not approved"));
        assert!(!r.runaway && !r.hit_bound);
        // It was asked about, and nothing came of it.
        assert_eq!((r.asks, r.edit_errors), (1, 0));
    }

    // ── the end of a turn ───────────────────────────────────────────────────

    /// **The closing request is what sets `hit_bound`**, and its words are the answer.
    #[test]
    fn the_closing_request_sets_hit_bound_and_its_words_are_the_answer() {
        let dir = Sandbox::new("bound");
        let no = |n: usize| rejected("edit_file", json!({ "path": format!("f{n}.rs") }), "error: old_text is not in the file");
        let played = play(&dir.setup(false), 2, vec![no(0), no(1), answer("I could not make the edit.")], vec![]);
        assert_eq!(played.closing, [false, false, true]);
        let r = played.run;
        assert!(r.hit_bound && !r.stopped && r.error.is_none());
        assert_eq!(r.rounds, 2, "the closing request is not a round");
        assert_eq!(r.answer, "I could not make the edit.");
        assert_eq!(r.edit_errors, 2);
        // The model's words come first, then what the person would have been shown.
        assert_eq!(r.log[2], "closing answer: I could not make the edit.");
        assert_eq!(r.log[3], "said: Stopped after 2 tool rounds.");
        assert_eq!(r.log.len(), 4);
        // The closing request is sent everything the turn said, and a turn that
        // answered on its own never made one.
        assert_eq!(played.sent, [1, 3, 5]);
        assert_eq!(played.convo.len(), 5);
        let done = play(&dir.setup(false), 2, vec![no(0), answer("Done.")], vec![]);
        assert!(!done.run.hit_bound);
        assert_eq!(done.closing, [false, false]);
    }

    #[test]
    fn a_closing_request_that_fails_is_said_and_is_not_the_runs_error() {
        let dir = Sandbox::new("closing-failed");
        let no = |n: usize| rejected("edit_file", json!({ "path": format!("f{n}.rs") }), "error: old_text is not in the file");
        let played = play(&dir.setup(false), 2, vec![no(0), no(1), Err("POST /api/chat: timed out".into())], vec![]);
        let r = played.run;
        assert!(r.hit_bound && r.error.is_none() && !r.runaway);
        assert_eq!(r.answer, "");
        assert_eq!(
            r.log.last().map(String::as_str),
            Some("said: Stopped after 2 tool rounds. The closing request failed: POST /api/chat: timed out")
        );
    }

    #[test]
    fn a_model_the_app_gives_up_on_is_stopped_and_the_run_says_why() {
        let dir = Sandbox::new("stuck");
        let why = "the model kept repeating a call to edit_file that could not be carried out, so it was stopped";
        let played = play(
            &dir.setup(false),
            10,
            vec![
                rejected("edit_file", json!({ "path": "a.rs" }), "error: old_text is not in the file"),
                round_of(Reply::Stuck { error: why.into() }),
            ],
            vec![],
        );
        let r = played.run;
        assert!(r.stopped && !r.hit_bound && r.error.is_none());
        assert_eq!(played.closing, [false, false], "no closing request after being stopped");
        assert_eq!(r.rounds, 2);
        assert_eq!(r.log[1], format!("stopped: {why}"));
    }

    #[test]
    fn a_failed_request_is_the_runs_error_and_only_one_that_ran_to_the_limit_is_runaway() {
        let dir = Sandbox::new("error");
        let played = play(&dir.setup(false), 10, vec![Err("POST /api/chat: connection refused".into())], vec![]);
        let r = played.run;
        assert_eq!(r.error.as_deref(), Some("POST /api/chat: connection refused"));
        assert!(!r.runaway && !r.hit_bound);
        assert_eq!(r.rounds, 1, "the request that failed was a round");
        // The same failure after as long as the request's own limit is the model's.
        let mut book = Book::default();
        book.asked(false, &Err("POST /api/chat: timed out".into()), RUNAWAY);
        let r = book.finish(Err("POST /api/chat: timed out".into()));
        assert!(r.runaway && r.error.is_some());
        let mut book = Book::default();
        book.asked(false, &Err("POST /api/chat: refused".into()), RUNAWAY - Duration::from_secs(1));
        assert!(!book.finish(Err("POST /api/chat: refused".into())).runaway);
    }

    // ── reads outside the folder ────────────────────────────────────────────

    /// **The model cannot read its way to the answer**: a read outside the folder, or of
    /// `.git`, is declined at its card when reads are confined — logged, told to the
    /// model, never run — and one inside goes through.
    #[test]
    fn a_confined_read_outside_the_folder_is_denied_and_logged() {
        let dir = Sandbox::new("confine");
        std::fs::write(dir.base.join("tree/a.txt"), "hello\n").unwrap();
        std::fs::write(dir.base.join("tree/.git"), "gitdir: C:/somewhere/.git/worktrees/x\n").unwrap();
        std::fs::write(dir.base.join("answer.rs"), "the reference solution\n").unwrap();
        let outside = dir.base.join("answer.rs").to_string_lossy().into_owned();

        let outside_args = json!({ "path": outside });
        let played = play(
            &dir.setup(true),
            10,
            vec![
                asks_about("read_file", outside_args.clone(), &outside),
                asks_about("read_file", json!({ "path": "../answer.rs" }), &outside),
                asks_about("read_file", json!({ "path": ".git" }), ".git"),
                asks_about("read_file", json!({ "path": "a.txt" }), "a.txt"),
                answer("Done."),
            ],
            vec![ran("hello\n")],
        );
        // Only the read inside the folder ran.
        assert_eq!(played.ran, ["read_file:a.txt:t-a.txt"]);
        let r = played.run;
        assert_eq!(r.log[0], format!("read_file({}) → declined: read outside the folder", short(&outside_args.to_string(), 140)));
        assert_eq!(r.log[1], "read_file({\"path\":\"../answer.rs\"}) → declined: read outside the folder");
        assert_eq!(r.log[2], "read_file({\"path\":\".git\"}) → declined: read outside the folder");
        assert_eq!(r.log[3], "read_file({\"path\":\"a.txt\"}) → hello⏎");
        // A declined read is not an edit error, and the app would have asked about each.
        assert_eq!((r.edit_errors, r.asks, r.rounds), (0, 4, 5));
        // The model is told what the app tells it for a card the person said no to.
        assert_eq!(played.convo[2], Message::tool_result("read_file", format!("The user declined to let you read {outside}.")));
        assert_eq!(played.convo[8], Message::tool_result("read_file", "hello\n"));
    }

    #[test]
    fn reads_that_are_not_confined_are_approved_wherever_they_are() {
        let dir = Sandbox::new("unconfined");
        let outside = dir.base.join("answer.rs").to_string_lossy().into_owned();
        let played = play(
            &dir.setup(false),
            10,
            vec![asks_about("read_file", json!({ "path": outside }), &outside), answer("Done.")],
            vec![ran("the reference solution\n")],
        );
        assert_eq!(played.ran, [format!("read_file:{outside}:t-{outside}")]);
        assert!(played.run.log[0].ends_with(" → the reference solution⏎"), "{}", played.run.log[0]);
    }

    #[test]
    fn a_confined_read_is_allowed_only_for_a_file_that_exists_inside_the_folder() {
        let dir = Sandbox::new("allowed");
        std::fs::create_dir_all(dir.base.join("tree/src")).unwrap();
        std::fs::write(dir.base.join("tree/src/lib.rs"), "pub fn inside() {}\n").unwrap();
        std::fs::write(dir.base.join("tree/.git"), "gitdir: C:/somewhere/.git/worktrees/x\n").unwrap();
        std::fs::write(dir.base.join("answer.rs"), "the reference solution\n").unwrap();
        assert!(read_allowed(&dir.ws, "src/lib.rs"));
        assert!(!read_allowed(&dir.ws, "src"), "a folder");
        assert!(!read_allowed(&dir.ws, "src/missing.rs"), "a file that is not there");
        assert!(!read_allowed(&dir.ws, "../answer.rs"));
        assert!(!read_allowed(&dir.ws, &dir.base.join("answer.rs").to_string_lossy()));
        assert!(!read_allowed(&dir.ws, ".git"));
    }

    // ── the counts ──────────────────────────────────────────────────────────

    #[test]
    fn a_delete_a_move_and_a_create_at_a_deleted_path_are_counted_by_what_the_model_asked_for() {
        let dir = Sandbox::new("ops");
        let written = |text: &str, id: &str| Ok(Ran { undo: Some(id.into()), ..Ran::text(text.into()) });
        let played = play(
            &dir.setup(false),
            10,
            vec![
                // The same file however it is spelled.
                asks_about("delete_file", json!({ "path": "./Src\\Old.rs" }), "Src/Old.rs"),
                asks_about("create_file", json!({ "path": "src/old.rs", "content": "x" }), "src/old.rs"),
                asks_about("create_file", json!({ "path": "src/new.rs", "content": "y" }), "src/new.rs"),
                asks_about("move_file", json!({ "path": "a.rs", "to": "b.rs" }), "a.rs"),
                // A delete that was turned away is still a delete asked for, and not a file gone.
                rejected("delete_file", json!({ "path": "gone.rs" }), "error: gone.rs does not exist"),
                asks_about("create_file", json!({ "path": "gone.rs", "content": "z" }), "gone.rs"),
                answer("Done."),
            ],
            vec![
                written("Deleted Src/Old.rs.", "u1"),
                written("Created src/old.rs (1 lines).", "u2"),
                written("Created src/new.rs (1 lines).", "u3"),
                written("Moved a.rs to b.rs.", "u4"),
                written("Created gone.rs (1 lines).", "u5"),
            ],
        );
        let r = played.run;
        assert_eq!((r.delete_calls, r.move_calls, r.delete_then_create), (2, 1, 1));
        assert_eq!(r.undo_ids, ["u1", "u2", "u3", "u4", "u5"]);
        assert_eq!(r.edit_errors, 1, "the delete that was turned away");
    }

    #[test]
    fn checks_are_counted_by_how_they_ended() {
        let dir = Sandbox::new("checks");
        let ask = || asks_about("run_check", json!({}), "cargo check");
        let played = play(
            &dir.setup(false),
            10,
            vec![ask(), ask(), ask(), ask(), ask(), answer("Done.")],
            vec![
                checked(outcome(check::Ended::Exited(Some(0)), None)),
                checked(outcome(check::Ended::Exited(Some(101)), None)),
                checked(outcome(check::Ended::TimedOut, None)),
                checked(outcome(check::Ended::Stopped, None)),
                // Smart App Control refused the build: not a timeout, though it has no exit code.
                checked(outcome(check::Ended::Exited(None), Some("the program `rustc`"))),
            ],
        );
        let r = played.run;
        assert_eq!(r.checks, [true, false, false, false, false]);
        assert_eq!(r.check_timeouts, 2, "timed out and stopped, not blocked");
        assert!(r.blocked);
        assert_eq!((r.asks, r.edit_errors), (5, 0));
    }

    #[test]
    fn ranged_reads_and_the_recovery_are_counted() {
        let dir = Sandbox::new("ranged");
        let recovered = Round { recovered: true, ..auto("read_file", json!({ "path": "a.txt", "start_line": 2 }), "two\n", true).unwrap() };
        let whole = auto("read_file", json!({ "path": "a.txt" }), "one\ntwo\n", true);
        let played = play(&dir.setup(false), 10, vec![Ok(recovered), whole, answer("Done.")], vec![]);
        let r = played.run;
        assert_eq!((r.ranged_reads, r.recovered, r.auto_calls), (1, 1, 2));
        assert_eq!(r.log[0], "(ollama rejected a malformed call; recovered through the raw retry)");
        assert_eq!(r.rounds, 1, "only the answer");
    }

    #[test]
    fn the_prompt_sizes_are_kept_and_a_drop_with_nothing_set_aside_is_a_truncation() {
        let round = |prompt_eval_count, set_aside| RoundNumbers {
            prompt_eval_count,
            prompt_chars: 0,
            set_aside,
            fits: true,
            recovered: false,
        };
        let mut book = Book::default();
        book.counted(round(1000, 0));
        // A reply that did not say neither counts as a cut nor is kept.
        book.counted(round(0, 0));
        book.counted(round(1400, 0));
        // Fell with nothing set aside: Ollama cut the prompt.
        book.counted(round(900, 0));
        // Fell, but two results were set aside to make room.
        book.counted(round(700, 2));
        book.counted(round(800, 1));
        assert_eq!(book.run.prompt_tokens, [1000, 1400, 900, 700, 800]);
        assert_eq!(book.run.truncated, 1);
        assert_eq!(book.run.set_aside, 3);
    }

    // ── what the probe hands the app's round ────────────────────────────────

    #[test]
    fn the_round_reads_the_probes_folder_and_check_as_the_apps_config() {
        let dir = Sandbox::new("config");
        let cargo = Check {
            argv: vec!["cargo".into(), "check".into()],
            env: vec![("CARGO_TARGET_DIR".into(), "target".into())],
        };
        let opts = Options::measured(8192);
        let root = dir.ws.root_display();

        let mut with = dir.setup(false);
        with.check = Some(&cargo);
        let cfg = round_config(&with, opts);
        assert_eq!(cfg.workspace.as_deref(), Some(root.as_str()));
        assert_eq!(cfg.check_for(&root), Some(&cargo.argv));
        assert_eq!(cfg.num_ctx, 8192);
        assert!(!cfg.ask_every_call && cfg.instructions.is_empty());
        // The round opens the same folder from it.
        assert_eq!(bifrost_core::agent::workspace(&cfg).map(|ws| ws.root_display()), Some(root));

        assert!(round_config(&dir.setup(false), opts).checks.is_empty(), "no check, no entry");
    }

    #[test]
    fn the_probes_setting_is_the_apps_with_only_what_the_probe_varies_changed() {
        let dir = Sandbox::new("env");
        let cargo = Check { argv: vec!["cargo".into(), "check".into()], env: vec![("CARGO_NET_OFFLINE".into(), "true".into())] };
        let offered = tools::available(true, Some("cargo check"));
        let opts = Options::measured(8192);

        let mut setup = dir.setup(false);
        setup.check = Some(&cargo);
        setup.compact = false;
        let env = round_env(Some("the prompt".into()), &offered, opts, Transport::Native, &setup);
        assert!(!env.recover && !env.compact);
        assert!(env.options.is_some_and(|o| o.num_ctx == 8192 && o.temperature == Some(0.0)));
        assert_eq!(env.offered.as_ref().map(Vec::len), Some(offered.len()));
        assert_eq!(env.system.as_deref(), Some("the prompt"));
        assert_eq!(env.backups.as_deref(), Some(dir.backups.as_path()));
        assert_eq!(env.check_env, cargo.env);

        // What the app does, a probe does unless it says otherwise.
        let app = round_env(None, &offered, opts, Transport::Recover, &dir.setup(false));
        assert!(app.recover && app.compact && app.system.is_none() && app.check_env.is_empty());
    }

    #[test]
    fn the_system_prompt_is_lifted_off_the_front_of_the_conversation_if_it_has_one() {
        let mut messages = vec![Message::system("the prompt"), Message::user("go")];
        assert_eq!(take_system(&mut messages), Some(Message::system("the prompt")));
        assert_eq!(messages, [Message::user("go")]);
        assert_eq!(take_system(&mut messages), None);
        assert_eq!(messages, [Message::user("go")]);
    }
}
