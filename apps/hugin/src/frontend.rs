//! The turn loop's [`Frontend`], over a [`Ui`]: what the loop says becomes [`Event`]s,
//! and its questions become [`Ui::approve`].
//!
//! **A card is one line, shown when it ends.** The loop reports a card when it goes up
//! and again as it changes; a terminal wants one line per call, so cards are kept here
//! and the line is made when one reaches an end. A card that is waiting for a person is
//! shown once, as its approval, and its line comes after.

use bifrost_core::agent::send::Calibration;
use bifrost_core::agent::ticket::PendingCall;
use bifrost_core::agent::turn::{Card, CardState, Decision, Frontend, Patch, RoundNumbers};

use crate::stop::Stopper;
use crate::ui::{Event, Ui};
use crate::view;

pub struct Fe<'a> {
    ui: &'a mut dyn Ui,
    stopper: &'a Stopper,
    /// Every card of the turn, by the position the loop was given.
    cards: Vec<Card>,
    /// What the next round is calibrated from: the last round that was measured.
    pub calibration: Option<Calibration>,
    /// What the model last said, which the session adds to the conversation — the loop
    /// leaves its final answer out of it.
    pub answer: Option<String>,
}

impl<'a> Fe<'a> {
    pub fn new(ui: &'a mut dyn Ui, stopper: &'a Stopper) -> Self {
        Self { ui, stopper, cards: Vec::new(), calibration: None, answer: None }
    }

    /// The card's line, or the task list for `update_plan`.
    fn show(&mut self, card: usize) {
        let card = &self.cards[card];
        let event = match &card.plan {
            Some(plan) => Event::Plan(plan.clone()),
            None => Event::Tool(view::tool_line(card)),
        };
        self.ui.show(event);
    }
}

impl Frontend for Fe<'_> {
    fn stopped(&self) -> bool {
        self.stopper.stopped()
    }

    fn status(&mut self, text: &str) {
        self.ui.show(Event::Status(text.to_string()));
    }

    fn round(&mut self, numbers: RoundNumbers) {
        if numbers.prompt_eval_count > 0 {
            self.calibration = Some(Calibration::new(numbers.prompt_chars, numbers.prompt_eval_count));
        }
    }

    fn answer(&mut self, content: &str, _prompt_eval_count: u32, _eval_count: u32) {
        if content.trim().is_empty() {
            return;
        }
        self.answer = Some(content.to_string());
        self.ui.show(Event::Answer(content.to_string()));
    }

    fn error(&mut self, message: &str) {
        self.ui.show(Event::Error(message.to_string()));
    }

    fn card(&mut self, card: Card) -> usize {
        let waiting = card.state == CardState::Pending;
        self.cards.push(card);
        let at = self.cards.len() - 1;
        if !waiting {
            self.show(at);
        }
        at
    }

    fn update(&mut self, card: usize, patch: Patch) {
        let shown = &mut self.cards[card];
        if let Some(state) = patch.state {
            shown.state = state;
        }
        if patch.ok.is_some() {
            shown.ok = patch.ok;
        }
        if patch.result.is_some() {
            shown.result = patch.result;
        }
        if patch.error.is_some() {
            shown.error = patch.error;
        }
        if patch.undo.is_some() {
            shown.undo = patch.undo;
        }
        if matches!(shown.state, CardState::Ran | CardState::Failed | CardState::Denied | CardState::Stopped) {
            self.show(card);
        }
    }

    fn decide(&mut self, card: usize, call: &PendingCall) -> Decision {
        let approval = view::approval(&self.cards[card], call);
        self.ui.approve(&approval)
    }

    /// Nothing to do: the loop cannot lend the conversation, and the session saves it
    /// when the turn ends, which is where every Stop ends up.
    fn save(&mut self) {}
}
