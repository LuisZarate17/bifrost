//! What Warden's CLI and its app both need to know about the model's power state.
//!
//! Two questions, two modules: what Ollama *believes* is loaded (`ollama`), and
//! what the card is *actually* holding (`vram`). PLAN.md §6b requires the second,
//! because the first is not proof — measured at C4a, the card read 8,773 MiB
//! while Ollama claimed 6,233 for the same model.
//!
//! Nothing here prints or panics. Callers format, and a tray has no stdout.

pub mod config;
pub mod ollama;
pub mod vram;
