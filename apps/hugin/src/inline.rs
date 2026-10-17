//! The interactive interface: inline, like Claude Code. It scrolls in the normal terminal and
//! never takes the alternate screen. Everything finished goes into the terminal's own
//! scrollback ([`render::scrollback`], by `insert_before`); only the live area at the bottom,
//! a status line and the input box, is redrawn.
//!
//! **Two threads.** The turn runs on a worker that owns the [`Session`]; its [`Ui`] is a
//! [`Proxy`] that sends [`Event`]s over a channel, and an approval is a message with a reply
//! channel to wait on. This thread reads the keyboard and draws. [`run`] is generic over the
//! backend and over where keys come from ([`Keys`]), so a test drives a whole session over a
//! `TestBackend` with scripted keys.
//!
//! **The live area's height changes** with the input, and a ratatui inline viewport has a
//! height fixed when it is made. So when the height changes the old viewport is cleared and a
//! new terminal is made at the same place, over a [`Shared`] backend both can use.

use std::cell::RefCell;
use std::io::{self, Write};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use bifrost_core::agent::turn::Decision;
use crossterm::event::{Event as Term, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use ratatui::text::Line;
use ratatui::{Terminal, TerminalOptions, Viewport};

use crate::input::Input;
use crate::render::{self, Status, Theme};
use crate::session::Session;
use crate::stop::Stopper;
use crate::ui::{Approval, Event, Ui};

/// How long the loop waits for a key before it looks at the worker and the clock again.
const POLL: Duration = Duration::from_millis(50);

/// The most key events handled before the screen is drawn: a paste without bracketed paste
/// arrives as one key per character.
const BURST: usize = 512;

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

// ── the screen ──────────────────────────────────────────────────────────────

/// A backend several owners hold, so a terminal can be made again over the same one.
pub struct Shared<B>(Rc<RefCell<B>>);

impl<B> Shared<B> {
    pub fn new(backend: B) -> Self {
        Self(Rc::new(RefCell::new(backend)))
    }

    #[cfg(test)]
    pub fn with<T>(&self, f: impl FnOnce(&B) -> T) -> T {
        f(&self.0.borrow())
    }
}

impl<B> Clone for Shared<B> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<B: Backend> Backend for Shared<B> {
    type Error = B::Error;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.0.borrow_mut().draw(content)
    }

    fn append_lines(&mut self, n: u16) -> Result<(), Self::Error> {
        self.0.borrow_mut().append_lines(n)
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.0.borrow_mut().hide_cursor()
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.0.borrow_mut().show_cursor()
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        self.0.borrow_mut().get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        self.0.borrow_mut().set_cursor_position(position)
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.0.borrow_mut().clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.0.borrow_mut().clear_region(clear_type)
    }

    fn size(&self) -> Result<Size, Self::Error> {
        self.0.borrow().size()
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        self.0.borrow_mut().window_size()
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.0.borrow_mut().flush()
    }
}

fn io_err(e: impl std::error::Error) -> io::Error {
    io::Error::other(e.to_string())
}

/// The terminal, with a live area of a height that can change.
struct Screen<B: Backend> {
    shared: Shared<B>,
    term: Terminal<Shared<B>>,
    height: u16,
}

impl<B: Backend> Screen<B> {
    fn new(shared: Shared<B>, height: u16) -> io::Result<Self> {
        let term = Self::make(&shared, height)?;
        Ok(Self { shared, term, height })
    }

    fn make(shared: &Shared<B>, height: u16) -> io::Result<Terminal<Shared<B>>> {
        Terminal::with_options(shared.clone(), TerminalOptions { viewport: Viewport::Inline(height) }).map_err(io_err)
    }

    fn width(&self) -> u16 {
        self.term.size().map_or(80, |s| s.width)
    }

