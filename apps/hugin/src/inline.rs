//! The interactive interface: full screen, like Claude Code's. It takes the terminal's alternate
//! screen and gives it back on the way out, so the shell's own screen is as it was left.
//!
//! **Two threads.** The turn runs on a worker that owns the [`Session`]; its [`Ui`] is a
//! [`Proxy`] that sends [`Event`]s over a channel, and an approval is a message with a reply
//! channel to wait on. This thread reads the keyboard and the mouse and draws. [`run`] is generic
//! over the backend and over where keys come from ([`Keys`]), so a test drives a whole session
//! over a `TestBackend` with scripted keys.
//!
//! **The scrollback is Hugin's own.** What is said is kept ([`Transcript`]) and laid out, as rows,
//! for the width the screen has. The screen is, top to bottom: a pinned row (the person's last
//! prompt, once it has scrolled off the top), a window onto those rows, and the box (the input
//! between two rules, with the status line under it) on the last rows. A resize lays the rows out
//! again for the new width, and the next frame draws them.
//!
//! **Scrolling** is the wheel (3 rows), PgUp and PgDn (a page), Ctrl+Home and Ctrl+End. Home and
//! End stay with the input. At the end the window follows new rows; scrolled up it stays where it
//! is, and the status line says there is more below. The mouse is captured for this, so the
//! terminal's own selection needs shift held.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use bifrost_core::agent::turn::Decision;
use crossterm::event::{Event as Term, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind};
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::{Backend, CrosstermBackend};

use crate::input::Input;
use crate::render::{self, Follow, Status, Theme};
use crate::session::Session;
use crate::stop::Stopper;
use crate::transcript::{Said, Transcript};
use crate::ui::{Approval, Event, Ui};

/// How long the loop waits for a key before it looks at the worker and the clock again.
const POLL: Duration = Duration::from_millis(50);

/// The most key events handled before the screen is drawn: a paste without bracketed paste
/// arrives as one key per character.
const BURST: usize = 512;

/// The rows one turn of the wheel scrolls.
const WHEEL: usize = 3;

// ── the worker ──────────────────────────────────────────────────────────────

/// What the worker tells the interface.
pub enum Msg {
    Show(Event),
    /// A call that needs a yes; the answer goes back on the channel.
    Ask(Approval, Sender<Decision>),
}

/// The worker's [`Ui`]: everything is a message.
struct Proxy {
    tx: Sender<Msg>,
    stopper: Stopper,
}

impl Ui for Proxy {
    fn show(&mut self, e: Event) {
        let _ = self.tx.send(Msg::Show(e));
    }

    fn approve(&mut self, a: &Approval) -> Decision {
        if self.stopper.stopped() {
            return Decision::Deny;
        }
        let (reply, answer) = mpsc::channel();
        if self.tx.send(Msg::Ask(a.clone(), reply)).is_err() {
            return Decision::Deny;
        }
        loop {
            match answer.recv_timeout(POLL) {
                Ok(decision) => return decision,
                // A Stop pressed while waiting ends the wait, so the turn can end.
                Err(RecvTimeoutError::Timeout) if self.stopper.stopped() => return Decision::Deny,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Decision::Deny,
            }
        }
    }
}

/// The thread that runs turns, and the ends of its channels.
pub struct Worker {
    prompts: Sender<String>,
    events: Receiver<Msg>,
    stopper: Stopper,
    handle: JoinHandle<()>,
}

impl Worker {
    /// Start the worker. The session is made *on* the thread by `make`, because its model and
    /// runner need not be `Send`; the thread hands back its stopper before anything else.
    pub fn spawn(make: impl FnOnce() -> Session + Send + 'static) -> Worker {
        let (prompts, prompt_rx) = mpsc::channel::<String>();
        let (tx, events) = mpsc::channel();
        let (stop_tx, stop_rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            let mut session = make();
            let stopper = session.stopper();
            let _ = stop_tx.send(stopper.clone());
            let mut proxy = Proxy { tx, stopper };
            for prompt in prompt_rx {
                session.turn(&prompt, &mut proxy);
            }
        });
        let stopper = stop_rx.recv().unwrap_or_default();
        Worker { prompts, events, stopper, handle }
    }
}

