//! The seam between Hugin's session and whatever shows it.
//!
//! The session and the turn loop know nothing of a terminal: they say what happened as
//! [`Event`]s and, when a call needs a person, ask through [`Ui::approve`]. The plain
//! line mode ([`crate::line`]) is the first thing behind it; the inline interface comes
//! later behind the same trait, so nothing above this file changes when it does.

use bifrost_core::agent::turn::Decision;
use bifrost_core::tools::PlanStep;

/// One thing the person is told, in the order it happened.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// What Hugin learned on start: the folder it works in, where git stands there and
    /// the model it asks, and the shell its commands run in (`""` for none shown).
    /// `branch` and `changed` are `None` outside a repository.
    Header {
        folder: String,
        branch: Option<String>,
        changed: Option<usize>,
        model: String,
        shell: String,
    },
    /// The person's own words, echoed, so a transcript reads in order.
    Prompt(String),
    /// What the turn is waiting for; empty clears it.
    Status(String),
    /// A command the person allowed for the whole session with `a`, asked about before:
    /// its card is shown again, with a line saying it was not asked.
    Allowed(Approval),
    /// A tool call, in one line, once it has ended.
    Tool(ToolLine),
    /// The model's task list as it now stands.
    Plan(Vec<PlanStep>),
    /// The model's answer.
    Answer(String),
    /// Something went wrong, or a bound was reached.
    Error(String),
    /// The turn is over and its work is not done: the check it ran last still fails, whatever the
    /// model's last words said.
    Unfinished(String),
    /// The turn was stopped.
    Stopped,
    /// The turn is over and the next prompt may be asked for.
    TurnEnd,
}

/// One tool call as a line: `● search "x" → 6 hits`.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolLine {
    pub name: String,
    pub target: String,
    pub outcome: String,
}

/// A call that needs a yes, with what a person must see before giving it.
#[derive(Debug, Clone, PartialEq)]
pub struct Approval {
    pub name: String,
    pub target: String,
    /// The lines to read before answering: an edit's diff, a check's full command.
    pub detail: Vec<String>,
}

/// What shows a session to a person and brings their decisions back.
pub trait Ui {
    /// Show something. Never fails: a screen that cannot be written to has nothing
    /// better to do than carry on.
    fn show(&mut self, e: Event);

    /// Ask about a call and wait for the answer. **A Stop ends the wait** with
    /// [`Decision::Deny`], so the turn can end.
    fn approve(&mut self, a: &Approval) -> Decision;
}
