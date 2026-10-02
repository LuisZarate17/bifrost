//! What Heimdall's CLI and its app both need to know about the model's power state.
//!
//! Four modules, and the split is about what each one is the authority on:
//!
//! - [`ollama`] — what Ollama *believes* is loaded, and `/api/generate`
//! - [`vram`] — what the card is *actually* holding. PLAN.md §6b requires this
//!   separately because the first is not proof: measured at C4a, the card read
//!   8,773 MiB while Ollama claimed 6,233 for the same model
//! - [`config`] — the settings all three binaries share, and the built-in
//!   defaults a missing config falls back to
//! - [`chat`] — `/api/chat`: messages with roles, and the tool calls that ride
//!   on them. Added at G0, because everything Stage G builds is a field on a
//!   message and `/api/generate` has nowhere to put one
//! - [`store`] — conversations on disk. A file format, kept deliberately
//!   separate from `chat`'s wire format so a change to one is not silently a
//!   migration of the other
//! - [`tools`] — the local tools the model may ask for, and the rule that
//!   nothing runs without an explicit per-call approval
//! - [`check`] — running the one check command chosen for the project folder,
//!   stoppable and with its output capped. Added at H1
//! - [`search`] — which files the project folder has, and which lines hold
//!   some text: inside the folder only, build output skipped, capped. Added at H2
//!
//! Nothing here prints or panics. Callers format, and a tray has no stdout.

pub mod chat;
pub mod check;
pub mod config;
pub mod edit;
pub mod ollama;
pub mod search;
pub mod store;
pub mod toolcall;
pub mod tools;
pub mod vram;
pub mod xmlcall;
