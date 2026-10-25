//! The turn loop: ask the model, show what it asked for, wait for a decision, run
//! the call, send the result back, and go round again until the model answers.
//!
//! This is `askWithTools` from Mimir's `App.tsx` — the loop that lived in the
//! webview — ported step for step, so that Mimir, Hugin and the probes can all run
//! the one loop, and the probes measure exactly what ships. Everything a person
//! sees or decides goes through a [`Frontend`]; the loop itself touches no window,
//! no terminal and no Tauri.
//!
//! - [`run_turn`] — the entry a frontend calls: it loads the config before every
//!   round, as each `ask_with_tools` call did, and wires [`round`] and
//!   [`run_approved`] in
//! - [`run_turn_with`] — the loop itself, with the model and the runner passed in,
//!   so the tests can script both
//! - [`Frontend`] — what the loop needs from a window or a terminal: say something,
//!   show a card, ask for a decision, answer whether Stop was pressed
//!
//! **The bounds.** A turn is at most [`tools::MAX_ROUNDS`] rounds and at most
//! `MAX_ROUNDS + MAX_FREE_CALLS` requests. ([`run_turn`] passes `MAX_ROUNDS`;
//! [`run_turn_with`] takes the round bound, so that a probe can run past it to
//! record how many rounds a task really takes.) **A call that cannot change anything is
//! not a round** (H10, R-03): an accepted task-list update, and an automatic read,
//! listing or search that succeeded, give their round back. A failed automatic call,
//! a rejected one and any call that asked still count, and `requests` counts
//! everything, so a model reading in circles still stops. At either bound the model
//! is asked once more with no tools (the closing request), so the turn ends in its
//! own words, and the banner says which bound it was and whether the last step ran.
//!
//! **Stop is asked at every point** the loop could carry on: before a round, after
//! it, when a card goes up, when a decision comes back, after a run. Each point
//! ends the turn the way the webview did — a card that went up is marked `stopped`
//! and, where the earlier save may have missed it, [`Frontend::save`] is called
//! again; nothing after the Stop is sent to the model.
//!
//! **A failing check can keep a turn going — for a frontend that asks for it.** The model
//! may run the project's check, see it fail, and still end the turn with an answer that
//! says the work is done. A frontend that [insists on checks](Frontend::insist_on_checks)
//! does not take that answer: while the model's latest `run_check` is failing (a failed
//! check stays failing until a later one passes), the answer goes into the conversation
//! and the model is told the work is not done ([`tools::CHECK_FAILED_NOTE`]) — at most
//! [`MAX_PUSH_BACKS`] times in a turn, each one a round. Past that the answer is taken,
//! and the frontend is told the turn ended with its check failing
//! ([`Frontend::ended_failing`]). Mimir does not ask, so for it nothing changes.
//!
//! **Mimir runs it (I1).** Its `run_turn` command drives [`run_turn`] with the
//! window as the [`Frontend`], and the webview only shows what it is told. The probes
//! (2c) drive [`run_turn_with`] with a frontend that approves everything and writes
//! down what it is told.

use serde::Serialize;

use super::round::{round, Reply, Round};
use super::run::{run_approved, Ran};
use super::send::Calibration;
use super::ticket::{DiffView, PendingCall};
use crate::chat::{self, Message};
use crate::check::Ended;
use crate::{config, ollama, tools};

/// The most times one turn pushes the model back to fix a failing check instead of taking
/// its answer, for a frontend that [insists on checks](Frontend::insist_on_checks). Three
/// is enough for a model that was careless once; one that has been told three times and
/// still ends the turn is not going to fix it, and the turn should end rather than loop.
pub const MAX_PUSH_BACKS: usize = 3;

/// What a person, or a rule, said to a card that asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
    /// Allow, and do not ask again about this exact call for the rest of the
    /// session. **For now it behaves as [`Decision::Allow`]**: the loop remembers
    /// nothing, and the next identical call asks again. Hugin's I3 gives it memory.
    AllowForSession,
}

/// What one round cost, for a frontend's meters. Said after every round of the
/// turn but not the closing request.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoundNumbers {
    /// Ollama's own count of the prompt this round answered; 0 when it did not say.
    pub prompt_eval_count: u32,
    /// Characters actually sent, after anything was set aside.
    pub prompt_chars: u64,
    /// How many old results were set aside to fit **this** round. A frontend that
    /// shows a running total adds them up.
    pub set_aside: usize,
    /// Whether the prompt fit the context once those were set aside. Said every
    /// round, so a round that did not fit and a later one that does (the first
    /// round guesses; the next is measured) clears the warning.
    pub fits: bool,
    /// Ollama rejected a malformed call and the round recovered it through the raw
    /// retry. Mimir logs it; the probes count it.
    pub recovered: bool,
    /// Ollama could not read the model's call and it was sent back to the model as a
    /// rejection to try again. Counted apart from `recovered`: that call was read.
    pub fed_back: bool,
}

/// Where a tool card stands. **pending**: waiting on a person. **running**:
/// approved and under way. **ran**: done. **denied**: the person said no.
/// **rejected**: it could never have worked and went straight back to the model.
/// **failed**: approved, but refused at the last moment (the file changed after the
/// diff was shown). **stopped**: ended by Stop.
///
/// The webview has one more, `undone`, which only an undo sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CardState {
    Pending,
    Running,
    Ran,
    Failed,
    Denied,
    Stopped,
    Rejected,
}

/// A tool card, as the thread first shows it: what the model asked for and what
/// happened. The fields are the webview's tool entry, in its spelling, so 2b can
/// send one as it stands.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Card {
    pub name: String,
    /// What the card names: the file for a read or an edit, the command for a
    /// check.
    pub path: String,
    pub state: CardState,
    /// The arguments the model sent, replayed as sent — always an object, so a call
    /// with none at all (a check, say) is `{}`, not rebuilt as `{path}`. Not set on
    /// a rejected card, which has nothing worth replaying.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Value>,
    /// The diff to approve, for an edit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<DiffView>,
    /// For a read of part of a file: the lines, as Rust normalised them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<tools::Lines>,
    /// For a check: build files an edit changed earlier in the turn.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changed_build_files: Vec<String>,
    /// For a move: where the file goes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// For a create or a move: the folders it will make.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub folders: Vec<String>,
    /// For a delete or a move: the file's size in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Whether it worked, as Rust reported it. A check that finds a broken build
    /// ran fine and still failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    /// What went back to the model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The handle to undo a change that was written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub undo: Option<String>,
    /// Ran without asking: a read, listing or search inside the project folder, or
    /// the task list. Saved with the thread, so reopening it still says nothing was
    /// approved by hand.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub auto: bool,
    /// For `update_plan`: the task list as Rust parsed it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<Vec<tools::PlanStep>>,
}

impl Card {
    /// A card with a name, a path and a state, and nothing else yet.
    pub fn new(name: impl Into<String>, path: impl Into<String>, state: CardState) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
            state,
            args: None,
            diff: None,
            range: None,
            changed_build_files: Vec::new(),
            to: None,
            folders: Vec::new(),
            size: None,
            ok: None,
            result: None,
            error: None,
            undo: None,
            auto: false,
            plan: None,
        }
    }
}

/// What changes on a card after it is shown: the fields the loop ever sets, the
/// webview's `{ ...card, ...patch }`. A field left out stays as it was.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Patch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<CardState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub undo: Option<String>,
}

impl Patch {
    /// A patch that only moves the card to another state.
    pub fn state(state: CardState) -> Self {
        Self { state: Some(state), ..Self::default() }
    }
}

/// What the loop needs from whatever shows the turn: a webview, a terminal.
///
/// Every method is called from the one thread running the loop, in the order the
/// webview's loop did things. None may fail: a frontend that cannot show something
/// has nothing better to do than carry on.
pub trait Frontend {
    /// Whether Stop was pressed. Asked at every point the loop could carry on, so
    /// it should be cheap.
    fn stopped(&self) -> bool;

    /// What the turn is waiting for, such as `Starting Ollama…`; empty clears it.
    fn status(&mut self, text: &str);

    /// What a round cost, after every round of the turn.
    fn round(&mut self, numbers: RoundNumbers);

    /// The model's words: its answer, or what it said at the bound. `prompt_eval_count`
    /// and `eval_count` are Ollama's own counts for the request that produced it.
    fn answer(&mut self, content: &str, prompt_eval_count: u32, eval_count: u32);

    /// Something the person has to be told: the turn stopped for a reason, or ran
    /// into a bound.
    fn error(&mut self, message: &str);

    /// Show a tool card. Returns the card's position, which is how [`Frontend::update`]
    /// and [`Frontend::decide`] name it afterwards — by position, never "the last
    /// one": a result that lands after Stop can find a new request's cards after it.
    fn card(&mut self, card: Card) -> usize;

    /// Change a card that was shown.
    fn update(&mut self, card: usize, patch: Patch);

    /// Wait for a decision on the card that asked. **Stop must end the wait**: a
    /// frontend that is stopped while it waits answers [`Decision::Deny`], and the
    /// loop, seeing [`Frontend::stopped`], goes no further.
    fn decide(&mut self, card: usize, call: &PendingCall) -> Decision;

    /// Save the thread as it now stands. Called only where the turn ends on a Stop
    /// with a card on screen that the Stop's own save may have missed.
    fn save(&mut self);

    /// Whether a check that failed keeps the turn going. **Off unless a frontend says
    /// otherwise**: Mimir's turns end on the model's answer, failing check or not.
    ///
    /// When it is on and the model answers while its latest `run_check` is failing, that
    /// answer does not end the turn. It goes into the conversation, the model is told the
    /// work is not done and to fix it ([`tools::CHECK_FAILED_NOTE`]), and the loop goes on —
    /// at most [`MAX_PUSH_BACKS`] times in a turn, after which the answer is taken as it is.
    /// An answer pushed back is not passed to [`Frontend::answer`]. A check that failed
    /// stays failing until a later one passes; one that never judged the code (it timed out,
    /// was stopped or refused by Smart App Control, or could not start) changes nothing.
    fn insist_on_checks(&self) -> bool {
        false
    }

