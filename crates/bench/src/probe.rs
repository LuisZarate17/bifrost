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
//! - the tool words and the system prompt are `odin_core::tools`' own
//!
//! What it leaves out is the person: every call is approved.

// Each probe is its own binary and uses only part of this — editprobe compares
// transports, fixprobe runs checks — so what one leaves unused is not dead.
#![allow(dead_code)]

use std::path::Path;

use odin_core::chat::{self, Message, Options, Tool, ToolCall};
use odin_core::edit::{self, Workspace};
use odin_core::{check, search};
use odin_core::tools::{self, Request};

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
}

/// One call, run.
pub struct Ran {
    pub text: String,
    /// A result the app would mark failed — it starts `error:`.
    pub failed: bool,
    /// For `run_check`: how the command ended.
    pub check: Option<check::Outcome>,
}

/// Run one call, approved. Mirrors what `run_tool` does in the app.
pub fn execute(call: &ToolCall, s: &Setup) -> Ran {
    let text = |t: String, failed: bool| Ran { text: t, failed, check: None };
    let edit_result = |r: Result<edit::Plan, String>| match r.and_then(|p| edit::apply(&p, s.backups)) {
        Ok(a) => text(tools::applied_message(&a), false),
        Err(e) => text(format!("error: {e}"), true),
    };
    let found = |r: Result<String, String>| match r {
        Ok(t) => text(t, false),
        Err(e) => text(format!("error: {e}"), true),
    };
    if call.function.name == chat::INVALID_TOOL_CALL {
        let why = call.function.arguments["error"].as_str().unwrap_or("unreadable");
        return text(format!("error: {why}. Send the call again as valid JSON."), true);
    }
    match tools::interpret(call) {
        Err(e) => text(format!("error: {e}"), true),
        Ok(Request::Read { path }) if s.confine_reads => match s.ws.resolve_existing(&path) {
            Ok(real) => text(tools::run_read_file(&real.to_string_lossy()), false),
            Err(e) => text(format!("error: {e}"), true),
        },
        Ok(Request::Read { path }) => text(tools::run_read_file(&tools::read_target(&path, Some(s.ws))), false),
        Ok(Request::Edit { path, old_text, new_text }) => {
            edit_result(edit::plan_replace(s.ws, &path, &old_text, &new_text))
        }
        Ok(Request::Create { path, content }) => edit_result(edit::plan_create(s.ws, &path, &content)),
        Ok(Request::List { pattern }) => found(search::list(s.ws, pattern.as_deref(), || false)),
        Ok(Request::Search { text: what, pattern }) => {
            found(search::search(s.ws, &what, pattern.as_deref(), || false))
        }
        Ok(Request::Rewrite { path, content }) => edit_result(edit::plan_rewrite(s.ws, &path, &content)),
        Ok(Request::Check) => {
            let Some(c) = s.check else {
                return text("error: run_check is not available — no check command is set for this folder".into(), true);
            };
            match check::run(s.ws.root(), &c.argv, check::timeout_for(&c.argv), &c.env, || false) {
                Ok(o) => Ran { text: check::for_model(&c.argv, &o), failed: false, check: Some(o) },
                Err(e) => text(format!("error: {e}"), true),
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
    /// Calls other than reads that came back as errors.
    pub edit_errors: usize,
    pub rounds: usize,
    /// Still calling tools when the bound ran out.
    pub hit_bound: bool,
    /// Ended by the app's loop-breaking: the same call failed three times.
    pub stopped: bool,
    /// Each call and what came back, for reading a failure rather than guessing.
    pub log: Vec<String>,
    pub recovered: usize,
    /// Each `run_check`, passed or not, in order.
    pub checks: Vec<bool>,
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
}

/// A request that failed this close to `chat`'s five-minute limit was cut off
/// by it, not refused.
const RUNAWAY: std::time::Duration = std::time::Duration::from_secs(290);

fn short(s: &str, n: usize) -> String {
    let one = s.replace('\n', "⏎");
    if one.chars().count() > n { format!("{}…", one.chars().take(n).collect::<String>()) } else { one }
}

/// Drive the conversation until the model answers, the bound runs out, the
/// loop-breaking stops it, or Ollama fails.
///
/// `bound` is the probe's own. The app stops at `tools::MAX_ROUNDS`; a probe can
/// run past it to record how many rounds a task really takes (H7 sets the app's
/// bound from that).
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

    for _ in 0..bound {
        run.rounds += 1;
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
        // The app's loop-breaking, measured rather than assumed: the third
        // identical failure ends the turn instead of burning every round.
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
        let ran = execute(&call, s);
        if let Some(o) = &ran.check {
            run.checks.push(o.passed());
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
        if ran.failed && call.function.name != "read_file" {
            run.edit_errors += 1;
        }
        messages.push(Message::tool_result(call.function.name.clone(), result));
    }
    run
}
