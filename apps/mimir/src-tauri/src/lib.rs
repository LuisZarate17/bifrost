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
use odin_core::{check, config, edit, ollama, search, store, tools, vram};

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
    let id = STREAMS_STARTED.fetch_add(1, Ordering::SeqCst) + 1;

    // Every odin_core call goes off the async worker. It is blocking by design,
    // and calling it from an async command drops a tokio runtime inside an async
    // context and kills the worker — which already cost Heimdall a request that
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
    /// For a check: build files an edit changed earlier in this turn. A check
    /// runs them, so the card names them before you approve it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    changed_build_files: Vec<String>,
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

/// What an approval carries beyond its name and path.
#[derive(Debug, Clone, PartialEq)]
enum Payload {
    Edit(edit::Plan),
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

/// Ask the model, offering the tools, and report back what it wants.
///
/// **This does not run anything.** It returns an answer, a request to approve,
/// or — for a request that could never succeed, like an edit whose text is not
/// in the file — a rejection, which goes straight back to the model so it can
/// read the file again and retry without the user being asked about something
/// impossible.
#[tauri::command]
async fn ask_with_tools(
    mut messages: Vec<Message>,
    model: Option<String>,
) -> Result<serde_json::Value, String> {
    let cfg = tauri::async_runtime::spawn_blocking(config::load)
        .await
        .unwrap_or_default();
    let model = model.unwrap_or(cfg.model.clone());

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
    if let Some(ws) = &ws {
        messages.insert(0, Message::system(tools::system_prompt(ws, check.is_some())));
    }

    // Kept for counting repeats: the history the model has already seen.
    let history = messages.clone();
    let num_ctx = cfg.num_ctx;
    let offered = tools::available(ws.is_some(), check_label.as_deref());
    let reply = tauri::async_runtime::spawn_blocking(move || {
        chat::chat_tools(&model, &messages, &offered, Options::new(num_ctx))
    })
    .await
    .map_err(|e| format!("tool turn failed: {e}"))??;
    if reply.recovered {
        println!("[mimir] a malformed tool call was recovered through the raw retry");
    }

    // The first call only. A model that asks for several at once is handled one
    // at a time, because each one needs its own approval.
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
    let reject = |name: &str, path: &str, why: String| {
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
        Err(e) => return Ok(reject(&call.function.name, "", e)),
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

    let (name, path, payload) = match request {
        tools::Request::Read { path } => {
            let target = tools::read_target(&path, ws.as_ref());
            ("read_file", target, None)
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
                    "error: run_check is not available — no check command is set for this folder. \
                     Carry on without it, and tell the user they can choose one."
                        .into(),
                ));
            }
        },
        tools::Request::Rewrite { path, .. } => {
            return Ok(reject("write_file", &path, "no such tool: write_file — use edit_file".into()));
        }
        tools::Request::Edit { .. } | tools::Request::Create { .. } if ws.is_none() => {
            return Ok(reject(
                request.name(),
                request.path(),
                "editing is off: the user has not chosen a project folder".into(),
            ));
        }
        tools::Request::Edit { path, old_text, new_text } => {
            let ws = ws.clone().expect("checked above");
            let planned = tauri::async_runtime::spawn_blocking(move || {
                edit::plan_replace(&ws, &path, &old_text, &new_text)
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
    let changed_build_files =
        if name == "run_check" { tools::build_files_changed(&history) } else { Vec::new() };
    Ok(serde_json::json!({
        "kind": "call",
        "call": PendingCall {
            token: arm(name, &path, payload),
            name: name.to_string(),
            path,
            diff,
            changed_build_files,
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
    fn text(result: String) -> Self {
        let ok = !result.starts_with("error:");
        Self { result, ok, stopped: false, undo: None }
    }
}

/// Run a call the user has just approved.
///
/// Redeems the ticket and runs **what the ticket holds** — for an edit, the plan
/// computed when the diff was shown; for a check, the folder and argv the card
/// showed. The webview names the call it approved; it never supplies what to
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
        ("read_file", None) => Ok(Ran::text(tools::run_read_file(&path))),
        ("edit_file" | "create_file", Some(Payload::Edit(plan))) => {
            let backups = edit::backups_dir().ok_or("APPDATA is not set")?;
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
        ("list_files" | "search", Some(Payload::Find { root, find, stop })) => {
            // Opened again rather than trusted: the folder may have been moved
            // or deleted while the card was on screen.
            let ws = match edit::Workspace::open(&root) {
                Ok(ws) => ws,
                Err(e) => return Ok(Ran::text(format!("error: {e}"))),
            };
            let found = match &find {
                Find::List { pattern } => search::list(&ws, pattern.as_deref(), || stopped(stop)),
                Find::Search { text, pattern } => search::search(&ws, text, pattern.as_deref(), || stopped(stop)),
            };
            Ok(match found {
                Ok(text) => Ran { stopped: stopped(stop), ..Ran::text(text) },
                Err(e) => Ran::text(format!("error: {e}")),
            })
        }
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
        edit::undo(&id, &backups)
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
}

fn folder(ws: &edit::Workspace, cfg: &config::Config) -> Folder {
    let path = ws.root_display();
    let name = std::path::Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.clone());
    let check = cfg.check_for(&path).map(|argv| check::label(argv));
    Folder { path, name, check }
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
        let mut cfg = config::load();
        let root = ws.root_display();
        remember(&mut cfg, &root);
        cfg.workspace = Some(root);
        config::save(&cfg)?;
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
        let mut cfg = config::load();
        let ws = cfg
            .recent_workspaces
            .iter()
            .filter_map(|p| edit::Workspace::open(p).ok())
            .nth(index)
            .ok_or_else(|| "that folder is no longer in the recent list".to_string())?;
        let root = ws.root_display();
        remember(&mut cfg, &root);
        cfg.workspace = Some(root);
        config::save(&cfg)?;
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
        let mut cfg = config::load();
        let ws = workspace(&cfg).ok_or("no project folder is chosen")?;
        let argv = check::candidates(&ws)
            .into_iter()
            .nth(index)
            .filter(|argv| check::label(argv) == label)
            .ok_or("that check is no longer offered for this folder — choose again")?;
        cfg.set_check(&ws.root_display(), Some(argv));
        config::save(&cfg)?;
        Ok(folder(&ws, &cfg))
    })
    .await
    .map_err(|e| format!("choosing the check failed: {e}"))?
}

/// Stop offering the model a check in the current folder.
#[tauri::command]
async fn clear_check() -> Result<Folder, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let mut cfg = config::load();
        let ws = workspace(&cfg).ok_or("no project folder is chosen")?;
        cfg.set_check(&ws.root_display(), None);
        config::save(&cfg)?;
        Ok(folder(&ws, &cfg))
    })
    .await
    .map_err(|e| format!("clearing the check failed: {e}"))?
}

/// Turn editing off. Reads keep working, one approved file at a time.
#[tauri::command]
async fn clear_project_folder() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(|| {
        let mut cfg = config::load();
        cfg.workspace = None;
        config::save(&cfg)
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
            project_folder,
            choose_project_folder,
            clear_project_folder,
            load_model,
            unload_model,
            set_model,
            system_accent,
            recent_folders,
            use_recent_folder,
            check_options,
            set_check,
            clear_check,
            stop
        ])
        .run(tauri::generate_context!())
        .expect("error while running Mímir");
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn a_check_ticket_cannot_be_redeemed_as_a_read() {
        // A read of a file called "cargo check" must not run on a check's approval.
        let mut slot = check_ticket();
        assert!(redeem_in(&mut slot, "t", "read_file", "cargo check").is_err());
    }
}
