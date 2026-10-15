//! One round of a tool turn: ask the model once, then say what it wants.
//!
//! This is the body of Mimir's `ask_with_tools` command with the Tauri taken out,
//! so the same round can be driven from a webview, a terminal or a probe. It does
//! what the command did, in the same order and in the same words: take a place on
//! the Stop counter, make sure Ollama is up, check that the model can call tools,
//! find the folder with its check and notes, send the conversation (setting old
//! results aside when it will not fit) and sort what came back into a [`Reply`].
//!
//! **The reply is typed, and decided here.** Whether a call needs a person is one
//! rule ([`tools::approval`]), applied once, in this file — a frontend is handed
//! the answer: an automatic call that has already run, a call armed and waiting for
//! a decision, or one rejected without asking anybody. None of them has to work out
//! what a call is allowed to do.
//!
//! **Blocking, and nothing here prints.** The caller drives it from a thread of its
//! own, and logs what it wants to know: [`Round::recovered`] says the reply needed
//! the raw retry, which Mimir writes to its log.
//!
//! **Moved here unchanged (I1).** Mimir's old `ask_with_tools` command did this for
//! its webview, one round per call; the loop over rounds is now [`super::turn`]'s,
//! which Mimir runs through its `run_turn` command, and 2c moves the probes onto it.

use super::run::run_auto;
use super::send::{compact_for_send, Calibration, Sent};
use super::stop::next_stop;
use super::ticket::{arm, plan_name, DiffView, Find, Payload, PendingCall};
use super::workspace;
use crate::chat::{self, Message, Options};
use crate::{check, config, edit, instructions, ollama, search, tools};

/// What one round came to, and what it cost.
pub struct Round {
    pub reply: Reply,
    /// Ollama's own count of the prompt this round answered; 0 when it did not say.
    /// What the next round is calibrated from.
    pub prompt_eval_count: u32,
    /// The reply needed the raw retry of [`chat::chat_tools`]: Ollama refused a
    /// malformed call, or the call came back as plain text. Said here, not printed,
    /// because nothing in this crate prints. How often it fires is how you know
    /// whether the recovery is still needed.
    pub recovered: bool,
    /// What was set aside to make the prompt fit, and how much was sent.
    pub sent: Sent,
}

/// What the model said, sorted by what happens next.
pub enum Reply {
    /// Words, and no call. It is also what the closing request always returns,
    /// whatever the reply holds.
    Answer { content: String, eval_count: u32 },
    /// A call that has already failed identically, twice, and was sent a third
    /// time. The turn ends: the file has not changed, so another try cannot go
    /// differently. `error` is for the person; the model is told nothing.
    Stuck { error: String },
    /// A call that could never have worked, like an edit whose text is not in the
    /// file. It goes straight back to the model with the reason, and nobody is asked.
    ///
    /// **Every rejection starts `error:` (R-20)**: the unreadable call, the refusals
    /// of [`tools::interpret`], the 7th plan update. The repeat rule counts only
    /// results that start that way, and the probes' results do. `raw_call` goes
    /// back with it, so the conversation shows the model its own attempt.
    Rejected { name: String, path: String, error: String, raw_call: chat::ToolCall },
    /// A call that needs no approval, already run: a read inside the project folder
    /// of a file the listing would show, a listing, a search, or the task list
    /// (`update_plan`, which touches nothing and works even with no folder).
    ///
    /// `plan` is set only for `update_plan`: the list, parsed. **Never arms a ticket
    /// and never touches the pending slot**: nothing here awaits a decision, so
    /// nothing may replace or leave one.
    Auto {
        name: String,
        /// What the card would show.
        path: String,
        /// The lines of a read, as Rust normalised them.
        range: Option<tools::Lines>,
        result: String,
        /// Whether it worked. A search with no matches worked.
        ok: bool,
        /// Ended by Stop rather than finishing.
        stopped: bool,
        plan: Option<Vec<tools::PlanStep>>,
        raw_call: chat::ToolCall,
    },
    /// A call that needs a decision. `call` is armed: the ticket in it is the only
    /// way to run exactly this call, via [`super::run::run_approved`].
    ///
    /// `raw_call` is what the model sent, so the caller can put the assistant turn
    /// that asked back into the conversation before the tool result. **Without it
    /// the loop does not close.** Ollama expects assistant(tool_calls) →
    /// tool(result); a tool message with no call in front of it answers a question
    /// the model cannot see itself asking.
    Call { call: PendingCall, raw_call: chat::ToolCall },
}

