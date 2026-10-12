//! Mímir — a local chat app sharing Heimdall's model.
//!
//! **Almost nothing from Heimdall's `lib.rs` belongs here.** That file is full of
//! Win32 work which exists only because the popup is a click-through overlay that
//! never takes focus: capture exclusion, `SW_SHOWNOACTIVATE`, a low-level mouse
//! hook, a tray icon, global shortcuts. Mímir is an ordinary window — focusable,
//! decorated, resizable, in the taskbar, visible on a screen share. Copying any of
//! that here would be a bug.
//!
//! What does carry over is the shape of the streaming: tokens go up through a
//! `Channel`, never as the command's return value.
//!
//! **The conversation lives in the frontend for now.** `send` takes the whole
//! message list rather than keeping turns in Rust state the way Heimdall does —
//! Heimdall's popup is stateless between summons, while this window owns its thread.
//! G4 is what moves storage into Rust properly.

use std::sync::atomic::{AtomicU64, Ordering};

use base64::prelude::*;
use serde::Serialize;
use tauri::ipc::Channel;

use odin_core::chat::{self, Message, Options, StreamChunk};
use odin_core::{check, compact, config, edit, instructions, ollama, search, store, tools, vram};

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

/// Make sure Ollama is answering, starting it if not, before a request that needs it.
///
/// Ollama has no autostart and does not survive a reboot (A1), so the first
/// request after a restart would otherwise fail rather than wait. `send` passes its
/// channel so the window can say what the wait is; `ask_with_tools` has none (the
/// front end asks [`ollama_running`] first and names the wait itself), but starts
/// Ollama all the same, so a tool turn never depends on the front end for it.
async fn start_ollama(on_event: Option<&Channel<StreamEvent>>) -> Result<(), String> {
    let running = tauri::async_runtime::spawn_blocking(ollama::is_running)
        .await
        .unwrap_or(false);
    if !running {
        if let Some(on_event) = on_event {
            let _ = on_event.send(StreamEvent::StartingOllama);
        }
        let started = tauri::async_runtime::spawn_blocking(ollama::ensure_running).await;
        if let Ok(Err(e)) = started {
            return Err(format!("could not start Ollama: {e}"));
        }
    }
    Ok(())
}

/// Whether Ollama is answering right now, so the window can say "Starting Ollama…"
/// before a tool turn's first round instead of looking stuck. Only a hint: the turn
/// starts Ollama itself either way.
#[tauri::command]
async fn ollama_running() -> bool {
    tauri::async_runtime::spawn_blocking(ollama::is_running)
        .await
        .unwrap_or(false)
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
    let id = STREAMS_STARTED.fetch_add(1, Ordering::SeqCst) + 1;

    // Every odin_core call goes off the async worker. It is blocking by design,
    // and calling it from an async command drops a tokio runtime inside an async
    // context and kills the worker — which already cost Heimdall a request that
    // hung forever on "Reading your screen…".
    let cfg = tauri::async_runtime::spawn_blocking(config::load)
        .await
        .unwrap_or_default();
    let model = model.unwrap_or(cfg.model);

    start_ollama(Some(&on_event)).await?;

    // Cold? Say so before the wait, not after. Checked rather than assumed — the
    // model may have been warmed from Heimdall's tray, from `odin warm`, or simply
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

    // Stopped while Ollama was starting or the model was loading: never ask.
    if stopped(id) {
        return Ok(());
    }

    // The stream itself is blocking, so the whole loop runs off-thread and sends
    // into the Channel from there. A Channel is Send, so this is fine — and it is
    // why odin_core::chat::stream takes a callback rather than being async.
    let num_ctx = cfg.num_ctx;
    let channel = on_event.clone();
    tauri::async_runtime::spawn_blocking(move || {
        chat::stream(&model, &messages, &[], Options::new(num_ctx), |chunk| {
            // Returning an error is how chat::stream is cancelled: it drops the
            // response, the connection closes, and Ollama stops generating.
            if stopped(id) {
                return Err(STOPPED.to_string());
            }
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
        // A stop is what was asked for, not a failure to report.
        .or_else(|e| if e == STOPPED { Ok(()) } else { Err(e) })
    })
    .await
    .map_err(|e| format!("stream task failed: {e}"))?
}

// ── stopping an answer ──────────────────────────────────────────────────────

/// How many answers `send` has started, and how many of those have been
/// stopped — every answer numbered up to this one.
///
/// **Counters, not a flag.** A flag would have to be reset when the next answer
/// starts, and an old stream still waiting on its first token would then read
/// the reset and run on. With numbers, stopping marks only the answers that
/// already exist; the next one gets a higher number and cannot be caught by it.
static STREAMS_STARTED: AtomicU64 = AtomicU64::new(0);
static STREAMS_STOPPED: AtomicU64 = AtomicU64::new(0);

/// What the stream callback returns to cancel, so `send` can tell a stop from
/// a real error.
const STOPPED: &str = "stopped";

fn stopped(id: u64) -> bool {
    STREAMS_STOPPED.load(Ordering::SeqCst) >= id
}

/// A number on the Stop counter for something that has not started yet — a
/// check or a search waiting on its card. Taken when the card is shown, so a
/// Stop pressed between Allow and the start still reaches it.
fn next_stop() -> u64 {
    STREAMS_STARTED.fetch_add(1, Ordering::SeqCst) + 1
}

/// Stop the answer that is streaming now.
///
/// Takes effect at the next token. The window stops showing the answer at once
/// either way, so a model still loading — which cannot be interrupted — only
/// costs the load, not a wait.
#[tauri::command]
fn stop() {
    STREAMS_STOPPED.fetch_max(STREAMS_STARTED.load(Ordering::SeqCst), Ordering::SeqCst);
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
/// "current model", so writing it here is what lets the CLI and (from G3) Heimdall
/// follow the choice without being told separately.
#[tauri::command]
async fn set_model(tag: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        config::update(|cfg| {
            cfg.model = tag;
            // A pick made by hand ends any switch-back story outright: the user
            // just chose a model on purpose, and clearing the folder afterwards
            // must not second-guess that with an offer back to something else.
            cfg.model_before_coding = None;
        })
    })
    .await
    .map_err(|e| format!("saving the model choice failed: {e}"))?
}

/// `llama3.1` and `llama3.1:latest` name the same model to Ollama; a bare
/// comparison of the two strings calls them different, so the coding-model
/// offer and the switch-back would each think the model it cares about was
/// never installed, or never current.
fn same_tag(a: &str, b: &str) -> bool {
    a.strip_suffix(":latest").unwrap_or(a) == b.strip_suffix(":latest").unwrap_or(b)
}

/// An offer to switch between the current model and another one, shown once
/// a project folder is chosen or cleared.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelOffer {
    /// "coding" or "back" — which direction this offer switches.
    kind: &'static str,
    from: String,
    to: String,
}

/// Work out whether to offer a switch, from the config and what is installed.
///
/// A pure function on purpose: [`model_offer`] is the only caller that talks
/// to Ollama, and keeping the decision itself free of I/O is what makes the
/// cases in `mod tests` below checkable without one running.
fn offer_for(cfg: &config::Config, installed: &[String]) -> Option<ModelOffer> {
    let has = |tag: &str| installed.iter().any(|t| same_tag(t, tag));

    if cfg.workspace.is_some()
        && !cfg.coding_model.is_empty()
        && !same_tag(&cfg.model, &cfg.coding_model)
        && has(&cfg.coding_model)
    {
        return Some(ModelOffer { kind: "coding", from: cfg.model.clone(), to: cfg.coding_model.clone() });
    }

    if cfg.workspace.is_none() && !cfg.coding_model.is_empty() && same_tag(&cfg.model, &cfg.coding_model) {
        if let Some(prev) = cfg.model_before_coding.as_ref() {
            if !same_tag(prev, &cfg.coding_model) && has(prev) {
                return Some(ModelOffer { kind: "back", from: cfg.model.clone(), to: prev.clone() });
            }
        }
    }

    None
}

/// Whether to offer switching models, now that the folder just changed.
///
/// Ollama being unreachable is not an error here — it just means there is
/// nothing to offer yet, the same as no coding model being installed.
#[tauri::command]
async fn model_offer() -> Result<Option<ModelOffer>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let cfg = config::load();
        // Nothing could be offered even if every model were installed: leave
        // Ollama alone.
        let assumed = [cfg.coding_model.clone(), cfg.model_before_coding.clone().unwrap_or_default()];
        if offer_for(&cfg, &assumed).is_none() {
            return Ok(None);
        }
        // Right after a reboot Ollama is not running until something starts
        // it, and a folder is usually chosen before the first question —
        // measured 2026-10-02, when the offer silently never appeared. Start it
        // the way `send` does.
        let tags = ollama::tags().or_else(|_| ollama::ensure_running().and_then(|_| ollama::tags()));
        let Ok(tags) = tags else {
            return Ok(None);
        };
        let installed: Vec<String> = tags.into_iter().map(|m| m.name).collect();
        Ok(offer_for(&cfg, &installed))
    })
    .await
    .map_err(|e| format!("checking for a model switch failed: {e}"))?
}

