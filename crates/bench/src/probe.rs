//! The tool loop every probe runs: the app's loop, with the person taken out.
//!
//! Included by each probe as `#[path = "../probe.rs"] mod probe;` rather than
//! through a library target. A lib in this crate would be named `bench`, and on
//! Windows a lib and a bin cannot share a name (rust-lang/cargo#8519) — the same
//! reason Mímir's own lib is `mimir_lib`.
//!
//! **What it copies from the app, and must keep copying**, because a probe that
//! drives a different loop measures a different product:
//!
//! - only the first call in a reply runs
//! - the asking turn goes back before its result, or the model asks again
//! - three identical failures end the turn, and a repeat carries the app's note
//! - the tool words and the system prompt are `bifrost_core::tools`' own
//!
//! What it leaves out is the person: every call is approved. It does count the
//! calls the app would have stopped to ask about (`Run::asks`).

// Each probe is its own binary and uses only part of this — editprobe compares
// transports, fixprobe runs checks — so what one leaves unused is not dead.
#![allow(dead_code)]

use std::path::Path;

use bifrost_core::chat::{self, Message, Options, Tool, ToolCall};
use bifrost_core::edit::{self, Workspace};
use bifrost_core::{check, compact, search};
use bifrost_core::tools::{self, Request};

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
    /// the repo holding the reference solution — a read must not reach it.
    pub confine_reads: bool,
    /// Set earlier results aside before each request, as the app does from H7
    /// (`bifrost_core::compact`). Off measures the loop as it was: the whole
    /// conversation sent every round, and Ollama's cut when it overflows.
    pub compact: bool,
}

/// One call, run.
pub struct Ran {
    pub text: String,
    /// A result the app would mark failed — it starts `error:`.
    pub failed: bool,
    /// For `run_check`: how the command ended.
    pub check: Option<check::Outcome>,
    /// The app, under its default policy, would have stopped on a card: the
    /// same `tools::approval` it asks, with `ask_every_call` off. A call it
    /// rejects before any card (an edit whose `old_text` is not in the file, a
    /// read of a folder), a malformed call and a `run_check` with no check set
    /// never ask.
    pub asked: bool,
    /// The same, under the policy before H5, where every call that reached a
    /// card asked. The difference is what H5 saved.
    pub asked_every_call: bool,
    /// The undo id of a change this call made — an edit, a create, a delete or a
    /// move — so a probe can undo the whole run afterwards, as Undo all does.
    pub undo: Option<String>,
}