/// Ask the model, offering the tools, and report back what it wants.
///
/// **What it runs itself is only what [`tools::approval`] calls automatic** — a
/// read inside the project folder of a file the listing would show, a listing, a
/// search (H5; `ask_every_call` in the config turns it off), and the task list
/// (`update_plan`, H10, which touches nothing and works even with no folder or
/// with `ask_every_call` on). Those come back as [`Reply::Auto`], already run.
/// Everything else it returns as an answer, a request to approve, or — for a
/// request that could never succeed, like an edit whose text is not in the file —
/// a rejection, which goes straight back to the model so it can read the file again
/// and retry without the user being asked about something impossible.
///
/// **`closing` is the last request of a turn that hit its bound (R-03).** It
/// offers no tools, adds [`tools::CLOSING_NOTE`] as one last user message, runs
/// nothing, and returns an [`Reply::Answer`] whatever the reply holds — so the turn
/// ends in the model's own words about what was done. Ollama is still started and
/// the conversation still compacted, since it may be the largest request of the
/// turn; the note is added **after** compaction, so it is never in
/// [`Sent::set_aside`] and the caller's copy stays a prefix of what was sent.
///
/// `messages` is the caller's conversation, without the system prompt: this puts
/// it in front when a folder is chosen. `cfg` is read by the caller for this round,
/// so an edit to the config applies to the next one.
pub fn round(
    messages: &[Message],
    model: &str,
    cfg: &config::Config,
    calibration: Option<Calibration>,
    closing: bool,
) -> Result<Round, String> {
    // Taken **before the model is asked**, for an automatic search or listing:
    // there is no card to take it at, and a Stop pressed while the model is
    // still answering must reach the walk that follows.
    let stop = next_stop();

    // The capability check below ignores an error, so without this a stopped Ollama
    // would pass it and fail later with a raw connection error — after a reboot,
    // with nothing else having started it.
    start_ollama()?;

    // Offering tools to a model that cannot carry them wastes a round trip and
    // produces a confusing prose answer instead of a call. Better to say so.
    if let Ok(detail) = ollama::show_cached(model)
        && !detail.has("tools")
    {
        return Err(format!("{model} cannot call tools — pick one with the tools badge"));
    }

    let ws = workspace(cfg);
    // The folder's check command, if one was chosen for it. Read with the folder,
    // so a check can never be offered for a folder other than the one in force.
    let check: Option<Vec<String>> = ws.as_ref().and_then(|ws| cfg.check_for(&ws.root_display()).cloned());
    let check_label = check.as_deref().map(check::label);

    // The folder and the read-before-edit habit, stated once. Added here rather
    // than stored in the thread, so it always names the folder chosen *now*.
    //
    // **The project's notes ride in that same message** (H10), after the prompt, so
    // there is still exactly one system message and `lead` below is unchanged. Only
    // when the user turned them on for this folder, and read fresh every turn: an
    // edited file applies to the next question.
    let mut messages = messages.to_vec();
    if let Some(ws) = &ws {
        let mut prompt = tools::system_prompt(ws, check.is_some());
        if cfg.instructions_for(&ws.root_display())
            && let Some(notes) = instructions::find(ws)
        {
            prompt = tools::with_notes(prompt, &notes);
        }
        messages.insert(0, Message::system(prompt));
    }

    // Kept for counting repeats: the history the model has already seen.
    let history = messages.clone();
    let num_ctx = cfg.num_ctx;
    // Nothing is offered to a closing request, and compaction counts what is sent.
    let offered = if closing { Vec::new() } else { tools::available(ws.is_some(), check_label.as_deref()) };

    // Inside a turn the conversation only grows, and past `num_ctx` Ollama cuts
    // the middle of it without a word (H7). So old results are set aside here,
    // in the copy being sent, and the caller is told which so that its own copy
    // ends up identical — a prefix that changed would also cost the prompt cache.
    let sent = compact_for_send(&mut messages, &offered, num_ctx, calibration, usize::from(ws.is_some()));
    if closing {
        messages.push(Message::user(tools::CLOSING_NOTE));
        // `chat`, not `chat_tools`: with no tools there is nothing for the recovery
        // to turn text into, and a call the model writes anyway stays words.
        let reply = chat::chat(model, &messages, &[], Options::new(num_ctx))?;
        return Ok(Round {
            reply: Reply::Answer { content: reply.message.content, eval_count: reply.eval_count },
            prompt_eval_count: reply.prompt_eval_count,
            recovered: reply.recovered,
            sent,
        });
    }
    let reply = chat::chat_tools(model, &messages, &offered, Options::new(num_ctx))?;
    let cx = Context {
        history: &history,
        ws: ws.as_ref(),
        check: check.as_deref(),
        ask_every_call: cfg.ask_every_call,
        stop,
    };
    Ok(Round {
        reply: classify(&reply, &cx, &mut arm),
        prompt_eval_count: reply.prompt_eval_count,
        recovered: reply.recovered,
        sent,
    })
}

/// Make sure Ollama is answering, starting it if not, before a request that needs it.
///
/// Ollama has no autostart and does not survive a reboot (A1), so the first
/// request after a restart would otherwise fail rather than wait. A frontend that
/// wants to say what the wait is asks [`ollama::is_running`] first; the round starts
/// Ollama all the same, so a tool turn never depends on the frontend for it.
fn start_ollama() -> Result<(), String> {
    if !ollama::is_running() {
        ollama::ensure_running().map_err(|e| format!("could not start Ollama: {e}"))?;
    }
    Ok(())
}

