//! Citadel — a local chat app sharing Warden's model.
//!
//! **Almost nothing from Warden's `lib.rs` belongs here.** That file is full of
//! Win32 work which exists only because the popup is a click-through overlay that
//! never takes focus: capture exclusion, `SW_SHOWNOACTIVATE`, a low-level mouse
//! hook, a tray icon, global shortcuts. Citadel is an ordinary window — focusable,
//! decorated, resizable, in the taskbar, visible on a screen share. Copying any of
//! that here would be a bug.
//!
//! What does carry over is the shape of the streaming: tokens go up through a
//! `Channel`, never as the command's return value.
//!
//! **The conversation lives in the frontend for now.** `send` takes the whole
//! message list rather than keeping turns in Rust state the way Warden does —
//! Warden's popup is stateless between summons, while this window owns its thread.
//! G4 is what moves storage into Rust properly.

use base64::prelude::*;
use serde::Serialize;
use tauri::ipc::Channel;

use odin_core::chat::{self, Message, Options, StreamChunk};
use odin_core::{config, ollama, store, vram};

/// One message pushed up to the UI.
///
/// Sent through a `Channel`, never returned from the command. **Tauri sends
/// exactly one IPC reply per `invoke()`**, so a command whose return value
/// carried the answer would buffer the whole thing — the tell is that the reply
/// arrives all at once instead of streaming. The `Result` carries success or
/// failure; the tokens come through here.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "event", content = "data")]
enum StreamEvent {
    /// Ollama was not running, so this request is paying to start it.
    StartingOllama,
    /// The model was not resident, so this request is paying to load it.
    ///
    /// Saying so is the whole point: an unexplained stall reads as broken, and a
    /// cold load is seconds.
    Warming,
    /// One fragment of the answer.
    Token { text: String },
    /// The final object, carrying Ollama's own metrics.
    ///
    /// `prompt_eval_count` is what the frontend budgets against — it is Ollama's
    /// own count of what the conversation actually cost, not an estimate.
    Done {
        prompt_eval_count: u32,
        eval_count: u32,
        total_ms: f64,
        ttft_ms: f64,
        load_ms: f64,
    },
}

