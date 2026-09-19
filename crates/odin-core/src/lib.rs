//! What Warden's CLI and its app both need to know about the model's power state.
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
//!
//! Nothing here prints or panics. Callers format, and a tray has no stdout.

pub mod chat;
pub mod config;
pub mod ollama;
pub mod vram;
