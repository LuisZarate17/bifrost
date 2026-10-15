//! The parts of an agent turn that every frontend shares.
//!
//! A turn is the model asking for a tool, a person or a rule deciding, the tool
//! running and its result going back, round after round until the model answers.
//! Mimir and the probes drive it today; Hugin will later. All of them
//! need the same rules — what a ticket allows, what Stop reaches, what is set
//! aside when the prompt outgrows the context — or the probes would measure
//! something other than what ships.
//!
//! - [`stop`] — the Stop counter: a number taken before something starts, and a
//!   Stop that reaches every number taken so far
//! - [`ticket`] — the single-use approval for one call, and the exact plan, file
//!   or command it hands back when spent
//! - [`run`] — running a call, for an approved card and for an automatic call alike,
//!   and the lock every write to the folder takes
//! - [`send`] — setting old tool results aside from the prompt before it is sent,
//!   and saying which
//! - [`env`](mod@env) — what a turn takes from where it runs, beyond the config: the sampling,
//!   the tools and system prompt, where backups go. The app's own is the default, so
//!   a probe can change what it measures with and nothing else
//! - [`round`] — one request to the model, and what it wants sorted into a
//!   [`round::Reply`]: an answer, a call to ask about, one already run, or one
//!   turned away
//! - [`turn`] — the loop over rounds, behind a [`turn::Frontend`] that shows a card,
//!   asks for a decision and says whether Stop was pressed
//! - [`workspace`] — the project folder the config names, if it still exists
//!
//! **Moved here unchanged (I1).** The first commit of I1 lifted the plain pieces out
//! of Mimir's backend, `apps/mimir/src-tauri/src/lib.rs`, with no change to what
//! they do or to the JSON the webview sees. [`round`] and [`turn`] are the second
//! commit's: the body of Mimir's old `ask_with_tools` command without the Tauri, and
//! the loop `App.tsx` ran over it. Mimir runs every tool turn through
//! [`turn::run_turn`] now, its window only showing what the loop says. The probes
//! (2c) run [`turn::run_turn_with`] over [`round::round_in`] and
//! [`run::run_approved_in`], which take an [`env::Env`]; `round` and `run_approved`
//! are those with the app's.

pub mod env;
pub mod round;
pub mod run;
pub mod send;
pub mod stop;
pub mod ticket;
pub mod turn;

use crate::{config, edit};

/// The project folder, if one is chosen and still exists.
///
/// Opened fresh each time rather than cached: the folder can be renamed or
/// deleted while the app is open, and editing into a stale path would be worse
/// than saying editing is off.
pub fn workspace(cfg: &config::Config) -> Option<edit::Workspace> {
    cfg.workspace.as_deref().and_then(|p| edit::Workspace::open(p).ok())
}