/// Act on an offer the webview was shown.
///
/// **The offer is recomputed here, not trusted from the caller** — the
/// webview never sends a model tag, only which direction it clicked, so
/// nothing reaching the IPC boundary can choose what model to load.
///
/// The config is written *before* the load: Heimdall decides whether to
/// refuse by reading `cfg.model`, so this is what makes it refuse for the
/// whole switch rather than racing to reload the vision model while the
/// coding model is still coming up.
#[tauri::command]
async fn accept_model_offer(kind: String) -> Result<f64, String> {
    tauri::async_runtime::spawn_blocking(move || {
        // The network call comes first, with no config held: the config is read and
        // written in one step afterwards, so a change made while Ollama answered
        // is not written over.
        let installed: Vec<String> = ollama::tags()
            .map_err(|e| format!("could not reach Ollama: {e}"))?
            .into_iter()
            .map(|m| m.name)
            .collect();
        let (offer, num_ctx) = config::update(|cfg| {
            let offer = offer_for(cfg, &installed)
                .filter(|o| o.kind == kind)
                .ok_or_else(|| "That offer no longer applies.".to_string())?;
            if offer.kind == "coding" {
                cfg.model_before_coding = Some(offer.from.clone());
            } else {
                cfg.model_before_coding = None;
            }
            cfg.model = offer.to.clone();
            Ok::<_, String>((offer, cfg.num_ctx))
        })??;

        let _ = ollama::unload(&offer.from);
        ollama::load(&offer.to, num_ctx).map(|d| d.as_secs_f64()).map_err(|e| {
            format!("Switched to {}, but loading it failed: {e}. It will load with the next question.", offer.to)
        })
    })
    .await
    .map_err(|e| format!("switching models failed: {e}"))?
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

        // **Merge, never overwrite blindly.** The caller sends the messages it
        // has; the title and creation time belong to the file. Writing the
        // caller's empty strings straight through meant every new message
        // silently erased a conversation the user had renamed — and reset the
        // date the list sorts by.
        if let Ok(existing) = store::load(&c.id) {
            if c.title.trim().is_empty() {
                c.title = existing.title;
            }
            if c.created_at.is_empty() {
                c.created_at = existing.created_at;
            }
        }
        if c.created_at.is_empty() {
            c.created_at = store::stamp();
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

// ── tools ───────────────────────────────────────────────────────────────────

/// What the model asked to run, for the UI to show before anything happens.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PendingCall {
    name: String,
    /// For a read, the exact path that will be read. For an edit, the file
    /// relative to the project folder.
    path: String,
    /// A single-use ticket for exactly this call.
    ///
    /// **Without it `run_tool` would be a filesystem operation that takes
    /// arguments.** The approval would live entirely in the frontend, and
    /// `tools.rs` would be promising a guarantee Rust could not enforce. With
    /// it, the only thing the webview can do is redeem what it was shown.
    token: String,
    /// The diff, for an edit. Computed by the same plan [`edit::apply`] writes,
    /// so what is approved and what is written cannot disagree.
    #[serde(skip_serializing_if = "Option::is_none")]
    diff: Option<DiffView>,
    /// For a read of part of a file: the lines, as Rust normalised them. What the
    /// card shows is what the ticket holds — see [`Payload::Read`].
    #[serde(skip_serializing_if = "Option::is_none")]
    range: Option<tools::Lines>,
    /// For a check: build files an edit changed earlier in this turn. A check
    /// runs them, so the card names them before you approve it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    changed_build_files: Vec<String>,
    /// For a move: where the file goes, relative to the project folder, as the
    /// plan resolved it. The webview sends back only `(name, path, token)`; the
    /// destination is in the ticket's plan, so what is approved is what is moved.
    #[serde(skip_serializing_if = "Option::is_none")]
    to: Option<String>,
    /// For a create or a move: the folders it will make, shallowest first, each
    /// with a trailing slash.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    folders: Vec<String>,
    /// For a delete or a move: the size of the file, in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DiffView {
    hunks: Vec<edit::Hunk>,
    added: usize,
    removed: usize,
    create: bool,
}

/// A call that is waiting for a decision.
///
/// **For an edit, the whole plan lives here** — resolved path, exact bytes to
/// write, the hash of the file it was planned against. `run_tool` redeems the
/// ticket and applies *this*. Nothing about the edit is taken back from the
/// webview, so a compromised or buggy page cannot approve one thing and write
/// another. **For a check, the folder and the exact argv live here**, for the
/// same reason: what runs is what the card showed, even if the config changes
/// while it is on screen.
struct Armed {
    token: String,
    name: String,
    path: String,
    payload: Option<Payload>,
}

/// The tool name a plan belongs to. A ticket for one kind of change is never
/// redeemed as another: a delete's plan has no bytes to write, and a move's has
/// a destination an edit's does not.
fn plan_name(p: &edit::Plan) -> &'static str {
    match p.op {
        edit::Op::Edit => "edit_file",
        edit::Op::Create => "create_file",
        edit::Op::Delete => "delete_file",
        edit::Op::Move => "move_file",
    }
}

/// What an approval carries beyond its name and path.
#[derive(Debug, Clone, PartialEq)]
enum Payload {
    Edit(edit::Plan),
    /// A read that asked: the file the card showed, as `read_target` resolved
    /// it, and the lines the card showed. `run_tool` opens **this**, never a path
    /// or a range from the webview — so an approved `src/a.rs` cannot become a
    /// read of something else, nor lines 1–50 a read of the whole file.
    Read { real: std::path::PathBuf, range: Option<tools::Lines> },
    /// `stop` is this check's number on the Stop counter, taken **when the card
    /// is shown** rather than when the command starts: a Stop pressed between
    /// Allow and the start — while the approval is still on its way — must
    /// reach it too, or it would run with nothing left to stop it.
    Check { root: String, argv: Vec<String>, stop: u64 },
    /// A search or a listing: the folder it was approved for and exactly what
    /// the card showed, with a place on the Stop counter — a walk of a large
    /// folder can take a while, and Stop must reach it.
    Find { root: String, find: Find, stop: u64 },
}

/// What a finding call looks for.
#[derive(Debug, Clone, PartialEq)]
enum Find {
    List { pattern: Option<String> },
    Search { text: String, pattern: Option<String> },
}

/// The one call currently awaiting approval.
///
/// One slot, not a set: the UI shows a single approval card and waits, so a
/// second pending call would mean a bug rather than a queue. Issuing a new
/// ticket invalidates the previous one.
static PENDING: std::sync::OnceLock<std::sync::Mutex<Option<Armed>>> = std::sync::OnceLock::new();

fn pending() -> &'static std::sync::Mutex<Option<Armed>> {
    PENDING.get_or_init(|| std::sync::Mutex::new(None))
}

/// Issue a ticket for a call, replacing any outstanding one.
fn arm(name: &str, path: &str, payload: Option<Payload>) -> String {
    let token = format!(
        "{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    );
    if let Ok(mut slot) = pending().lock() {
        *slot = Some(Armed { token: token.clone(), name: name.into(), path: path.into(), payload });
    }
    token
}

/// The decision, without the global slot — so it is testable without one.
///
/// **The slot is taken either way.** A mismatched attempt burns the ticket
/// rather than leaving it for a second try, so a caller cannot guess repeatedly
/// against a standing approval.
fn redeem_in(
    slot: &mut Option<Armed>,
    token: &str,
    name: &str,
    path: &str,
) -> Result<Option<Payload>, String> {
    match slot.take() {
        Some(a) if a.token == token && a.name == name && a.path == path => Ok(a.payload),
        _ => Err("this call was not approved".to_string()),
    }
}

/// Spend a ticket. Fails unless it matches the exact call that was approved.
fn redeem(token: &str, name: &str, path: &str) -> Result<Option<Payload>, String> {
    let mut slot = pending().lock().map_err(|_| "approval state poisoned")?;
    redeem_in(&mut slot, token, name, path)
}

/// The project folder, if one is chosen and still exists.
///
/// Opened fresh each time rather than cached: the folder can be renamed or
/// deleted while the app is open, and editing into a stale path would be worse
/// than saying editing is off.
fn workspace(cfg: &config::Config) -> Option<edit::Workspace> {
    cfg.workspace.as_deref().and_then(|p| edit::Workspace::open(p).ok())
}

/// The previous round of a tool turn, as the webview remembers it: how many
/// characters were sent and how many tokens Ollama counted for them. It is what
/// turns "characters" into "tokens" for the next round's budget.
#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Calibration {
    chars: u64,
    tokens: u32,
}

/// What [`compact_for_send`] did, for the reply to carry.
#[derive(Debug, Clone, PartialEq)]
struct Sent {
    /// Characters actually sent, after anything was set aside.
    prompt_chars: u64,
    /// Indices into the webview's conversation, oldest first.
    set_aside: Vec<usize>,
    /// What each of those messages now is, in the same order, so the webview
    /// replaces them with exactly what was sent rather than re-deriving the stub.
    replacements: Vec<Message>,
    fits: bool,
}

impl Sent {
    fn into_fields(self) -> serde_json::Map<String, serde_json::Value> {
        let mut m = serde_json::Map::new();
        m.insert("promptChars".into(), self.prompt_chars.into());
        m.insert("setAside".into(), self.set_aside.into());
        m.insert("setAsideMessages".into(), serde_json::to_value(self.replacements).unwrap_or_default());
        m.insert("fits".into(), self.fits.into());
        m
    }
}