/// Send the conversation and stream the reply back.
///
/// Takes every turn each time rather than appending to server-side state: the
/// frontend owns the thread, including the decision about what to drop when the
/// context fills. That keeps the trimming visible in one place instead of split
/// across two.
#[tauri::command]
async fn send(
    messages: Vec<Message>,
    model: Option<String>,
    on_event: Channel<StreamEvent>,
) -> Result<(), String> {
    // Every odin_core call goes off the async worker. It is blocking by design,
    // and calling it from an async command drops a tokio runtime inside an async
    // context and kills the worker — which already cost Warden a request that
    // hung forever on "Reading your screen…".
    let cfg = tauri::async_runtime::spawn_blocking(config::load)
        .await
        .unwrap_or_default();
    let model = model.unwrap_or(cfg.model);

    // Ollama has no autostart and does not survive a reboot (A1), so the first
    // request after a restart would otherwise fail rather than wait.
    let running = tauri::async_runtime::spawn_blocking(ollama::is_running)
        .await
        .unwrap_or(false);
    if !running {
        let _ = on_event.send(StreamEvent::StartingOllama);
        let started = tauri::async_runtime::spawn_blocking(ollama::ensure_running).await;
        if let Ok(Err(e)) = started {
            return Err(format!("could not start Ollama: {e}"));
        }
    }

    // Cold? Say so before the wait, not after. Checked rather than assumed — the
    // model may have been warmed from Warden's tray, from `odin warm`, or simply
    // not have expired yet.
    let tag = model.clone();
    let resident = tauri::async_runtime::spawn_blocking(move || {
        ollama::ps()
            .map(|m| ollama::find(&m, &tag).is_some())
            .unwrap_or(false)
    })
    .await
    .unwrap_or(false);
    if !resident {
        let _ = on_event.send(StreamEvent::Warming);
    }

    // The stream itself is blocking, so the whole loop runs off-thread and sends
    // into the Channel from there. A Channel is Send, so this is fine — and it is
    // why odin_core::chat::stream takes a callback rather than being async.
    let num_ctx = cfg.num_ctx;
    let channel = on_event.clone();
    tauri::async_runtime::spawn_blocking(move || {
        chat::stream(&model, &messages, &[], Options::new(num_ctx), |chunk| {
            match chunk {
                StreamChunk::Token(text) => channel
                    .send(StreamEvent::Token { text: text.to_string() })
                    .map_err(|e| format!("channel send failed: {e}"))?,
                StreamChunk::Done(reply) => channel
                    .send(StreamEvent::Done {
                        prompt_eval_count: reply.prompt_eval_count,
                        eval_count: reply.eval_count,
                        total_ms: reply.total.as_secs_f64() * 1000.0,
                        ttft_ms: reply.ttft.unwrap_or_default().as_secs_f64() * 1000.0,
                        load_ms: reply.load.as_secs_f64() * 1000.0,
                    })
                    .map_err(|e| format!("channel send failed: {e}"))?,
            }
            Ok(())
        })
        .map(|_| ())
    })
    .await
    .map_err(|e| format!("stream task failed: {e}"))?
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelInfo {
    name: String,
    size: u64,
    parameter_size: String,
    quantization: String,
    /// Whether this is the tag the shared config currently names.
    is_current: bool,
    /// Whether Ollama has a runner for it in memory right now.
    is_resident: bool,
    /// Resident size per Ollama, which is **not** the card's figure — the two
    /// disagreed by ~2,500 MiB at C4a. The status bar shows the card.
    resident_mib: u64,
    /// Declared by `/api/show`: some of `completion`, `vision`, `tools`.
    capabilities: Vec<String>,
    /// The most the model itself supports, usually far above what fits.
    max_context: Option<u64>,
    /// Set when `/api/show` could not be reached for this tag. The model is
    /// still listed and still selectable — an unreachable capability check
    /// should not hide a model that is plainly installed.
    error: Option<String>,
}

/// What is installed, what it can do, and what is loaded.
///
/// One `/api/tags` plus one `/api/show` per model, and one `/api/ps` — all on a
/// single blocking hop rather than a round trip per field.
///
/// **A failed `/api/show` does not drop the model from the list.** Capabilities
/// are a label; being installed is a fact. Losing the second because the first
/// failed would make the picker lie about what you have.
#[tauri::command]
async fn models() -> Result<Vec<ModelInfo>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let cfg = config::load();
        let installed = ollama::tags()?;
        let loaded = ollama::ps().unwrap_or_default();

        Ok(installed
            .into_iter()
            .map(|m| {
                let resident = ollama::find(&loaded, &m.name);
                let (detail, error) = match ollama::show(&m.name) {
                    Ok(d) => (Some(d), None),
                    Err(e) => (None, Some(e)),
                };
                ModelInfo {
                    is_current: m.name == cfg.model,
                    is_resident: resident.is_some(),
                    resident_mib: resident
                        .and_then(|r| r.size_vram)
                        .map(|b| b / 1_048_576)
                        .unwrap_or(0),
                    capabilities: detail.as_ref().map(|d| d.capabilities.clone()).unwrap_or_default(),
                    max_context: detail.as_ref().and_then(|d| d.max_context),
                    name: m.name,
                    size: m.size,
                    parameter_size: m.details.parameter_size,
                    quantization: m.details.quantization_level,
                    error,
                }
            })
            .collect())
    })
    .await
    .map_err(|e| format!("listing models failed: {e}"))?
}

/// Pin a model in VRAM.
///
/// **Loads at the config's `num_ctx`, and that is the whole point.** Ollama keys
/// a runner by the model *and* its context, so loading at one value and asking at
/// another tears the runner down and rebuilds it — measured 2026-09-19 at ~3s in
/// both directions, which is exactly the bug the shared config closed. Going
/// through `ollama::load` rather than rolling a fresh call is what keeps them
/// agreeing.
#[tauri::command]
async fn load_model(tag: String) -> Result<f64, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let cfg = config::load();
        ollama::load(&tag, cfg.num_ctx).map(|d| d.as_secs_f64())
    })
    .await
    .map_err(|e| format!("load failed: {e}"))?
}

/// Release a model and give the card its memory back.
#[tauri::command]
async fn unload_model(tag: String) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || ollama::unload(&tag))
        .await
        .map_err(|e| format!("unload failed: {e}"))?
}