fn io_err(e: impl std::error::Error) -> io::Error {
    io::Error::other(e.to_string())
}

// ── keys ────────────────────────────────────────────────────────────────────

/// What the interface is doing, for a key source that wants to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum At {
    Idle,
    Turn,
    Approval,
}

/// Where terminal events come from.
pub trait Keys {
    /// The next event, waiting up to `wait` for one. A scripted source may hold an event back
    /// until the interface is `at` the state it was written for.
    fn next(&mut self, wait: Duration, at: At) -> io::Result<Option<Term>>;
}

/// The real keyboard.
pub struct Crossterm;

impl Keys for Crossterm {
    fn next(&mut self, wait: Duration, _at: At) -> io::Result<Option<Term>> {
        if crossterm::event::poll(wait)? { crossterm::event::read().map(Some) } else { Ok(None) }
    }
}

// ── the loop ────────────────────────────────────────────────────────────────

enum Phase {
    Idle,
    /// A turn is running: what it waits for, since when.
    Turn { what: String, since: Instant },
    /// An approval on screen. `armed` is when it starts taking answers: keys typed
    /// before it showed, still on their way, must not answer something nobody has seen.
    Approval { ask: Approval, reply: Sender<Decision>, armed: Instant },
}

/// How long an approval ignores y, n and a after it arrives. Long enough for keys typed
/// ahead to be read and for the card to be drawn; short enough that nobody notices.
const ARMING: Duration = Duration::from_millis(300);

const THINKING: &str = "thinking";

struct App {
    input: Input,
    phase: Phase,
    theme: Theme,
    stopper: Stopper,
    prompts: Sender<String>,
    /// Esc or Ctrl+C was pressed during the turn. A Stop pressed in the instant before the
    /// worker begins the turn would be reset by it, so it is pressed again until it holds.
    stop_asked: bool,
    quit: bool,
    /// Everything said, as the rows of the scrollback.
    transcript: Transcript,
    /// How many rows the window is above the end; 0 follows new rows.
    scroll: usize,
    /// Rows came while scrolled up.
    behind: bool,
    /// The rows a page scrolls: one less than the window's, as last drawn.
    page: usize,
}

impl App {
    fn at(&self) -> At {
        match self.phase {
            Phase::Idle => At::Idle,
            Phase::Turn { .. } => At::Turn,
            // Until it is armed an approval takes keys as the turn did: typed ahead, they
            // were meant for the input, and answer nothing.
            Phase::Approval { armed, .. } if Instant::now() < armed => At::Turn,
            Phase::Approval { .. } => At::Approval,
        }
    }

    fn status(&self) -> Status {
        match &self.phase {
            Phase::Idle => Status::Idle,
            Phase::Turn { what, since } => Status::Working(what.clone(), since.elapsed().as_secs()),
            Phase::Approval { .. } => Status::Approving,
        }
    }

    fn working(&mut self, what: &str) {
        let same = matches!(&self.phase, Phase::Turn { what: w, .. } if w == what);
        if !same {
            self.phase = Phase::Turn { what: what.to_string(), since: Instant::now() };
        }
    }

    fn say(&mut self, e: Event) {
        self.transcript.push(Said::Event(e));
    }

    /// Something the worker said.
    fn msg(&mut self, m: Msg) {
        match m {
            Msg::Show(Event::Status(text)) => {
                if matches!(self.phase, Phase::Turn { .. }) {
                    self.working(if text.is_empty() { THINKING } else { &text });
                }
            }
            Msg::Show(Event::TurnEnd) => {
                self.phase = Phase::Idle;
                self.stop_asked = false;
            }
            Msg::Show(e) => {
                // A result ends whatever the turn was waiting on.
                if matches!(e, Event::Tool(_)) && matches!(self.phase, Phase::Turn { .. }) {
                    self.working(THINKING);
                }
                self.say(e);
            }
            Msg::Ask(ask, reply) => {
                self.transcript.push(Said::Ask(ask.clone()));
                // The card is at the end, and what it asks cannot be answered unseen.
                self.scroll = 0;
                self.phase = Phase::Approval { ask, reply, armed: Instant::now() + ARMING };
            }
        }
    }