    /// Give the live area `height` rows: clear the old one and start a new one where it began.
    fn set_height(&mut self, height: u16) -> io::Result<()> {
        if height != self.height {
            self.term.clear().map_err(io_err)?;
            self.term = Self::make(&self.shared, height)?;
            self.height = height;
        }
        Ok(())
    }

    /// Put lines into the scrollback, above the live area.
    fn push(&mut self, lines: &[Line<'static>]) -> io::Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        self.term.autoresize().map_err(io_err)?;
        let height = u16::try_from(lines.len()).unwrap_or(u16::MAX);
        self.term.insert_before(height, |buf| render::paint(lines, buf)).map_err(io_err)
    }

    fn draw(&mut self, status: &Status, input: &Input, theme: &Theme) -> io::Result<()> {
        self.term.draw(|f| render::draw_live(f, status, input, theme)).map_err(io_err)?;
        Ok(())
    }

    /// Take the live area away, leaving the cursor where it began, so the scrollback ends
    /// at the last thing said and the shell carries on below it.
    fn finish(&mut self) -> io::Result<()> {
        self.term.clear().map_err(io_err)?;
        self.term.show_cursor().map_err(io_err)
    }
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
    /// Lines for the scrollback, not yet put there.
    pending: Vec<Line<'static>>,
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

    fn say(&mut self, e: &Event, width: u16) {
        self.pending.extend(render::scrollback(e, &self.theme, width));
    }

    /// Something the worker said.
    fn msg(&mut self, m: Msg, width: u16) {
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
                self.say(&e, width);
            }
            Msg::Ask(ask, reply) => {
                self.pending.extend(render::approval(&ask, &self.theme, width));
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

    fn key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
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
            _ => {}
        }
    }
}

/// Run the interface until the person leaves. `header` and `notices` (a failed warm-up, say)
/// are the first things said.
pub fn run<B: Backend, K: Keys>(
    backend: Shared<B>,
    keys: &mut K,
    theme: Theme,
    header: Option<Event>,
    notices: Receiver<String>,
    worker: Worker,
) -> io::Result<()> {
    let mut screen = Screen::new(backend, 2)?;
    let Worker { prompts, events, stopper, handle } = worker;
    let mut app = App {
        input: Input::default(),
        phase: Phase::Idle,
        theme,
        stopper,
        prompts,
        stop_asked: false,
        quit: false,
        pending: Vec::new(),
    };
    let width = screen.width();
    if let Some(h) = header {
        app.say(&h, width);
    }

    while !app.quit {
        let width = screen.width();
        loop {
            match events.try_recv() {
                Ok(m) => app.msg(m, width),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    app.say(&Event::Error("the session ended unexpectedly".into()), width);
                    app.quit = true;
                    break;
                }
            }
        }
        while let Ok(notice) = notices.try_recv() {
            app.say(&Event::Error(notice), width);
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

        screen.push(&std::mem::take(&mut app.pending))?;
        screen.set_height(render::live_height(&app.input, screen.width()))?;
        screen.draw(&app.status(), &app.input, &app.theme)?;
    }

    screen.push(&std::mem::take(&mut app.pending))?;
    screen.finish()?;
    // Nobody is waiting on a prompt any more; an idle worker ends with its channel.
    drop(app);
    let _ = handle.join();
    Ok(())
}

// ── the real terminal ───────────────────────────────────────────────────────

/// Put the terminal back: raw mode off, paste mode off, the cursor shown.
fn restore() {
    let _ = crossterm::terminal::disable_raw_mode();
    let mut out = io::stdout();
    let _ = crossterm::execute!(out, crossterm::event::DisableBracketedPaste, crossterm::cursor::Show);
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
    // Not every terminal has bracketed paste; those that do not send a paste as keys.
    let _ = crossterm::execute!(io::stdout(), crossterm::event::EnableBracketedPaste);
    let backend = Shared::new(CrosstermBackend::new(io::stdout()));
    run(backend, &mut Crossterm, theme, Some(header), notices, worker)
}