/// Run one call, approved. Mirrors what `run_tool` does in the app.
pub fn execute(call: &ToolCall, s: &Setup) -> Ran {
    let text = |t: String, failed: bool| Ran {
        text: t,
        failed,
        check: None,
        asked: false,
        asked_every_call: false,
        undo: None,
    };
    // An edit shows its card only once its plan succeeded; what the apply does
    // after that is the person's to judge, and the card was already shown.
    let edit_result = |r: Result<edit::Plan, String>| {
        let carded = r.is_ok();
        let mut undo = None;
        let mut ran = match r.and_then(|p| edit::apply(&p, s.backups)) {
            Ok(a) => {
                undo = Some(a.id.clone());
                text(tools::applied_message(&a), false)
            }
            Err(e) => text(format!("error: {e}"), true),
        };
        ran.undo = undo;
        ran.asked = carded;
        ran.asked_every_call = carded;
        ran
    };
    // A list or a search asks only once it has passed the app's `validate`,
    // and then only under the old policy.
    let found = |r: Result<String, String>, valid: bool| {
        let mut ran = match r {
            Ok(t) => text(t, false),
            Err(e) => text(format!("error: {e}"), true),
        };
        ran.asked_every_call = valid;
        ran
    };
    if call.function.name == chat::INVALID_TOOL_CALL {
        let why = call.function.arguments["error"].as_str().unwrap_or("unreadable");
        return text(tools::failure(&format!("{why}. Send the call again as valid JSON.")), true);
    }
    // `interpret`'s refusals already start `error:`, as the app sends them.
    let request = match tools::interpret(call) {
        Err(e) => return text(e, true),
        Ok(r) => r,
    };
    // The app's own rule. `Reject` never reaches a card, and `Auto` never asks.
    let decided = tools::approval(Some(s.ws), &request, false);
    let asked = matches!(decided, tools::Approval::Ask);
    let read = |t: String, failed: bool| {
        let mut ran = text(t, failed);
        ran.asked = asked;
        ran.asked_every_call = true;
        ran
    };
    // What the app answers for a read that could never work — a folder, a file that
    // is not there — in its words, not whatever the read itself would say (R-36).
    if let (Request::Read { .. }, tools::Approval::Reject(why)) = (&request, &decided) {
        return read(tools::failure(why), true);
    }
    match request {
        // A read that fails is marked so, as the app marks it, and gets the app's
        // repeat note; `run` still does not count it among the edit errors.
        Request::Read { path, range } if s.confine_reads => match s.ws.resolve_existing(&path) {
            Ok(real) => {
                let t = tools::run_read_file(&real, range);
                let failed = t.starts_with("error:");
                read(t, failed)
            }
            Err(e) => read(format!("error: {e}"), true),
        },
        Request::Read { path, range } => {
            let t = tools::run_read_file(&tools::read_target(&path, Some(s.ws)).0, range);
            let failed = t.starts_with("error:");
            read(t, failed)
        }
        Request::Edit { path, old_text, new_text } => {
            edit_result(tools::plan_edit(s.ws, &path, &old_text, &new_text, false))
        }
        Request::Create { path, content } => edit_result(edit::plan_create(s.ws, &path, &content)),
        Request::Delete { path } => edit_result(edit::plan_delete(s.ws, &path)),
        Request::Move { path, to } => edit_result(edit::plan_move(s.ws, &path, &to)),
        Request::List { pattern } => {
            let valid = search::validate(s.ws, None, pattern.as_deref()).is_ok();
            let mut ran = found(search::list(s.ws, pattern.as_deref(), || false), valid);
            ran.asked = asked && valid;
            ran
        }
        Request::Search { text: what, pattern } => {
            let valid = search::validate(s.ws, Some(&what), pattern.as_deref()).is_ok();
            let mut ran = found(search::search(s.ws, &what, pattern.as_deref(), || false), valid);
            ran.asked = asked && valid;
            ran
        }
        // `write_file` exists only in editprobe's variant, never in the app; were it
        // offered, it would show a card the way an edit does. No probe that prints
        // asks offers it.
        Request::Rewrite { path, content } => edit_result(edit::plan_rewrite(s.ws, &path, &content)),
        // The task list touches nothing and shows no card; the result is the app's line.
        Request::Plan { steps } => text(tools::plan_message(&steps), false),
        Request::Check => {
            let Some(c) = s.check else {
                return text(tools::failure(tools::CHECK_UNAVAILABLE), true);
            };
            match check::run(s.ws.root(), &c.argv, check::timeout_for(&c.argv), &c.env, || false) {
                Ok(o) => Ran {
                    text: check::for_model(&c.argv, &o),
                    failed: false,
                    check: Some(o),
                    asked: true,
                    asked_every_call: true,
                    undo: None,
                },
                Err(e) => {
                    let mut ran = text(format!("error: {e}"), true);
                    ran.asked = true;
                    ran.asked_every_call = true;
                    ran
                }
            }
        }
    }
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
    /// Ollama failed. The run measured nothing — unless `runaway` is set.
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
const RUNAWAY: std::time::Duration = std::time::Duration::from_secs(290);

fn short(s: &str, n: usize) -> String {
    let one = s.replace('\n', "⏎");
    if one.chars().count() > n { format!("{}…", one.chars().take(n).collect::<String>()) } else { one }
}

/// Whether a round that ran this call is free of the round bound, as in the app
/// (R-03): an `update_plan` the loop accepted, or a read, listing or search that
/// the app runs without a card (`asked` is false) and that succeeded. A malformed
/// plan, the 7th, a failed read, a rejected call and anything that would have asked
/// costs a round like any call, so a model that cannot get a call right is still
/// bounded. A search with no matches is a success.
pub fn free_round(name: &str, failed: bool, asked: bool) -> bool {
    !failed && (name == "update_plan" || (!asked && matches!(name, "read_file" | "list_files" | "search")))
}

/// Drive the conversation until the model answers, the bound runs out, the
/// loop-breaking stops it, or Ollama fails.
///
/// `bound` is the probe's own. The app stops at `tools::MAX_ROUNDS`; a probe can
/// run past it to record how many rounds a task really takes (H7 sets the app's
/// bound from that).
///
/// **A free call costs no round** (H10, R-03): an accepted `update_plan` and an
/// automatic read, listing or search that succeeded are not counted against
/// `bound` or in `Run::rounds` (see [`free_round`]), and `tools::plan_updates` — the
/// app's own rule — rejects the 7th plan update in a turn, which does count. So the
/// loop runs at most `bound + MAX_FREE_CALLS` requests.
///
/// **A turn that hits the bound gets one closing request with no tools** and
/// `CLOSING_NOTE` after the conversation, as the app's does, so the model ends in
/// its own words (`Run::answer`). `hit_bound` stays set. If that request fails it is
/// only logged: the task's work is judged elsewhere.
pub fn run_loop(
    model: &str,
    messages: &mut Vec<Message>,
    offered: &[Tool],
    opts: Options,
    transport: Transport,
    bound: usize,
    s: &Setup,
) -> Run {
    let mut run = Run { hit_bound: true, ..Run::default() };
    // The previous round's characters sent and tokens counted, which is how the
    // budget learns this model's characters per token, as the app's does.
    let mut calibration: Option<(u64, u32)> = None;
    let mut deleted: Vec<String> = Vec::new();

    let mut requests = 0;
    while run.rounds < bound && requests < bound + tools::MAX_FREE_CALLS {
        requests += 1;
        run.rounds += 1;
        let mut aside_now = 0;
        if s.compact {
            let plan = compact::compact(messages, offered, &compact::Budget::new(opts.num_ctx, calibration));
            for &i in &plan.set_aside {
                let call = i.checked_sub(1).and_then(|p| messages[p].tool_calls.as_deref()?.first().cloned());
                compact::set_aside(&mut messages[i], call.as_ref());
            }
            aside_now = plan.set_aside.len();
            run.set_aside += aside_now;
        }
        let sent_chars = compact::prompt_chars(messages, offered);
        let asked = std::time::Instant::now();
        let sent = match transport {
            Transport::Native => chat::chat(model, messages, offered, opts),
            Transport::Recover => chat::chat_tools(model, messages, offered, opts),
        };
        let reply = match sent {
            Ok(r) => r,
            Err(e) => {
                run.runaway = asked.elapsed() >= RUNAWAY;
                run.error = Some(e);
                run.hit_bound = false;
                return run;
            }
        };
        // A count of 0 is a reply that did not report one — not a prompt of no
        // tokens — so it neither counts as a cut nor calibrates anything.
        if reply.prompt_eval_count > 0 {
            if let Some(&before) = run.prompt_tokens.last()
                && reply.prompt_eval_count < before
                && aside_now == 0
            {
                run.truncated += 1;
            }
            run.prompt_tokens.push(reply.prompt_eval_count);
            calibration = Some((sent_chars, reply.prompt_eval_count));
        }
        if reply.recovered {
            run.recovered += 1;
            run.log.push("(ollama rejected a malformed call; recovered through the raw retry)".into());
        }
        let Some(call) = reply.tool_calls().first().cloned() else {
            run.log.push(format!("answer: {}", short(&reply.message.content, 300)));
            run.answer = reply.message.content.clone();
            run.hit_bound = false;
            break;
        };
        run.calls.push(call.function.name.clone());
        if call.function.name == "read_file" && tools::read_range(&call.function.arguments).ok().flatten().is_some() {
            run.ranged_reads += 1;
        }
        // The app's loop-breaking, measured rather than assumed: the third
        // identical failure ends the turn instead of burning every round.
        let over_plan = call.function.name == "update_plan"
            && tools::plan_updates(messages) >= tools::MAX_PLAN_UPDATES;
        let prior = tools::prior_failures(messages, &call);
        if prior + 1 >= tools::MAX_IDENTICAL_FAILURES {
            run.log.push(format!("stopped: {} sent identically after failing {prior} times", call.function.name));
            run.stopped = true;
            run.hit_bound = false;
            break;
        }
        // Exactly what the app sends back: the asking turn with only the call
        // that ran, then its result.
        messages.push(Message {
            role: "assistant".into(),
            content: String::new(),
            images: None,
            tool_calls: Some(vec![call.clone()]),
            tool_name: None,
        });
        let ran = if over_plan {
            Ran {
                text: tools::PLAN_LIMIT_NOTE.to_string(),
                failed: true,
                check: None,
                asked: false,
                asked_every_call: false,
                undo: None,
            }
        } else {
            execute(&call, s)
        };
        if free_round(&call.function.name, ran.failed, ran.asked) {
            if call.function.name == "update_plan" {
                run.plan_updates += 1;
            } else {
                run.auto_calls += 1;
            }
            run.rounds -= 1;
        }
        run.undo_ids.extend(ran.undo.clone());
        // Paths as the model wrote them, tidied — one file is one entry however it
        // was spelled.
        let at = || {
            call.function.arguments["path"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .trim_matches(['"', '\'', '`'])
                .trim()
                .replace('\\', "/")
                .trim_start_matches("./")
                .to_lowercase()
        };
        match call.function.name.as_str() {
            "delete_file" => {
                run.delete_calls += 1;
                if !ran.failed {
                    deleted.push(at());
                }
            }
            "move_file" => run.move_calls += 1,
            "create_file" if !ran.failed && deleted.contains(&at()) => run.delete_then_create += 1,
            _ => {}
        }
        run.asks += ran.asked as usize;
        run.asks_every_call += ran.asked_every_call as usize;
        if let Some(o) = &ran.check {
            run.checks.push(o.passed());
            if never_finished(&o.ended) && o.blocked.is_none() {
                run.check_timeouts += 1;
            }
            if o.blocked.is_some() {
                run.blocked = true;
            }
        }
        let result = if ran.failed && prior > 0 {
            format!("{}{}", ran.text, tools::repeated_note(&call.function.name))
        } else {
            ran.text
        };
        run.log.push(format!(
            "{}({}) → {}",
            call.function.name,
            short(&call.function.arguments.to_string(), 140),
            short(&result, 140)
        ));
        if ran.failed && !matches!(call.function.name.as_str(), "read_file" | "update_plan") {
            run.edit_errors += 1;
        }
        messages.push(Message::tool_result(call.function.name.clone(), result));
    }
    if run.hit_bound && run.error.is_none() && !run.stopped {
        // The closing request goes out on a copy, so `messages` stays as the loop
        // left it. Compaction runs first and the note is added after it, as in the app.
        let mut closing = messages.clone();
        if s.compact {
            let plan = compact::compact(&closing, &[], &compact::Budget::new(opts.num_ctx, calibration));
            for &i in &plan.set_aside {
                let call = i.checked_sub(1).and_then(|p| closing[p].tool_calls.as_deref()?.first().cloned());
                compact::set_aside(&mut closing[i], call.as_ref());
            }
        }
        closing.push(Message::user(tools::CLOSING_NOTE));
        match chat::chat(model, &closing, &[], opts) {
            Ok(r) => {
                run.log.push(format!("closing answer: {}", short(&r.message.content, 300)));
                run.answer = r.message.content;
            }
            Err(e) => run.log.push(format!("closing request failed: {e}")),
        }
    }
    run
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall { id: None, function: chat::FunctionCall { name: name.into(), arguments: args } }
    }

    /// **A task list is free, and shows no card.** The result is the app's line, it
    /// is never an ask, and only an accepted update is free of the round bound.
    #[test]
    fn a_plan_update_runs_unasked_and_costs_no_round_but_a_bad_one_does() {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("odin-probe-plan-{}-{n}", std::process::id()));
        let tree = base.join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        let ws = Workspace::open(&tree).unwrap();
        let backups = base.join("backups");
        let setup = Setup { ws: &ws, backups: &backups, check: None, confine_reads: true, compact: true };

        let ran = execute(&call("update_plan", serde_json::json!({ "plan": "[x] a\n[ ] b" })), &setup);
        assert_eq!((ran.text.as_str(), ran.failed, ran.asked, ran.asked_every_call), ("Plan updated: 1 of 2 done.", false, false, false));
        assert!(free_round("update_plan", ran.failed, ran.asked));
        let bad = execute(&call("update_plan", serde_json::json!({})), &setup);
        assert!(bad.failed && !free_round("update_plan", bad.failed, bad.asked));
        // The cap is the app's: the 7th in a turn is the first the shared count rejects.
        let plan = call("update_plan", serde_json::json!({ "plan": "[ ] a" }));
        let mut history = vec![Message::user("go")];
        for _ in 0..tools::MAX_PLAN_UPDATES {
            assert!(tools::plan_updates(&history) < tools::MAX_PLAN_UPDATES);
            history.push(Message {
                role: "assistant".into(),
                content: String::new(),
                images: None,
                tool_calls: Some(vec![plan.clone()]),
                tool_name: None,
            });
        }
        assert!(tools::plan_updates(&history) >= tools::MAX_PLAN_UPDATES);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// **Which calls cost no round (R-03).** An automatic read that worked is free;
    /// the same read when the app would have asked is not; a failed one is not; an
    /// accepted plan update is free and a rejected one is not; an edit never is.
    #[test]
    fn only_a_successful_unasked_read_list_or_search_and_an_accepted_plan_are_free() {
        assert!(free_round("read_file", false, false));
        assert!(free_round("list_files", false, false));
        assert!(free_round("search", false, false));
        assert!(!free_round("read_file", false, true), "a read that asked");
        assert!(!free_round("read_file", true, false), "a read that failed");
        assert!(free_round("update_plan", false, false));
        assert!(!free_round("update_plan", true, false), "a rejected plan");
        assert!(!free_round("edit_file", false, false));
        assert!(!free_round("run_check", false, true));
    }

    /// A failed read is told to change the range, not to copy text — and is not
    /// an edit error, since a probe counts those to judge the edit tools.
    #[test]
    fn a_failed_read_gets_the_read_repeat_note_and_is_not_an_edit_error() {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("odin-probe-read-{}-{n}", std::process::id()));
        let tree = base.join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::write(tree.join("a.txt"), "one\ntwo\n").unwrap();
        let ws = Workspace::open(&tree).unwrap();
        let backups = base.join("backups");
        let setup = Setup { ws: &ws, backups: &backups, check: None, confine_reads: true, compact: true };

        let past = call("read_file", serde_json::json!({ "path": "a.txt", "start_line": 9 }));
        let ran = execute(&past, &setup);
        assert!(ran.failed && ran.text.contains("start_line 9 is past the end"), "{}", ran.text);
        let ok = execute(&call("read_file", serde_json::json!({ "path": "a.txt", "start_line": "2" })), &setup);
        assert!(!ok.failed);
        assert_eq!(ok.text, "[lines 2–2 of 2]\ntwo\n");
        // The app appends this to a failed read it has already seen fail.
        let note = tools::repeated_note("read_file");
        assert!(note.contains("a different range") && !note.contains("copy the text"));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// **What counts as an ask is the app's own rule.** An in-folder read, list
    /// and search run without a card; a read outside asks (even though a
    /// confined probe refuses it); an edit asks only if its plan succeeded; an
    /// invalid call never asks. Under the old policy all of those that reach a
    /// card ask.
    #[test]
    fn asks_follow_the_apps_rule() {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("odin-probe-asks-{}-{n}", std::process::id()));
        let tree = base.join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::write(tree.join("a.txt"), "hello
").unwrap();
        std::fs::write(base.join("out.txt"), "outside
").unwrap();
        let ws = Workspace::open(&tree).unwrap();
        let backups = base.join("backups");
        let setup = Setup { ws: &ws, backups: &backups, check: None, confine_reads: true, compact: true };
        let asks = |c: ToolCall| {
            let r = execute(&c, &setup);
            (r.asked, r.asked_every_call)
        };
        let outside = base.join("out.txt").to_string_lossy().into_owned();

        assert_eq!(asks(call("read_file", serde_json::json!({ "path": "a.txt" }))), (false, true));
        assert_eq!(asks(call("read_file", serde_json::json!({ "path": outside }))), (true, true));
        assert_eq!(asks(call("list_files", serde_json::json!({}))), (false, true));
        assert_eq!(asks(call("search", serde_json::json!({ "text": "hello" }))), (false, true));
        assert_eq!(
            asks(call("edit_file", serde_json::json!({ "path": "a.txt", "old_text": "nope", "new_text": "x" }))),
            (false, false)
        );
        assert_eq!(
            asks(call("edit_file", serde_json::json!({ "path": "a.txt", "old_text": "hello", "new_text": "bye" }))),
            (true, true)
        );
        assert_eq!(asks(call("run_check", serde_json::json!({}))), (false, false));
        assert_eq!(asks(call(chat::INVALID_TOOL_CALL, serde_json::json!({ "error": "bad" }))), (false, false));
        let _ = std::fs::remove_dir_all(&base);
    }
}
