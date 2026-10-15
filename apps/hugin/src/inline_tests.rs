//! Whole sessions through the interface, over a `TestBackend` with scripted keys: the
//! scripted model's rounds stand where Ollama does (as in [`crate::tests`]), so what is
//! checked is what lands in the scrollback and what the keys do.
//!
//! **A key is held back until the interface is in the state it was written for** ([`At`]), so
//! an `y` is never typed before the approval it answers has been asked, and nothing depends
//! on timing.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use bifrost_core::agent::round::Round;
use bifrost_core::agent::run::Ran;
use bifrost_core::agent::send::Calibration;
use bifrost_core::chat::Message;
use bifrost_core::store;
use crossterm::event::{Event as Term, KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;

use crate::inline::{At, Keys, Shared, Worker, run};
use crate::render::Theme;
use crate::session::Session;
use crate::tests::{answer, edit, edited, header};

struct Scripted {
    items: VecDeque<(At, Term)>,
    started: Instant,
}

impl Keys for Scripted {
    fn next(&mut self, wait: Duration, at: At) -> std::io::Result<Option<Term>> {
        assert!(self.started.elapsed() < Duration::from_secs(20), "the scripted session hung; left: {:?}", self.items);
        match self.items.front() {
            Some((need, _)) if *need == at => Ok(self.items.pop_front().map(|(_, e)| e)),
            _ => {
                std::thread::sleep(wait.min(Duration::from_millis(2)));
                Ok(None)
            }
        }
    }
}

fn key(code: KeyCode, mods: KeyModifiers) -> Term {
    Term::Key(KeyEvent::new(code, mods))
}

/// Keys, each for the state it is typed in.
struct Keystrokes(Vec<(At, Term)>);

impl Keystrokes {
    fn new() -> Self {
        Self(Vec::new())
    }

    fn typed(mut self, at: At, text: &str) -> Self {
        self.0.extend(text.chars().map(|c| (at, key(KeyCode::Char(c), KeyModifiers::NONE))));
        self
    }

    fn press(mut self, at: At, code: KeyCode, mods: KeyModifiers) -> Self {
        self.0.push((at, key(code, mods)));
        self
    }

    fn enter(self, at: At) -> Self {
        self.press(at, KeyCode::Enter, KeyModifiers::NONE)
    }

    fn quit(self) -> Self {
        self.press(At::Idle, KeyCode::Char('d'), KeyModifiers::CONTROL)
    }
}

/// A session whose model says `asks` in turn, each after `delay`, and whose runner gives `runs`.
fn session(
    asks: Vec<Result<Round, String>>,
    runs: Vec<Result<Ran, String>>,
    delay: Duration,
) -> impl FnOnce() -> Session + Send + 'static {
    move || {
        let (asks, runs) = (RefCell::new(VecDeque::from(asks)), RefCell::new(VecDeque::from(runs)));
        Session::new(
            "m".into(),
            Box::new(move |_: &[Message], _: Option<Calibration>, _: bool| {
                std::thread::sleep(delay);
                asks.borrow_mut().pop_front().expect("the loop asked the model more often than the script allows")
            }),
            Box::new(move |_: &str, _: &str, _: &str| {
                runs.borrow_mut().pop_front().expect("the loop ran a call the script did not expect")
            }),
            Box::new(|_: &store::Conversation| Ok(())),
        )
    }
}

/// Play a session to its end and return everything the person saw: the scrollback, then the
/// screen, a line each with its trailing blanks cut.
fn play(keys: Keystrokes, make: impl FnOnce() -> Session + Send + 'static) -> Vec<String> {
    let shared = Shared::new(TestBackend::new(60, 24));
    let (_keep, notices) = mpsc::channel();
    let mut script = Scripted { items: keys.0.into(), started: Instant::now() };
    run(shared.clone(), &mut script, Theme::new(false), header(), notices, Worker::spawn(make)).unwrap();
    assert!(script.items.is_empty(), "keys left unused: {:?}", script.items);
    shared.with(|b| {
        let rows = |buf: &ratatui::buffer::Buffer| -> Vec<String> {
            (0..buf.area.height)
                .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>().trim_end().to_string())
                .collect()
        };
        let mut all = rows(b.scrollback());
        all.extend(rows(b.buffer()));
        all
    })
}