/// The webview's numbering for indices into what is sent. `lead` is how many
/// messages the backend put in front (the system prompt, when a folder is
/// chosen); an index inside them is never the webview's and is dropped.
fn frontend_indices(sent: &[usize], lead: usize) -> Vec<usize> {
    sent.iter().filter_map(|i| i.checked_sub(lead)).collect()
}

/// Set aside what will not fit, in `messages` as they are about to be sent.
fn compact_for_send(
    messages: &mut [Message],
    offered: &[chat::Tool],
    num_ctx: u32,
    calibration: Option<Calibration>,
    lead: usize,
) -> Sent {
    let budget = compact::Budget::new(num_ctx, calibration.map(|c| (c.chars, c.tokens)));
    let plan = compact::compact(messages, offered, &budget);
    let mut replacements = Vec::new();
    for &i in &plan.set_aside {
        // The call that produced a result sits just before it.
        let call = i.checked_sub(1).and_then(|p| messages[p].tool_calls.as_deref()?.first().cloned());
        compact::set_aside(&mut messages[i], call.as_ref());
        replacements.push(messages[i].clone());
    }
    // Anything in front of the webview's messages is never reported; `compact`
    // does not set the system prompt aside, so nothing is lost by the filter.
    let set_aside = frontend_indices(&plan.set_aside, lead);
    replacements.drain(..plan.set_aside.len() - set_aside.len());
    Sent {
        prompt_chars: compact::prompt_chars(messages, offered),
        set_aside,
        replacements,
        fits: plan.fits,
    }
}

