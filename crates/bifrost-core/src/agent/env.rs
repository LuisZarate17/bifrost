//! What a turn takes from where it runs, beyond the config.
//!
//! The app's turn asks the model at Ollama's own sampling, through the recovery for
//! calls Ollama cannot read, offering the tools and the system prompt its folder
//! implies, and sets old results aside when the prompt outgrows the context. A probe
//! has to vary some of that — it samples at temperature 0, it offers the tool list a
//! comparison is about, it keeps its backups and its check's build output in its own
//! scratch — and it must do so **inside the same round**, not in a copy of it, or it
//! would measure a loop that does not ship.
//!
//! [`Env`] is the whole of that difference, and nothing else may differ: the rules
//! that decide a reply, what is run and what a ticket allows are not in it. **Its
//! [`Default`] is the app**, so [`super::round::round`] and [`super::run::run_approved`]
//! are the `_in` functions with `&Env::default()`, and Mimir does not know this exists.

use std::path::PathBuf;
use std::time::Duration;

use crate::chat;
use crate::check::Shell;

/// The setting a round and the running of an approved call are done in. Every field
/// has the app's value, which [`Env::default`] is made of.
#[derive(Debug, Clone)]
pub struct Env {
    /// Sampling for the request. `None` is the app's: Ollama's own, at the config's
    /// `num_ctx` ([`chat::Options::new`]). A probe passes its own — for a result that
    /// gets written down, [`chat::Options::measured`], temperature 0 — because a
    /// verdict that moves between runs is not a measurement.
    pub options: Option<chat::Options>,
    /// How a call travels. `true` is the app's, [`chat::chat_tools`]: native first, and
    /// a call Ollama rejects or the model writes as plain text is recovered. `false` is
    /// [`chat::chat`] alone, where a call Ollama cannot parse fails the turn — the edit
    /// probe's "native" transport, measured against the recovery.
    pub recover: bool,
    /// The tools offered in place of [`crate::tools::available`]'s for the folder and
    /// its check, which is what `None` means. The closing request still offers none.
    ///
    /// **Offering `write_file` is what lets a round plan one**: the app never does, so
    /// without it a `write_file` call is turned away as the tool that does not exist.
    /// A probe offers the lists it compares — H8's reads, H9's tools left out, a
    /// whole-file rewrite in place of the edit.
    pub offered: Option<Vec<chat::Tool>>,
    /// The system prompt in place of [`crate::tools::system_prompt`] and the project's
    /// notes, which is what `None` means — and that one is added only when there is a
    /// folder. **`Some` is added even without one**: a probe words its own prompt, and
    /// the conversation it hands over holds none.
    pub system: Option<String>,
    /// Whether old results are set aside before a request is sent (H7). `true` is the
    /// app's. `false` sends the whole conversation every round
    /// ([`super::send::send_whole`]), which measures the loop as it was before H7:
    /// Ollama's cut when the prompt overflows, and nothing else.
    pub compact: bool,
    /// Where an approved edit's backups go. `None` is the app's
    /// ([`crate::edit::backups_dir`]); a probe points it at its own scratch folder, so
    /// that a run's edits are never written into the person's data.
    pub backups: Option<PathBuf>,
    /// Added to a check's environment, on top of what it inherits. A probe sets a shared
    /// `CARGO_TARGET_DIR`, so that no run rebuilds from nothing, and `CARGO_NET_OFFLINE`,
    /// so that none can reach the network. The app adds nothing.
    pub check_env: Vec<(String, String)>,
    /// The shell Hugin's `run_command` runs in. `Some` adds the tool to what is offered
    /// by default and its sentence to the app's own system prompt, and lets a call to
    /// it be asked about; `None` is the app's — Mimir never offers it, and a call to
    /// it is turned away as the tool that does not exist.
    pub commands: Option<Shell>,
    /// How long a `run_command` may run before it is killed. The app's is the
    /// config's default; Hugin sets it from `command_timeout_secs`.
    pub command_timeout: Duration,
}

impl Default for Env {
    /// The app's: Ollama's sampling, the recovery on, the tools and the prompt of the
    /// folder, results set aside, backups in the app's data folder, nothing added to a
    /// check.
    fn default() -> Self {
        Self {
            options: None,
            recover: true,
            offered: None,
            system: None,
            compact: true,
            backups: None,
            check_env: Vec::new(),
            commands: None,
            command_timeout: Duration::from_secs(crate::config::DEFAULT_COMMAND_TIMEOUT_SECS),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mimir calls `round` and `run_approved`, which pass this. A field that stopped
    /// being the app's here would change the app without its code changing.
    #[test]
    fn the_default_is_the_app() {
        let env = Env::default();
        assert!(env.options.is_none() && env.offered.is_none() && env.system.is_none() && env.backups.is_none());
        assert!(env.recover && env.compact);
        assert!(env.check_env.is_empty());
        assert!(env.commands.is_none(), "Mimir must not offer run_command");
        assert_eq!(env.command_timeout, Duration::from_secs(120));
    }
}