    /// The person answers an approval, and the turn carries on.
    fn answer(&mut self, decision: Decision) {
        if let Phase::Approval { ask, reply, .. } = std::mem::replace(&mut self.phase, Phase::Idle) {
            let _ = reply.send(decision);
            let running = ask.name == "check" && decision != Decision::Deny;
            let what = if running { format!("running {}", ask.target) } else { THINKING.to_string() };
            self.phase = Phase::Turn { what, since: Instant::now() };
        }
    }

    fn stop(&mut self) {
        self.stop_asked = true;
        self.stopper.stop();
        if matches!(self.phase, Phase::Approval { .. }) {
            self.answer(Decision::Deny);
        }
        self.working("stopping");
    }

    /// A key that moves the window and nothing else. Home and End are the input's.
    fn scrolled_by(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::PageUp => self.scroll = self.scroll.saturating_add(self.page),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_sub(self.page),
            KeyCode::End if ctrl => self.scroll = 0,
            KeyCode::Home if ctrl => self.scroll = usize::MAX,
            _ => return false,
        }
        true
    }

    fn key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release || self.scrolled_by(key) {
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let code = key.code;
        let ctrl_c = ctrl && matches!(code, KeyCode::Char('c' | 'C'));
        match self.at() {
            At::Approval => match code {
                KeyCode::Char('y' | 'Y') if !ctrl => self.answer(Decision::Allow),
                KeyCode::Char('n' | 'N') if !ctrl => self.answer(Decision::Deny),
                KeyCode::Char('a' | 'A') if !ctrl => self.answer(Decision::AllowForSession),
                KeyCode::Esc => self.stop(),
                _ if ctrl_c => self.stop(),
                _ => {}
            },
            At::Turn => {
                if code == KeyCode::Esc || ctrl_c {
                    self.stop();
                } else {
                    self.input.key(key, true);
                }
            }
            At::Idle => {
                if ctrl_c {
                    self.input.clear();
                } else if ctrl && matches!(code, KeyCode::Char('d' | 'D')) {
                    self.quit = true;
                } else if let Some(prompt) = self.input.key(key, false) {
                    if prompt == "/exit" {
                        self.quit = true;
                    } else {
                        self.stopper.begin();
                        // What was asked is at the end, with the answer to come.
                        self.scroll = 0;
                        self.phase = Phase::Turn { what: THINKING.into(), since: Instant::now() };
                        self.stop_asked = false;
                        if self.prompts.send(prompt).is_err() {
                            self.quit = true;
                        }
                    }
                }
            }
        }
    }

    fn event(&mut self, e: Term) {
        match e {
            Term::Key(k) => self.key(k),
            Term::Paste(text) => self.input.paste(&text),
            Term::Mouse(m) => match m.kind {
                MouseEventKind::ScrollUp => self.scroll = self.scroll.saturating_add(WHEEL),
                MouseEventKind::ScrollDown => self.scroll = self.scroll.saturating_sub(WHEEL),
                _ => {}
            },
            // The next frame is laid out for the size the screen has; the event only wakes the loop.
            Term::Resize(..) => {}
            _ => {}
        }
    }

    /// Draw the screen into `f`: lay out what is new for its width, keep the window where the
    /// person left it, and show the window and the box.
    fn paint(&mut self, f: &mut Frame) {
        let area = f.area();
        let added = self.transcript.lay(area.width, &self.theme);
        if self.scroll > 0 && added > 0 {
            self.scroll = self.scroll.saturating_add(added);
            self.behind = true;
        }
        let room = usize::from(area.height.saturating_sub(render::live_height(&self.input, area.width)));
        self.page = room.saturating_sub(1).max(1);
        self.scroll = self.scroll.min(self.transcript.rows().saturating_sub(room));
        if self.scroll == 0 {
            self.behind = false;
        }
        let follow = match (self.scroll, self.behind) {
            (0, _) => Follow::End,
            (_, false) => Follow::Up,
            (_, true) => Follow::Behind,
        };
        let view = self.transcript.view(room, self.scroll, &self.theme, area.width);
        render::draw_screen(f, &view, &self.status(), follow, &self.input, &self.theme);
    }
}


