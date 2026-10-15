//! Whole sessions, through the line mode, over in-memory input and output: the scripted
//! model's rounds and the scripted runner stand where Ollama and the tools do, so what
//! is checked is Hugin's own — what it prints, asks, saves and sends back.
//!
//! **Input arrives when it is asked for.** The test's output [`Tap`] answers each `› `
//! with the next scripted prompt and each `approve?` with the next scripted answer, so
//! a line is never read by the wrong question and nothing depends on timing. (Text
//! that was all there from the start could be taken by an approval before the turn
//! that should have asked for it.)

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{self, BufRead, Read, Write};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bifrost_core::agent::round::{Reply, Round};
use bifrost_core::agent::run::Ran;
use bifrost_core::agent::send::{Calibration, Sent, compact_for_send};
use bifrost_core::agent::ticket::{DiffView, PendingCall};
use bifrost_core::chat::{self, Message};
use bifrost_core::edit::{DiffLine, Hunk};
use bifrost_core::store;
use bifrost_core::tools::{PlanStatus, PlanStep};
use serde_json::json;

use crate::line::{self, Line};
use crate::session::Session;
use crate::ui::{Approval, Event, Ui};

/// Input that comes from a channel, one scripted chunk at a time; it ends when every
/// sender is gone.
struct Feed {
    rx: Receiver<String>,
    buf: Vec<u8>,
    at: usize,
}

impl Read for Feed {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = {
            let have = self.fill_buf()?;
            let n = have.len().min(out.len());
            out[..n].copy_from_slice(&have[..n]);
            n
        };
        self.consume(n);
        Ok(n)
    }
}

impl BufRead for Feed {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.at >= self.buf.len() {
            self.buf = self.rx.recv().map(String::into_bytes).unwrap_or_default();
            self.at = 0;
        }
        Ok(&self.buf[self.at..])
    }

    fn consume(&mut self, n: usize) {
        self.at += n;
    }
}

/// Output that is written down, and that sends the next scripted line whenever it is
/// asked for one.
struct Tap {
    text: Arc<Mutex<String>>,
    prompts: VecDeque<String>,
    answers: VecDeque<String>,
    tx: Sender<String>,
}