    /// The turn is over and the model's latest check is still failing, whatever its last
    /// words said: the work is not done, and a person should be told so. Called after the
    /// answer or the banner, and only for a frontend that insists
    /// ([`Frontend::insist_on_checks`]) — not when Stop was pressed, nor when the turn ended
    /// on an error, which was shown.
    fn ended_failing(&mut self) {}
}

/// Run one tool turn on `convo`, which ends up the conversation as the model has
/// seen it: every call the model made and what came back. `model` is the tag to ask,
/// or `None` for the config's.
///
/// The config is loaded **fresh before every round**, as each `ask_with_tools` call
/// did, so a change made while a card waits applies to the next round.
///
/// `Err` is a request that could not be made or a call that could not be run at
/// all — the card says so first — and it ends the turn. A turn that ends by itself,
/// by a bound, by Stop or by the model being stuck, is `Ok`: the frontend was told.
///
/// **What the frontend does when the turn is over is its own**, as `send` in
/// `App.tsx` did in its `finally`: show an `Err` (unless Stop was pressed
/// meanwhile), clear the status — a failed request leaves `Starting Ollama…` up —
/// and save the thread.
pub fn run_turn(
    convo: &mut Vec<Message>,
    model: Option<&str>,
    calibration: Option<Calibration>,
    fe: &mut dyn Frontend,
) -> Result<(), String> {
    // Rust starts Ollama itself before the first round; this only says why that
    // round is waiting. Here and not in `run_turn_with`, so the loop's tests never
    // go to the network.
    if !opening(fe, ollama::is_running()) {
        return Ok(());
    }
    run_turn_with(
        convo,
        calibration,
        tools::MAX_ROUNDS,
        &mut |messages, calibration, closing| {
            let cfg = config::load();
            round(messages, model.unwrap_or(cfg.model.as_str()), &cfg, calibration, closing)
        },
        &mut run_approved,
        fe,
    )
}

/// Before the first round: if Ollama is not running, the round will start it and
/// wait, so say so — unless Stop was pressed already, when there is nothing to wait
/// for. `false` ends the turn.
fn opening(fe: &mut dyn Frontend, running: bool) -> bool {
    if !running {
        if fe.stopped() {
            return false;
        }
        fe.status("Starting Ollama…");
    }
    true
}

/// One request to the model: the conversation so far, the calibration to budget the
/// prompt from, and whether it is the closing request. [`round`] is the real one.
pub type Ask<'a> = dyn FnMut(&[Message], Option<Calibration>, bool) -> Result<Round, String> + 'a;

/// Redeem an approved call's ticket and run what it holds: the call's name, its path
/// and its ticket. [`run_approved`] is the real one.
pub type Run<'a> = dyn FnMut(&str, &str, &str) -> Result<Ran, String> + 'a;

/// The loop, with the model and the runner passed in: `ask` is one request to the
/// model, `run` redeems an approved call's ticket and runs it.
///
/// `bound` is the most rounds the turn may take: [`tools::MAX_ROUNDS`] in the app. A
/// probe can pass more, to record how many rounds a task really takes; free calls
/// still come on top of it, and the hard stop is `bound + MAX_FREE_CALLS` requests.
///
/// **Mirrors `askWithTools` step for step**, so that a review can walk the old
/// TypeScript against this. Where a step sends something to the webview there, it
/// calls `fe` here.
pub fn run_turn_with(
    convo: &mut Vec<Message>,
    calibration: Option<Calibration>,
    bound: usize,
    ask: &mut Ask<'_>,
    run: &mut Run<'_>,
    fe: &mut dyn Frontend,
) -> Result<(), String> {
    let mut turn = Turn { convo, calibration, last_ran: false, check_failing: false, pushed_back: 0, ask, run, fe };
    let result = turn.run(bound);
    // Over with the check still failing: the model was pushed back as often as it is
    // and ended the turn anyway, or a bound or its being stuck did. A frontend that
    // insisted is told, since the model's own words may say otherwise. Not after a
    // Stop, which the person asked for, nor an error, which they were shown.
    if result.is_ok() && turn.check_failing && turn.fe.insist_on_checks() && !turn.fe.stopped() {
        turn.fe.ended_failing();
    }
    result
}

/// The loop's state: the conversation, what the next round is calibrated from, and
/// whether the last call actually ran — an automatic call that did, or an approved
/// one that did. A check that found errors still ran.
struct Turn<'a> {
    convo: &'a mut Vec<Message>,
    calibration: Option<Calibration>,
    last_ran: bool,
    /// The model's latest `run_check` ran and failed, and none has passed since.
    check_failing: bool,
    /// How many times this turn has pushed the model back to fix a failing check.
    pushed_back: usize,
    ask: &'a mut Ask<'a>,
    run: &'a mut Run<'a>,
    fe: &'a mut dyn Frontend,
}

/// What the loop does after a trip round its body.
enum Flow {
    /// The turn is over: the model answered, the person was told, Stop was pressed.
    End,
    /// Go round again. `free` is a successful automatic call, which gives its round
    /// back.
    Again { free: bool },
}

