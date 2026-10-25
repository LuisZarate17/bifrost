//! The plain line mode: one prompt per line in, plain text out, no colors and no cursor
//! tricks. It is what runs when stdin or stdout is a pipe, so a script can drive Hugin
//! and read what it said, and it is the whole interface until the inline one arrives
//! behind the same [`Ui`].
//!
//! **Stdin is read on its own thread**, because a turn can sit in one request for a
//! minute and `/stop` has to reach it meanwhile. The thread sets the Stop flag the
//! moment `/stop` arrives during a turn; every other line is queued, to be an approval's
//! answer or the next prompt. It is generic over [`BufRead`] and [`Write`] so a test
//! can hand it text and read back what it printed.

use std::collections::VecDeque;
use std::io::{BufRead, Write};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use bifrost_core::agent::turn::Decision;
use bifrost_core::tools::{PlanStatus, PlanStep};

use crate::session::Session;
use crate::stop::Stopper;
use crate::ui::{Approval, Event, ToolLine, Ui};
use crate::view;

const PROMPT: &str = "› ";
const ASK: &str = "approve? [y/n/a] ";

/// How often a waiting approval looks to see whether it was stopped.
const POLL: Duration = Duration::from_millis(50);

#[derive(Default)]
struct Lines {
    queued: VecDeque<String>,
    /// Input has ended: nothing more will arrive.
    ended: bool,
    /// A `/stop` came while a prompt was still waiting here: that prompt's turn is
    /// stopped as it begins.
    stop_next: bool,
}

/// The lines stdin has delivered and nobody has used yet.
#[derive(Default)]
pub struct Queue {
    lines: Mutex<Lines>,
    arrived: Condvar,
}

/// What waiting for a line came to.
enum Next {
    Line(String),
    Ended,
    /// The wait was cut short by its `give_up`.
    GaveUp,
}

impl Queue {
    fn push(&self, line: String) {
        if let Ok(mut l) = self.lines.lock() {
            l.queued.push_back(line);
        }
        self.arrived.notify_all();
    }

    /// A `/stop` read from input. A turn that runs is stopped there and then; a turn whose
    /// prompt is still waiting here is stopped as it begins; with neither, it stops
    /// nothing and is dropped — kept, it would answer some later approval no.
    ///
    /// **Decided under the lock [`Queue::prompt`] hands a prompt out under**, so a `/stop`
    /// sent right behind its prompt cannot fall between the prompt leaving the queue and
    /// its turn beginning.
    fn stop(&self, stopper: &Stopper) {
        let Ok(mut l) = self.lines.lock() else { return };
        if stopper.running() {
            stopper.stop();
        } else if l.queued.iter().any(|q| !matches!(q.trim(), "" | "/exit")) {
            l.stop_next = true;
        }
    }

    /// The next prompt, or `None` to leave: `/exit`, or the end of input. A line ending
    /// in `\` goes on to the next, the two joined with a newline; empty lines are
    /// ignored. **Its turn begins here**, under the queue's lock (see [`Queue::stop`]).
    fn prompt(&self, stopper: &Stopper) -> Option<String> {
        let Ok(mut l) = self.lines.lock() else { return None };
        let mut text = String::new();
        loop {
            let Some(line) = l.queued.pop_front() else {
                if l.ended {
                    return (!text.is_empty()).then(|| begin(&mut l, stopper, text));
                }
                l = match self.arrived.wait(l) {
                    Ok(l) => l,
                    Err(_) => return None,
                };
                continue;
            };
            let line = line.trim_end();
            if text.is_empty() {
                match line.trim() {
                    "" | "/stop" => continue,
                    "/exit" => return None,
                    _ => {}
                }
            }
            match line.strip_suffix('\\') {
                Some(more) => {
                    text.push_str(more);
                    text.push('\n');
                }
                None => {
                    text.push_str(line);
                    return Some(begin(&mut l, stopper, text));
                }
            }
        }
    }