/// Run the interface until the person leaves. `header` and `notices` (a failed warm-up, say)
/// are the first things said.
pub fn run<B: Backend, K: Keys>(
    backend: B,
    keys: &mut K,
    theme: Theme,
    header: Option<Event>,
    notices: Receiver<String>,
    worker: Worker,
) -> io::Result<()> {
    let mut terminal = Terminal::new(backend).map_err(io_err)?;
    let Worker { prompts, events, stopper, handle } = worker;
    let mut app = App {
        input: Input::default(),
        phase: Phase::Idle,
        theme,
        stopper,
        prompts,
        stop_asked: false,
        quit: false,
        transcript: Transcript::default(),
        scroll: 0,
        behind: false,
        page: 1,
    };
    if let Some(h) = header {
        app.say(h);
    }

    while !app.quit {
        loop {
            match events.try_recv() {
                Ok(m) => app.msg(m),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    app.say(Event::Error("the session ended unexpectedly".into()));
                    app.quit = true;
                    break;
                }
            }
        }
        while let Ok(notice) = notices.try_recv() {
            app.say(Event::Error(notice));
        }
        // Stop is held until the worker has seen it, and an approval it ended is left.
        if app.stop_asked && app.stopper.running() && !app.stopper.stopped() {
            app.stopper.stop();
        }
        if matches!(app.phase, Phase::Approval { .. }) && app.stopper.stopped() {
            app.answer(Decision::Deny);
            app.working("stopping");
        }

        if let Some(e) = keys.next(POLL, app.at())? {
            app.event(e);
            for _ in 0..BURST {
                match keys.next(Duration::ZERO, app.at())? {
                    Some(e) => app.event(e),
                    None => break,
                }
            }
        }

        // The frame is laid out for the size the screen has when it is drawn, a resize or not.
        terminal.draw(|f| app.paint(f)).map_err(io_err)?;
    }

    // Nobody is waiting on a prompt any more; an idle worker ends with its channel.
    drop(app);
    let _ = handle.join();
    Ok(())
}

// ── the real terminal ───────────────────────────────────────────────────────

/// Whether the terminal has been put back already: it is once, by whichever comes first.
static RESTORED: AtomicBool = AtomicBool::new(false);

/// Put the terminal back: the alternate screen left, the mouse and paste modes off, raw mode off,
/// the cursor shown; then one line on the shell's own screen, so what comes next starts clean.
fn restore() {
    if RESTORED.swap(true, Ordering::SeqCst) {
        return;
    }
    let mut out = io::stdout();
    let _ = crossterm::execute!(
        out,
        crossterm::event::DisableMouseCapture,
        crossterm::event::DisableBracketedPaste,
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::cursor::Show
    );
    let _ = crossterm::terminal::disable_raw_mode();
    let _ = write!(out, "\x1b[2mhugin · session saved\x1b[0m\r\n");
    let _ = out.flush();
}

/// Restores the terminal when dropped, whichever way [`start`] ends.
struct Restore;

impl Drop for Restore {
    fn drop(&mut self) {
        restore();
    }
}

/// The interface on the real terminal. The terminal is put back on every way out: Ctrl+D, an
/// error, and a panic, whose message is then printed on a terminal that can show it.
pub fn start(
    make: impl FnOnce() -> Session + Send + 'static,
    header: Event,
    notices: Receiver<String>,
) -> io::Result<()> {
    let worker = Worker::spawn(make);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
    }));
    let theme = Theme::from_env();
    crossterm::terminal::enable_raw_mode()?;
    let _restore = Restore;
    // Not every terminal has bracketed paste or a mouse; those that do not just ignore the asking.
    crossterm::execute!(
        io::stdout(),
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture,
        crossterm::event::EnableBracketedPaste
    )?;
    run(CrosstermBackend::new(io::stdout()), &mut Crossterm, theme, Some(header), notices, worker)
}
