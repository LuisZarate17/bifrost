//! Whole sessions through the interface, over a `TestBackend` with scripted keys: the
//! scripted model's rounds stand where Ollama does (as in [`crate::tests`]), so what is
//! checked is what lands on the screen and what the keys do.
//!
//! **A key is held back until the interface is in the state it was written for** ([`At`]), so
//! an `y` is never typed before the approval it answers has been asked, and nothing depends
//! on timing.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use bifrost_core::agent::round::Round;
use bifrost_core::agent::run::Ran;
use bifrost_core::agent::send::Calibration;
use bifrost_core::chat::Message;
use bifrost_core::store;
use crossterm::event::{Event as Term, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::backend::{Backend, ClearType, TestBackend, WindowSize};
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::{Position, Size};

use crate::inline::{At, Keys, Worker, run};
use crate::render::Theme;
use crate::session::Session;
use crate::tests::{answer, edit, edited, header};

/// A `TestBackend` that can be looked at while the interface owns it.
#[derive(Clone)]
struct Peek(Rc<RefCell<TestBackend>>);

impl Peek {
    fn new(width: u16, height: u16) -> Self {
        Self(Rc::new(RefCell::new(TestBackend::new(width, height))))
    }

    fn with<T>(&self, f: impl FnOnce(&mut TestBackend) -> T) -> T {
        f(&mut self.0.borrow_mut())
    }
}

type Never = <TestBackend as Backend>::Error;

impl Backend for Peek {
    type Error = Never;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Never>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.0.borrow_mut().draw(content)
    }

    fn append_lines(&mut self, n: u16) -> Result<(), Never> {
        self.0.borrow_mut().append_lines(n)
    }

    fn hide_cursor(&mut self) -> Result<(), Never> {
        self.0.borrow_mut().hide_cursor()
    }

    fn show_cursor(&mut self) -> Result<(), Never> {
        self.0.borrow_mut().show_cursor()
    }

    fn get_cursor_position(&mut self) -> Result<Position, Never> {
        self.0.borrow_mut().get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Never> {
        self.0.borrow_mut().set_cursor_position(position)
    }

    fn clear(&mut self) -> Result<(), Never> {
        self.0.borrow_mut().clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Never> {
        self.0.borrow_mut().clear_region(clear_type)
    }

    fn size(&self) -> Result<Size, Never> {
        self.0.borrow().size()
    }

    fn window_size(&mut self) -> Result<WindowSize, Never> {
        self.0.borrow_mut().window_size()
    }

    fn flush(&mut self) -> Result<(), Never> {
        self.0.borrow_mut().flush()
    }
}

fn rows_of(buf: &Buffer) -> Vec<String> {
    (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>().trim_end().to_string())
        .collect()
}

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

/// Play a session to its end and return what the person saw last: the screen, a row each with its
/// trailing blanks cut.
fn play(keys: Keystrokes, make: impl FnOnce() -> Session + Send + 'static) -> Vec<String> {
    let screen = Peek::new(60, 24);
    let (_keep, notices) = mpsc::channel();
    let mut script = Scripted { items: keys.0.into(), started: Instant::now() };
    run(screen.clone(), &mut script, Theme::new(false), Some(header()), notices, Worker::spawn(make)).unwrap();
    assert!(script.items.is_empty(), "keys left unused: {:?}", script.items);
    screen.with(|b| rows_of(b.buffer()))
}

fn has_run(seen: &[String], want: &[&str]) -> bool {
    seen.windows(want.len()).any(|w| w.iter().map(String::as_str).eq(want.iter().copied()))
}

#[test]
fn a_prompt_is_sent_an_approval_answered_y_and_the_answer_lands_in_the_transcript() {
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

/// A key typed while the model thinks — or in the moment an approval arrives, before
/// anyone could have read it — goes to the input and answers nothing: only a key pressed
/// once the approval is armed does (review I2, finding 1).
#[test]
fn a_y_typed_ahead_never_answers_an_approval() {
    let keys = Keystrokes::new()
        .typed(At::Idle, "fix it")
        .enter(At::Idle)
        .typed(At::Turn, "y")
        .typed(At::Approval, "n")
        .quit();
    let seen = play(keys, session(vec![edit(), answer("Understood.")], vec![], Duration::from_millis(30)));
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
    assert!(has_run(&seen, &["› a", "one", "", "› nx", "two"]), "{seen:#?}");
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
// ── the screen ──

/// What is done to the terminal between two passes of the loop.
enum Step {
    Key(Term),
    Resize(u16, u16),
    /// A step, once the screen shows what the first asks for.
    When(Until, Box<Step>),
}

/// What a screen must show.
type Until = Box<dyn Fn(&[String]) -> bool>;

/// Keys that also look at the screen: a snapshot at the start of each pass, then one step. When
/// the steps are done and a snapshot satisfies `until`, the person leaves.
struct Watch {
    screen: Peek,
    steps: VecDeque<Step>,
    until: Until,
    seen: Vec<Vec<String>>,
    started: Instant,
}

impl Keys for Watch {
    fn next(&mut self, wait: Duration, at: At) -> std::io::Result<Option<Term>> {
        assert!(self.started.elapsed() < Duration::from_secs(20), "the watched session hung; last: {:#?}", self.seen.last());
        if wait.is_zero() {
            return Ok(None);
        }
        self.seen.push(self.screen.with(|b| rows_of(b.buffer())));
        let shown = self.seen.last().unwrap();
        // The first pass has nothing on screen yet: the steps begin once something is drawn.
        let ready = self.seen.len() > 1 && !matches!(self.steps.front(), Some(Step::When(when, _)) if !when(shown));
        let step = if ready { self.steps.pop_front() } else { None };
        let step = match step {
            Some(Step::When(_, inner)) => Some(*inner),
            other => other,
        };
        match step {
            Some(Step::Key(e)) => Ok(Some(e)),
            Some(Step::When(..)) => unreachable!("a step waits on one condition"),
            Some(Step::Resize(w, h)) => {
                self.screen.with(|b| b.resize(w, h));
                Ok(Some(Term::Resize(w, h)))
            }
            None if self.seen.len() > 1 && self.steps.is_empty() && at == At::Idle && (self.until)(shown) => {
                Ok(Some(key(KeyCode::Char('d'), KeyModifiers::CONTROL)))
            }
            None => {
                std::thread::sleep(wait.min(Duration::from_millis(2)));
                Ok(None)
            }
        }
    }
}

fn typed(text: &str) -> impl Iterator<Item = Step> + '_ {
    text.chars().map(|c| Step::Key(key(KeyCode::Char(c), KeyModifiers::NONE)))
}

fn press(code: KeyCode, mods: KeyModifiers) -> Step {
    Step::Key(key(code, mods))
}

fn wheel(kind: MouseEventKind) -> Step {
    Step::Key(Term::Mouse(MouseEvent { kind, column: 0, row: 0, modifiers: KeyModifiers::NONE }))
}

/// A screen with `text` on one of its rows, and the interface idle.
fn idle_with(text: &'static str) -> Until {
    Box::new(move |s| s.iter().any(|l| l.contains(text)) && s.iter().any(|l| l.contains("ctrl+d quit")))
}

fn has(text: &'static str) -> Until {
    Box::new(move |s| s.iter().any(|l| l.contains(text)))
}

/// Play a session on a lit `width` by `height` screen until `until`; the snapshots.
fn watched(
    (width, height): (u16, u16),
    steps: Vec<Step>,
    until: Until,
    make: impl FnOnce() -> Session + Send + 'static,
) -> Vec<Vec<String>> {
    let screen = Peek::new(width, height);
    let (_keep, notices) = mpsc::channel();
    let mut watch = Watch { screen: screen.clone(), steps: steps.into(), until, seen: Vec::new(), started: Instant::now() };
    run(screen, &mut watch, Theme::new(true), Some(header()), notices, Worker::spawn(make)).unwrap();
    assert!(watch.steps.is_empty(), "steps left unused");
    watch.seen
}

fn quiet() -> impl FnOnce() -> Session + Send + 'static {
    session(vec![], vec![], Duration::ZERO)
}

fn bar(width: usize) -> String {
    "─".repeat(width - 1)
}

fn hint() -> String {
    format!("  {}", crate::render::HINT)
}

/// A session of one long answer: `n` lines, `l0` to `l{n-1}`.
fn long(n: usize) -> String {
    (0..n).map(|k| format!("l{k}")).collect::<Vec<_>>().join("\n")
}

fn enter() -> Step {
    press(KeyCode::Enter, KeyModifiers::NONE)
}

#[test]
fn the_start_screen_is_at_the_top_and_the_box_on_the_bottom_row() {
    let seen = watched((60, 24), vec![], has(crate::render::HINT), quiet());
    let screen = seen.last().unwrap();
    assert_eq!(screen.len(), 24);
    assert!(screen[0].contains("Hugin"), "{screen:#?}");
    assert_eq!(screen[4], "");
    assert!(screen[5..20].iter().all(String::is_empty), "{screen:#?}");
    assert_eq!(screen[20..], [bar(60), "›".to_string(), bar(60), hint()]);
}

#[test]
fn a_prompt_and_its_answer_follow_the_start_screen_without_a_second_blank_row() {
    let steps: Vec<Step> = typed("hi").chain([enter()]).collect();
    let seen = watched((60, 24), steps, idle_with("Done."), session(vec![answer("Done.")], vec![], Duration::ZERO));
    let screen = seen.last().unwrap();
    assert!(screen[0].contains("Hugin"));
    assert_eq!(screen[4], "");
    assert_eq!(screen[5..7], ["› hi", "Done."]);
    assert!(screen[7..20].iter().all(String::is_empty), "{screen:#?}");
    assert_eq!(screen[20..], [bar(60), "›".to_string(), bar(60), hint()]);
}

#[test]
fn a_prompt_that_has_scrolled_off_is_pinned_on_the_top_row_while_the_answer_follows() {
    let steps: Vec<Step> = typed("go").chain([enter()]).collect();
    let seen = watched((60, 24), steps, idle_with("l39"), session(vec![answer(&long(40))], vec![], Duration::ZERO));
    let screen = seen.last().unwrap();
    // The end is in view, the prompt above it is on the top row, and the box is under them.
    assert_eq!(screen[0], "› go", "{screen:#?}");
    assert_eq!(screen[1], "l21");
    assert_eq!(screen[19], "l39");
    assert_eq!(screen[20..], [bar(60), "›".to_string(), bar(60), hint()]);
}

#[test]
fn the_wheel_and_the_page_keys_scroll_and_ctrl_end_returns_to_the_end() {
    let go: Vec<Step> = typed("go").chain([enter()]).collect();
    let mut steps = go;
    steps.push(Step::When(idle_with("l39"), Box::new(Step::Key(key(KeyCode::Char('x'), KeyModifiers::NONE)))));
    // A typed x is not a key for the window: it goes to the input.
    steps.push(wheel(MouseEventKind::ScrollUp));
    let seen = watched((60, 24), steps, has("↓ ctrl+end"), session(vec![answer(&long(40))], vec![], Duration::ZERO));
    let screen = seen.last().unwrap();
    // Three rows up: the window starts three rows earlier, the prompt is still the pinned one.
    assert_eq!(screen[0], "› go", "{screen:#?}");
    assert_eq!(screen[1], "l18");
    assert_eq!(screen[19], "l36");
    assert!(screen[23].trim_start().starts_with("↓ ctrl+end"), "{screen:#?}");
    assert_eq!(screen[21], "› x");
}

#[test]
fn page_up_moves_a_page_and_end_and_home_stay_with_the_input() {
    let mut steps: Vec<Step> = typed("go").chain([enter()]).collect();
    steps.push(Step::When(idle_with("l39"), Box::new(Step::Key(key(KeyCode::PageUp, KeyModifiers::NONE)))));
    steps.extend(typed("ab"));
    // End and Home move the input's cursor, not the window.
    steps.push(press(KeyCode::Home, KeyModifiers::NONE));
    steps.push(press(KeyCode::Char('z'), KeyModifiers::NONE));
    steps.push(press(KeyCode::End, KeyModifiers::NONE));
    steps.push(press(KeyCode::Char('y'), KeyModifiers::NONE));
    let seen = watched((60, 24), steps, has("› zaby"), session(vec![answer(&long(40))], vec![], Duration::ZERO));
    let screen = seen.last().unwrap();
    // A page is the window less one row: 19 up.
    assert_eq!(screen[0], "› go", "{screen:#?}");
    assert_eq!(screen[1], "l2");
    assert_eq!(screen[19], "l20");
    assert_eq!(screen[21], "› zaby");
}

#[test]
fn ctrl_end_and_page_down_come_back_to_the_end() {
    let mut steps: Vec<Step> = typed("go").chain([enter()]).collect();
    steps.push(Step::When(idle_with("l39"), Box::new(Step::Key(key(KeyCode::PageUp, KeyModifiers::NONE)))));
    steps.push(Step::When(has("↓ ctrl+end"), Box::new(Step::Key(key(KeyCode::PageDown, KeyModifiers::NONE)))));
    steps.push(Step::When(Box::new(|s| s.iter().all(|l| !l.contains("↓"))), Box::new(Step::Key(key(KeyCode::Home, KeyModifiers::CONTROL)))));
    steps.push(Step::When(has("↓ ctrl+end"), Box::new(Step::Key(key(KeyCode::End, KeyModifiers::CONTROL)))));
    let seen = watched((60, 24), steps, Box::new(|s| s.len() == 24 && s[19] == "l39" && s[0] == "› go"), session(vec![answer(&long(40))], vec![], Duration::ZERO));
    let screen = seen.last().unwrap();
    assert_eq!(screen[19], "l39", "{screen:#?}");
    assert_eq!(screen[23], hint());
    // Ctrl+Home had shown the start of everything, with no row to pin.
    assert!(seen.iter().any(|s| s[0].contains("Hugin") && s[23].trim_start().starts_with("↓ ctrl+end")));
}

#[test]
fn scrolled_up_new_rows_do_not_move_the_window_and_the_status_line_says_so() {
    let mut steps: Vec<Step> = typed("go").chain([enter()]).collect();
    steps.push(Step::When(idle_with("l39"), Box::new(Step::Key(key(KeyCode::Char('x'), KeyModifiers::NONE)))));
    steps.push(Step::When(idle_with("› x"), Box::new(Step::Key(key(KeyCode::Enter, KeyModifiers::NONE)))));
    // The second turn is at its request when the window is scrolled up.
    steps.push(Step::When(has("thinking"), Box::new(Step::Key(key(KeyCode::PageUp, KeyModifiers::NONE)))));
    let model = session(vec![answer(&long(40)), answer("late answer")], vec![], Duration::from_millis(150));
    let seen = watched((60, 24), steps, has("new below"), model);
    let screen = seen.last().unwrap();
    assert!(!screen.iter().any(|l| l.contains("late answer")), "{screen:#?}");
    assert!(screen[23].contains("↓ new below · ctrl+end"), "{screen:#?}");
}

#[test]
fn the_box_grows_upward_with_the_input_and_the_transcript_gives_up_rows() {
    let mut steps: Vec<Step> = typed("go").chain([enter()]).collect();
    steps.push(Step::When(idle_with("l39"), Box::new(Step::Key(key(KeyCode::Char('o'), KeyModifiers::NONE)))));
    steps.push(press(KeyCode::Enter, KeyModifiers::SHIFT));
    steps.extend(typed("two"));
    let seen = watched((60, 24), steps, has("  two"), session(vec![answer(&long(40))], vec![], Duration::ZERO));
    let screen = seen.last().unwrap();
    assert_eq!(screen[0], "› go");
    assert_eq!(screen[18], "l39", "{screen:#?}");
    assert_eq!(screen[19..], [bar(60), "› o".to_string(), "  two".to_string(), bar(60), hint()]);
}

#[test]
fn a_taller_screen_lays_the_start_out_again_with_the_box_on_the_new_bottom_row() {
    let steps = vec![Step::Resize(60, 30)];
    let seen = watched((60, 24), steps, Box::new(|s| s.len() == 30 && s[26] == bar(60)), quiet());
    let screen = seen.last().unwrap();
    assert!(screen[0].contains("Hugin"), "{screen:#?}");
    assert!(screen[5..26].iter().all(String::is_empty), "{screen:#?}");
    assert_eq!(screen[26..], [bar(60), "›".to_string(), bar(60), hint()]);
}

/// After a resize the transcript, start screen and all, is wrapped again for the new width.
#[test]
fn a_resize_lays_the_start_screen_and_an_answer_out_again_at_the_new_width() {
    let said = "aaaa bbbb cccc dddd eeee ffff gggg hhhh iiii jjjj";
    let mut steps: Vec<Step> = typed("hi").chain([enter()]).collect();
    steps.push(Step::When(idle_with("jjjj"), Box::new(Step::Resize(30, 24))));
    let until = Box::new(|s: &[String]| s.len() == 24 && s[20] == bar(30) && s.iter().any(|l| l == "› hi") && s.iter().any(|l| l == "gggg hhhh iiii jjjj"));
    let seen = watched((60, 24), steps, until, session(vec![answer(said)], vec![], Duration::ZERO));
    let screen = seen.last().unwrap();
    // Under 48 columns the art comes first and the text under it.
    assert!(screen[4].starts_with("  Hugin") && screen[8].is_empty(), "{screen:#?}");
    assert_eq!(screen[9..12], ["› hi", "aaaa bbbb cccc dddd eeee ffff", "gggg hhhh iiii jjjj"]);
    assert!(screen[12..20].iter().all(String::is_empty), "{screen:#?}");
    assert_eq!(screen[20..23], [bar(30), "›".to_string(), bar(30)]);
}