/// Ask the model, offering the tools, and report back what it wants.
///
/// **What it runs itself is only what `tools::approval` calls automatic** — a
/// read inside the project folder of a file the listing would show, a listing, a
/// search (H5; `ask_every_call` in the config turns it off), and the task list
/// (`update_plan`, H10, which touches nothing and works even with no folder or
/// with `ask_every_call` on). Those come back as `"auto"`, already run. Everything
/// else it returns as an answer, a request to approve, or — for a request that
/// could never succeed, like an edit whose text is not in the file — a rejection,
/// which goes straight back to the model so it can read the file again and retry
/// without the user being asked about something impossible.
///
/// **`closing` is the last request of a turn that hit its bound (R-03).** It
/// offers no tools, adds [`tools::CLOSING_NOTE`] as one last user message, runs
/// nothing, and returns an `"answer"` whatever the reply holds — so the turn ends
/// in the model's own words about what was done. Ollama is still started and the
/// conversation still compacted, since it may be the largest request of the turn;
/// the note is added **after** compaction, so it is never in `setAside` and the
/// webview's copy stays a prefix of what was sent.
#[tauri::command]
async fn ask_with_tools(
    mut messages: Vec<Message>,
    model: Option<String>,
    calibration: Option<Calibration>,
    closing: Option<bool>,
) -> Result<serde_json::Value, String> {
    let closing = closing.unwrap_or(false);
    // Taken **before the model is asked**, for an automatic search or listing:
    // there is no card to take it at, and a Stop pressed while the model is
    // still answering must reach the walk that follows.
    let stop = next_stop();
    let cfg = tauri::async_runtime::spawn_blocking(config::load)
        .await
        .unwrap_or_default();
    let model = model.unwrap_or(cfg.model.clone());

    // The capability check below ignores an error, so without this a stopped Ollama
    // would pass it and fail later with a raw connection error — after a reboot,
    // with nothing else having started it.
    start_ollama(None).await?;

    // Offering tools to a model that cannot carry them wastes a round trip and
    // produces a confusing prose answer instead of a call. Better to say so.
    let tag = model.clone();
    let can = tauri::async_runtime::spawn_blocking(move || odin_core::ollama::show_cached(&tag))
        .await
        .map_err(|e| format!("capability check failed: {e}"))?;
    if let Ok(detail) = can {
        if !detail.has("tools") {
            return Err(format!("{model} cannot call tools — pick one with the tools badge"));
        }
    }

    let ws = tauri::async_runtime::spawn_blocking({
        let cfg = cfg.clone();
        move || workspace(&cfg)
    })
    .await
    .unwrap_or(None);
    // The folder's check command, if one was chosen for it. Read with the folder,
    // so a check can never be offered for a folder other than the one in force.
    let check: Option<Vec<String>> =
        ws.as_ref().and_then(|ws| cfg.check_for(&ws.root_display()).cloned());
    let check_label = check.as_deref().map(check::label);

    // The folder and the read-before-edit habit, stated once. Added here rather
    // than stored in the thread, so it always names the folder chosen *now*.
    //
    // **The project's notes ride in that same message** (H10), after the prompt, so
    // there is still exactly one system message and `lead` below is unchanged. Only
    // when the user turned them on for this folder, and read fresh every turn: an
    // edited file applies to the next question.
    if let Some(ws) = &ws {
        let mut prompt = tools::system_prompt(ws, check.is_some());
        if cfg.instructions_for(&ws.root_display()) {
            let notes = tauri::async_runtime::spawn_blocking({
                let ws = ws.clone();
                move || instructions::find(&ws)
            })
            .await
            .unwrap_or(None);
            if let Some(notes) = notes {
                prompt = tools::with_notes(prompt, &notes);
            }
        }
        messages.insert(0, Message::system(prompt));
    }

    // Kept for counting repeats: the history the model has already seen.
    let history = messages.clone();
    let num_ctx = cfg.num_ctx;
    // Nothing is offered to a closing request, and compaction counts what is sent.
    let offered = if closing { Vec::new() } else { tools::available(ws.is_some(), check_label.as_deref()) };

    // Inside a turn the conversation only grows, and past `num_ctx` Ollama cuts
    // the middle of it without a word (H7). So old results are set aside here,
    // in the copy being sent, and the webview is told which so that its own copy
    // ends up identical — a prefix that changed would also cost the prompt cache.
    let sent = compact_for_send(&mut messages, &offered, num_ctx, calibration, usize::from(ws.is_some()));
    if closing {
        messages.push(Message::user(tools::CLOSING_NOTE));
        // `chat`, not `chat_tools`: with no tools there is nothing for the recovery
        // to turn text into, and a call the model writes anyway stays words.
        let reply = tauri::async_runtime::spawn_blocking(move || {
            chat::chat(&model, &messages, &[], Options::new(num_ctx))
        })
        .await
        .map_err(|e| format!("closing request failed: {e}"))??;
        let mut body = serde_json::Map::new();
        body.insert("kind".into(), "answer".into());
        body.insert("content".into(), reply.message.content.into());
        body.insert("promptEvalCount".into(), reply.prompt_eval_count.into());
        body.insert("evalCount".into(), reply.eval_count.into());
        body.extend(sent.into_fields());
        return Ok(serde_json::Value::Object(body));
    }
    let reply = tauri::async_runtime::spawn_blocking(move || {
        chat::chat_tools(&model, &messages, &offered, Options::new(num_ctx))
    })
    .await
    .map_err(|e| format!("tool turn failed: {e}"))??;
    if reply.recovered {
        println!("[mimir] a malformed tool call was recovered through the raw retry");
    }

    // The reply is built inside a block so that every kind of it — answer, call,
    // auto, rejected, stuck — leaves through the one place that adds the prompt's
    // numbers, instead of each of a dozen returns remembering to.
    let mut outcome: Result<serde_json::Value, String> = async {
        // The first call only; a model that asks for several at once gets the rest
        // dropped and asks again. That was because each needed its own approval;
        // since H5 an automatic one does not, so several could run in one round.
        // Left as it is (review R-34): Qwen3-Coder asks for one call per reply.
        let Some(call) = reply.tool_calls().first().cloned() else {
            return Ok(serde_json::json!({
                "kind": "answer",
                "content": reply.message.content,
                "promptEvalCount": reply.prompt_eval_count,
                "evalCount": reply.eval_count,
            }));
        };

        // A call that has already failed identically, twice, ends the turn: the file
        // has not changed, so a third try cannot go differently, and each one costs
        // a round. Measured at G7 - the model once repeated one impossible edit
        // eight times running.
        let prior = tools::prior_failures(&history, &call);
        if prior + 1 >= tools::MAX_IDENTICAL_FAILURES {
            return Ok(serde_json::json!({
                "kind": "stuck",
                "error": format!(
                    "the model kept repeating a call to {} that could not be carried out, so it was stopped",
                    call.function.name
                ),
            }));
        }

        // Rejected without asking anyone: the reason goes back to the model. The raw
        // call goes with it, so the conversation shows the model its own attempt.
        //
        // **Every rejection starts `error:` (R-20)**: the unreadable call, the refusals of
        // `interpret`, the 7th plan update. The repeat rule counts only results that start
        // that way, and the probe's results do.
        let reject = |name: &str, path: &str, why: String| {
            let why = tools::failure(&why);
            let why = if prior > 0 { format!("{why}{}", tools::repeated_note(name)) } else { why };
            serde_json::json!({
                "kind": "rejected",
                "name": name,
                "path": path,
                "error": why,
                "rawCall": call,
            })
        };

        if call.function.name == chat::INVALID_TOOL_CALL {
            let why = call.function.arguments["error"].as_str().unwrap_or("unreadable").to_string();
            return Ok(reject("tool call", "", format!("{why}. Send the call again as valid JSON.")));
        }
        let request = match tools::interpret(&call) {
            Ok(r) => r,
            Err(e) => return Ok(reject(&call.function.name, request_path(&call), e)),
        };

        // A finding call that could never run — a pattern outside the folder, text
        // over several lines — goes back to the model with the reason, without
        // asking anyone to approve something that can only fail.
        if let (Some(ws), tools::Request::List { pattern } | tools::Request::Search { pattern, .. }) = (&ws, &request) {
            let text = match &request {
                tools::Request::Search { text, .. } => Some(text.as_str()),
                _ => None,
            };
            if let Err(e) = search::validate(ws, text, pattern.as_deref()) {
                return Ok(reject(request.name(), "", format!("error: {e}")));
            }
        }

        // The task list: at most 6 updates a turn, by the rule the probes share. The
        // 7th goes back to the model and costs a round like any rejected call.
        if matches!(request, tools::Request::Plan { .. }) && tools::plan_updates(&history) >= tools::MAX_PLAN_UPDATES {
            return Ok(reject("update_plan", "", tools::PLAN_LIMIT_NOTE.to_string()));
        }

        // What may run without a card. Decided here, once, by the one rule the probe
        // shares — and on a blocking thread, because it walks the folder.
        let decided = {
            let (ws, request, ask_every_call) = (ws.clone(), request.clone(), cfg.ask_every_call);
            tauri::async_runtime::spawn_blocking(move || tools::approval(ws.as_ref(), &request, ask_every_call))
                .await
                .map_err(|e| format!("deciding the call failed: {e}"))?
        };
        match decided {
            tools::Approval::Reject(why) => {
                return Ok(reject(request.name(), request.path(), format!("error: {why}")));
            }
            // Touches nothing, so there is nothing to run: the card is the list itself.
            // It is not a round (the webview does not count it), and works with no
            // folder or with `ask_every_call`, as `approval` says.
            tools::Approval::Auto(tools::Auto::Plan { steps }) => {
                return Ok(serde_json::json!({
                    "kind": "auto",
                    "name": "update_plan",
                    "path": "",
                    "range": null,
                    "result": tools::plan_message(&steps),
                    "ok": true,
                    "stopped": false,
                    "plan": steps,
                    "rawCall": call,
                }));
            }
            tools::Approval::Auto(auto) => {
                let ws = ws.clone().expect("a call is only automatic inside a project folder");
                let ran = tauri::async_runtime::spawn_blocking(move || run_auto(&ws, auto, stop))
                    .await
                    .map_err(|e| format!("running the call failed: {e}"))?;
                // `None`: the read could not be made safely, so it asks like any other.
                if let Some((name, path, range, mut ran)) = ran {
                    if !ran.ok && prior > 0 {
                        ran.result.push_str(tools::repeated_note(name));
                    }
                    // **Never `arm`s a ticket and never touches PENDING**: nothing
                    // here awaits a decision, so nothing may replace or leave one.
                    return Ok(serde_json::json!({
                        "kind": "auto",
                        "name": name,
                        "path": path,
                        "range": range,
                        "result": ran.result,
                        "ok": ran.ok,
                        "stopped": ran.stopped,
                        "rawCall": call,
                    }));
                }
            }
            tools::Approval::Ask => {}
        }

        let (name, path, payload) = match request {
            tools::Request::Read { path, range } => {
                let (open, shown) = tools::read_target(&path, ws.as_ref());
                ("read_file", shown, Some(Payload::Read { real: open, range }))
            }
            tools::Request::List { .. } | tools::Request::Search { .. } if ws.is_none() => {
                return Ok(reject(
                    request.name(),
                    "",
                    "error: there is no project folder to search — the user has not chosen one".into(),
                ));
            }
            tools::Request::List { pattern } => {
                let ws = ws.as_ref().expect("checked above");
                let shown = search::describe_list(pattern.as_deref());
                let find = Find::List { pattern };
                ("list_files", shown, Some(Payload::Find { root: ws.root_display(), find, stop: next_stop() }))
            }
            tools::Request::Search { text, pattern } => {
                let ws = ws.as_ref().expect("checked above");
                let shown = search::describe_search(&text, pattern.as_deref());
                let find = Find::Search { text, pattern };
                ("search", shown, Some(Payload::Find { root: ws.root_display(), find, stop: next_stop() }))
            }
            // The model can call it from history, or invent it. Either way there is
            // only something to run when a person chose a command for this folder.
            tools::Request::Check => match (&ws, &check) {
                (Some(ws), Some(argv)) => (
                    "run_check",
                    check::label(argv),
                    Some(Payload::Check { root: ws.root_display(), argv: argv.clone(), stop: next_stop() }),
                ),
                _ => {
                    return Ok(reject(
                        "run_check",
                        "",
                        format!("error: {}", tools::CHECK_UNAVAILABLE),
                    ));
                }
            },
            tools::Request::Plan { .. } => {
                return Ok(reject("update_plan", "", "error: the task list could not be read".into()));
            }
            tools::Request::Rewrite { path, .. } => {
                return Ok(reject("write_file", &path, "no such tool: write_file — use edit_file".into()));
            }
            tools::Request::Edit { .. }
            | tools::Request::Create { .. }
            | tools::Request::Delete { .. }
            | tools::Request::Move { .. }
                if ws.is_none() =>
            {
                return Ok(reject(
                    request.name(),
                    request.path(),
                    "editing is off: the user has not chosen a project folder".into(),
                ));
            }
            tools::Request::Edit { path, old_text, new_text } => {
                let ws = ws.clone().expect("checked above");
                let ask_every_call = cfg.ask_every_call;
                let planned = tauri::async_runtime::spawn_blocking(move || {
                    tools::plan_edit(&ws, &path, &old_text, &new_text, ask_every_call)
                })
                .await
                .map_err(|e| format!("planning the edit failed: {e}"))?;
                match planned {
                    Ok(p) => ("edit_file", p.display.clone(), Some(Payload::Edit(p))),
                    Err(e) => return Ok(reject("edit_file", request_path(&call), format!("error: {e}"))),
                }
            }
            tools::Request::Create { path, content } => {
                let ws = ws.clone().expect("checked above");
                let planned =
                    tauri::async_runtime::spawn_blocking(move || edit::plan_create(&ws, &path, &content))
                        .await
                        .map_err(|e| format!("planning the new file failed: {e}"))?;
                match planned {
                    Ok(p) => ("create_file", p.display.clone(), Some(Payload::Edit(p))),
                    Err(e) => return Ok(reject("create_file", request_path(&call), format!("error: {e}"))),
                }
            }
            tools::Request::Delete { path } => {
                let ws = ws.clone().expect("checked above");
                let planned = tauri::async_runtime::spawn_blocking(move || edit::plan_delete(&ws, &path))
                    .await
                    .map_err(|e| format!("planning the delete failed: {e}"))?;
                match planned {
                    Ok(p) => ("delete_file", p.display.clone(), Some(Payload::Edit(p))),
                    Err(e) => return Ok(reject("delete_file", request_path(&call), format!("error: {e}"))),
                }
            }
            tools::Request::Move { path, to } => {
                let ws = ws.clone().expect("checked above");
                let planned = tauri::async_runtime::spawn_blocking(move || edit::plan_move(&ws, &path, &to))
                    .await
                    .map_err(|e| format!("planning the move failed: {e}"))?;
                match planned {
                    Ok(p) => ("move_file", p.display.clone(), Some(Payload::Edit(p))),
                    Err(e) => return Ok(reject("move_file", request_path(&call), format!("error: {e}"))),
                }
            }
        };
        // The plan's kind is the call's name; `run_tool` checks it again.
        if let Some(Payload::Edit(p)) = &payload {
            assert_eq!(plan_name(p), name, "a plan must be armed under its own tool name");
        }
        let (to, folders, size) = match &payload {
            Some(Payload::Edit(p)) => (
                p.to_display.clone(),
                p.folders.clone(),
                matches!(p.op, edit::Op::Delete | edit::Op::Move).then_some(p.size),
            ),
            _ => (None, Vec::new(), None),
        };

        let diff = match &payload {
            Some(Payload::Edit(p)) => Some(DiffView {
                hunks: p.hunks.clone(),
                added: p.added,
                removed: p.removed,
                create: p.create,
            }),
            _ => None,
        };
        let range = match &payload {
            Some(Payload::Read { range, .. }) => *range,
            _ => None,
        };
        let changed_build_files =
            if name == "run_check" { tools::build_files_changed(&history) } else { Vec::new() };
        Ok(serde_json::json!({
            "kind": "call",
            "call": PendingCall {
                token: arm(name, &path, payload),
                name: name.to_string(),
                path,
                diff,
                range,
                changed_build_files,
                to,
                folders,
                size,
            },
            // The raw call, so the caller can put the assistant turn that asked
            // for it back into the conversation before the tool result.
            //
            // **Without it the loop does not close.** Ollama expects
            // assistant(tool_calls) → tool(result); a tool message with no call in
            // front of it answers a question the model cannot see itself asking.
            "rawCall": call,
        }))
    }
    .await;

    // What every kind of reply also says about the prompt it answered: the
    // numbers the next round is calibrated from, and what was set aside.
    if let Ok(serde_json::Value::Object(body)) = &mut outcome {
        body.insert("promptEvalCount".into(), reply.prompt_eval_count.into());
        body.extend(sent.into_fields());
    }
    outcome
}