impl Write for Tap {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut text = self.text.lock().unwrap();
        text.push_str(&String::from_utf8_lossy(bytes));
        if text.ends_with("› ") {
            // Past the script, leave rather than wait for a line that never comes.
            let next = self.prompts.pop_front().unwrap_or_else(|| "/exit".into());
            let _ = self.tx.send(format!("{next}\n"));
        } else if text.ends_with("approve? [y/n/a] ") {
            let next = self.answers.pop_front().expect("an approval asked and no answer was scripted");
            let _ = self.tx.send(format!("{next}\n"));
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn tap(prompts: &[&str], answers: &[&str]) -> (Tap, Feed, Sender<String>, Arc<Mutex<String>>) {
    let (tx, rx) = mpsc::channel();
    let text = Arc::new(Mutex::new(String::new()));
    let out = Tap {
        text: text.clone(),
        prompts: prompts.iter().map(|s| s.to_string()).collect(),
        answers: answers.iter().map(|s| s.to_string()).collect(),
        tx: tx.clone(),
    };
    (out, Feed { rx, buf: Vec::new(), at: 0 }, tx, text)
}

// ── what the model says ─────────────────────────────────────────────────────

pub(crate) fn sent() -> Sent {
    compact_for_send(&mut [], &[], 8192, None, 0)
}

pub(crate) fn round_with(reply: Reply, prompt_eval_count: u32) -> Result<Round, String> {
    Ok(Round { reply, prompt_eval_count, recovered: false, sent: sent() })
}

pub(crate) fn round_of(reply: Reply) -> Result<Round, String> {
    round_with(reply, 0)
}

pub(crate) fn raw(name: &str, arguments: serde_json::Value) -> chat::ToolCall {
    chat::ToolCall { id: None, function: chat::FunctionCall { name: name.into(), arguments } }
}

pub(crate) fn answer(text: &str) -> Result<Round, String> {
    round_of(Reply::Answer { content: text.into(), eval_count: 7 })
}

pub(crate) fn auto_search(result: &str) -> Result<Round, String> {
    round_of(Reply::Auto {
        name: "search".into(),
        path: "\"x\"".into(),
        range: None,
        result: result.into(),
        ok: true,
        stopped: false,
        plan: None,
        raw_call: raw("search", json!({ "text": "x" })),
    })
}

pub(crate) fn plan_update(steps: &[(&str, PlanStatus)]) -> Result<Round, String> {
    let plan = steps.iter().map(|(t, s)| PlanStep { text: t.to_string(), status: *s }).collect();
    round_of(Reply::Auto {
        name: "update_plan".into(),
        path: String::new(),
        range: None,
        result: "Plan updated.".into(),
        ok: true,
        stopped: false,
        plan: Some(plan),
        raw_call: raw("update_plan", json!({})),
    })
}

pub(crate) fn dl(tag: &str, text: &str) -> DiffLine {
    DiffLine { tag: tag.into(), text: text.into(), old: None, new: None }
}

/// An edit of `a.rs` that replaces `old` with `new`, waiting for a decision.
pub(crate) fn edit() -> Result<Round, String> {
    let hunk = Hunk { lines: vec![dl(" ", "fn a() {"), dl("-", "    old();"), dl("+", "    new();"), dl(" ", "}")] };
    round_of(Reply::Call {
        call: PendingCall {
            token: "t-a.rs".into(),
            name: "edit_file".into(),
            path: "a.rs".into(),
            diff: Some(DiffView { hunks: vec![hunk], added: 1, removed: 1, create: false }),
            range: None,
            changed_build_files: Vec::new(),
            to: None,
            folders: Vec::new(),
            size: None,
        },
        raw_call: raw("edit_file", json!({ "path": "a.rs" })),
    })
}

pub(crate) const EDITED: &str = "Edited a.rs: 1 line removed, 1 added.";

pub(crate) fn edited() -> Result<Ran, String> {
    Ok(Ran::text(EDITED.into()))
}

// ── the harness ─────────────────────────────────────────────────────────────

/// What the model was asked, per request: whether it was the closing request, how many
/// messages it was sent, and the calibration it was given.
type Asked = (bool, usize, Option<Calibration>);

/// A hook called as each request is made.
type Hook = dyn FnMut(usize, &Sender<String>);

struct Script {
    prompts: Vec<&'static str>,
    answers: Vec<&'static str>,
    asks: Vec<Result<Round, String>>,
    runs: Vec<Result<Ran, String>>,
    /// Called as each request is made, with its number from 0 — a line typed while the
    /// model is thinking.
    during: Box<Hook>,
    notices: Vec<&'static str>,
}

impl Script {
    fn new(prompts: &[&'static str]) -> Self {
        Self {
            prompts: prompts.to_vec(),
            answers: Vec::new(),
            asks: Vec::new(),
            runs: Vec::new(),
            during: Box::new(|_, _| {}),
            notices: Vec::new(),
        }
    }

    fn answering(mut self, answers: &[&'static str]) -> Self {
        self.answers = answers.to_vec();
        self
    }

    fn model(mut self, asks: Vec<Result<Round, String>>) -> Self {
        self.asks = asks;
        self
    }

    fn running(mut self, runs: Vec<Result<Ran, String>>) -> Self {
        self.runs = runs;
        self
    }
}

struct Played {
    out: String,
    saved: Vec<store::Conversation>,
    asked: Vec<Asked>,
    ran: Vec<String>,
    convo: Vec<Message>,
}

pub(crate) fn header() -> Event {
    Event::Header { folder: "C:\\work\\demo".into(), branch: Some("main".into()), changed: Some(2), model: "m".into() }
}

/// Play a session to its end.
fn play(script: Script) -> Played {
    let Script { prompts, answers, asks, runs, mut during, notices } = script;
    let (out, input, tx, text) = tap(&prompts, &answers);

    let (asks, runs) = (RefCell::new(VecDeque::from(asks)), RefCell::new(VecDeque::from(runs)));
    let asked = Rc::new(RefCell::new(Vec::<Asked>::new()));
    let ran = Rc::new(RefCell::new(Vec::<String>::new()));
    let saved = Rc::new(RefCell::new(Vec::<store::Conversation>::new()));
    let n = std::cell::Cell::new(0);

    let mut session = Session::new(
        "m".into(),
        Box::new({
            let asked = asked.clone();
            move |messages: &[Message], calibration: Option<Calibration>, closing: bool| {
                asked.borrow_mut().push((closing, messages.len(), calibration));
                during(n.get(), &tx);
                n.set(n.get() + 1);
                asks.borrow_mut().pop_front().expect("the loop asked the model more often than the script allows")
            }
        }),
        Box::new({
            let ran = ran.clone();
            move |name: &str, path: &str, token: &str| {
                ran.borrow_mut().push(format!("{name}:{path}:{token}"));
                runs.borrow_mut().pop_front().expect("the loop ran a call the script did not expect")
            }
        }),
        Box::new({
            let saved = saved.clone();
            move |c: &store::Conversation| {
                saved.borrow_mut().push(c.clone());
                Ok(())
            }
        }),
    );

    let (ntx, nrx) = mpsc::channel();
    for notice in notices {
        ntx.send(notice.to_string()).unwrap();
    }
    line::run(input, out, &mut session, header(), nrx, true);

    let out = text.lock().unwrap().clone();
    let (asked, ran, saved) = (asked.borrow().clone(), ran.borrow().clone(), saved.borrow().clone());
    Played { out, saved, asked, ran, convo: session.conversation().to_vec() }
}

pub(crate) fn assistant(call: chat::ToolCall) -> Message {
    Message { role: "assistant".into(), content: String::new(), images: None, tool_calls: Some(vec![call]), tool_name: None }
}

// ── the tests ───────────────────────────────────────────────────────────────

#[test]
fn an_answer_is_echoed_and_shown_after_the_header() {
    let p = play(Script::new(&["hi"]).model(vec![answer("hello")]));
    assert_eq!(p.out, "hugin · demo · main, 2 changed · m\n› hi\nhello\n› ");
    assert_eq!(p.asked, [(false, 1, None)]);
    // The answer is part of the conversation, so the next turn can see it.
    assert_eq!(p.convo.len(), 2);
    assert_eq!((p.convo[1].role.as_str(), p.convo[1].content.as_str()), ("assistant", "hello"));
}

#[test]
fn an_automatic_search_is_one_line_with_its_first_line_of_result() {
    let p = play(Script::new(&["find x"]).model(vec![auto_search("6 matches for \"x\" in 2 files:\nsrc/a.rs:3: x"), answer("Found it.")]));
    assert!(p.out.contains("● search \"x\" → 6 hits\n"), "{}", p.out);
    assert!(p.out.contains("Found it.\n"));
    assert!(p.ran.is_empty(), "an automatic call is not run again");
}

#[test]
fn the_task_list_is_shown_as_checkboxes() {
    let steps = [("read it", PlanStatus::Done), ("change it", PlanStatus::Doing), ("check it", PlanStatus::Pending)];
    let p = play(Script::new(&["go"]).model(vec![plan_update(&steps), answer("ok")]));
    assert!(p.out.contains("[x] read it\n[ ] change it\n[ ] check it\n"), "{}", p.out);
    assert!(!p.out.contains("update_plan"));
}

#[test]
fn an_edit_approved_with_y_shows_its_diff_then_what_was_applied() {
    let p = play(Script::new(&["fix it"]).answering(&["y"]).model(vec![edit(), answer("Done.")]).running(vec![edited()]));
    let want = "› fix it\n\
                ? edit a.rs +1 −1\n\
                \x20\x20 fn a() {\n\
                \x20\x20-    old();\n\
                \x20\x20+    new();\n\
                \x20\x20 }\n\
                approve? [y/n/a] y\n\
                ● edit a.rs +1 −1 → applied\n\
                Done.\n";
    assert!(p.out.contains(want), "{}", p.out);
    assert_eq!(p.ran, ["edit_file:a.rs:t-a.rs"]);
    assert_eq!(p.convo[2], Message::tool_result("edit_file", EDITED));
}

#[test]
fn an_edit_declined_with_n_is_declined_and_the_model_is_told() {
    let p = play(Script::new(&["fix it"]).answering(&["n"]).model(vec![edit(), answer("Understood.")]));
    assert!(p.out.contains("approve? [y/n/a] n\n● edit a.rs +1 −1 → declined\n"), "{}", p.out);
    assert!(p.ran.is_empty());
    assert_eq!(p.convo[1], assistant(raw("edit_file", json!({ "path": "a.rs" }))));
    assert_eq!(
        p.convo[2],
        Message::tool_result("edit_file", "The user declined this change to a.rs. Nothing was written.")
    );
}

#[test]
fn a_runs_the_call_and_anything_else_asks_again() {
    let p = play(Script::new(&["fix it"]).answering(&["maybe", "a"]).model(vec![edit(), answer("Done.")]).running(vec![edited()]));
    assert_eq!(p.out.matches("approve? [y/n/a] ").count(), 2);
    assert!(p.out.contains("→ applied"));
    assert_eq!(p.ran.len(), 1);
}

#[test]
fn a_allows_for_the_session_when_asked_directly() {
    let (out, input, _tx, _text) = tap(&[], &["yes", "a", "no"]);
    let stopper = crate::stop::Stopper::default();
    let mut ui = Line::new(out, line::feed(input, stopper.clone()), stopper, true);
    let ask = Approval { name: "run_check".into(), target: "cargo check".into(), detail: vec!["cargo check".into()] };
    assert_eq!(ui.approve(&ask), bifrost_core::agent::turn::Decision::Allow);
    assert_eq!(ui.approve(&ask), bifrost_core::agent::turn::Decision::AllowForSession);
    assert_eq!(ui.approve(&ask), bifrost_core::agent::turn::Decision::Deny);
}

#[test]
fn stop_at_an_approval_denies_it_and_ends_the_turn() {
    // Nothing more is scripted for the model: a request after the stop would panic.
    let p = play(Script::new(&["fix it"]).answering(&["/stop"]).model(vec![edit()]));
    // The reading thread takes `/stop` the moment it arrives, so the question is
    // answered by the flag, not by a line.
    assert!(p.out.contains("approve? [y/n/a] \n● edit a.rs +1 −1 → declined\n· stopped\n"), "{}", p.out);
    assert!(p.ran.is_empty());
    assert_eq!(p.asked.len(), 1);
}

#[test]
fn stop_typed_while_the_model_is_thinking_ends_the_turn() {
    let mut script = Script::new(&["fix it"]).model(vec![edit()]);
    script.during = Box::new(|_, tx| {
        tx.send("/stop\n".into()).unwrap();
        // Give the reading thread a moment to see it, as a real model's seconds would.
        std::thread::sleep(Duration::from_millis(300));
    });
    let p = play(script);
    assert!(p.out.contains("· stopped\n"), "{}", p.out);
    assert!(!p.out.contains("approve?"), "nothing is asked once it is stopped");
    assert!(p.ran.is_empty());
}

#[test]
fn a_line_typed_during_a_turn_is_the_next_prompt() {
    let mut script = Script::new(&["first"]).model(vec![answer("one"), answer("two")]);
    script.during = Box::new(|n, tx| {
        if n == 0 {
            tx.send("second\n".into()).unwrap();
            std::thread::sleep(Duration::from_millis(200));
        }
    });
    let p = play(script);
    assert!(p.out.contains("› first\none\n"), "{}", p.out);
    assert!(p.out.contains("› second\ntwo\n"), "{}", p.out);
}

#[test]
fn a_line_ending_in_a_backslash_goes_on_to_the_next() {
    let p = play(Script::new(&["first\\\nsecond"]).model(vec![answer("ok")]));
    assert_eq!(p.convo[0], Message::user("first\nsecond"));
    assert!(p.out.contains("› first\nsecond\nok\n"), "{}", p.out);
}

#[test]
fn empty_lines_are_ignored_and_exit_leaves_without_asking_the_model() {
    let p = play(Script::new(&["\n\n  \nhi", "/exit", "never"]).model(vec![answer("hello")]));
    assert_eq!(p.convo.len(), 2);
    assert_eq!(p.asked.len(), 1);
    assert!(p.out.ends_with("hello\n› "), "{}", p.out);
    assert_eq!(p.saved.len(), 1);
}

#[test]
fn the_header_names_the_branch_only_inside_a_repository() {
    let mut shown = Vec::new();
    for (branch, changed) in [(Some("main"), Some(0)), (None, None)] {
        let (out, input, _tx, text) = tap(&[], &[]);
        let stopper = crate::stop::Stopper::default();
        let mut ui = Line::new(out, line::feed(input, stopper.clone()), stopper, true);
        ui.show(Event::Header {
            folder: "C:\\work\\demo".into(),
            branch: branch.map(String::from),
            changed,
            model: "qwen".into(),
        });
        shown.push(text.lock().unwrap().clone());
    }
    assert_eq!(shown, ["hugin · demo · main, 0 changed · qwen\n", "hugin · demo · qwen\n"]);
}

#[test]
fn a_status_is_shown_when_it_changes_and_not_when_it_repeats() {
    let (out, input, _tx, text) = tap(&[], &[]);
    let stopper = crate::stop::Stopper::default();
    let mut ui = Line::new(out, line::feed(input, stopper.clone()), stopper, true);
    for s in ["Starting Ollama…", "Starting Ollama…", "", "Starting Ollama…"] {
        ui.show(Event::Status(s.into()));
    }
    assert_eq!(*text.lock().unwrap(), "… Starting Ollama…\n… Starting Ollama…\n");
}

#[test]
fn the_session_is_saved_after_every_turn_as_hugins_with_its_title_from_the_first_prompt() {
    let p = play(Script::new(&["Why is the build red?", "and now?"]).model(vec![answer("a"), answer("b")]));
    assert_eq!(p.saved.len(), 2);
    let (first, second) = (&p.saved[0], &p.saved[1]);
    assert_eq!(first.app.as_deref(), Some("hugin"));
    assert_eq!(first.title, "Why is the build red?");
    assert_eq!(first.messages.len(), 2);
    assert_eq!((first.id.as_str(), second.title.as_str()), (second.id.as_str(), "Why is the build red?"));
    let roles: Vec<_> = second.messages.iter().map(|m| (m.role.as_str(), m.content.as_str())).collect();
    assert_eq!(roles, [("user", "Why is the build red?"), ("assistant", "a"), ("user", "and now?"), ("assistant", "b")]);
    assert_eq!(second.messages[3].model, "m");
}

#[test]
fn calibration_from_one_turn_is_given_to_the_next() {
    let measured = round_with(Reply::Answer { content: "a".into(), eval_count: 1 }, 500);
    let p = play(Script::new(&["one", "two"]).model(vec![measured, answer("b")]));
    assert_eq!(p.asked[0].2, None);
    assert_eq!(p.asked[1].2, Some(Calibration::new(sent().prompt_chars(), 500)));
    // And the second request sees everything the first turn said.
    assert_eq!(p.asked[1].1, 3);
}

#[test]
fn a_turn_that_fails_is_shown_and_the_session_goes_on() {
    let p = play(Script::new(&["one", "two"]).model(vec![Err("POST /api/chat: connection refused".into()), answer("fine")]));
    assert!(p.out.contains("! POST /api/chat: connection refused\n"), "{}", p.out);
    assert!(p.out.contains("› two\nfine\n"));
    assert_eq!(p.saved.len(), 2);
}

#[test]
fn a_failed_warm_up_is_shown_once_before_the_next_prompt() {
    let mut script = Script::new(&[]);
    script.notices = vec!["could not warm up m: no ollama"];
    let p = play(script);
    assert!(p.out.contains("! could not warm up m: no ollama\n› "), "{}", p.out);
    assert_eq!(p.out.matches("could not warm up").count(), 1);
}

/// A guard against the harness itself hanging: a session must end within seconds.
#[test]
fn a_session_ends_promptly() {
    let started = Instant::now();
    let _ = play(Script::new(&[]));
    assert!(started.elapsed() < Duration::from_secs(5));
}