    fn end(&self) {
        if let Ok(mut l) = self.lines.lock() {
            l.ended = true;
        }
        self.arrived.notify_all();
    }

    /// The next line, waiting for one. `give_up` is asked every [`POLL`] and, when it
    /// says so, ends the wait: that is how Stop reaches an approval.
    fn next(&self, give_up: impl Fn() -> bool) -> Next {
        let Ok(mut l) = self.lines.lock() else { return Next::Ended };
        loop {
            if let Some(line) = l.queued.pop_front() {
                return Next::Line(line);
            }
            if l.ended {
                return Next::Ended;
            }
            if give_up() {
                return Next::GaveUp;
            }
            l = match self.arrived.wait_timeout(l, POLL) {
                Ok((l, _)) => l,
                Err(_) => return Next::Ended,
            };
        }
    }
}

/// Read `input` on a thread of its own. **A `/stop` is never queued**: it stops the turn
/// that runs, or the one whose prompt is waiting ([`Queue::stop`]).
pub fn feed<R: BufRead + Send + 'static>(input: R, stopper: Stopper) -> Arc<Queue> {
    let queue = Arc::new(Queue::default());
    let reading = queue.clone();
    std::thread::spawn(move || {
        for line in input.lines() {
            let Ok(line) = line else { break };
            if line.trim() == "/stop" {
                reading.stop(&stopper);
            } else {
                reading.push(line);
            }
        }
        reading.end();
    });
    queue
}

pub struct Line<W: Write> {
    out: W,
    queue: Arc<Queue>,
    stopper: Stopper,
    /// Nobody is typing at a terminal, so what is read is not shown by the terminal and
    /// is repeated here, ending the line the prompt left open. At a terminal the person
    /// has just typed it, and repeating it would print everything twice.
    echo: bool,
    /// The status last shown, so one that is said twice is shown once.
    status: String,
}

impl<W: Write> Line<W> {
    pub fn new(out: W, queue: Arc<Queue>, stopper: Stopper, echo: bool) -> Self {
        Self { out, queue, stopper, echo, status: String::new() }
    }

    /// Write and flush. A pipe closed by the reader is not worth stopping for.
    fn say(&mut self, text: &str) {
        let _ = self.out.write_all(text.as_bytes());
        let _ = self.out.flush();
    }

    /// The next prompt, or `None` to leave; see [`Queue::prompt`]. Its turn has begun
    /// when this returns.
    pub fn read_prompt(&mut self) -> Option<String> {
        self.say(PROMPT);
        self.queue.prompt(&self.stopper)
    }

    /// What `/stop` or end of input at an approval comes to: no, and the turn ends.
    fn refuse(&mut self) -> Decision {
        self.stopper.stop();
        Decision::Deny
    }
}

impl<W: Write> Ui for Line<W> {
    fn show(&mut self, e: Event) {
        match e {
            Event::Header { folder, branch, changed, model, shell } => {
                self.say(&format!("{}\n", view::header(&folder, branch.as_deref(), changed, &model, &shell)));
            }
            Event::Allowed(a) => {
                let mut shown = card_text(&a);
                shown.push_str(&format!("  {}\n", view::ALLOWED_NOTE));
                self.say(&shown);
            }
            Event::Prompt(text) => {
                if self.echo {
                    self.say(&format!("{text}\n"));
                }
            }
            Event::Status(text) => {
                if text.is_empty() {
                    self.status.clear();
                } else if text != self.status {
                    self.say(&format!("… {text}\n"));
                    self.status = text;
                }
            }
            Event::Tool(ToolLine { name, target, outcome }) => {
                let target = if target.is_empty() { String::new() } else { format!(" {target}") };
                let outcome = if outcome.is_empty() { String::new() } else { format!(" → {outcome}") };
                self.say(&format!("● {name}{target}{outcome}\n"));
            }
            Event::Plan(steps) => {
                let lines: String = steps.iter().map(|s| format!("{}\n", plan_line(s))).collect();
                self.say(&lines);
            }
            Event::Answer(text) => self.say(&format!("{text}\n")),
            Event::Error(text) => self.say(&format!("! {text}\n")),
            Event::Unfinished(text) => self.say(&format!("✗ {text}\n")),
            Event::Stopped => self.say("· stopped\n"),
            Event::TurnEnd => {}
        }
    }