fn request_path(call: &chat::ToolCall) -> &str {
    call.function.arguments["path"].as_str().unwrap_or("")
}

/// What running an approved call produced.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Ran {
    /// Goes back to the model.
    result: String,
    /// Whether it worked. **Said here, not inferred from the text:** a check
    /// that fails has a result that does not start with `error:` — it worked,
    /// and what it found is the news — and the card must still show it failed.
    ok: bool,
    /// Ended by Stop rather than finishing.
    stopped: bool,
    /// For an edit that was written: the handle to undo it.
    #[serde(skip_serializing_if = "Option::is_none")]
    undo: Option<String>,
}

impl Ran {
    /// A result worded by a tool. **Whether it worked is read from its first word,
    /// which is sound because of the contract in `tools`'s header (R-25): a result
    /// starts `error:` if and only if the call failed**, and a read of a file that
    /// itself begins that way is given a header so it does not.
    fn text(result: String) -> Self {
        let ok = !result.trim_start().starts_with("error:");
        Self { result, ok, stopped: false, undo: None }
    }
}

/// Read a file the user approved — the one the ticket holds, the lines it holds.
fn run_read(real: &std::path::Path, range: Option<tools::Lines>) -> Ran {
    Ran::text(tools::run_read_file(real, range))
}

/// Run a listing or a search, for an approved card and for an automatic call
/// alike. The folder is opened again rather than trusted: it may have been moved
/// or deleted since it was chosen, or while the card was on screen.
///
/// `stop` is a place on the Stop counter taken before the call began, so a Stop
/// pressed at any point since reaches the walk — and one already pressed ends it
/// at once, having read nothing.
fn run_find(root: &str, find: &Find, stop: u64) -> Ran {
    let ws = match edit::Workspace::open(root) {
        Ok(ws) => ws,
        Err(e) => return Ran::text(format!("error: {e}")),
    };
    let found = match find {
        Find::List { pattern } => search::list(&ws, pattern.as_deref(), || stopped(stop)),
        Find::Search { text, pattern } => search::search(&ws, text, pattern.as_deref(), || stopped(stop)),
    };
    match found {
        Ok(text) => Ran { stopped: stopped(stop), ..Ran::text(text) },
        Err(e) => Ran::text(format!("error: {e}")),
    }
}

/// An automatic call, run: `(tool name, what the card would show, the lines of a
/// read, outcome)`.
/// `None` for a read that could not be made safely — see
/// [`tools::read_unasked`] — which the caller then asks about like any other.
fn run_auto(
    ws: &edit::Workspace,
    auto: tools::Auto,
    stop: u64,
) -> Option<(&'static str, String, Option<tools::Lines>, Ran)> {
    match auto {
        tools::Auto::Read { real, range } => {
            let text = tools::read_unasked(&real, range)?;
            Some(("read_file", edit::plain(&real), range, Ran::text(text)))
        }
        tools::Auto::List { pattern } => {
            let shown = search::describe_list(pattern.as_deref());
            Some(("list_files", shown, None, run_find(&ws.root_display(), &Find::List { pattern }, stop)))
        }
        tools::Auto::Search { text, pattern } => {
            let shown = search::describe_search(&text, pattern.as_deref());
            Some(("search", shown, None, run_find(&ws.root_display(), &Find::Search { text, pattern }, stop)))
        }
        // Answered by the caller before it gets here: it needs no folder.
        tools::Auto::Plan { steps } => Some(("update_plan", String::new(), None, Ran::text(tools::plan_message(&steps)))),
    }
}

/// Run a call the user has just approved.
///
/// Redeems the ticket and runs **what the ticket holds** — for a read, the file
/// the card showed; for an edit, the plan computed when the diff was shown; for a
/// check, the folder and argv the card showed. The webview names the call it approved; it never supplies what to
/// write or what to run.
///
/// **A check can be stopped.** Its ticket holds a number from the same counter as
/// a streamed answer, so the Stop button that ends an answer ends a check too —
/// including one stopped before it had started.
#[tauri::command]
async fn run_tool(name: String, path: String, token: String) -> Result<Ran, String> {
    // Before anything touches the disk. The ticket was issued when the UI was
    // shown this exact call, and is spent here - so what runs is what was
    // approved, and it runs once.
    let payload = redeem(&token, &name, &path)?;
    tauri::async_runtime::spawn_blocking(move || match (name.as_str(), payload) {
        ("read_file", Some(Payload::Read { real, range })) => Ok(run_read(&real, range)),
        ("edit_file" | "create_file" | "delete_file" | "move_file", Some(Payload::Edit(plan))) => {
            if plan_name(&plan) != name {
                return Err(format!("this approval was for {}, not {name}", plan_name(&plan)));
            }
            let backups = edit::backups_dir().ok_or("APPDATA is not set")?;
            let _edits = edits_lock();
            // A file changed since the diff was shown is refused, not written:
            // the model is told, and can read it again.
            Ok(match edit::apply(&plan, &backups) {
                Ok(done) => {
                    let said = tools::applied_message(&done);
                    Ran { undo: Some(done.id), ..Ran::text(said) }
                }
                Err(e) => Ran::text(format!("error: {e}")),
            })
        }
        ("run_check", Some(Payload::Check { root, argv, stop })) => {
            if stopped(stop) {
                return Ok(Ran {
                    result: format!("`{}` was stopped by the user before it ran.", check::label(&argv)),
                    ok: false,
                    stopped: true,
                    undo: None,
                });
            }
            let root = std::path::PathBuf::from(root);
            Ok(match check::run(&root, &argv, check::timeout_for(&argv), &[], || stopped(stop)) {
                Ok(o) => Ran {
                    result: check::for_model(&argv, &o),
                    ok: o.passed(),
                    stopped: o.ended == check::Ended::Stopped,
                    undo: None,
                },
                Err(e) => Ran::text(format!("error: {e}")),
            })
        }
        ("list_files" | "search", Some(Payload::Find { root, find, stop })) => Ok(run_find(&root, &find, stop)),
        (other, _) => Err(format!("no such tool: {other}")),
    })
    .await
    .map_err(|e| format!("running the tool failed: {e}"))?
}

/// Put a file back the way it was before one edit.
///
/// Refused — by [`edit::undo`] — if the file has changed since, so undo never
/// throws away work done after the edit.
#[tauri::command]
async fn undo_edit(id: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let backups = edit::backups_dir().ok_or("APPDATA is not set")?;
        let _edits = edits_lock();
        edit::undo(&id, &backups)
    })
    .await
    .map_err(|e| format!("undo failed: {e}"))?
}

/// Every write to a project file — an edit, an undo, a turn's undo — takes this
/// first, so two of them never touch the same file at once. Two undos from two
/// clicks, or an undo while the next edit is being applied, would otherwise each
/// pass their own "is the file still what I expect" check and then write over
/// one another. It lives here rather than in `edit.rs` because the tests and
/// probes there apply edits in parallel on purpose.
static EDITS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A panic while holding the lock leaves nothing half-written that the next
/// holder's own checks would not catch, so a poisoned lock is simply taken.
fn edits_lock() -> std::sync::MutexGuard<'static, ()> {
    EDITS.lock().unwrap_or_else(|e| e.into_inner())
}

/// More edits than this in one turn is not something the UI produces.
const MAX_TURN_UNDO: usize = 64;

/// Put back every edit of one turn, or none.
///
/// All of [`edit::undo_turn`]'s pre-check and restores run under one hold of
/// the edits lock. `Err` means refused and nothing written; `Ok` with `failed`
/// set means it stopped partway and the rest are still applied.
#[tauri::command]
async fn undo_turn(ids: Vec<String>) -> Result<edit::TurnUndo, String> {
    if ids.len() > MAX_TURN_UNDO {
        return Err(format!("a turn of more than {MAX_TURN_UNDO} edits cannot be undone at once"));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let backups = edit::backups_dir().ok_or("APPDATA is not set")?;
        let _edits = edits_lock();
        edit::undo_turn(&ids, &backups)
    })
    .await
    .map_err(|e| format!("undo failed: {e}"))?
}

// ── the project folder ──────────────────────────────────────────────────────

#[derive(Serialize)]
struct Folder {
    path: String,
    name: String,
    /// The check command chosen for this folder, as a person reads it.
    check: Option<String>,
    /// The folder's own notes file, if it has one that can be read (H10): which
    /// file, whether the user has turned it on for this folder, and whether it is
    /// longer than what is sent. **Only the name and two flags** — the text is never
    /// sent to the webview, only into the system message.
    notes: Option<NotesView>,
}

#[derive(Serialize)]
struct NotesView {
    name: String,
    on: bool,
    cut: bool,
}

fn folder(ws: &edit::Workspace, cfg: &config::Config) -> Folder {
    let path = ws.root_display();
    let name = std::path::Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.clone());
    let check = cfg.check_for(&path).map(|argv| check::label(argv));
    let notes = instructions::find(ws).map(|n| NotesView { on: cfg.instructions_for(&path), name: n.name, cut: n.cut });
    Folder { path, name, check, notes }
}

/// The folder edits are confined to, or `None` when editing is off.
#[tauri::command]
async fn project_folder() -> Result<Option<Folder>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let cfg = config::load();
        workspace(&cfg).map(|ws| folder(&ws, &cfg))
    })
    .await
    .map_err(|e| format!("reading the project folder failed: {e}"))
}

