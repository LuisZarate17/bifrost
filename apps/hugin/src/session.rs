//! One Hugin session: the conversation as the model has seen it, run turn by turn.
//!
//! The model, the runner and the saver are passed in, so a test can script all three
//! and drive a whole session without a network, a model or a disk. [`Session::real`]
//! wires the real ones.

use std::collections::HashSet;
use std::time::Duration;

use bifrost_core::agent::env::Env;
use bifrost_core::agent::round::round_in;
use bifrost_core::agent::run::run_approved_in;
use bifrost_core::agent::send::Calibration;
use bifrost_core::agent::turn::{self, run_turn_with};
use bifrost_core::chat::Message;
use bifrost_core::check::Shell;
use bifrost_core::store::{self, StoredMessage};
use bifrost_core::{config, tools};

use crate::frontend::Fe;
use crate::stop::Stopper;
use crate::ui::{Event, Ui};

/// What a session's conversations are filed under in the store, so Mimir's sidebar
/// does not list them.
pub const APP: &str = "hugin";

/// Saves the session's record; [`store::save`] is the real one.
pub type Saver = dyn FnMut(&store::Conversation) -> Result<(), String>;

pub struct Session {
    model: String,
    /// The conversation as the model has seen it, including what was set aside.
    convo: Vec<Message>,
    /// What the next round is calibrated from.
    calibration: Option<Calibration>,
    /// The file the session is kept in, and how much of `convo` is already in it.
    record: store::Conversation,
    stored: usize,
    ask: Box<turn::Ask<'static>>,
    run: Box<turn::Run<'static>>,
    save: Box<Saver>,
    stopper: Stopper,
    /// The shell the model's commands run in.
    shell: Shell,
    /// The commands allowed for the rest of the session, each the exact string.
    allowed: HashSet<String>,
}

impl Session {
    pub fn new(
        model: String,
        ask: Box<turn::Ask<'static>>,
        run: Box<turn::Run<'static>>,
        save: Box<Saver>,
    ) -> Self {
        Self {
            model,
            convo: Vec::new(),
            calibration: None,
            record: store::Conversation::for_app(APP),
            stored: 0,
            ask,
            run,
            save,
            stopper: Stopper::default(),
            shell: Shell::PowerShell,
            allowed: HashSet::new(),
        }
    }

    /// The shell commands run in, named in their approvals.
    pub fn with_shell(mut self, shell: Shell) -> Self {
        self.shell = shell;
        self
    }

    /// A session that asks the model and runs the tools for real, in `root`, offering the
    /// model commands in `shell`.
    pub fn real(model: String, root: String, shell: Shell) -> Self {
        let asked = model.clone();
        let ask = move |messages: &[Message], calibration: Option<Calibration>, closing: bool| {
            // Loaded fresh each round, as the app does, so a change to the config
            // applies to the next one. The folder is set in memory only and never
            // saved: it is Hugin's own, not the one Mimir has chosen. Hugin always reads
            // the folder's project notes, so those are switched on the same way.
            let mut cfg = config::load();
            cfg.workspace = Some(root.clone());
            cfg.set_instructions(&root, true);
            round_in(messages, &asked, &cfg, &env(shell), calibration, closing)
        };
        // Read when the command runs, so a change to the time limit applies to the next one.
        let run = move |name: &str, path: &str, token: &str| run_approved_in(name, path, token, &env(shell));
        Self::new(model, Box::new(ask), Box::new(run), Box::new(store::save)).with_shell(shell)
    }

    /// The handle that stops the running turn, for whatever reads the keyboard.
    pub fn stopper(&self) -> Stopper {
        self.stopper.clone()
    }

    /// The conversation as the model has seen it.
    #[cfg(test)]
    pub fn conversation(&self) -> &[Message] {
        &self.convo
    }

    /// Run one turn on `prompt`. **Nothing here ends the session**: a turn that fails
    /// is shown as an error and the next prompt is asked for as usual.
    pub fn turn(&mut self, prompt: &str, ui: &mut dyn Ui) {
        self.stopper.begin();
        ui.show(Event::Prompt(prompt.to_string()));
        self.convo.push(Message::user(prompt));

        let mut fe = Fe::new(ui, &self.stopper, self.shell, &mut self.allowed);
        let result = run_turn_with(
            &mut self.convo,
            self.calibration,
            tools::MAX_ROUNDS,
            &mut *self.ask,
            &mut *self.run,
            &mut fe,
        );
        let (calibration, answer) = (fe.calibration, fe.answer);
        self.stopper.end();
        let stopped = self.stopper.stopped();

        if calibration.is_some() {
            self.calibration = calibration;
        }
        // The loop leaves the final answer out of the conversation, as the webview's
        // loop did and left it to the window to add. Without it the next turn would
        // begin with the model unable to see what it said.
        if let Some(text) = answer {
            self.convo.push(Message {
                role: "assistant".into(),
                content: text,
                images: None,
                tool_calls: None,
                tool_name: None,
            });
        }

        // A Stop pressed meanwhile is the reason the turn ended, and what it returned is
        // not news.
        match result {
            Err(e) if !stopped => ui.show(Event::Error(e)),
            _ => {}
        }
        if stopped {
            ui.show(Event::Stopped);
        }
        // A failed request leaves `Starting Ollama…` up.
        ui.show(Event::Status(String::new()));
        if let Err(e) = self.save() {
            ui.show(Event::Error(format!("could not save the session: {e}")));
        }
        ui.show(Event::TurnEnd);
    }

    /// Bring the record up to date with the conversation and write it. Saved after
    /// every turn, so a crash or a closed window costs the turn in progress and no more.
    fn save(&mut self) -> Result<(), String> {
        for m in &self.convo[self.stored..] {
            self.record.messages.push(stored(m, &self.model));
        }
        self.stored = self.convo.len();
        if self.record.title.is_empty() {
            self.record.title = self.record.display_title();
        }
        self.record.updated_at = store::stamp();
        (self.save)(&self.record)
    }
}

/// Who Hugin says it is: put before the system prompt, because the model otherwise
/// answers as the assistant it was trained as. `hugin -p` says the same.
pub const HUGIN_IDENTITY: &str = "You are Hugin, a coding agent running in the user's terminal on their own computer. If asked your name, say Hugin. Answer greetings and simple questions directly, without tools.";

/// The setting Hugin's rounds and runs are done in: the app's, with commands offered in
/// `shell` and the config's time limit for them.
fn env(shell: Shell) -> Env {
    Env {
        identity: Some(HUGIN_IDENTITY.into()),
        commands: Some(shell),
        command_timeout: Duration::from_secs(config::load().command_timeout_secs),
        ..Env::default()
    }
}

/// A message as the store keeps it. The tool calls go in as raw JSON, as the store's
/// own docs ask, so a file still parses when Ollama's shape gains a field.
fn stored(m: &Message, model: &str) -> StoredMessage {
    StoredMessage {
        role: m.role.clone(),
        content: m.content.clone(),
        images: Vec::new(),
        tool_calls: m.tool_calls.iter().flatten().filter_map(|c| serde_json::to_value(c).ok()).collect(),
        model: if m.role == "assistant" { model.to_string() } else { String::new() },
        created_at: store::stamp(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_setting_carries_the_identity() {
        let env = env(Shell::PowerShell);
        assert_eq!(env.identity.as_deref(), Some(HUGIN_IDENTITY));
        assert!(env.commands.is_some());
    }
}
