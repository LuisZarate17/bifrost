//! The parts of an agent turn that every frontend shares.
//!
//! A turn is the model asking for a tool, a person or a rule deciding, the tool
//! running and its result going back, round after round until the model answers.
//! Mimir drives it today; Hugin and the probes will drive it later. All of them
//! need the same rules — what a ticket allows, what Stop reaches, what is set
//! aside when the prompt outgrows the context — or the probes would measure
//! something other than what ships.
//!
//! - [`stop`] — the Stop counter: a number taken before something starts, and a
//!   Stop that reaches every number taken so far
//! - [`ticket`] — the single-use approval for one call, and the exact plan, file
//!   or command it hands back when spent
//! - [`run`] — running a call, for an approved card and for an automatic call alike
//! - [`send`] — setting old tool results aside from the prompt before it is sent,
//!   and saying which
//! - [`workspace`] — the project folder the config names, if it still exists
//!
//! **Moved here unchanged (I1).** The first commit of I1 lifted these out of
//! Mimir's backend, `apps/mimir/src-tauri/src/lib.rs`, with no change to what they
//! do or to the JSON the webview sees. The loop itself comes next: `run_turn`,
//! behind a `Frontend` trait that streams text, shows a card, asks for a decision
//! and says whether Stop was pressed.

pub mod run;
pub mod send;
pub mod stop;
pub mod ticket;

use crate::{config, edit};

/// The project folder, if one is chosen and still exists.
///
/// Opened fresh each time rather than cached: the folder can be renamed or
/// deleted while the app is open, and editing into a stale path would be worse
/// than saying editing is off.
pub fn workspace(cfg: &config::Config) -> Option<edit::Workspace> {
    cfg.workspace.as_deref().and_then(|p| edit::Workspace::open(p).ok())
}