/// What a reply is judged against.
struct Context<'a> {
    /// The conversation as it was sent, before anything was set aside.
    history: &'a [Message],
    ws: Option<&'a edit::Workspace>,
    /// The folder's check command, if one was chosen for it.
    check: Option<&'a [String]>,
    ask_every_call: bool,
    /// This round's place on the Stop counter, for an automatic search or listing.
    stop: u64,
}

/// What the model's call is, and what happens to it.
///
/// `arm` issues the ticket for a call that has to be asked about — the real one in
/// [`round`], and a recording one in the tests, which must not touch the one pending
/// slot the whole process shares.
fn classify(
    reply: &chat::ChatReply,
    cx: &Context,
    arm: &mut dyn FnMut(&str, &str, Option<Payload>) -> String,
) -> Reply {
    let (history, ws) = (cx.history, cx.ws);

    // The first call only; a model that asks for several at once gets the rest
    // dropped and asks again. That was because each needed its own approval;
    // since H5 an automatic one does not, so several could run in one round.
    // Left as it is (review R-34): Qwen3-Coder asks for one call per reply.
    let Some(call) = reply.tool_calls().first().cloned() else {
        return Reply::Answer { content: reply.message.content.clone(), eval_count: reply.eval_count };
    };

    // A call that has already failed identically, twice, ends the turn: the file
    // has not changed, so a third try cannot go differently, and each one costs
    // a round. Measured at G7 - the model once repeated one impossible edit
    // eight times running.
    let prior = tools::prior_failures(history, &call);
    if prior + 1 >= tools::MAX_IDENTICAL_FAILURES {
        return Reply::Stuck {
            error: format!(
                "the model kept repeating a call to {} that could not be carried out, so it was stopped",
                call.function.name
            ),
        };
    }

    // Rejected without asking anyone: the reason goes back to the model. The raw
    // call goes with it, so the conversation shows the model its own attempt.
    //
    // **Every rejection starts `error:` (R-20)**: the unreadable call, the refusals of
    // `interpret`, the 7th plan update. The repeat rule counts only results that start
    // that way, and the probe's results do.
    let reject = |name: &str, path: &str, why: String| {
        let why = tools::failure(&why);
        let why = if prior > 0 { format!("{why}{}", tools::repeated_note(name)) } else { why };
        Reply::Rejected { name: name.to_string(), path: path.to_string(), error: why, raw_call: call.clone() }
    };

    if call.function.name == chat::INVALID_TOOL_CALL {
        let why = call.function.arguments["error"].as_str().unwrap_or("unreadable").to_string();
        return reject("tool call", "", format!("{why}. Send the call again as valid JSON."));
    }
    let request = match tools::interpret(&call) {
        Ok(r) => r,
        Err(e) => return reject(&call.function.name, request_path(&call), e),
    };

    // A finding call that could never run — a pattern outside the folder, text
    // over several lines — goes back to the model with the reason, without
    // asking anyone to approve something that can only fail.
    if let (Some(ws), tools::Request::List { pattern } | tools::Request::Search { pattern, .. }) = (ws, &request) {
        let text = match &request {
            tools::Request::Search { text, .. } => Some(text.as_str()),
            _ => None,
        };
        if let Err(e) = search::validate(ws, text, pattern.as_deref()) {
            return reject(request.name(), "", format!("error: {e}"));
        }
    }

    // The task list: at most 6 updates a turn, by the rule the probes share. The
    // 7th goes back to the model and costs a round like any rejected call.
    if matches!(request, tools::Request::Plan { .. }) && tools::plan_updates(history) >= tools::MAX_PLAN_UPDATES {
        return reject("update_plan", "", tools::PLAN_LIMIT_NOTE.to_string());
    }

    // What may run without a card. Decided here, once, by the one rule the probe
    // shares.
    match tools::approval(ws, &request, cx.ask_every_call) {
        tools::Approval::Reject(why) => {
            return reject(request.name(), request.path(), format!("error: {why}"));
        }
        // Touches nothing, so there is nothing to run: the card is the list itself.
        // It is not a round (the loop does not count it), and works with no folder
        // or with `ask_every_call`, as `approval` says.
        tools::Approval::Auto(tools::Auto::Plan { steps }) => {
            return Reply::Auto {
                name: "update_plan".to_string(),
                path: String::new(),
                range: None,
                result: tools::plan_message(&steps),
                ok: true,
                stopped: false,
                plan: Some(steps),
                raw_call: call,
            };
        }
        tools::Approval::Auto(auto) => {
            let ws = ws.expect("a call is only automatic inside a project folder");
            // `None`: the read could not be made safely, so it asks like any other.
            if let Some((name, path, range, mut ran)) = run_auto(ws, auto, cx.stop) {
                if !ran.ok && prior > 0 {
                    ran.result.push_str(tools::repeated_note(name));
                }
                // **Never `arm`s a ticket and never touches PENDING**: nothing
                // here awaits a decision, so nothing here may replace or leave one.
                return Reply::Auto {
                    name: name.to_string(),
                    path,
                    range,
                    result: ran.result,
                    ok: ran.ok,
                    stopped: ran.stopped,
                    plan: None,
                    raw_call: call,
                };
            }
        }
        tools::Approval::Ask => {}
    }

    let (name, path, payload) = match request {
        tools::Request::Read { path, range } => {
            let (open, shown) = tools::read_target(&path, ws);
            ("read_file", shown, Some(Payload::Read { real: open, range }))
        }
        tools::Request::List { .. } | tools::Request::Search { .. } if ws.is_none() => {
            return reject(
                request.name(),
                "",
                "error: there is no project folder to search — the user has not chosen one".into(),
            );
        }
        tools::Request::List { pattern } => {
            let ws = ws.expect("checked above");
            let shown = search::describe_list(pattern.as_deref());
            let find = Find::List { pattern };
            ("list_files", shown, Some(Payload::Find { root: ws.root_display(), find, stop: next_stop() }))
        }
        tools::Request::Search { text, pattern } => {
            let ws = ws.expect("checked above");
            let shown = search::describe_search(&text, pattern.as_deref());
            let find = Find::Search { text, pattern };
            ("search", shown, Some(Payload::Find { root: ws.root_display(), find, stop: next_stop() }))
        }
        // The model can call it from history, or invent it. Either way there is
        // only something to run when a person chose a command for this folder.
        tools::Request::Check => match (ws, cx.check) {
            (Some(ws), Some(argv)) => (
                "run_check",
                check::label(argv),
                Some(Payload::Check { root: ws.root_display(), argv: argv.to_vec(), stop: next_stop() }),
            ),
            _ => {
                return reject("run_check", "", format!("error: {}", tools::CHECK_UNAVAILABLE));
            }
        },
        tools::Request::Plan { .. } => {
            return reject("update_plan", "", "error: the task list could not be read".into());
        }
        tools::Request::Rewrite { path, .. } => {
            return reject("write_file", &path, "no such tool: write_file — use edit_file".into());
        }
        tools::Request::Edit { .. }
        | tools::Request::Create { .. }
        | tools::Request::Delete { .. }
        | tools::Request::Move { .. }
            if ws.is_none() =>
        {
            return reject(
                request.name(),
                request.path(),
                "editing is off: the user has not chosen a project folder".into(),
            );
        }
        tools::Request::Edit { path, old_text, new_text } => {
            let ws = ws.expect("checked above");
            match tools::plan_edit(ws, &path, &old_text, &new_text, cx.ask_every_call) {
                Ok(p) => ("edit_file", p.display.clone(), Some(Payload::Edit(p))),
                Err(e) => return reject("edit_file", request_path(&call), format!("error: {e}")),
            }
        }
        tools::Request::Create { path, content } => {
            let ws = ws.expect("checked above");
            match edit::plan_create(ws, &path, &content) {
                Ok(p) => ("create_file", p.display.clone(), Some(Payload::Edit(p))),
                Err(e) => return reject("create_file", request_path(&call), format!("error: {e}")),
            }
        }
        tools::Request::Delete { path } => {
            let ws = ws.expect("checked above");
            match edit::plan_delete(ws, &path) {
                Ok(p) => ("delete_file", p.display.clone(), Some(Payload::Edit(p))),
                Err(e) => return reject("delete_file", request_path(&call), format!("error: {e}")),
            }
        }
        tools::Request::Move { path, to } => {
            let ws = ws.expect("checked above");
            match edit::plan_move(ws, &path, &to) {
                Ok(p) => ("move_file", p.display.clone(), Some(Payload::Edit(p))),
                Err(e) => return reject("move_file", request_path(&call), format!("error: {e}")),
            }
        }
    };
    // The plan's kind is the call's name; `run_approved` checks it again.
    if let Some(Payload::Edit(p)) = &payload {
        assert_eq!(plan_name(p), name, "a plan must be armed under its own tool name");
    }
    let (to, folders, size) = match &payload {
        Some(Payload::Edit(p)) => (
            p.to_display.clone(),
            p.folders.clone(),
            matches!(p.op, edit::Op::Delete | edit::Op::Move).then_some(p.size),
        ),
        _ => (None, Vec::new(), None),
    };

    let diff = match &payload {
        Some(Payload::Edit(p)) => Some(DiffView {
            hunks: p.hunks.clone(),
            added: p.added,
            removed: p.removed,
            create: p.create,
        }),
        _ => None,
    };
    let range = match &payload {
        Some(Payload::Read { range, .. }) => *range,
        _ => None,
    };
    let changed_build_files = if name == "run_check" { tools::build_files_changed(history) } else { Vec::new() };
    Reply::Call {
        call: PendingCall {
            token: arm(name, &path, payload),
            name: name.to_string(),
            path,
            diff,
            range,
            changed_build_files,
            to,
            folders,
            size,
        },
        // The raw call, so the caller can put the assistant turn that asked
        // for it back into the conversation before the tool result.
        raw_call: call,
    }
}