fn has_run(seen: &[String], want: &[&str]) -> bool {
    seen.windows(want.len()).any(|w| w.iter().map(String::as_str).eq(want.iter().copied()))
}

#[test]
fn a_prompt_is_sent_an_approval_answered_y_and_the_answer_lands_in_the_scrollback() {
    let keys = Keystrokes::new().typed(At::Idle, "fix it").enter(At::Idle).typed(At::Approval, "y").quit();
    let seen = play(keys, session(vec![edit(), answer("Done.")], vec![edited()], Duration::ZERO));
    let want = [
        "› fix it",
        "? edit a.rs +1 −1",
        "   fn a() {",
        "  -    old();",
        "  +    new();",
        "   }",
        "● edit a.rs +1 −1 → applied",
        "Done.",
    ];
    assert!(has_run(&seen, &want), "{seen:#?}");
    assert_eq!(seen.iter().find(|l| l.starts_with("hugin")).map(String::as_str), Some("hugin · demo · main, 2 changed · m"));
}

#[test]
fn n_declines_and_the_turn_goes_on() {
    let keys = Keystrokes::new().typed(At::Idle, "fix it").enter(At::Idle).typed(At::Approval, "n").quit();
    let seen = play(keys, session(vec![edit(), answer("Understood.")], vec![], Duration::ZERO));
    assert!(has_run(&seen, &["● edit a.rs +1 −1 → declined", "Understood."]), "{seen:#?}");
}

#[test]
fn esc_during_a_turn_stops_it() {
    let keys = Keystrokes::new()
        .typed(At::Idle, "go")
        .enter(At::Idle)
        .press(At::Turn, KeyCode::Esc, KeyModifiers::NONE)
        .quit();
    // The model takes its time, so the Esc lands while the turn is in its request.
    let seen = play(keys, session(vec![answer("late")], vec![], Duration::from_millis(300)));
    assert!(seen.iter().any(|l| l == "· stopped"), "{seen:#?}");
}

#[test]
fn esc_at_an_approval_declines_it_and_stops() {
    let keys = Keystrokes::new()
        .typed(At::Idle, "fix it")
        .enter(At::Idle)
        .press(At::Approval, KeyCode::Esc, KeyModifiers::NONE)
        .quit();
    let seen = play(keys, session(vec![edit()], vec![], Duration::ZERO));
    assert!(has_run(&seen, &["● edit a.rs +1 −1 → declined", "· stopped"]), "{seen:#?}");
}

#[test]
fn what_is_typed_during_a_turn_waits_and_enter_does_not_send_it() {
    let keys = Keystrokes::new()
        .typed(At::Idle, "a")
        .enter(At::Idle)
        .typed(At::Turn, "nx")
        .enter(At::Turn)
        .enter(At::Idle)
        .quit();
    let seen = play(keys, session(vec![answer("one"), answer("two")], vec![], Duration::from_millis(200)));
    assert!(has_run(&seen, &["› a", "one", "› nx", "two"]), "{seen:#?}");
}

#[test]
fn a_backslash_makes_a_new_line_and_slash_exit_leaves() {
    let keys = Keystrokes::new()
        .typed(At::Idle, "one\\")
        .enter(At::Idle)
        .typed(At::Idle, "two")
        .enter(At::Idle)
        .typed(At::Idle, "/exit");
    let keys = keys.enter(At::Idle);
    let seen = play(keys, session(vec![answer("ok")], vec![], Duration::ZERO));
    assert!(has_run(&seen, &["› one", "  two", "ok"]), "{seen:#?}");
}

#[test]
fn a_failed_turn_is_shown_as_an_error() {
    let keys = Keystrokes::new().typed(At::Idle, "go").enter(At::Idle).quit();
    let seen = play(keys, session(vec![Err("POST /api/chat: connection refused".into())], vec![], Duration::ZERO));
    assert!(seen.iter().any(|l| l == "! POST /api/chat: connection refused"), "{seen:#?}");
}