    fn approve(&mut self, a: &Approval) -> Decision {
        self.say(&card_text(a));
        loop {
            self.say(ASK);
            let stopper = self.stopper.clone();
            let answer = match self.queue.next(|| stopper.stopped()) {
                Next::Line(line) => line,
                Next::GaveUp | Next::Ended => {
                    if self.echo {
                        self.say("\n");
                    }
                    return self.refuse();
                }
            };
            if self.echo {
                self.say(&format!("{answer}\n"));
            }
            match answer.trim().to_lowercase().as_str() {
                "y" | "yes" => return Decision::Allow,
                "n" | "no" => return Decision::Deny,
                "a" => return Decision::AllowForSession,
                "/stop" => return self.refuse(),
                _ => {}
            }
        }
    }
}

/// An approval as text: what is asked, then every line to read before answering.
fn card_text(a: &Approval) -> String {
    let target = if a.target.is_empty() { String::new() } else { format!(" {}", a.target) };
    let mut shown = format!("? {}{target}\n", a.name);
    for line in &a.detail {
        shown.push_str(&format!("  {line}\n"));
    }
    shown
}

/// A prompt leaves the queue: its turn begins, stopped already if a `/stop` came for it.
fn begin(l: &mut Lines, stopper: &Stopper, text: String) -> String {
    stopper.begin();
    if std::mem::take(&mut l.stop_next) {
        stopper.stop();
    }
    text
}

/// `[x] step` for one that is done, `[ ] step` for the others.
fn plan_line(step: &PlanStep) -> String {
    let mark = if step.status == PlanStatus::Done { 'x' } else { ' ' };
    format!("[{mark}] {}", step.text)
}

/// Run the session in line mode until `/exit` or the end of input. Anything `notices`
/// holds, such as a failed warm-up, is shown before the next prompt.
pub fn run<R: BufRead + Send + 'static, W: Write>(
    input: R,
    out: W,
    session: &mut Session,
    header: Event,
    notices: Receiver<String>,
    echo: bool,
) {
    let queue = feed(input, session.stopper());
    let mut ui = Line::new(out, queue, session.stopper(), echo);
    ui.show(header);
    loop {
        while let Ok(notice) = notices.try_recv() {
            ui.show(Event::Error(notice));
        }
        let Some(prompt) = ui.read_prompt() else { return };
        session.turn(&prompt, &mut ui);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `/stop` that the reading thread sees before the prompt it follows has been taken
    /// stops that prompt's turn as it begins; one with nothing waiting stops nothing, and
    /// is not kept to answer a later approval (review I2, finding 2).
    #[test]
    fn a_stop_before_its_prompt_is_taken_stops_that_turn_and_a_stray_one_is_dropped() {
        let (queue, stopper) = (Queue::default(), Stopper::default());
        queue.push("fix it".into());
        queue.stop(&stopper);
        assert!(!stopper.stopped(), "nothing runs yet");
        assert_eq!(queue.prompt(&stopper).as_deref(), Some("fix it"));
        assert!(stopper.running() && stopper.stopped(), "the turn begins stopped");
        // The session begins it again; the Stop holds.
        stopper.begin();
        assert!(stopper.stopped());
        stopper.end();

        // Idle, nothing waiting: dropped, so the next turn is not stopped by it.
        queue.stop(&stopper);
        queue.push("next".into());
        assert_eq!(queue.prompt(&stopper).as_deref(), Some("next"));
        assert!(!stopper.stopped());
        assert!(queue.lines.lock().unwrap().queued.is_empty(), "no `/stop` was queued");
    }
}