impl Turn<'_> {
    /// Go round until the turn ends or a bound is reached.
    fn run(&mut self, bound: usize) -> Result<(), String> {
        // A bound, not a timeout. Without one, a model that keeps asking loops forever
        // and the only symptom is the app getting warm.
        let mut step = 0;
        let mut requests = 0;
        while step < bound && requests < bound + tools::MAX_FREE_CALLS {
            match self.go()? {
                Flow::End => return Ok(()),
                // `requests` counts everything. `step` does not count a free call: the
                // webview's `step--` before its `continue`.
                Flow::Again { free } => {
                    requests += 1;
                    if !free {
                        step += 1;
                    }
                }
            }
        }
        self.close(bound, step, requests)
    }

    /// One trip round the loop: ask, show what came back, and — if it was a call —
    /// run it and put the pair in the conversation.
    fn go(&mut self) -> Result<Flow, String> {
        // Stopped: a tool round is not streamed, so it cannot be cut short, but
        // nothing after the stop is shown or acted on.
        if self.fe.stopped() {
            return Ok(Flow::End);
        }
        let Round { reply, prompt_eval_count, sent, recovered, fed_back, metrics: _ } = (self.ask)(&self.convo[..], self.calibration, false)?;
        // The server is up now (if "Starting Ollama…" was showing, it is done).
        self.fe.status("");
        // `convo` must end up exactly what was sent, or the next round's prefix
        // differs and Ollama's prompt cache is lost.
        for (&at, replacement) in sent.set_aside().iter().zip(sent.replacements()) {
            if let Some(slot) = self.convo.get_mut(at) {
                *slot = replacement.clone();
            }
        }
        if prompt_eval_count > 0 {
            self.calibration = Some(Calibration::new(sent.prompt_chars(), prompt_eval_count));
        }
        // Every round, so a round that did not fit and a later one that does clears
        // the warning.
        self.fe.round(RoundNumbers {
            prompt_eval_count,
            prompt_chars: sent.prompt_chars(),
            set_aside: sent.set_aside().len(),
            fits: sent.fits(),
            recovered,
            fed_back,
        });

        // An automatic call has already run by the time it comes back, so Stop
        // pressed meanwhile still gets its card — the arm below shows and saves it,
        // and sends nothing on.
        if self.fe.stopped() && !matches!(reply, Reply::Auto { .. }) {
            return Ok(Flow::End);
        }

        match reply {
            Reply::Answer { content, eval_count } => {
                // Done, says the model, with its check failing. A frontend that insists
                // does not take that: what the model said goes back into the conversation
                // with the loop's reply to it, and the turn goes on. It is a round like
                // any other, so a bound still ends the turn, and only so many times.
                if self.check_failing && self.pushed_back < MAX_PUSH_BACKS && self.fe.insist_on_checks() {
                    self.pushed_back += 1;
                    if !content.trim().is_empty() {
                        self.convo.push(Message {
                            role: "assistant".to_string(),
                            content,
                            images: None,
                            tool_calls: None,
                            tool_name: None,
                        });
                    }
                    self.convo.push(Message::user(tools::CHECK_FAILED_NOTE));
                    return Ok(Flow::Again { free: false });
                }
                self.fe.answer(&content, prompt_eval_count, eval_count);
                Ok(Flow::End)
            }
            Reply::Stuck { error } => {
                // The third identical failure. The file has not changed, so another
                // try cannot go differently — better to stop and say so.
                self.fe.error(&error);
                Ok(Flow::End)
            }
            Reply::Rejected { name, path, error, raw_call } => {
                // It could never have worked — an edit whose text is not in the file,
                // say. Not a decision for the person, so it goes straight back to the
                // model with the reason, and is shown here so nothing happens unseen.
                let mut card = Card::new(name.clone(), path, CardState::Rejected);
                card.error = Some(error.clone());
                self.fe.card(card);
                self.pair(raw_call, &name, error);
                self.last_ran = false;
                Ok(Flow::Again { free: false })
            }
            Reply::Auto { name, path, range, result, ok, stopped, plan, raw_call } => {
                // Already run, because the rule says a read, listing or search inside
                // the project folder needs no approval. Shown as a ran card marked
                // `auto` so nothing happens unseen, and sent back to the model
                // exactly as an approved call would be.
                let state = if stopped { CardState::Stopped } else { CardState::Ran };
                let mut card = Card::new(name.clone(), path, state);
                card.args = Some(args_of(&raw_call));
                card.ok = Some(ok);
                card.result = Some(result.clone());
                card.auto = true;
                card.range = range;
                card.plan = plan;
                self.fe.card(card);
                // Stopped while the card went up: Stop's own save may have missed it,
                // so the thread is saved again as it now stands. The result is not
                // sent to the model.
                if self.fe.stopped() {
                    self.fe.save();
                    return Ok(Flow::End);
                }
                self.pair(raw_call, &name, result);
                // Free when it worked (a search with no matches worked). `ok` is
                // Rust's own word for it, and the probes' free rounds are this one.
                self.last_ran = ok;
                Ok(Flow::Again { free: ok })
            }
            Reply::Call { call, raw_call } => self.call(call, raw_call),
        }
    }

    /// A call that needs a decision: show the card, wait, and run it if allowed.
    fn call(&mut self, call: PendingCall, raw_call: chat::ToolCall) -> Result<Flow, String> {
        // Show exactly what would happen — the path for a read, the diff for an
        // edit, the command for a check — and stop. Nothing is read, written or run
        // until it is approved, and what is approved is what runs: the ticket
        // carries the whole plan.
        let mut card = Card::new(call.name.clone(), call.path.clone(), CardState::Pending);
        card.args = Some(args_of(&raw_call));
        card.diff = call.diff.clone();
        card.range = call.range;
        card.changed_build_files = call.changed_build_files.clone();
        card.to = call.to.clone();
        card.folders = call.folders.clone();
        card.size = call.size;
        let at = self.fe.card(card);

        // Stop pressed while the card was going up: there was nothing waiting to
        // answer it, so waiting here would leave a live card wired to the next
        // request.
        if self.fe.stopped() {
            self.fe.update(at, Patch::state(CardState::Stopped));
            self.fe.save();
            return Ok(Flow::End);
        }
        match self.fe.decide(at, &call) {
            Decision::Deny => {
                self.fe.update(at, Patch::state(CardState::Denied));
                // The model is told, rather than left waiting on a call that silently
                // never happened. The assistant turn that asked has to go back too,
                // or the refusal answers a question it cannot see itself having asked.
                self.pair(raw_call, &call.name, refusal_for(&call.name, &call.path));
                self.last_ran = false;
                Ok(Flow::Again { free: false })
            }
            // Behaves as Allow until Hugin's I3 gives the session a memory.
            Decision::Allow | Decision::AllowForSession => {
                // Stopped between Allow and here: nothing has run, and nothing will.
                if self.fe.stopped() {
                    self.fe.update(at, Patch::state(CardState::Stopped));
                    return Ok(Flow::End);
                }
                // The buttons go at once. A check can take minutes, and a card still
                // offering Allow would invite a second click on a spent ticket.
                self.fe.update(at, Patch::state(CardState::Running));
                let ran = match (self.run)(&call.name, &call.path, &call.token) {
                    Ok(ran) => ran,
                    Err(e) => {
                        // It could not be run at all. The card says so rather than
                        // spinning.
                        let patch = Patch {
                            state: Some(CardState::Failed),
                            ok: Some(false),
                            error: Some(e.clone()),
                            ..Patch::default()
                        };
                        self.fe.update(at, patch);
                        return Err(e);
                    }
                };
                self.note_check(&call.name, &ran);
                // An edit refused at the last moment — the file changed after the diff
                // was shown — is failed: nothing was written. A check that finds
                // errors is not: it ran, and `ok` says it did not pass.
                let failed = is_change(&call.name) && !ran.ok;
                let patch = Patch {
                    state: Some(if ran.stopped {
                        CardState::Stopped
                    } else if failed {
                        CardState::Failed
                    } else {
                        CardState::Ran
                    }),
                    ok: Some(ran.ok),
                    result: Some(ran.result.clone()),
                    error: failed.then(|| ran.result.clone()),
                    undo: ran.undo.clone(),
                };
                // Stopped while it ran. What is left is saving the result — an edit
                // that was written has an undo handle worth keeping.
                if self.fe.stopped() {
                    self.fe.update(at, patch);
                    self.fe.save();
                    return Ok(Flow::End);
                }
                self.fe.update(at, patch);
                self.pair(raw_call, &call.name, ran.result);
                self.last_ran = !failed;
                Ok(Flow::Again { free: false })
            }
        }
    }

    /// What a finished `run_check` says about the work: **a check that ran and failed makes
    /// it failing until a later check passes**, whatever the model does in between. One that
    /// never judged the code — it timed out, was stopped, could not start or was refused by
    /// Smart App Control — changes nothing: asking the model to fix its code over that would
    /// send it after a fault it does not have.
    fn note_check(&mut self, name: &str, ran: &Ran) {
        if name != "run_check" {
            return;
        }
        match &ran.check {
            Some(outcome) if outcome.passed() => self.check_failing = false,
            Some(outcome) if matches!(outcome.ended, Ended::Exited(_)) && outcome.blocked.is_none() => {
                self.check_failing = true;
            }
            _ => {}
        }
    }

    /// The loop ended at a bound. Say which, and not more than is true — after one
    /// more request with no tools, so the turn ends in the model's own words.
    fn close(&mut self, bound: usize, step: usize, requests: usize) -> Result<(), String> {
        let mut banner = if step >= bound {
            format!("Stopped after {bound} tool rounds.")
        } else {
            format!("Stopped after {requests} tool calls.")
        };
        if self.last_ran {
            banner.push_str(" The last step ran — check the result.");
        }
        if self.fe.stopped() {
            return Ok(());
        }
        let mut why = String::new();
        match (self.ask)(&self.convo[..], self.calibration, true) {
            Ok(closing) => {
                if self.fe.stopped() {
                    return Ok(());
                }
                if let Reply::Answer { content, eval_count } = closing.reply
                    && !content.trim().is_empty()
                {
                    self.fe.answer(&content, closing.prompt_eval_count, eval_count);
                }
            }
            Err(e) => why = format!(" The closing request failed: {e}"),
        }
        if self.fe.stopped() {
            return Ok(());
        }
        self.fe.error(&format!("{banner}{why}"));
        Ok(())
    }

    /// assistant(tool_calls) then tool(result) - the pair Ollama expects. Sending
    /// only the result left the model looking at an answer to a question it could
    /// not see itself having asked, so it asked again until the round bound
    /// tripped. The G6 probe did this correctly; the app did not, which is exactly
    /// the gap a probe can hide.
    fn pair(&mut self, raw_call: chat::ToolCall, name: &str, result: String) {
        self.convo.push(Message {
            role: "assistant".to_string(),
            content: String::new(),
            images: None,
            tool_calls: Some(vec![raw_call]),
            tool_name: None,
        });
        self.convo.push(Message::tool_result(name, result));
    }
}

/// The arguments to replay for a call, as the model sent them. Always an object: a
/// call with no arguments at all is `{}`.
fn args_of(raw_call: &chat::ToolCall) -> serde_json::Value {
    match &raw_call.function.arguments {
        serde_json::Value::Null => serde_json::json!({}),
        args => args.clone(),
    }
}

/// A call that changes the project and can be undone: an edit, a create, a delete
/// or a move.
fn is_change(name: &str) -> bool {
    matches!(name, "edit_file" | "create_file" | "delete_file" | "move_file")
}