/// Choose the project folder **with the system's own folder picker.**
///
/// There is deliberately no command that takes a path. The only way to widen
/// what the model may edit is a native dialog a person operates — so nothing
/// that reaches the IPC boundary, a hostile page included, can choose a folder
/// by itself. Cancelling leaves the current choice alone.
#[tauri::command]
async fn choose_project_folder(app: tauri::AppHandle) -> Result<Option<Folder>, String> {
    use tauri_plugin_dialog::DialogExt;

    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog().file().set_title("Choose the folder Mímir may edit").blocking_pick_folder()
    })
    .await
    .map_err(|e| format!("the folder picker failed: {e}"))?;
    let Some(picked) = picked else {
        return Ok(None);
    };
    let dir = picked.into_path().map_err(|e| format!("that is not a folder on this machine: {e}"))?;

    tauri::async_runtime::spawn_blocking(move || {
        let ws = edit::Workspace::open(&dir)?;
        let root = ws.root_display();
        let cfg = config::update(|cfg| {
            remember(cfg, &root);
            cfg.workspace = Some(root);
            cfg.clone()
        })?;
        Ok(Some(folder(&ws, &cfg)))
    })
    .await
    .map_err(|e| format!("saving the project folder failed: {e}"))?
}

/// How many folders the new-chat screen offers again.
const RECENT_FOLDERS: usize = 5;

/// Put a picked folder at the front of the recent list, once.
fn remember(cfg: &mut config::Config, root: &str) {
    cfg.recent_workspaces.retain(|p| !p.eq_ignore_ascii_case(root));
    cfg.recent_workspaces.insert(0, root.to_string());
    cfg.recent_workspaces.truncate(RECENT_FOLDERS);
}

/// Folders picked before, newest first, that still exist.
///
/// A folder that has since been deleted or moved is left out rather than shown
/// and then refused.
#[tauri::command]
async fn recent_folders() -> Result<Vec<Folder>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let cfg = config::load();
        cfg.recent_workspaces
            .iter()
            .filter_map(|p| edit::Workspace::open(p).ok())
            .map(|ws| folder(&ws, &cfg))
            .collect()
    })
    .await
    .map_err(|e| format!("reading recent folders failed: {e}"))
}

/// Make a recent folder the project folder again.
///
/// **Takes a position, not a path**, and the position is into a list only
/// `choose_project_folder` writes — so this can only ever restore a folder the
/// user picked in the system dialog. Counting skips folders that no longer
/// exist, the same way `recent_folders` does, so the index means the same row.
#[tauri::command]
async fn use_recent_folder(index: usize) -> Result<Folder, String> {
    tauri::async_runtime::spawn_blocking(move || {
        // Opening the recent folders touches the disk, and a folder on a dead network
        // share can take a long time to answer: do it before taking the config lock,
        // which every other config write waits on.
        let ws = config::load()
            .recent_workspaces
            .iter()
            .filter_map(|p| edit::Workspace::open(p).ok())
            .nth(index)
            .ok_or_else(|| "that folder is no longer in the recent list".to_string())?;
        let root = ws.root_display();
        let cfg = config::update(|cfg| {
            // The list may have changed while the folders were being opened.
            if !cfg.recent_workspaces.iter().any(|p| p.eq_ignore_ascii_case(&root)) {
                return Err("that folder is no longer in the recent list".to_string());
            }
            remember(cfg, &root);
            cfg.workspace = Some(root.clone());
            Ok(cfg.clone())
        })??;
        Ok(folder(&ws, &cfg))
    })
    .await
    .map_err(|e| format!("reopening the folder failed: {e}"))?
}

// ── the check command ───────────────────────────────────────────────────────

/// The check commands this folder could use, as a person reads them.
///
/// Detected by Rust from the folder's own manifests. The webview can only show
/// them and pick one **by position** — there is no command that takes a command,
/// so nothing reaching the IPC boundary can choose what runs.
#[tauri::command]
async fn check_options() -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let ws = workspace(&config::load()).ok_or("no project folder is chosen")?;
        Ok(check::candidates(&ws).iter().map(|argv| check::label(argv)).collect())
    })
    .await
    .map_err(|e| format!("reading the check commands failed: {e}"))?
}

/// Use the check at this position for the current folder.
///
/// The candidates are detected again here rather than trusted from the list the
/// webview was shown, and `label` must still match: if the folder or its
/// manifests changed in between, the position could now mean a different
/// command, and choosing one thing must never run another.
#[tauri::command]
async fn set_check(index: usize, label: String) -> Result<Folder, String> {
    tauri::async_runtime::spawn_blocking(move || {
        // Finding the candidates reads the folder; do that before the config lock is
        // taken, and take it only for the in-memory change.
        let ws = workspace(&config::load()).ok_or("no project folder is chosen")?;
        let argv = check::candidates(&ws)
            .into_iter()
            .nth(index)
            .filter(|argv| check::label(argv) == label)
            .ok_or("that check is no longer offered for this folder — choose again")?;
        let root = ws.root_display();
        let cfg = config::update(|cfg| {
            // The folder may have been changed meanwhile; the check was found for
            // another one then, and must not be saved against this one.
            if !cfg.workspace.as_deref().is_some_and(|p| p.eq_ignore_ascii_case(&root)) {
                return Err("the project folder changed — choose the check again".to_string());
            }
            cfg.set_check(&root, Some(argv));
            Ok(cfg.clone())
        })??;
        Ok(folder(&ws, &cfg))
    })
    .await
    .map_err(|e| format!("choosing the check failed: {e}"))?
}

/// Stop offering the model a check in the current folder.
#[tauri::command]
async fn clear_check() -> Result<Folder, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let (ws, cfg) = config::update(|cfg| {
            let ws = workspace(cfg).ok_or("no project folder is chosen")?;
            cfg.set_check(&ws.root_display(), None);
            Ok::<_, String>((ws, cfg.clone()))
        })??;
        Ok(folder(&ws, &cfg))
    })
    .await
    .map_err(|e| format!("clearing the check failed: {e}"))?
}

/// Send the folder's notes file with every question, or stop.
///
/// **Takes only the switch.** The folder is the one in the config and the file is
/// found by `instructions::find`, so nothing reaching the IPC boundary can name a
/// path to be read into the prompt. Off by default for every folder.
#[tauri::command]
async fn set_notes(on: bool) -> Result<Folder, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (ws, cfg) = config::update(|cfg| {
            let ws = workspace(cfg).ok_or("no project folder is chosen")?;
            cfg.set_instructions(&ws.root_display(), on);
            Ok::<_, String>((ws, cfg.clone()))
        })??;
        Ok(folder(&ws, &cfg))
    })
    .await
    .map_err(|e| format!("changing the notes failed: {e}"))?
}

/// Turn editing off. Reads keep working, one approved file at a time.
#[tauri::command]
async fn clear_project_folder() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(|| {
        config::update(|cfg| cfg.workspace = None)
    })
    .await
    .map_err(|e| format!("clearing the project folder failed: {e}"))?
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

// ── appearance ──────────────────────────────────────────────────────────────

/// The Windows accent colour as `#rrggbb`, for the "Match Windows" accent.
///
/// Read from the registry value Explorer and DWM write when you pick a colour in
/// Settings → Personalisation, stored as 0xAABBGGRR. Tauri has no API for it,
/// and WebView2 does not expose the CSS `AccentColor` keyword outside installed
/// web apps. `None` when the value is missing, which the webview treats as "keep
/// the colour you have".
#[tauri::command]
fn system_accent() -> Option<String> {
    windows_accent()
}

#[cfg(windows)]
fn windows_accent() -> Option<String> {
    use windows::core::w;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};

    let mut value: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: `value` is a u32 and `size` says so; RegGetValueW writes at most
    // `size` bytes, and RRF_RT_REG_DWORD refuses any other value type.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\DWM"),
            w!("AccentColor"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut core::ffi::c_void),
            Some(&mut size),
        )
    };
    status.ok().ok()?;
    let (r, g, b) = (value & 0xff, (value >> 8) & 0xff, (value >> 16) & 0xff);
    Some(format!("#{r:02x}{g:02x}{b:02x}"))
}

#[cfg(not(windows))]
fn windows_accent() -> Option<String> {
    None
}

/// Send `println!` and `eprintln!` to a file, since there is no console.
///
/// `main.rs` sets `windows_subsystem = "windows"`, so this process has no stdout
/// to write to — without this, everything printed vanishes. Heimdall's `lib.rs`
/// has the twin of this function writing `heimdall.log`, for the same reason.
///
/// **Two copies of twenty lines, deliberately.** It is app-level plumbing: each
/// app picks its own filename and its own answer to whether it is windowless at
/// all. `odin-core`'s stated rule is that nothing in it prints, and giving it a
/// Win32 console dependency to serve something only apps need would cost more
/// than the duplication. **If a third app wants this, that is the signal to
/// extract it.**
#[cfg(windows)]
fn redirect_log_to_file() -> Option<std::path::PathBuf> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE};

    let path = std::env::temp_dir().join("mimir.log");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;

    let handle = HANDLE(file.as_raw_handle());
    // SAFETY: a valid handle to a file this process just opened.
    unsafe {
        let _ = SetStdHandle(STD_OUTPUT_HANDLE, handle);
        let _ = SetStdHandle(STD_ERROR_HANDLE, handle);
    }
    // Deliberately leaked: the handles outlive this scope and are used for the
    // life of the process.
    std::mem::forget(file);
    Some(path)
}