fn request_path(call: &chat::ToolCall) -> &str {
    call.function.arguments["path"].as_str().unwrap_or("")
}

// What the model's call comes to is decided here, and none of it had a test while
// it was a block inside a Tauri command. These drive `classify` - everything in
// `round` after the model has answered - so no Ollama is needed. A recording `arm`
// stands in for the real one: the real one writes the one pending slot the whole
// process shares, which `ticket`'s own tests read.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::{ChatReply, FunctionCall, ToolCall};
    use serde_json::json;
    use std::path::PathBuf;
    use std::time::Duration;

    /// A throwaway project at `dir/project`, removed on drop.
    struct Sandbox {
        dir: PathBuf,
    }

    impl Sandbox {
        fn new(tag: &str) -> Self {
            let n = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            let dir = std::env::temp_dir().join(format!("bifrost-round-{tag}-{}-{n}", std::process::id()));
            std::fs::create_dir_all(dir.join("project")).unwrap();
            Self { dir }
        }

        fn ws(&self) -> edit::Workspace {
            edit::Workspace::open(self.dir.join("project")).unwrap()
        }

        fn write(&self, rel: &str, text: &str) {
            let p = self.dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall { id: None, function: FunctionCall { name: name.into(), arguments } }
    }

    fn said(content: &str, calls: Vec<ToolCall>) -> ChatReply {
        ChatReply {
            message: Message {
                role: "assistant".into(),
                content: content.into(),
                images: None,
                tool_calls: (!calls.is_empty()).then_some(calls),
                tool_name: None,
            },
            load: Duration::ZERO,
            total: Duration::ZERO,
            ttft: None,
            prompt_eval_count: 0,
            eval_count: 5,
            recovered: false,
        }
    }

    /// What `arm` was asked to issue a ticket for: the tool, the path, the payload.
    type Armed = Vec<(String, String, Option<Payload>)>;

    /// Judge `reply` in this setting. A walk is given a Stop number that is never
    /// stopped, so it does not matter what else is running.
    fn judge(
        reply: &ChatReply,
        history: &[Message],
        ws: Option<&edit::Workspace>,
        check: Option<&[String]>,
        ask_every_call: bool,
    ) -> (Reply, Armed) {
        let mut armed = Armed::new();
        let cx = Context { history, ws, check, ask_every_call, stop: u64::MAX };
        let reply = classify(reply, &cx, &mut |name: &str, path: &str, payload: Option<Payload>| {
            armed.push((name.to_string(), path.to_string(), payload));
            format!("ticket-{}", armed.len())
        });
        (reply, armed)
    }

    /// One call, judged.
    fn judge_call(
        call: ToolCall,
        ws: Option<&edit::Workspace>,
        check: Option<&[String]>,
        ask_every_call: bool,
    ) -> (Reply, Armed) {
        judge(&said("", vec![call]), &[], ws, check, ask_every_call)
    }

    fn kind(reply: &Reply) -> &'static str {
        match reply {
            Reply::Answer { .. } => "answer",
            Reply::Stuck { .. } => "stuck",
            Reply::Rejected { .. } => "rejected",
            Reply::Auto { .. } => "auto",
            Reply::Call { .. } => "call",
        }
    }

    /// `(name, path, error)` of a rejection, and nothing was armed for it.
    fn rejection((reply, armed): (Reply, Armed)) -> (String, String, String) {
        assert!(armed.is_empty(), "a rejected call must not be armed");
        match reply {
            Reply::Rejected { name, path, error, .. } => (name, path, error),
            other => panic!("expected a rejection, got {}", kind(&other)),
        }
    }

    /// The card and the one ticket issued for it.
    fn asking((reply, mut armed): (Reply, Armed)) -> (PendingCall, (String, String, Option<Payload>)) {
        assert_eq!(armed.len(), 1, "exactly one ticket is issued for a call that asks");
        match reply {
            Reply::Call { call, .. } => (call, armed.remove(0)),
            other => panic!("expected a call to ask about, got {}", kind(&other)),
        }
    }

    /// The tool calls the assistant made and what came back, as the history holds them.
    fn exchange(name: &str, args: serde_json::Value, result: &str) -> [Message; 2] {
        let asked = Message {
            role: "assistant".into(),
            content: String::new(),
            images: None,
            tool_calls: Some(vec![call(name, args)]),
            tool_name: None,
        };
        [asked, Message::tool_result(name, result)]
    }

    // ── answered, stuck, turned away ────────────────────────────────────────

    #[test]
    fn words_with_no_call_are_an_answer() {
        let (reply, armed) = judge(&said("It is in main.rs.", vec![]), &[], None, None, false);
        let Reply::Answer { content, eval_count } = reply else { panic!("not an answer") };
        assert_eq!((content.as_str(), eval_count), ("It is in main.rs.", 5));
        assert!(armed.is_empty());
    }

    #[test]
    fn only_the_first_call_of_a_reply_is_looked_at() {
        let calls = vec![call("search", json!({})), call("update_plan", json!({ "plan": "[ ] a" }))];
        let (name, ..) = rejection(judge(&said("", calls), &[], None, None, false));
        assert_eq!(name, "search");
    }

    #[test]
    fn a_call_that_failed_twice_already_ends_the_turn_as_stuck() {
        let args = json!({ "path": "a.rs", "old_text": "x", "new_text": "y" });
        let mut history = vec![Message::user("fix it")];
        for _ in 0..2 {
            history.extend(exchange("edit_file", args.clone(), "error: old_text was not found"));
        }
        let (reply, armed) = judge(&said("", vec![call("edit_file", args)]), &history, None, None, false);
        let Reply::Stuck { error } = reply else { panic!("not stuck: {}", kind(&reply)) };
        assert_eq!(
            error,
            "the model kept repeating a call to edit_file that could not be carried out, so it was stopped"
        );
        assert!(armed.is_empty());
    }

    #[test]
    fn a_call_that_failed_once_is_rejected_again_with_the_repeat_note() {
        let args = json!({ "old_text": "x" });
        let mut history = vec![Message::user("fix it")];
        history.extend(exchange("edit_file", args.clone(), "error: edit_file was called without a path"));
        let got = rejection(judge(&said("", vec![call("edit_file", args)]), &history, None, None, false));
        let first_time = "error: edit_file was called without a path";
        let again = format!("{first_time}{}", tools::repeated_note("edit_file"));
        assert_eq!(got, ("edit_file".to_string(), String::new(), again));
    }

    #[test]
    fn a_call_that_could_not_be_read_goes_back_with_the_reason() {
        let unreadable = call(chat::INVALID_TOOL_CALL, json!({ "error": "unterminated string" }));
        assert_eq!(
            rejection(judge_call(unreadable, None, None, false)),
            (
                "tool call".to_string(),
                String::new(),
                "error: unterminated string. Send the call again as valid JSON.".to_string()
            )
        );
        let no_reason = call(chat::INVALID_TOOL_CALL, json!({}));
        let (_, _, error) = rejection(judge_call(no_reason, None, None, false));
        assert_eq!(error, "error: unreadable. Send the call again as valid JSON.");
    }

    #[test]
    fn a_call_the_tools_refuse_goes_back_with_their_words() {
        let got = rejection(judge_call(call("edit_file", json!({ "old_text": "x" })), None, None, false));
        let no_path = "error: edit_file was called without a path".to_string();
        assert_eq!(got, ("edit_file".to_string(), String::new(), no_path));
        let got = rejection(judge_call(call("frobnicate", json!({ "path": "a.rs" })), None, None, false));
        let no_tool = "error: no such tool: frobnicate".to_string();
        assert_eq!(got, ("frobnicate".to_string(), "a.rs".to_string(), no_tool));
    }

    #[test]
    fn write_file_is_not_a_tool_the_app_has() {
        let write = call("write_file", json!({ "path": "a.rs", "content": "x" }));
        assert_eq!(
            rejection(judge_call(write, None, None, false)),
            (
                "write_file".to_string(),
                "a.rs".to_string(),
                "error: no such tool: write_file — use edit_file".to_string()
            )
        );
    }

    #[test]
    fn editing_is_off_with_no_folder_and_a_search_has_nowhere_to_look() {
        for (name, args) in [
            ("edit_file", json!({ "path": "a.rs", "old_text": "x", "new_text": "y" })),
            ("create_file", json!({ "path": "a.rs", "content": "x" })),
            ("delete_file", json!({ "path": "a.rs" })),
            ("move_file", json!({ "path": "a.rs", "to": "b.rs" })),
        ] {
            assert_eq!(
                rejection(judge_call(call(name, args), None, None, false)),
                (
                    name.to_string(),
                    "a.rs".to_string(),
                    "error: editing is off: the user has not chosen a project folder".to_string()
                ),
                "{name}"
            );
        }
        for name in ["search", "list_files"] {
            assert_eq!(
                rejection(judge_call(call(name, json!({ "text": "x" })), None, None, false)),
                (
                    name.to_string(),
                    String::new(),
                    "error: there is no project folder to search — the user has not chosen one".to_string()
                ),
                "{name}"
            );
        }
    }

    #[test]
    fn a_check_with_no_command_chosen_is_turned_away() {
        let s = Sandbox::new("nocheck");
        let ws = s.ws();
        assert_eq!(
            rejection(judge_call(call("run_check", json!({})), Some(&ws), None, false)),
            ("run_check".to_string(), String::new(), format!("error: {}", tools::CHECK_UNAVAILABLE))
        );
        // The same with no folder at all.
        let (name, _, error) = rejection(judge_call(call("run_check", json!({})), None, None, false));
        assert_eq!(name, "run_check");
        assert_eq!(error, format!("error: {}", tools::CHECK_UNAVAILABLE));
    }

    #[test]
    fn a_search_that_could_never_run_goes_back_without_asking_anyone() {
        let s = Sandbox::new("badsearch");
        let ws = s.ws();
        let got = rejection(judge_call(call("search", json!({ "text": "a\nb" })), Some(&ws), None, false));
        assert_eq!(
            got,
            (
                "search".to_string(),
                String::new(),
                "error: search looks at one line at a time — send text without line breaks".to_string()
            )
        );
        let outside = call("list_files", json!({ "pattern": "../*.rs" }));
        let (name, path, error) = rejection(judge_call(outside, Some(&ws), None, false));
        assert_eq!((name.as_str(), path.as_str()), ("list_files", ""));
        assert!(error.starts_with("error: ../*.rs is not inside the project folder"), "{error}");
    }

    // ── the task list ───────────────────────────────────────────────────────

    #[test]
    fn the_task_list_runs_with_no_folder_even_when_everything_asks() {
        let plan = call("update_plan", json!({ "plan": "[x] read the file\n[ ] edit it" }));
        let (reply, armed) = judge_call(plan, None, None, true);
        assert!(armed.is_empty(), "the task list never arms a ticket");
        let Reply::Auto { name, path, range, result, ok, stopped, plan, raw_call } = reply else {
            panic!("not automatic: {}", kind(&reply))
        };
        assert_eq!((name.as_str(), path.as_str(), range, ok, stopped), ("update_plan", "", None, true, false));
        assert_eq!(result, "Plan updated: 1 of 2 done.");
        let steps = plan.expect("the parsed list");
        assert_eq!(steps.len(), 2);
        assert_eq!((steps[0].text.as_str(), steps[0].status), ("read the file", tools::PlanStatus::Done));
        assert_eq!(raw_call.function.name, "update_plan");
    }

    #[test]
    fn the_seventh_task_list_update_of_a_turn_goes_back_rejected() {
        let mut history = vec![Message::user("do it")];
        for n in 0..tools::MAX_PLAN_UPDATES {
            let plan = json!({ "plan": format!("[ ] step {n}") });
            history.extend(exchange("update_plan", plan, "Plan updated: 0 of 1 done."));
        }
        let plan = call("update_plan", json!({ "plan": "[ ] one more" }));
        let (reply, armed) = judge(&said("", vec![plan]), &history, None, None, false);
        assert_eq!(
            rejection((reply, armed)),
            ("update_plan".to_string(), String::new(), tools::PLAN_LIMIT_NOTE.to_string())
        );
    }

    // ── what runs without a card ────────────────────────────────────────────

    #[test]
    fn a_search_and_a_listing_inside_the_folder_run_at_once_and_arm_nothing() {
        let s = Sandbox::new("auto");
        s.write("project/a.txt", "a needle in a haystack\n");
        let ws = s.ws();

        let (reply, armed) = judge_call(call("search", json!({ "text": "needle" })), Some(&ws), None, false);
        assert!(armed.is_empty(), "an automatic call never touches the pending slot");
        let Reply::Auto { name, path, range, result, ok, stopped, plan, .. } = reply else {
            panic!("not automatic: {}", kind(&reply))
        };
        assert_eq!((name.as_str(), path.as_str(), range, ok, stopped, plan), ("search", "\"needle\"", None, true, false, None));
        assert!(result.contains("a.txt:1"), "{result}");

        let (reply, armed) = judge_call(call("list_files", json!({})), Some(&ws), None, false);
        assert!(armed.is_empty());
        let Reply::Auto { name, path, result, ok, .. } = reply else { panic!("not automatic: {}", kind(&reply)) };
        assert_eq!((name.as_str(), path.as_str(), ok), ("list_files", "the whole project", true));
        assert!(result.contains("a.txt"), "{result}");
    }

    #[test]
    fn a_failing_automatic_call_that_was_tried_before_carries_the_repeat_note() {
        let s = Sandbox::new("repeat");
        let ws = s.ws();
        // The folder is gone by the time the walk starts, so the search fails - and
        // the model has already seen this same call fail once.
        let args = json!({ "text": "needle" });
        let mut history = vec![Message::user("find it")];
        history.extend(exchange("search", args.clone(), "error: could not read the folder"));
        drop(s);
        let (reply, _) = judge(&said("", vec![call("search", args)]), &history, Some(&ws), None, false);
        let Reply::Auto { ok, result, .. } = reply else { panic!("not automatic: {}", kind(&reply)) };
        assert!(!ok, "{result}");
        assert!(result.ends_with(tools::repeated_note("search")), "{result}");
    }

    // ── what asks ───────────────────────────────────────────────────────────

    #[test]
    fn an_edit_asks_with_its_diff_and_is_armed_with_the_plan_the_diff_came_from() {
        let s = Sandbox::new("edit");
        s.write("project/a.txt", "one\ntwo\n");
        let ws = s.ws();
        let args = json!({ "path": "a.txt", "old_text": "two", "new_text": "2" });
        let (card, (name, path, payload)) = asking(judge_call(call("edit_file", args.clone()), Some(&ws), None, false));
        assert_eq!((card.name.as_str(), card.path.as_str(), card.token.as_str()), ("edit_file", "a.txt", "ticket-1"));
        assert_eq!((name.as_str(), path.as_str()), ("edit_file", "a.txt"), "armed for the call the card shows");
        let Some(Payload::Edit(plan)) = payload else { panic!("an edit is armed with its plan") };
        assert_eq!(plan_name(&plan), "edit_file");
        let diff = card.diff.expect("the diff to approve");
        assert_eq!((diff.added, diff.removed, diff.create), (1, 1, false));
        assert_eq!(diff.hunks, plan.hunks, "what is approved is what is written");
        assert!(card.to.is_none() && card.folders.is_empty() && card.size.is_none() && card.range.is_none());

        // The raw call goes with it, for the conversation.
        let (reply, _) = judge_call(call("edit_file", args.clone()), Some(&ws), None, false);
        let Reply::Call { raw_call, .. } = reply else { panic!("not a call") };
        assert_eq!(raw_call, call("edit_file", args));
    }

    #[test]
    fn a_move_a_create_and_a_delete_say_what_they_will_do() {
        let s = Sandbox::new("changes");
        s.write("project/a.txt", "one\n");
        s.write("project/gone.txt", "bye!\n");
        let ws = s.ws();

        let moved = call("move_file", json!({ "path": "a.txt", "to": "sub/b.txt" }));
        let (card, (_, _, payload)) = asking(judge_call(moved, Some(&ws), None, false));
        assert_eq!(card.name, "move_file");
        assert_eq!((card.to.as_deref(), card.size), (Some("sub/b.txt"), Some(4)));
        assert_eq!(card.folders, ["sub/"]);
        assert!(matches!(payload, Some(Payload::Edit(ref p)) if plan_name(p) == "move_file"));

        let made = call("create_file", json!({ "path": "new/n.txt", "content": "hi\n" }));
        let (card, _) = asking(judge_call(made, Some(&ws), None, false));
        assert_eq!((card.name.as_str(), card.size), ("create_file", None));
        assert_eq!(card.folders, ["new/"]);
        assert!(card.diff.is_some_and(|d| d.create));

        let deleted = call("delete_file", json!({ "path": "gone.txt" }));
        let (card, _) = asking(judge_call(deleted, Some(&ws), None, false));
        assert_eq!((card.name.as_str(), card.size, card.to), ("delete_file", Some(5), None));
    }

    #[test]
    fn an_edit_that_could_never_apply_is_rejected_with_the_plans_own_words() {
        let s = Sandbox::new("noedit");
        s.write("project/a.txt", "one\n");
        let ws = s.ws();
        let args = json!({ "path": "a.txt", "old_text": "not in the file", "new_text": "y" });
        let (name, path, error) = rejection(judge_call(call("edit_file", args), Some(&ws), None, false));
        assert_eq!((name.as_str(), path.as_str()), ("edit_file", "a.txt"));
        assert!(error.starts_with("error: "), "{error}");
    }

    #[test]
    fn a_read_outside_the_folder_asks_and_the_ticket_holds_the_file_and_the_lines() {
        let s = Sandbox::new("outside");
        s.write("outside.txt", "one\ntwo\nthree\n");
        let ws = s.ws();
        let args = json!({ "path": "../outside.txt", "start_line": 2, "end_line": 3 });
        let (card, (name, path, payload)) = asking(judge_call(call("read_file", args), Some(&ws), None, false));
        let real = std::fs::canonicalize(s.dir.join("outside.txt")).unwrap();
        let lines = Some(tools::Lines { start: 2, end: Some(3) });
        assert_eq!(card.name, "read_file");
        assert_eq!(card.path, edit::plain(&real), "the card shows the file that will really be opened");
        assert_eq!(card.range, lines);
        assert_eq!((name.as_str(), path.as_str()), ("read_file", card.path.as_str()));
        assert_eq!(payload, Some(Payload::Read { real, range: lines }));
    }

    #[test]
    fn a_check_asks_names_the_build_files_an_edit_changed_and_is_armed_with_its_command() {
        let s = Sandbox::new("check");
        let ws = s.ws();
        let argv = vec!["cargo".to_string(), "check".to_string()];
        let mut history = vec![Message::user("go")];
        let edited = "Edited Cargo.toml: 1 line removed, 1 added.";
        history.extend(exchange("edit_file", json!({ "path": "Cargo.toml" }), edited));
        let check = said("", vec![call("run_check", json!({}))]);
        let (card, (name, path, payload)) = asking(judge(&check, &history, Some(&ws), Some(&argv), false));
        assert_eq!((card.name.as_str(), card.path.as_str()), ("run_check", "cargo check"));
        assert_eq!(card.changed_build_files, ["Cargo.toml"]);
        assert_eq!((name.as_str(), path.as_str()), ("run_check", "cargo check"));
        let Some(Payload::Check { root, argv: armed_argv, .. }) = payload else {
            panic!("a check is armed with its command")
        };
        assert_eq!((root, armed_argv), (ws.root_display(), argv));
    }

    #[test]
    fn with_ask_every_call_a_search_asks_and_the_ticket_holds_what_the_card_showed() {
        let s = Sandbox::new("askall");
        let ws = s.ws();
        let args = json!({ "text": "needle", "pattern": "src/**/*.rs" });
        let (card, (name, path, payload)) = asking(judge_call(call("search", args), Some(&ws), None, true));
        assert_eq!((card.name.as_str(), card.path.as_str()), ("search", "\"needle\" in src/**/*.rs"));
        assert_eq!((name.as_str(), path.as_str()), ("search", card.path.as_str()));
        let Some(Payload::Find { root, find, .. }) = payload else {
            panic!("a search is armed with what it looks for")
        };
        assert_eq!(root, ws.root_display());
        assert_eq!(find, Find::Search { text: "needle".into(), pattern: Some("src/**/*.rs".into()) });
    }
}