/// What the model is told when you say no, in terms of what it asked for.
fn refusal_for(name: &str, path: &str) -> String {
    if name == "delete_file" {
        return format!("The user declined to delete {path}. Nothing was deleted.");
    }
    if name == "move_file" {
        return format!("The user declined to move {path}. Nothing was moved.");
    }
    if is_change(name) {
        return format!("The user declined this change to {path}. Nothing was written.");
    }
    if name == "run_check" || name == "run_command" {
        return format!("The user declined to run `{path}`. Nothing was run.");
    }
    if name == "search" || name == "list_files" {
        let what = if name == "search" { "search for" } else { "list" };
        return format!("The user declined to let you {what} {path}.");
    }
    format!("The user declined to let you read {path}.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::send::{compact_for_send, Sent};
    use serde_json::json;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::rc::Rc;

    type Log = Rc<RefCell<Vec<String>>>;

    /// What the loop asked the model: whether it was the closing request, how many
    /// messages it was sent, and the calibration it was given.
    type Asked = (bool, usize, Option<Calibration>);

    /// A frontend that writes down everything it is told, and answers a card from a
    /// script. Its cards are kept as the person would see them: each with the patches
    /// applied, the way the webview's `{ ...card, ...patch }` does.
    struct Fake {
        log: Log,
        /// Stop, as a person presses it: every `stopped()` reads this.
        stop: Rc<Cell<bool>>,
        decisions: VecDeque<Decision>,
        /// Stop is pressed while a card waits: `decide` raises it, then answers as scripted.
        stop_in_decide: bool,
        /// Stop is pressed while the card is going up.
        stop_on_card: bool,
        /// Stop is pressed while the model is answering (or an automatic call is running).
        stop_in_ask: bool,
        /// Stop is pressed while the model is answering its n-th request (counting from 0).
        stop_at_ask: Option<usize>,
        /// Stop is pressed while an approved call runs.
        stop_in_run: bool,
        /// The frontend insists on a passing check: see [`Frontend::insist_on_checks`].
        insist: bool,
        cards: Vec<Card>,
        answers: Vec<(String, u32, u32)>,
        errors: Vec<String>,
        numbers: Vec<RoundNumbers>,
        saves: usize,
        /// How often the turn said it ended with its check failing.
        unfinished: usize,
    }

    impl Fake {
        fn new() -> Self {
            Self {
                log: Rc::default(),
                stop: Rc::default(),
                decisions: VecDeque::new(),
                stop_in_decide: false,
                stop_on_card: false,
                stop_in_ask: false,
                stop_at_ask: None,
                stop_in_run: false,
                insist: false,
                cards: Vec::new(),
                answers: Vec::new(),
                errors: Vec::new(),
                numbers: Vec::new(),
                saves: 0,
                unfinished: 0,
            }
        }

        fn deciding(decisions: &[Decision]) -> Self {
            Self { decisions: decisions.iter().copied().collect(), ..Self::new() }
        }

        /// A frontend that insists on a passing check, and allows what it is asked.
        fn insisting(allows: usize) -> Self {
            Self { insist: true, ..Self::deciding(&vec![Decision::Allow; allows]) }
        }

        fn events(&self) -> Vec<String> {
            self.log.borrow().clone()
        }

        fn say(&self, event: String) {
            self.log.borrow_mut().push(event);
        }
    }

    impl Frontend for Fake {
        fn stopped(&self) -> bool {
            self.stop.get()
        }

        fn status(&mut self, text: &str) {
            self.say(format!("status:{text}"));
        }

        fn round(&mut self, numbers: RoundNumbers) {
            self.say("round".into());
            self.numbers.push(numbers);
        }

        fn answer(&mut self, content: &str, prompt_eval_count: u32, eval_count: u32) {
            self.say(format!("answer:{content}"));
            self.answers.push((content.into(), prompt_eval_count, eval_count));
        }

        fn error(&mut self, message: &str) {
            self.say(format!("error:{message}"));
            self.errors.push(message.into());
        }

        fn card(&mut self, card: Card) -> usize {
            self.say(format!("card:{}:{}:{:?}", card.name, card.path, card.state));
            if self.stop_on_card {
                self.stop.set(true);
            }
            self.cards.push(card);
            self.cards.len() - 1
        }

        fn update(&mut self, card: usize, patch: Patch) {
            self.say(format!("update:{card}:{}", patch.state.map_or("-".to_string(), |s| format!("{s:?}"))));
            let shown = &mut self.cards[card];
            if let Some(state) = patch.state {
                shown.state = state;
            }
            if patch.ok.is_some() {
                shown.ok = patch.ok;
            }
            if patch.result.is_some() {
                shown.result = patch.result;
            }
            if patch.error.is_some() {
                shown.error = patch.error;
            }
            if patch.undo.is_some() {
                shown.undo = patch.undo;
            }
        }

        fn decide(&mut self, card: usize, call: &PendingCall) -> Decision {
            self.say(format!("decide:{card}:{}:{}", call.name, call.token));
            if self.stop_in_decide {
                self.stop.set(true);
            }
            self.decisions.pop_front().expect("a card asked and no decision was scripted")
        }

        fn save(&mut self) {
            self.say("save".into());
            self.saves += 1;
        }

        fn insist_on_checks(&self) -> bool {
            self.insist
        }

        fn ended_failing(&mut self) {
            self.say("unfinished".into());
            self.unfinished += 1;
        }
    }

    /// Run the loop on a script: the model's replies in order, and what each approved
    /// call comes to. Panics if the loop asks for more than the script holds, so a
    /// request that should not have been made is a failure in itself. The app's bound.
    fn play(
        convo: &mut Vec<Message>,
        calibration: Option<Calibration>,
        asks: Vec<Result<Round, String>>,
        runs: Vec<Result<Ran, String>>,
        fe: &mut Fake,
    ) -> (Result<(), String>, Vec<Asked>) {
        play_bounded(tools::MAX_ROUNDS, convo, calibration, asks, runs, fe)
    }

    /// [`play`], with a bound of the caller's own.
    fn play_bounded(
        bound: usize,
        convo: &mut Vec<Message>,
        calibration: Option<Calibration>,
        asks: Vec<Result<Round, String>>,
        runs: Vec<Result<Ran, String>>,
        fe: &mut Fake,
    ) -> (Result<(), String>, Vec<Asked>) {
        let (log, stop) = (fe.log.clone(), fe.stop.clone());
        let (stop_in_ask, stop_at_ask, stop_in_run) = (fe.stop_in_ask, fe.stop_at_ask, fe.stop_in_run);
        let (mut asks, mut runs) = (VecDeque::from(asks), VecDeque::from(runs));
        let mut asked = Vec::new();
        let result = run_turn_with(
            convo,
            calibration,
            bound,
            &mut |messages: &[Message], calibration: Option<Calibration>, closing: bool| {
                log.borrow_mut().push(format!("ask:{}", if closing { "closing" } else { "open" }));
                if stop_in_ask || stop_at_ask == Some(asked.len()) {
                    stop.set(true);
                }
                asked.push((closing, messages.len(), calibration));
                asks.pop_front().expect("the loop asked the model more often than the script allows")
            },
            &mut |name: &str, path: &str, token: &str| {
                log.borrow_mut().push(format!("run:{name}:{path}:{token}"));
                if stop_in_run {
                    stop.set(true);
                }
                runs.pop_front().expect("the loop ran a call the script did not expect")
            },
            fe,
        );
        (result, asked)
    }

    // ── what the model says ─────────────────────────────────────────────────

    fn sent() -> Sent {
        compact_for_send(&mut [], &[], 8192, None, 0)
    }

    fn round_of(reply: Reply) -> Result<Round, String> {
        Ok(Round { reply, prompt_eval_count: 0, recovered: false, fed_back: false, metrics: Default::default(), sent: sent() })
    }

    fn raw(name: &str, arguments: serde_json::Value) -> chat::ToolCall {
        chat::ToolCall { id: None, function: chat::FunctionCall { name: name.into(), arguments } }
    }

    fn answer(text: &str) -> Result<Round, String> {
        round_of(Reply::Answer { content: text.into(), eval_count: 7 })
    }

    fn rejected(n: usize) -> Result<Round, String> {
        let path = format!("f{n}.rs");
        round_of(Reply::Rejected {
            name: "edit_file".into(),
            path: path.clone(),
            error: format!("error: old_text {n} is not in the file"),
            raw_call: raw("edit_file", json!({ "path": path })),
        })
    }

    fn auto_search(n: usize, ok: bool) -> Result<Round, String> {
        round_of(Reply::Auto {
            name: "search".into(),
            path: format!("\"x{n}\""),
            range: None,
            result: if ok { format!("src/a.rs:{n}: x{n}") } else { "error: no such folder".into() },
            ok,
            stopped: false,
            plan: None,
            raw_call: raw("search", json!({ "text": format!("x{n}") })),
        })
    }

    /// A call that asks. Its ticket is `t-<path>`, which `run` is handed back.
    fn asks_about(name: &str, path: &str) -> Result<Round, String> {
        round_of(Reply::Call {
            call: PendingCall {
                token: format!("t-{path}"),
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

    fn ran(result: &str) -> Result<Ran, String> {
        Ok(Ran::text(result.into()))
    }

    /// The assistant turn that asked for a call, as the loop puts it back.
    fn assistant(call: chat::ToolCall) -> Message {
        Message {
            role: "assistant".into(),
            content: String::new(),
            images: None,
            tool_calls: Some(vec![call]),
            tool_name: None,
        }
    }

    // ── the end of a turn ───────────────────────────────────────────────────

    #[test]
    fn an_answer_on_the_first_round_ends_the_turn() {
        let mut fe = Fake::new();
        let mut convo = vec![Message::user("hi")];
        let (result, asked) = play(&mut convo, None, vec![answer("hello")], vec![], &mut fe);
        assert_eq!(result, Ok(()));
        assert_eq!(asked, vec![(false, 1, None)]);
        assert_eq!(fe.answers, [("hello".to_string(), 0, 7)]);
        let prompt_chars = sent().prompt_chars();
        let numbers = RoundNumbers { prompt_eval_count: 0, prompt_chars, set_aside: 0, fits: true, recovered: false, fed_back: false };
        assert_eq!(fe.numbers, [numbers]);
        assert!(fe.errors.is_empty() && fe.cards.is_empty());
        assert_eq!(convo, vec![Message::user("hi")]);
        // The status is cleared as soon as the model has answered, then the numbers,
        // then the answer.
        assert_eq!(fe.events(), ["ask:open", "status:", "round", "answer:hello"]);
    }

    #[test]
    fn a_stuck_model_shows_its_error_and_gets_no_closing_request() {
        let mut fe = Fake::new();
        let mut convo = vec![Message::user("fix it")];
        let stuck = round_of(Reply::Stuck { error: "the model kept repeating a call to edit_file".into() });
        let (result, asked) = play(&mut convo, None, vec![stuck], vec![], &mut fe);
        assert_eq!(result, Ok(()));
        assert_eq!(asked.len(), 1, "no closing request");
        assert_eq!(fe.errors, ["the model kept repeating a call to edit_file"]);
        assert!(fe.answers.is_empty() && fe.cards.is_empty());
    }

    #[test]
    fn a_failed_request_in_a_round_ends_the_turn_with_its_error() {
        let mut fe = Fake::new();
        let mut convo = vec![Message::user("hi")];
        let asks = vec![Err("POST /api/chat: connection refused".to_string())];
        let (result, asked) = play(&mut convo, None, asks, vec![], &mut fe);
        assert_eq!(result, Err("POST /api/chat: connection refused".to_string()));
        assert_eq!(asked.len(), 1, "no closing request after a failed one");
        assert!(fe.errors.is_empty() && fe.answers.is_empty());
    }

    // ── the bounds ──────────────────────────────────────────────────────────

    #[test]
    fn ten_rejections_stop_the_turn_at_the_round_bound_and_the_model_has_the_last_word() {
        let mut fe = Fake::new();
        let mut convo = vec![Message::user("fix it")];
        let mut asks: Vec<_> = (0..10).map(rejected).collect();
        asks.push(answer("I could not make the edit."));
        let (result, asked) = play(&mut convo, None, asks, vec![], &mut fe);
        assert_eq!(result, Ok(()));
        assert_eq!(asked.len(), 11);
        assert!(asked[..10].iter().all(|(closing, ..)| !closing));
        assert_eq!(asked[10], (true, 21, None), "the closing request is sent everything the turn said");
        // Every rejection is a card the person can see, in the state that says so, and
        // with no arguments to replay.
        assert_eq!(fe.cards.len(), 10);
        assert!(fe.cards.iter().all(|c| c.state == CardState::Rejected && c.args.is_none() && !c.auto));
        assert_eq!(fe.cards[3].error.as_deref(), Some("error: old_text 3 is not in the file"));
        // The closing words come first, then the banner; with nothing run, no suffix.
        assert_eq!(fe.answers, [("I could not make the edit.".to_string(), 0, 7)]);
        assert_eq!(fe.errors, ["Stopped after 10 tool rounds."]);
        // The pair for the first rejection: the model's own attempt, then the reason.
        assert_eq!(convo[1], assistant(raw("edit_file", json!({ "path": "f0.rs" }))));
        assert_eq!(convo[2], Message::tool_result("edit_file", "error: old_text 0 is not in the file"));
    }

    #[test]
    fn successful_automatic_calls_are_free_but_thirty_requests_is_still_the_hard_stop() {
        let mut fe = Fake::new();
        let mut convo = vec![Message::user("look around")];
        let mut asks: Vec<_> = (0..30).map(|n| auto_search(n, true)).collect();
        asks.push(answer("I looked at thirty things."));
        let (result, asked) = play(&mut convo, None, asks, vec![], &mut fe);
        assert_eq!(result, Ok(()));
        // Thirty calls and the closing request. Ten rounds would have ended it at ten:
        // none of these used one up.
        assert_eq!(asked.len(), 31);
        assert_eq!(asked[30], (true, 61, None));
        assert_eq!(fe.cards.len(), 30);
        assert!(fe.cards.iter().all(|c| c.auto && c.state == CardState::Ran && c.ok == Some(true)));
        assert_eq!(fe.cards[0].args, Some(json!({ "text": "x0" })));
        assert_eq!(fe.answers, [("I looked at thirty things.".to_string(), 0, 7)]);
        assert_eq!(fe.errors, ["Stopped after 30 tool calls. The last step ran — check the result."]);
        assert_eq!(convo.len(), 1 + 2 * 30);
        // Sent back exactly as an approved call would be.
        assert_eq!(convo[1], assistant(raw("search", json!({ "text": "x0" }))));
        assert_eq!(convo[2], Message::tool_result("search", "src/a.rs:0: x0"));
    }

    #[test]
    fn a_failed_automatic_call_is_a_round_and_is_not_a_step_that_ran() {
        let mut fe = Fake::new();
        let mut convo = vec![Message::user("look around")];
        let mut asks: Vec<_> = (0..10).map(|n| auto_search(n, false)).collect();
        asks.push(answer(""));
        let (result, asked) = play(&mut convo, None, asks, vec![], &mut fe);
        assert_eq!(result, Ok(()));
        assert_eq!(asked.len(), 11);
        assert!(fe.cards.iter().all(|c| c.state == CardState::Ran && c.ok == Some(false)));
        // A blank closing answer is not shown; the banner is all there is.
        assert!(fe.answers.is_empty());
        assert_eq!(fe.errors, ["Stopped after 10 tool rounds."]);
    }

    #[test]
    fn the_round_bound_is_named_when_both_bounds_are_reached_together() {
        let mut fe = Fake::new();
        let mut convo = vec![Message::user("look around")];
        // Twenty free calls, then ten rejections: the thirtieth request is also the
        // tenth round.
        let mut asks: Vec<_> = (0..20).map(|n| auto_search(n, true)).collect();
        asks.extend((0..10).map(rejected));
        asks.push(answer("done"));
        let (_, asked) = play(&mut convo, None, asks, vec![], &mut fe);
        assert_eq!(asked.len(), 31);
        assert_eq!(fe.errors, ["Stopped after 10 tool rounds."]);
    }

    /// A call Ollama could not read comes back as a rejection the model is told of, the
    /// turn goes on to the model's next reply, and the round is counted as fed back.
    #[test]
    fn a_call_that_was_fed_back_is_counted_apart_from_a_recovered_one() {
        let mut fe = Fake::new();
        let mut convo = vec![Message::user("fix it")];
        let fed = Round { fed_back: true, ..rejected(0).unwrap() };
        let (result, _) = play(&mut convo, None, vec![Ok(fed), answer("done")], vec![], &mut fe);
        assert_eq!(result, Ok(()));
        assert_eq!(fe.numbers.iter().map(|n| (n.recovered, n.fed_back)).collect::<Vec<_>>(), [(false, true), (false, false)]);
        assert_eq!(fe.answers.len(), 1);
    }

    #[test]
    fn a_bound_of_three_stops_the_turn_after_three_rounds_and_names_it() {
        let mut fe = Fake::new();
        let mut convo = vec![Message::user("fix it")];
        let mut asks: Vec<_> = (0..3).map(rejected).collect();
        asks.push(answer("I could not make the edit."));
        let (result, asked) = play_bounded(3, &mut convo, None, asks, vec![], &mut fe);
        assert_eq!(result, Ok(()));
        assert_eq!(asked.len(), 4, "three rounds and the closing request");
        assert!(asked[..3].iter().all(|(closing, ..)| !closing));
        assert_eq!(asked[3], (true, 7, None));
        assert_eq!(fe.answers, [("I could not make the edit.".to_string(), 0, 7)]);
        assert_eq!(fe.errors, ["Stopped after 3 tool rounds."]);
    }

    #[test]
    fn a_bound_of_three_still_lets_free_calls_come_on_top_of_it() {
        let mut fe = Fake::new();
        let mut convo = vec![Message::user("look around")];
        // None of these uses a round up, so what ends the turn is the hard stop:
        // the bound and the free calls together, 3 + 20.
        let mut asks: Vec<_> = (0..23).map(|n| auto_search(n, true)).collect();
        asks.push(answer("I looked at twenty-three things."));
        let (result, asked) = play_bounded(3, &mut convo, None, asks, vec![], &mut fe);
        assert_eq!(result, Ok(()));
        assert_eq!(asked.len(), 24, "twenty-three calls and the closing request");
        assert_eq!(fe.errors, ["Stopped after 23 tool calls. The last step ran — check the result."]);
    }

    /// Ten approved calls, the last of which is `last`; whether it worked is `ok`.
    /// Returns the banner.
    fn banner_after_ten(last: &str, ok: bool) -> String {
        let mut fe = Fake::deciding(&[Decision::Allow; 10]);
        let mut convo = vec![Message::user("go")];
        let mut asks: Vec<_> = (0..10).map(|n| asks_about(if n == 9 { last } else { "edit_file" }, "f.rs")).collect();
        asks.push(answer(""));
        let mut runs: Vec<_> = (0..9).map(|_| ran("Edited f.rs: 1 line removed, 1 added.")).collect();
        runs.push(Ok(Ran { result: "the last one".into(), ok, stopped: false, undo: None, check: None }));
        let (result, _) = play(&mut convo, None, asks, runs, &mut fe);
        assert_eq!(result, Ok(()));
        assert_eq!(fe.errors.len(), 1);
        fe.errors.remove(0)
    }

    #[test]
    fn the_banner_says_the_last_step_ran_when_it_did() {
        let ran_it = "Stopped after 10 tool rounds. The last step ran — check the result.";
        assert_eq!(banner_after_ten("edit_file", true), ran_it);
        // An edit that was refused at the last moment did not run.
        assert_eq!(banner_after_ten("edit_file", false), "Stopped after 10 tool rounds.");
        // A check that found errors did: it ran, and what it found is the news.
        assert_eq!(banner_after_ten("run_check", false), ran_it);
    }

    #[test]
    fn a_closing_request_that_fails_is_named_in_the_banner() {
        let mut fe = Fake::new();
        let mut convo = vec![Message::user("fix it")];
        let mut asks: Vec<_> = (0..10).map(rejected).collect();
        asks.push(Err("POST /api/chat: timed out".into()));
        let (result, asked) = play(&mut convo, None, asks, vec![], &mut fe);
        assert_eq!(result, Ok(()));
        assert_eq!(asked.len(), 11);
        assert!(fe.answers.is_empty());
        assert_eq!(
            fe.errors,
            ["Stopped after 10 tool rounds. The closing request failed: POST /api/chat: timed out"]
        );
    }

    // ── approvals ───────────────────────────────────────────────────────────

    #[test]
    fn an_allowed_call_runs_and_a_denied_one_is_refused_to_the_model_in_so_many_words() {
        let mut fe = Fake::deciding(&[Decision::Allow, Decision::Deny]);
        let mut convo = vec![Message::user("fix it")];
        let asks = vec![asks_about("edit_file", "a.rs"), asks_about("delete_file", "b.txt"), answer("Done.")];
        let said = "Edited a.rs: 1 line removed, 1 added.";
        let runs = vec![Ok(Ran { result: said.into(), ok: true, stopped: false, undo: Some("ab12".into()), check: None })];
        let (result, asked) = play(&mut convo, None, asks, runs, &mut fe);
        assert_eq!(result, Ok(()));
        assert_eq!(asked.len(), 3);
        assert_eq!(fe.answers, [("Done.".to_string(), 0, 7)]);
        assert!(fe.errors.is_empty());

        // What happened, in order: a card goes up, the person decides, and only an
        // allowed call runs - handed its own ticket.
        assert_eq!(
            fe.events(),
            [
                "ask:open",
                "status:",
                "round",
                "card:edit_file:a.rs:Pending",
                "decide:0:edit_file:t-a.rs",
                "update:0:Running",
                "run:edit_file:a.rs:t-a.rs",
                "update:0:Ran",
                "ask:open",
                "status:",
                "round",
                "card:delete_file:b.txt:Pending",
                "decide:1:delete_file:t-b.txt",
                "update:1:Denied",
                "ask:open",
                "status:",
                "round",
                "answer:Done.",
            ]
        );
        let first = &fe.cards[0];
        assert_eq!(first.state, CardState::Ran);
        assert_eq!((first.ok, first.result.as_deref(), first.undo.as_deref()), (Some(true), Some(said), Some("ab12")));
        assert_eq!(first.args, Some(json!({ "path": "a.rs" })));
        assert_eq!(fe.cards[1].state, CardState::Denied);

        // The conversation: for each call the assistant turn that asked, then the
        // result - or, for a no, what the model is told about it.
        assert_eq!(
            convo,
            vec![
                Message::user("fix it"),
                assistant(raw("edit_file", json!({ "path": "a.rs" }))),
                Message::tool_result("edit_file", said),
                assistant(raw("delete_file", json!({ "path": "b.txt" }))),
                Message::tool_result("delete_file", "The user declined to delete b.txt. Nothing was deleted."),
            ]
        );
    }

    #[test]
    fn allowing_for_the_session_runs_the_call_like_any_allow() {
        let mut fe = Fake::deciding(&[Decision::AllowForSession]);
        let mut convo = vec![Message::user("go")];
        let asks = vec![asks_about("run_check", "cargo check"), answer("Passes.")];
        let (result, _) = play(&mut convo, None, asks, vec![ran("ok")], &mut fe);
        assert_eq!(result, Ok(()));
        assert_eq!(fe.cards[0].state, CardState::Ran);
        assert_eq!(convo[2], Message::tool_result("run_check", "ok"));
    }

    #[test]
    fn a_call_that_could_not_be_run_fails_its_card_and_ends_the_turn_with_the_error() {
        let mut fe = Fake::deciding(&[Decision::Allow]);
        let mut convo = vec![Message::user("fix it")];
        let asks = vec![asks_about("edit_file", "a.rs")];
        let runs = vec![Err("this call was not approved".to_string())];
        let (result, asked) = play(&mut convo, None, asks, runs, &mut fe);
        assert_eq!(result, Err("this call was not approved".to_string()));
        assert_eq!(asked.len(), 1, "nothing is asked after it");
        assert_eq!(fe.cards[0].state, CardState::Failed);
        assert_eq!(fe.cards[0].ok, Some(false));
        assert_eq!(fe.cards[0].error.as_deref(), Some("this call was not approved"));
        assert_eq!(fe.events()[5..], ["update:0:Running", "run:edit_file:a.rs:t-a.rs", "update:0:Failed"]);
        // The model is told nothing: there is no result to send.
        assert_eq!(convo.len(), 1);
    }

    #[test]
    fn an_edit_refused_at_the_last_moment_is_failed_and_a_check_that_found_errors_is_not() {
        let mut fe = Fake::deciding(&[Decision::Allow, Decision::Allow]);
        let mut convo = vec![Message::user("go")];
        let asks = vec![asks_about("edit_file", "a.rs"), asks_about("run_check", "cargo check"), answer("Hm.")];
        let refused = "error: a.rs changed since the diff was shown";
        let broken = "cargo check failed: 2 errors";
        let runs = vec![
            Ok(Ran { result: refused.into(), ok: false, stopped: false, undo: None, check: None }),
            Ok(Ran { result: broken.into(), ok: false, stopped: false, undo: None, check: None }),
        ];
        let (result, _) = play(&mut convo, None, asks, runs, &mut fe);
        assert_eq!(result, Ok(()));
        assert_eq!(fe.cards[0].state, CardState::Failed);
        assert_eq!(fe.cards[0].error.as_deref(), Some(refused));
        assert_eq!(fe.cards[1].state, CardState::Ran);
        assert_eq!((fe.cards[1].ok, fe.cards[1].error.as_deref()), (Some(false), None));
        // Both results still go back to the model.
        assert_eq!(convo[2], Message::tool_result("edit_file", refused));
        assert_eq!(convo[4], Message::tool_result("run_check", broken));
    }

    #[test]
    fn a_card_carries_what_the_ticket_showed() {
        let mut fe = Fake::deciding(&[Decision::Deny]);
        let mut convo = vec![Message::user("go")];
        let call = PendingCall {
            token: "tk".into(),
            name: "create_file".into(),
            path: "src/new/mod.rs".into(),
            diff: Some(DiffView { hunks: Vec::new(), added: 3, removed: 0, create: true }),
            range: Some(tools::Lines { start: 2, end: None }),
            changed_build_files: vec!["Cargo.toml".into()],
            to: Some("src/other.rs".into()),
            folders: vec!["src/new/".into()],
            size: Some(0),
        };
        let reply = Reply::Call { call, raw_call: raw("create_file", serde_json::Value::Null) };
        let _ = play(&mut convo, None, vec![round_of(reply), answer("ok")], vec![], &mut fe);
        let card = &fe.cards[0];
        assert_eq!(card.state, CardState::Denied);
        assert_eq!(card.args, Some(json!({})), "a call with no arguments is replayed as {{}}");
        let diff = card.diff.as_ref().expect("the diff to approve");
        assert_eq!((diff.added, diff.removed, diff.create), (3, 0, true));
        assert_eq!(card.range, Some(tools::Lines { start: 2, end: None }));
        assert_eq!(card.changed_build_files, ["Cargo.toml"]);
        assert_eq!(card.to.as_deref(), Some("src/other.rs"));
        assert_eq!(card.folders, ["src/new/"]);
        assert_eq!(card.size, Some(0));
        assert!(!card.auto && card.ok.is_none() && card.result.is_none());
    }

    #[test]
    fn what_the_person_declined_is_told_to_the_model_in_terms_of_what_it_asked_for() {
        assert_eq!(refusal_for("delete_file", "b.txt"), "The user declined to delete b.txt. Nothing was deleted.");
        assert_eq!(refusal_for("move_file", "a.txt"), "The user declined to move a.txt. Nothing was moved.");
        for name in ["edit_file", "create_file"] {
            assert_eq!(refusal_for(name, "a.rs"), "The user declined this change to a.rs. Nothing was written.");
        }
        assert_eq!(
            refusal_for("run_check", "cargo check"),
            "The user declined to run `cargo check`. Nothing was run."
        );
        assert_eq!(
            refusal_for("search", "\"greet(\" in src/**/*.rs"),
            "The user declined to let you search for \"greet(\" in src/**/*.rs."
        );
        assert_eq!(
            refusal_for("list_files", "the whole project"),
            "The user declined to let you list the whole project."
        );
        assert_eq!(
            refusal_for("read_file", r"C:\work\a.txt"),
            r"The user declined to let you read C:\work\a.txt."
        );
    }

    // ── Stop ────────────────────────────────────────────────────────────────

    #[test]
    fn stop_while_a_card_waits_denies_it_and_asks_the_model_nothing_more() {
        let mut fe = Fake::deciding(&[Decision::Deny]);
        fe.stop_in_decide = true;
        let mut convo = vec![Message::user("fix it")];
        // One reply and no closing answer: the script runs dry if the loop asks again.
        let (result, asked) = play(&mut convo, None, vec![asks_about("edit_file", "a.rs")], vec![], &mut fe);
        assert_eq!(result, Ok(()));
        assert_eq!(asked.len(), 1, "no second round and no closing request");
        assert_eq!(fe.cards[0].state, CardState::Denied);
        assert!(fe.errors.is_empty() && fe.answers.is_empty());
    }

    #[test]
    fn stop_at_the_bound_asks_no_closing_request() {
        let mut fe = Fake::deciding(&[Decision::Deny]);
        fe.stop_in_decide = true;
        let mut convo = vec![Message::user("fix it")];
        // Nine rejections, then a card that is stopped on its way out: the tenth round.
        let mut asks: Vec<_> = (0..9).map(rejected).collect();
        asks.push(asks_about("edit_file", "a.rs"));
        let (result, asked) = play(&mut convo, None, asks, vec![], &mut fe);
        assert_eq!(result, Ok(()));
        assert_eq!(asked.len(), 10, "the closing request is not made");
        assert!(fe.errors.is_empty() && fe.answers.is_empty());
    }

    #[test]
    fn stop_while_the_card_goes_up_marks_it_stopped_and_saves() {
        let mut fe = Fake::new();
        fe.stop_on_card = true;
        let mut convo = vec![Message::user("fix it")];
        let (result, asked) = play(&mut convo, None, vec![asks_about("edit_file", "a.rs")], vec![], &mut fe);
        assert_eq!(result, Ok(()));
        assert_eq!(asked.len(), 1);
        assert_eq!(fe.cards[0].state, CardState::Stopped);
        assert_eq!(fe.saves, 1);
        // Nobody was asked: there was nothing waiting to answer.
        assert!(fe.events().iter().all(|e| !e.starts_with("decide")));
        assert_eq!(convo.len(), 1);
    }

    #[test]
    fn stop_between_allow_and_the_run_runs_nothing() {
        let mut fe = Fake::deciding(&[Decision::Allow]);
        fe.stop_in_decide = true;
        let mut convo = vec![Message::user("fix it")];
        let (result, _) = play(&mut convo, None, vec![asks_about("edit_file", "a.rs")], vec![], &mut fe);
        assert_eq!(result, Ok(()));
        assert_eq!(fe.cards[0].state, CardState::Stopped);
        assert!(fe.events().iter().all(|e| !e.starts_with("run")), "{:?}", fe.events());
        assert_eq!(fe.saves, 0);
        assert_eq!(convo.len(), 1);
    }

    #[test]
    fn stop_while_a_check_runs_keeps_its_result_saves_and_sends_nothing_on() {
        let mut fe = Fake::deciding(&[Decision::Allow]);
        fe.stop_in_run = true;
        let mut convo = vec![Message::user("check it")];
        let result = "`cargo check` was stopped by the user";
        let stopped_check = Ran { result: result.into(), ok: false, stopped: true, undo: None, check: None };
        let asks = vec![asks_about("run_check", "cargo check")];
        let (done, asked) = play(&mut convo, None, asks, vec![Ok(stopped_check)], &mut fe);
        assert_eq!(done, Ok(()));
        assert_eq!(asked.len(), 1, "no closing request");
        assert_eq!(fe.cards[0].state, CardState::Stopped);
        assert_eq!(fe.cards[0].result.as_deref(), Some(result));
        assert_eq!(fe.saves, 1);
        assert_eq!(convo.len(), 1, "the result is not sent to the model");
    }

    #[test]
    fn an_edit_that_landed_as_stop_was_pressed_keeps_its_undo_handle() {
        let mut fe = Fake::deciding(&[Decision::Allow]);
        fe.stop_in_run = true;
        let mut convo = vec![Message::user("fix it")];
        let written = Ran { result: "Edited a.rs".into(), ok: true, stopped: false, undo: Some("ab12".into()), check: None };
        let (result, _) = play(&mut convo, None, vec![asks_about("edit_file", "a.rs")], vec![Ok(written)], &mut fe);
        assert_eq!(result, Ok(()));
        assert_eq!(fe.cards[0].state, CardState::Ran);
        assert_eq!(fe.cards[0].undo.as_deref(), Some("ab12"));
        assert_eq!(fe.saves, 1);
        assert_eq!(convo.len(), 1);
    }

    #[test]
    fn a_reply_that_comes_back_after_stop_is_not_shown() {
        let mut fe = Fake::new();
        fe.stop_in_ask = true;
        let mut convo = vec![Message::user("hi")];
        let (result, _) = play(&mut convo, None, vec![answer("too late")], vec![], &mut fe);
        assert_eq!(result, Ok(()));
        assert!(fe.answers.is_empty());
        // The round was still counted: it cost what it cost.
        assert_eq!(fe.numbers.len(), 1);
    }

    #[test]
    fn an_automatic_call_that_finished_as_stop_was_pressed_still_gets_its_card_and_a_save() {
        for (stopped, shown) in [(false, CardState::Ran), (true, CardState::Stopped)] {
            let mut fe = Fake::new();
            fe.stop_in_ask = true;
            let mut convo = vec![Message::user("look")];
            let reply = Reply::Auto {
                name: "search".into(),
                path: "\"x\"".into(),
                range: None,
                result: "src/a.rs:1: x".into(),
                ok: true,
                stopped,
                plan: None,
                raw_call: raw("search", json!({ "text": "x" })),
            };
            let (result, asked) = play(&mut convo, None, vec![round_of(reply)], vec![], &mut fe);
            assert_eq!(result, Ok(()));
            assert_eq!(asked.len(), 1);
            assert_eq!(fe.cards[0].state, shown);
            assert!(fe.cards[0].auto);
            assert_eq!(fe.saves, 1);
            assert_eq!(convo.len(), 1, "the result is not sent to the model");
        }
    }

    #[test]
    fn an_already_stopped_turn_asks_nothing() {
        let mut fe = Fake::new();
        fe.stop.set(true);
        let mut convo = vec![Message::user("hi")];
        let (result, asked) = play(&mut convo, None, vec![], vec![], &mut fe);
        assert_eq!(result, Ok(()));
        assert!(asked.is_empty());
        assert!(fe.events().is_empty());
    }

    #[test]
    fn starting_ollama_is_said_unless_the_turn_was_stopped_first() {
        let mut fe = Fake::new();
        assert!(opening(&mut fe, true));
        assert!(fe.events().is_empty(), "a running Ollama needs no word");
        assert!(opening(&mut fe, false));
        assert_eq!(fe.events(), ["status:Starting Ollama…"]);
        let mut stopped = Fake::new();
        stopped.stop.set(true);
        assert!(!opening(&mut stopped, false), "nothing to wait for");
        assert!(stopped.events().is_empty());
    }

    // ── a failing check ─────────────────────────────────────────────────────

    /// A `run_check` that finished: how it ended, and whether Smart App Control refused a
    /// file — all the loop reads of it.
    fn checked(ended: Ended, blocked: Option<&str>) -> Result<Ran, String> {
        let outcome = crate::check::Outcome {
            ended: ended.clone(),
            elapsed: std::time::Duration::ZERO,
            output: String::new(),
            bytes: 0,
            complete: true,
            blocked: blocked.map(str::to_string),
            unfinished_tests: 0,
        };
        Ok(Ran {
            result: "the check's report".into(),
            ok: outcome.passed(),
            stopped: ended == Ended::Stopped,
            undo: None,
            check: Some(outcome),
        })
    }

    fn check_failed() -> Result<Ran, String> {
        checked(Ended::Exited(Some(101)), None)
    }

    fn check_passed() -> Result<Ran, String> {
        checked(Ended::Exited(Some(0)), None)
    }

    /// The assistant's words, as the loop puts them in the conversation.
    fn said(text: &str) -> Message {
        Message { role: "assistant".into(), content: text.into(), images: None, tool_calls: None, tool_name: None }
    }

    /// The model runs the check once for each of `checks`, then answers "answer 1",
    /// "answer 2" and so on: four of them, the three a turn can push back and the one it takes.
    fn play_checks(insist: bool, checks: Vec<Result<Ran, String>>) -> (Fake, Vec<Message>, Vec<Asked>) {
        let mut fe = Fake::insisting(checks.len());
        fe.insist = insist;
        let mut convo = vec![Message::user("fix it")];
        let mut asks: Vec<_> = checks.iter().map(|_| asks_about("run_check", "cargo test")).collect();
        asks.extend((1..=4).map(|n| answer(&format!("answer {n}"))));
        let (result, asked) = play(&mut convo, None, asks, checks, &mut fe);
        assert_eq!(result, Ok(()));
        (fe, convo, asked)
    }

    /// The answers the frontend was shown.
    fn answers_shown(fe: &Fake) -> Vec<&str> {
        fe.answers.iter().map(|a| a.0.as_str()).collect()
    }

    #[test]
    fn an_answer_after_a_failed_check_is_pushed_back_when_the_frontend_insists() {
        let mut fe = Fake::insisting(2);
        let mut convo = vec![Message::user("fix it")];
        let asks = vec![
            asks_about("run_check", "cargo test"),
            answer("Fixed it."),
            asks_about("run_check", "cargo test"),
            answer("Now it passes."),
        ];
        let (result, asked) = play(&mut convo, None, asks, vec![check_failed(), check_passed()], &mut fe);
        assert_eq!(result, Ok(()));
        assert_eq!(asked.len(), 4);
        // The answer that was pushed back was never shown; the one that ended the turn was.
        assert_eq!(fe.answers, [("Now it passes.".to_string(), 0, 7)]);
        assert_eq!(fe.unfinished, 0);
        // What the model said is in the conversation, with the loop's reply to it, and the
        // next request was sent both.
        assert_eq!(convo[3], said("Fixed it."));
        assert_eq!(convo[4], Message::user(tools::CHECK_FAILED_NOTE));
        assert_eq!(asked[1].1, 3);
        assert_eq!(asked[2].1, 5);
        // The final answer is left to the frontend, as ever.
        assert_eq!(convo.len(), 7);
    }

    #[test]
    fn the_push_back_tells_the_model_the_work_is_not_done_and_what_to_do() {
        let note = tools::CHECK_FAILED_NOTE;
        assert!(note.starts_with("The last check failed, so the work is not done."), "{note}");
        assert!(note.contains("Fix the code and run the check again."), "{note}");
        assert!(note.contains("say plainly that it is not done and why"), "{note}");
    }

    #[test]
    fn an_answer_after_a_check_that_failed_and_then_passed_ends_the_turn() {
        let (fe, convo, asked) = play_checks(true, vec![check_failed(), check_passed()]);
        assert_eq!(asked.len(), 3);
        assert_eq!(answers_shown(&fe), ["answer 1"]);
        assert_eq!(fe.unfinished, 0);
        assert_eq!(convo.len(), 5, "nothing was pushed back");
    }

    #[test]
    fn an_answer_with_no_check_run_or_a_check_that_passed_ends_the_turn() {
        let cases: [Vec<Result<Ran, String>>; 3] = [vec![], vec![check_passed()], vec![check_passed(), check_passed()]];
        for checks in cases {
            let requests = checks.len() + 1;
            let (fe, convo, asked) = play_checks(true, checks);
            assert_eq!(answers_shown(&fe), ["answer 1"]);
            assert_eq!(fe.unfinished, 0);
            assert_eq!(asked.len(), requests, "one for each check, and the answer");
            assert!(convo.iter().all(|m| m.content != tools::CHECK_FAILED_NOTE));
        }
    }

    #[test]
    fn a_failed_check_stays_failing_through_edits_until_a_check_passes() {
        let mut fe = Fake::insisting(3);
        let mut convo = vec![Message::user("fix it")];
        let asks = vec![
            asks_about("run_check", "cargo test"),
            asks_about("edit_file", "a.rs"),
            answer("Edited it."),
            asks_about("run_check", "cargo test"),
            answer("Passes now."),
        ];
        let runs = vec![check_failed(), ran("Edited a.rs: 1 line removed, 1 added."), check_passed()];
        let (result, asked) = play(&mut convo, None, asks, runs, &mut fe);
        assert_eq!(result, Ok(()));
        assert_eq!(asked.len(), 5);
        // "Edited it." was pushed back: the edit did not make the check pass.
        assert_eq!(answers_shown(&fe), ["Passes now."]);
        assert_eq!(convo.iter().filter(|m| m.content == tools::CHECK_FAILED_NOTE).count(), 1);
    }

    #[test]
    fn a_turn_is_pushed_back_at_most_three_times_and_then_says_it_is_not_done() {
        let (fe, convo, asked) = play_checks(true, vec![check_failed()]);
        // The check, three answers pushed back, and the fourth taken.
        assert_eq!(asked.len(), 5);
        assert_eq!(answers_shown(&fe), ["answer 4"]);
        let notes = convo.iter().filter(|m| m.content == tools::CHECK_FAILED_NOTE).count();
        assert_eq!(notes, MAX_PUSH_BACKS);
        assert_eq!(convo.len(), 3 + 2 * notes);
        // Said after the answer, so a person reads the model's words and then the fact.
        assert_eq!(fe.unfinished, 1);
        let events = fe.events();
        assert_eq!(events[events.len() - 2..], ["answer:answer 4", "unfinished"]);
    }

    /// A `cargo test` that exited 0 with a test program cut short: not a pass (`Outcome::passed`).
    fn check_cut_short() -> Result<Ran, String> {
        let mut ran = checked(Ended::Exited(Some(0)), None)?;
        if let Some(outcome) = ran.check.as_mut() {
            outcome.unfinished_tests = 1;
            ran.ok = outcome.passed();
        }
        Ok(ran)
    }

    #[test]
    fn a_check_that_exited_0_with_a_test_program_cut_short_is_a_failing_check() {
        assert!(!check_cut_short().unwrap().ok);
        // The model says it is done over it, and is pushed back as over any failed check.
        let (fe, convo, asked) = play_checks(true, vec![check_cut_short()]);
        assert_eq!(asked.len(), 5);
        assert_eq!(answers_shown(&fe), ["answer 4"]);
        assert_eq!(convo.iter().filter(|m| m.content == tools::CHECK_FAILED_NOTE).count(), MAX_PUSH_BACKS);
        assert_eq!(fe.unfinished, 1);
        // ...and a later check that really passes clears it.
        let (fe, _, _) = play_checks(true, vec![check_cut_short(), check_passed()]);
        assert_eq!((answers_shown(&fe), fe.unfinished), (vec!["answer 1"], 0));
    }

    #[test]
    fn a_frontend_that_does_not_insist_gets_the_answer_after_a_failed_check_as_ever() {
        let (fe, convo, asked) = play_checks(false, vec![check_failed()]);
        assert_eq!(asked.len(), 2);
        assert_eq!(answers_shown(&fe), ["answer 1"]);
        assert_eq!(fe.unfinished, 0);
        assert_eq!(convo.len(), 3, "the pair for the check, and nothing pushed back");
        assert!(fe.events().iter().all(|e| e != "unfinished"));
    }

    /// A frontend that implements only what has to be, so what it gets is the trait's own.
    struct Bare;

    impl Frontend for Bare {
        fn stopped(&self) -> bool {
            false
        }
        fn status(&mut self, _: &str) {}
        fn round(&mut self, _: RoundNumbers) {}
        fn answer(&mut self, _: &str, _: u32, _: u32) {}
        fn error(&mut self, _: &str) {}
        fn card(&mut self, _: Card) -> usize {
            0
        }
        fn update(&mut self, _: usize, _: Patch) {}
        fn decide(&mut self, _: usize, _: &PendingCall) -> Decision {
            Decision::Allow
        }
        fn save(&mut self) {}
    }

    #[test]
    fn a_frontend_does_not_insist_on_checks_unless_it_says_so() {
        let mut bare = Bare;
        assert!(!bare.insist_on_checks());
        // Run on a failed check, the loop takes its answer like every turn before it.
        let mut convo = vec![Message::user("fix it")];
        let asks = vec![asks_about("run_check", "cargo test"), answer("Done.")];
        let mut asks = VecDeque::from(asks);
        let mut runs = VecDeque::from(vec![check_failed()]);
        let result = run_turn_with(
            &mut convo,
            None,
            tools::MAX_ROUNDS,
            &mut |_: &[Message], _: Option<Calibration>, _: bool| asks.pop_front().expect("one request too many"),
            &mut |_: &str, _: &str, _: &str| runs.pop_front().expect("one run too many"),
            &mut bare,
        );
        assert_eq!(result, Ok(()));
        assert_eq!(convo.len(), 3);
    }

    #[test]
    fn stop_after_a_push_back_ends_the_turn_and_says_nothing_of_the_check() {
        let mut fe = Fake::insisting(1);
        fe.stop_at_ask = Some(2);
        let mut convo = vec![Message::user("fix it")];
        let asks = vec![asks_about("run_check", "cargo test"), answer("Done."), answer("Really done.")];
        let (result, asked) = play(&mut convo, None, asks, vec![check_failed()], &mut fe);
        assert_eq!(result, Ok(()));
        assert_eq!(asked.len(), 3, "no closing request");
        assert!(fe.answers.is_empty() && fe.errors.is_empty(), "what comes back after Stop is not shown");
        assert_eq!(fe.unfinished, 0, "the person ended it");
        // What was pushed back before the Stop stays in the conversation the turn leaves.
        assert_eq!(convo[3], said("Done."));
    }

    #[test]
    fn stop_while_the_check_runs_ends_the_turn_without_a_word_about_it() {
        let mut fe = Fake::insisting(2);
        fe.stop_in_run = true;
        let mut convo = vec![Message::user("fix it")];
        let stopped = checked(Ended::Stopped, None);
        let (result, asked) = play(&mut convo, None, vec![asks_about("run_check", "cargo test")], vec![stopped], &mut fe);
        assert_eq!(result, Ok(()));
        assert_eq!(asked.len(), 1);
        assert_eq!(fe.unfinished, 0);
        assert_eq!(fe.cards[0].state, CardState::Stopped);
    }

    #[test]
    fn a_check_that_never_judged_the_code_neither_fails_the_turn_nor_clears_a_failure() {
        let timed_out = || checked(Ended::TimedOut, None);
        let blocked = || checked(Ended::Exited(Some(1)), Some("Smart App Control blocked build_script_build.exe"));
        let could_not_start = || ran("error: program not found");
        // The checks the model ran, and whether an answer after them is pushed back.
        let cases: Vec<(Vec<Result<Ran, String>>, bool)> = vec![
            (vec![timed_out()], false),
            (vec![blocked()], false),
            (vec![could_not_start()], false),
            (vec![check_failed(), timed_out()], true),
            (vec![check_failed(), blocked()], true),
            (vec![check_failed(), could_not_start()], true),
            (vec![check_failed(), check_passed(), timed_out()], false),
            (vec![check_passed(), check_failed()], true),
        ];
        for (n, (checks, pushed_back)) in cases.into_iter().enumerate() {
            let (fe, _, _) = play_checks(true, checks);
            let taken = if pushed_back { "answer 4" } else { "answer 1" };
            assert_eq!(answers_shown(&fe), [taken], "case {n}");
            assert_eq!(fe.unfinished, usize::from(pushed_back), "case {n}");
        }
    }

    #[test]
    fn a_blank_answer_after_a_failed_check_is_pushed_back_without_putting_the_blank_in_the_conversation() {
        let mut fe = Fake::insisting(2);
        let mut convo = vec![Message::user("fix it")];
        let asks = vec![asks_about("run_check", "cargo test"), answer("  "), asks_about("run_check", "cargo test"), answer("ok")];
        let (result, asked) = play(&mut convo, None, asks, vec![check_failed(), check_passed()], &mut fe);
        assert_eq!(result, Ok(()));
        assert_eq!(asked[2].1, 4, "only the loop's reply was added");
        assert_eq!(convo[3], Message::user(tools::CHECK_FAILED_NOTE));
        assert_eq!(answers_shown(&fe), ["ok"]);
    }

    #[test]
    fn a_push_back_is_a_round_so_the_bound_still_ends_the_turn() {
        let mut fe = Fake::insisting(1);
        let mut convo = vec![Message::user("fix it")];
        let asks = vec![asks_about("run_check", "cargo test"), answer("one"), answer("two"), answer("closing words")];
        let (result, asked) = play_bounded(3, &mut convo, None, asks, vec![check_failed()], &mut fe);
        assert_eq!(result, Ok(()));
        assert_eq!(asked.len(), 4, "three rounds and the closing request");
        assert_eq!(asked[3], (true, 7, None));
        // The closing words are the model's last, pushed back or not, and the banner follows.
        assert_eq!(answers_shown(&fe), ["closing words"]);
        assert_eq!(fe.errors, ["Stopped after 3 tool rounds. The last step ran — check the result."]);
        let events = fe.events();
        assert_eq!(events[events.len() - 2..], ["error:Stopped after 3 tool rounds. The last step ran — check the result.", "unfinished"]);
    }

    #[test]
    fn a_stuck_model_with_its_check_failing_is_said_to_have_ended_unfinished() {
        let mut fe = Fake::insisting(1);
        let mut convo = vec![Message::user("fix it")];
        let stuck = round_of(Reply::Stuck { error: "the model kept repeating a call to edit_file".into() });
        let asks = vec![asks_about("run_check", "cargo test"), stuck];
        let (result, _) = play(&mut convo, None, asks, vec![check_failed()], &mut fe);
        assert_eq!(result, Ok(()));
        assert_eq!(fe.errors, ["the model kept repeating a call to edit_file"]);
        assert_eq!(fe.unfinished, 1);
    }

    #[test]
    fn a_turn_that_fails_to_run_is_not_said_to_have_ended_unfinished() {
        let mut fe = Fake::insisting(1);
        let mut convo = vec![Message::user("fix it")];
        let asks = vec![asks_about("run_check", "cargo test"), Err("POST /api/chat: connection refused".to_string())];
        let (result, _) = play(&mut convo, None, asks, vec![check_failed()], &mut fe);
        assert_eq!(result, Err("POST /api/chat: connection refused".to_string()));
        assert_eq!(fe.unfinished, 0, "the error is what the person is shown");
    }

    // ── what the model was sent ─────────────────────────────────────────────

    #[test]
    fn what_was_set_aside_is_copied_into_the_conversation_and_the_calibration_carries_on() {
        let mut fe = Fake::new();
        let mut convo = vec![Message::user("find it")];
        for n in 0..4 {
            convo.push(assistant(raw("search", json!({ "text": format!("x{n}") }))));
            convo.push(Message::tool_result("search", "x".repeat(6000)));
        }
        // What a real round would have done to its copy: the older results are stubs.
        let mut copy = convo.clone();
        let first = compact_for_send(&mut copy, &[], 8192, None, 0);
        assert!(!first.set_aside().is_empty(), "24000 characters should not fit 8192");
        let (chars, aside) = (first.prompt_chars(), first.set_aside().len());
        let with = |sent: Sent, prompt_eval_count: u32, n: usize| {
            let mut round = rejected(n).unwrap();
            (round.sent, round.prompt_eval_count) = (sent, prompt_eval_count);
            Ok(round)
        };
        // Round 2 reports no count of its own, so the calibration stays as round 1 left it.
        let asks = vec![with(first, 4321, 0), with(sent(), 0, 1), answer("ok")];
        let start = Some(Calibration::new(1, 2));
        let (result, asked) = play(&mut convo, start, asks, vec![], &mut fe);
        assert_eq!(result, Ok(()));
        let cal = Some(Calibration::new(chars, 4321));
        assert_eq!(asked.iter().map(|a| a.2).collect::<Vec<_>>(), [start, cal, cal]);
        let numbers = RoundNumbers { prompt_eval_count: 4321, prompt_chars: chars, set_aside: aside, fits: true, recovered: false, fed_back: false };
        assert_eq!(fe.numbers[0], numbers);
        // `convo` is what was sent: the stubs, not the 6000 characters.
        assert_eq!(convo[..9], copy[..]);
        assert!(convo[2].content.starts_with(crate::compact::STUB_MARKER));
        assert_eq!(convo[8].content.len(), 6000, "the newest result stays whole");
    }

    // ── what the frontend is sent ───────────────────────────────────────────

    #[test]
    fn a_card_is_the_webviews_tool_card_in_camel_case_with_empty_fields_left_out() {
        let mut card = Card::new("move_file", "a.txt", CardState::Pending);
        let bare = json!({ "name": "move_file", "path": "a.txt", "state": "pending" });
        assert_eq!(serde_json::to_value(&card).unwrap(), bare);
        card.args = Some(json!({ "path": "a.txt", "to": "sub/b.txt" }));
        card.to = Some("sub/b.txt".into());
        card.folders = vec!["sub/".into()];
        card.size = Some(0);
        card.changed_build_files = vec!["Cargo.toml".into()];
        card.range = Some(tools::Lines { start: 1, end: None });
        card.auto = true;
        card.plan = Some(vec![tools::PlanStep { text: "a".into(), status: tools::PlanStatus::Doing }]);
        assert_eq!(
            serde_json::to_value(&card).unwrap(),
            json!({
                "name": "move_file",
                "path": "a.txt",
                "state": "pending",
                "args": { "path": "a.txt", "to": "sub/b.txt" },
                "range": { "start": 1, "end": null },
                "changedBuildFiles": ["Cargo.toml"],
                "to": "sub/b.txt",
                "folders": ["sub/"],
                "size": 0,
                "auto": true,
                "plan": [{ "text": "a", "status": "doing" }],
            })
        );
    }

    #[test]
    fn a_patch_names_only_what_changed() {
        assert_eq!(serde_json::to_value(Patch::state(CardState::Running)).unwrap(), json!({ "state": "running" }));
        let failed = Patch {
            state: Some(CardState::Failed),
            ok: Some(false),
            result: Some("error: gone".into()),
            error: Some("error: gone".into()),
            undo: Some("ab12".into()),
        };
        assert_eq!(
            serde_json::to_value(failed).unwrap(),
            json!({ "state": "failed", "ok": false, "result": "error: gone", "error": "error: gone", "undo": "ab12" })
        );
    }
}
