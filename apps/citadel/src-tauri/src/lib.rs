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

use serde::Serialize;
use tauri::ipc::Channel;

use odin_core::chat::{self, Message, Options, StreamChunk};
use odin_core::{config, ollama, vram};

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
    is_default: bool,
}

/// What is installed, for the picker.
///
/// **No capabilities here.** Whether a model can see images or call tools comes
/// from `/api/show`, one round trip per model, and belongs to G2's labelled
/// picker rather than to this list.
#[tauri::command]
async fn models() -> Result<Vec<ModelInfo>, String> {
    let (installed, cfg) = tauri::async_runtime::spawn_blocking(|| (ollama::tags(), config::load()))
        .await
        .map_err(|e| format!("listing models failed: {e}"))?;

    Ok(installed?
        .into_iter()
        .map(|m| ModelInfo {
            is_default: m.name == cfg.model,
            name: m.name,
            size: m.size,
            parameter_size: m.details.parameter_size,
            quantization: m.details.quantization_level,
        })
        .collect())
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![send, models, card])
        .run(tauri::generate_context!())
        .expect("error while running Citadel");
}