/// Write the chosen model to the shared config.
///
/// **This is the switchboard part.** The config is the single source of truth for
/// "current model", so writing it here is what lets the CLI and (from G3) Warden
/// follow the choice without being told separately.
#[tauri::command]
async fn set_model(tag: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut cfg = config::load();
        cfg.model = tag;
        config::save(&cfg)
    })
    .await
    .map_err(|e| format!("saving the model choice failed: {e}"))?
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CardState {
    used_mib: u64,
    total_mib: u64,
    name: String,
    /// What the shared config says the current model is, and its context budget.
    model: String,
    num_ctx: u32,
}

/// What the card is *actually* holding.
///
/// Read through NVML rather than `/api/ps`, because the two disagree — measured
/// at C4a, the card read 8,773 MiB while Ollama claimed 6,233 for the same model.
/// PLAN.md §6b requires the card's own figure for exactly this reason.
#[tauri::command]
async fn card() -> Result<CardState, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let cfg = config::load();
        let card = vram::Card::open()?;
        let sample = card.sample()?;
        Ok(CardState {
            used_mib: sample.used_mib,
            total_mib: sample.total_mib,
            name: card.name.clone(),
            model: cfg.model,
            num_ctx: cfg.num_ctx,
        })
    })
    .await
    .map_err(|e| format!("reading the card failed: {e}"))?
}

// ── conversations ───────────────────────────────────────────────────────────
//
// Storage lives in odin_core::store, one JSON file per conversation beside
// config.json. These commands are a thin pass-through: the frontend still owns
// the thread while it is being written, and hands the whole thing back to be
// saved. That keeps the trimming decision in one place, which is the same
// reason `send` takes every turn.

#[tauri::command]
async fn conversations() -> Result<Vec<store::Summary>, String> {
    tauri::async_runtime::spawn_blocking(store::list)
        .await
        .map_err(|e| format!("listing conversations failed: {e}"))?
}

#[tauri::command]
async fn open_conversation(id: String) -> Result<store::Conversation, String> {
    tauri::async_runtime::spawn_blocking(move || store::load(&id))
        .await
        .map_err(|e| format!("opening the conversation failed: {e}"))?
}

/// Save, and hand back what was written.
///
/// Returns the conversation because the id and the derived title are decided
/// here — a new conversation arrives without an id, and the caller needs the one
/// it was given in order to keep saving to the same file.
#[tauri::command]
async fn save_conversation(conversation: store::Conversation) -> Result<store::Conversation, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut c = conversation;
        if c.id.is_empty() {
            c = store::Conversation { id: store::Conversation::new().id, ..c };
        }
        c.updated_at = store::stamp();
        store::save(&c)?;
        Ok(c)
    })
    .await
    .map_err(|e| format!("saving the conversation failed: {e}"))?
}

#[tauri::command]
async fn rename_conversation(id: String, title: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || store::rename(&id, &title))
        .await
        .map_err(|e| format!("renaming failed: {e}"))?
}

#[tauri::command]
async fn delete_conversation(id: String) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || store::delete(&id))
        .await
        .map_err(|e| format!("deleting failed: {e}"))?
}

// ── attachments ─────────────────────────────────────────────────────────────

/// Store an attachment and return the name to reference it by.
///
/// Takes base64 because that is what the webview already has: the window sets
/// `dragDropEnabled: false`, so a dropped file arrives as a browser `File` and
/// is read with `FileReader`. That keeps path handling out of Rust entirely -
/// nothing here ever sees, or trusts, a path from outside.
#[tauri::command]
async fn put_attachment(data: String, ext: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let bytes = BASE64_STANDARD
            .decode(data.as_bytes())
            .map_err(|e| format!("decoding attachment: {e}"))?;
        store::put_attachment(&bytes, &ext)
    })
    .await
    .map_err(|e| format!("saving the attachment failed: {e}"))?
}

/// Read a stored attachment back as base64, for redisplay and for re-sending.
#[tauri::command]
async fn get_attachment(name: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        store::get_attachment(&name).map(|b| BASE64_STANDARD.encode(b))
    })
    .await
    .map_err(|e| format!("reading the attachment failed: {e}"))?
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            send,
            models,
            card,
            conversations,
            open_conversation,
            save_conversation,
            rename_conversation,
            delete_conversation,
            put_attachment,
            get_attachment,
            load_model,
            unload_model,
            set_model
        ])
        .run(tauri::generate_context!())
        .expect("error while running Citadel");
}