#[cfg(not(windows))]
fn redirect_log_to_file() -> Option<std::path::PathBuf> {
    None
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    if let Some(log) = redirect_log_to_file() {
        println!("[mimir] log: {}", log.display());
    }

    tauri::Builder::default()
        // Used only from Rust, by choose_project_folder. The capability file grants
        // the webview no dialog permission, so a page cannot open a picker itself.
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            send,
            ollama_running,
            models,
            card,
            conversations,
            open_conversation,
            save_conversation,
            rename_conversation,
            delete_conversation,
            put_attachment,
            get_attachment,
            ask_with_tools,
            run_tool,
            undo_edit,
            undo_turn,
            project_folder,
            choose_project_folder,
            clear_project_folder,
            load_model,
            unload_model,
            set_model,
            model_offer,
            accept_model_offer,
            system_accent,
            recent_folders,
            use_recent_folder,
            check_options,
            set_check,
            clear_check,
            set_notes,
            stop
        ])
        .run(tauri::generate_context!())
        .expect("error while running Mímir");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indices_are_reported_in_the_webviews_numbering_and_the_system_prompt_is_never_one() {
        assert_eq!(frontend_indices(&[3, 5], 1), vec![2, 4]);
        assert_eq!(frontend_indices(&[3, 5], 0), vec![3, 5]);
        assert_eq!(frontend_indices(&[0, 3], 1), vec![2]);
    }

    /// The pairs a webview would hold, with the system prompt in front.
    fn long_turn() -> Vec<Message> {
        let mut m = vec![Message::system("sys"), Message::user("find it")];
        for _ in 0..4 {
            m.push(Message {
                role: "assistant".into(),
                content: String::new(),
                images: None,
                tool_calls: Some(vec![chat::ToolCall {
                    id: None,
                    function: chat::FunctionCall {
                        name: "search".into(),
                        arguments: serde_json::json!({ "text": "x" }),
                    },
                }]),
                tool_name: None,
            });
            m.push(Message::tool_result("search", "x".repeat(6000)));
        }
        m
    }

    #[test]
    fn what_is_set_aside_comes_back_numbered_without_the_system_prompt_and_as_sent() {
        let mut messages = long_turn();
        // 24000 characters at 2.5 a token is 9600 tokens; 0.75 of 8192 is 6144.
        let sent = compact_for_send(&mut messages, &[], 8192, None, 1);
        // Sent indices 3 and 5 are the webview's 2 and 4; the last result stays.
        assert_eq!(sent.set_aside, vec![2, 4]);
        assert!(sent.fits);
        assert_eq!(sent.replacements, vec![messages[3].clone(), messages[5].clone()]);
        assert!(sent.replacements[0].content.starts_with(compact::STUB_MARKER));
        assert_eq!(sent.prompt_chars, compact::prompt_chars(&messages, &[]));
        assert_eq!(messages[9].content.len(), 6000);
    }

    #[test]
    fn undoing_a_turn_of_more_than_the_limit_is_refused_before_anything_runs() {
        let ids: Vec<String> = (0..=MAX_TURN_UNDO).map(|i| format!("{i:x}")).collect();
        let err = tauri::async_runtime::block_on(undo_turn(ids)).unwrap_err();
        assert!(err.contains("more than 64"), "{err}");
    }

    /// The lock is what keeps two writers off one file, so a second taker must
    /// wait while the first holds it, and a panic in a holder must not wedge it.
    #[test]
    fn the_edits_lock_is_exclusive_and_survives_a_panicking_holder() {
        {
            let _held = edits_lock();
            assert!(EDITS.try_lock().is_err(), "a second writer got in while the lock was held");
        }
        let _ = std::thread::spawn(|| {
            let _held = edits_lock();
            panic!("a writer died holding the lock");
        })
        .join();
        drop(edits_lock());
    }

    /// The approval boundary. These are the only tests in this crate on purpose:
    /// everything else here is a thin `spawn_blocking` wrapper over `odin-core`,
    /// which has its own. This is the one place Mímir enforces a rule of its
    /// own, and a rule that is not tested is a comment.
    ///
    /// They drive `redeem_in` with a local slot rather than the process-wide
    /// one, because tests run in parallel and a shared static would make them
    /// interfere with each other instead of with the thing under test.
    fn armed(name: &str, path: &str) -> (Option<Armed>, String) {
        let token = "ticket-1".to_string();
        (
            Some(Armed { token: token.clone(), name: name.into(), path: path.into(), payload: None }),
            token,
        )
    }

    #[test]
    fn a_ticket_only_works_for_the_call_it_was_issued_for() {
        let (mut slot, t) = armed("read_file", "C:/a.txt");
        assert!(redeem_in(&mut slot, &t, "read_file", "C:/somewhere-else.txt").is_err());
    }

    #[test]
    fn a_ticket_is_rejected_for_another_tool() {
        let (mut slot, t) = armed("read_file", "C:/a.txt");
        assert!(redeem_in(&mut slot, &t, "delete_file", "C:/a.txt").is_err());
    }

    #[test]
    fn a_matching_ticket_is_accepted_once() {
        let (mut slot, t) = armed("read_file", "C:/a.txt");
        assert!(redeem_in(&mut slot, &t, "read_file", "C:/a.txt").is_ok());
        // A replay must not read the file a second time on one approval.
        assert!(redeem_in(&mut slot, &t, "read_file", "C:/a.txt").is_err());
    }

    #[test]
    fn a_wrong_ticket_burns_the_real_one() {
        let (mut slot, t) = armed("read_file", "C:/a.txt");
        assert!(redeem_in(&mut slot, "guessed", "read_file", "C:/a.txt").is_err());
        assert!(redeem_in(&mut slot, &t, "read_file", "C:/a.txt").is_err());
    }

    #[test]
    fn nothing_is_approved_by_default() {
        let mut slot = None;
        assert!(redeem_in(&mut slot, "anything", "read_file", "C:/a.txt").is_err());
    }

    #[test]
    fn arming_issues_a_ticket_for_exactly_that_call() {
        let token = arm("read_file", "C:/from-arm.txt", None);
        assert!(!token.is_empty());
        // Whatever was outstanding is replaced, so only the newest call stands.
        let slot = pending().lock().unwrap();
        let a = slot.as_ref().expect("a call should be armed");
        assert_eq!((a.token.as_str(), a.name.as_str(), a.path.as_str()), (token.as_str(), "read_file", "C:/from-arm.txt"));
    }

    /// **The edit that runs is the one that was planned** - the ticket hands back
    /// the stored plan, and nothing about the edit comes from the caller.
    #[test]
    fn an_edit_ticket_redeems_the_plan_it_was_armed_with() {
        let dir = std::env::temp_dir().join(format!("mimir-plan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "one
").unwrap();
        let ws = edit::Workspace::open(&dir).unwrap();
        let plan = edit::plan_replace(&ws, "a.txt", "one", "two").unwrap();
        let mut slot = Some(Armed {
            token: "t".into(),
            name: "edit_file".into(),
            path: "a.txt".into(),
            payload: Some(Payload::Edit(plan.clone())),
        });
        let got = redeem_in(&mut slot, "t", "edit_file", "a.txt").unwrap();
        assert_eq!(got, Some(Payload::Edit(plan)));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// **A move's destination is in the ticket**, not in what the webview sends
    /// back: redeeming it hands over the plan with `to`, and a different path
    /// does not redeem at all.
    #[test]
    fn a_move_ticket_redeems_its_destination() {
        let dir = std::env::temp_dir().join(format!("mimir-move-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        let ws = edit::Workspace::open(&dir).unwrap();
        let plan = edit::plan_move(&ws, "a.txt", "sub/b.txt").unwrap();
        assert_eq!(plan_name(&plan), "move_file");
        let armed = |plan: &edit::Plan| {
            Some(Armed {
                token: "t".into(),
                name: "move_file".into(),
                path: "a.txt".into(),
                payload: Some(Payload::Edit(plan.clone())),
            })
        };
        let mut slot = armed(&plan);
        let Some(Payload::Edit(got)) = redeem_in(&mut slot, "t", "move_file", "a.txt").unwrap() else {
            panic!("no plan")
        };
        assert_eq!(got.to_display.as_deref(), Some("sub/b.txt"));
        assert_eq!(got.folders, ["sub/"]);
        let mut slot = armed(&plan);
        assert!(redeem_in(&mut slot, "t", "move_file", "other.txt").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A delete's approval cannot be spent as an edit or a create — the name is
    /// part of the ticket, and the plan's own kind is checked again on redeeming.
    #[test]
    fn a_delete_ticket_cannot_be_redeemed_as_an_edit() {
        let dir = std::env::temp_dir().join(format!("mimir-delete-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        let ws = edit::Workspace::open(&dir).unwrap();
        let plan = edit::plan_delete(&ws, "a.txt").unwrap();
        assert_eq!(plan_name(&plan), "delete_file");
        for as_name in ["edit_file", "create_file", "move_file"] {
            let mut slot = Some(Armed {
                token: "t".into(),
                name: "delete_file".into(),
                path: "a.txt".into(),
                payload: Some(Payload::Edit(plan.clone())),
            });
            assert!(redeem_in(&mut slot, "t", as_name, "a.txt").is_err(), "{as_name}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_edit_ticket_cannot_be_redeemed_as_a_read() {
        // Same token and path, different tool: a read must not get the plan, and
        // must not be allowed to run on an edit's approval.
        let mut slot = Some(Armed { token: "t".into(), name: "edit_file".into(), path: "a.txt".into(), payload: None });
        assert!(redeem_in(&mut slot, "t", "read_file", "a.txt").is_err());
    }

    fn check_ticket() -> Option<Armed> {
        Some(Armed {
            token: "t".into(),
            name: "run_check".into(),
            path: "cargo check".into(),
            payload: Some(Payload::Check {
                root: r"C:\work\demo".into(),
                argv: vec!["cargo".into(), "check".into()],
                stop: 7,
            }),
        })
    }

    /// **The command that runs is the one the card showed** — folder, argv and
    /// its place on the Stop counter come back from the ticket, and nothing
    /// about them comes from the caller.
    #[test]
    fn a_check_ticket_redeems_the_command_it_was_armed_with() {
        let mut slot = check_ticket();
        let got = redeem_in(&mut slot, "t", "run_check", "cargo check").unwrap();
        assert_eq!(
            got,
            Some(Payload::Check {
                root: r"C:\work\demo".into(),
                argv: vec!["cargo".into(), "check".into()],
                stop: 7
            })
        );
    }

    /// What is searched for comes back from the ticket, as the card showed it —
    /// never from the caller.
    #[test]
    fn a_search_ticket_redeems_what_the_card_showed() {
        let shown = "\"greet(\" in src/**/*.rs";
        let find = Find::Search { text: "greet(".into(), pattern: Some("src/**/*.rs".into()) };
        let payload = Payload::Find { root: r"C:\work\demo".into(), find, stop: 3 };
        let armed = || {
            Some(Armed { token: "t".into(), name: "search".into(), path: shown.into(), payload: Some(payload.clone()) })
        };
        // Not as a read of a file that happens to have that name, nor as a listing.
        assert!(redeem_in(&mut armed(), "t", "read_file", shown).is_err());
        assert!(redeem_in(&mut armed(), "t", "list_files", shown).is_err());
        assert_eq!(redeem_in(&mut armed(), "t", "search", shown).unwrap(), Some(payload.clone()));
    }

    /// **The file that is read is the one that was resolved when the card was
    /// shown** — the ticket hands back that path, and nothing about it comes from
    /// the caller.
    #[test]
    fn a_read_ticket_redeems_the_file_it_was_armed_with() {
        let open = std::path::PathBuf::from(r"C:\work\demo\src\a.rs");
        let shown = r"C:\work\demo\src\a.rs";
        let mut slot = Some(Armed {
            token: "t".into(),
            name: "read_file".into(),
            path: shown.into(),
            payload: Some(Payload::Read { real: open.clone(), range: None }),
        });
        assert_eq!(
            redeem_in(&mut slot, "t", "read_file", shown).unwrap(),
            Some(Payload::Read { real: open, range: None })
        );
    }

    /// **The lines are the ticket's too**: the webview names the call it approved
    /// and gets back the range the card showed, which it cannot change.
    #[test]
    fn a_read_ticket_redeems_with_its_range() {
        let open = std::path::PathBuf::from(r"C:\work\demo\src\a.rs");
        let shown = r"C:\work\demo\src\a.rs";
        let range = Some(tools::Lines { start: 120, end: Some(180) });
        let payload = Payload::Read { real: open, range };
        let mut slot = Some(Armed {
            token: "t".into(),
            name: "read_file".into(),
            path: shown.into(),
            payload: Some(payload.clone()),
        });
        assert_eq!(redeem_in(&mut slot, "t", "read_file", shown).unwrap(), Some(payload));
    }

    /// Run as `run_tool` runs it, the ticket's range is what is read.
    #[test]
    fn run_read_gives_the_ticket_range_and_the_whole_file_with_none() {
        let dir = std::env::temp_dir().join(format!("mimir-range-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, "one\r\ntwo\r\nthree\r\n").unwrap();
        let part = run_read(&file, Some(tools::Lines { start: 2, end: Some(3) }));
        assert!(part.ok);
        assert_eq!(part.result, "[lines 2–3 of 3]\ntwo\nthree\n");
        assert_eq!(run_read(&file, None).result, "one\ntwo\nthree\n");
        let past = run_read(&file, Some(tools::Lines { start: 9, end: None }));
        assert!(!past.ok && past.result.contains("is past the end"), "{}", past.result);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_read_ticket_cannot_be_redeemed_as_a_search() {
        let shown = r"C:\work\demo\src\a.rs";
        let armed = || {
            Some(Armed {
                token: "t".into(),
                name: "read_file".into(),
                path: shown.into(),
                payload: Some(Payload::Read { real: shown.into(), range: None }),
            })
        };
        assert!(redeem_in(&mut armed(), "t", "search", shown).is_err());
        assert!(redeem_in(&mut armed(), "t", "list_files", shown).is_err());
    }

    /// Stop pressed before an automatic walk starts: it ends at once, says so,
    /// and has read no file.
    #[test]
    fn run_find_with_an_already_stopped_number_reports_stopped_and_reads_nothing() {
        let dir = std::env::temp_dir().join(format!("mimir-stopped-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "needle\n").unwrap();
        let root = edit::Workspace::open(&dir).unwrap().root_display();
        let stop = next_stop();
        STREAMS_STOPPED.fetch_max(stop, Ordering::SeqCst);
        let ran = run_find(&root, &Find::Search { text: "needle".into(), pattern: None }, stop);
        assert!(ran.stopped);
        assert!(!ran.result.contains("a.txt:1"), "{}", ran.result);
        assert!(ran.result.contains("stopped"), "{}", ran.result);
        // And a number taken after the Stop is not caught by it.
        let later = run_find(&root, &Find::Search { text: "needle".into(), pattern: None }, next_stop());
        assert!(!later.stopped && later.result.contains("a.txt:1"), "{}", later.result);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_check_ticket_cannot_be_redeemed_as_a_read() {
        // A read of a file called "cargo check" must not run on a check's approval.
        let mut slot = check_ticket();
        assert!(redeem_in(&mut slot, "t", "read_file", "cargo check").is_err());
    }

    // ── model_offer ─────────────────────────────────────────────────────────

    const VISION: &str = "qwen3-vl:8b-instruct-q4_K_M";
    const CODER: &str = "qwen3-coder:30b";

    fn cfg_with_folder() -> config::Config {
        config::Config { model: VISION.into(), workspace: Some(r"C:\work\demo".into()), ..Default::default() }
    }

    #[test]
    fn a_folder_on_the_default_model_offers_the_coder() {
        let cfg = cfg_with_folder();
        assert_eq!(
            offer_for(&cfg, &[VISION.into(), CODER.into()]),
            Some(ModelOffer { kind: "coding", from: VISION.into(), to: CODER.into() })
        );
    }

    #[test]
    fn no_offer_when_the_coder_is_not_installed() {
        let cfg = cfg_with_folder();
        assert_eq!(offer_for(&cfg, &[VISION.into()]), None);
    }

    #[test]
    fn no_offer_when_already_on_the_coder_with_a_folder() {
        let mut cfg = cfg_with_folder();
        cfg.model = CODER.into();
        assert_eq!(offer_for(&cfg, &[VISION.into(), CODER.into()]), None);
    }

    #[test]
    fn an_empty_coding_model_turns_the_offer_off() {
        let mut cfg = cfg_with_folder();
        cfg.coding_model = String::new();
        assert_eq!(offer_for(&cfg, &[VISION.into(), CODER.into()]), None);
    }

    #[test]
    fn clearing_the_folder_while_on_the_coder_offers_to_switch_back() {
        let cfg =
            config::Config { model: CODER.into(), model_before_coding: Some(VISION.into()), ..Default::default() };
        assert_eq!(
            offer_for(&cfg, &[VISION.into(), CODER.into()]),
            Some(ModelOffer { kind: "back", from: CODER.into(), to: VISION.into() })
        );
    }

    #[test]
    fn no_switch_back_when_the_previous_model_is_gone() {
        let cfg =
            config::Config { model: CODER.into(), model_before_coding: Some(VISION.into()), ..Default::default() };
        assert_eq!(offer_for(&cfg, &[CODER.into()]), None);
    }

    #[test]
    fn no_switch_back_with_nothing_to_switch_back_to() {
        let cfg = config::Config { model: CODER.into(), model_before_coding: None, ..Default::default() };
        assert_eq!(offer_for(&cfg, &[VISION.into(), CODER.into()]), None);
    }

    #[test]
    fn no_switch_back_while_a_folder_is_still_open() {
        let mut cfg = cfg_with_folder();
        cfg.model = CODER.into();
        cfg.model_before_coding = Some(VISION.into());
        assert_eq!(offer_for(&cfg, &[VISION.into(), CODER.into()]), None);
    }

    #[test]
    fn a_bare_tag_and_its_latest_are_the_same_model() {
        assert!(same_tag("llama3.1", "llama3.1:latest"));
        assert!(same_tag("llama3.1:latest", "llama3.1"));
        assert!(!same_tag("llama3.1", "llama3.1:8b"));
    }
}
