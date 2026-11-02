//! Mimir — a local chat app sharing Vidar's model.
//!
//! **Almost nothing from Vidar's `lib.rs` belongs here.** That file is full of
//! Win32 work which exists only because the popup is a click-through overlay that
//! never takes focus: capture exclusion, `SW_SHOWNOACTIVATE`, a low-level mouse
//! hook, a tray icon, global shortcuts. Mimir is an ordinary window — focusable,
//! decorated, resizable, in the taskbar, visible on a screen share. Copying any of
//! that here would be a bug.
//!
//! What does carry over is the shape of the streaming: tokens go up through a
//! `Channel`, never as the command's return value.
//!
//! **The conversation lives in the frontend for now.** `send` takes the whole
//! message list rather than keeping turns in Rust state the way Vidar does —
//! Vidar's popup is stateless between summons, while this window owns its thread.
//! G4 is what moves storage into Rust properly.

use base64::prelude::*;
use serde::Serialize;
use tauri::ipc::Channel;

use bifrost_core::agent::run::edits_lock;
use bifrost_core::agent::send::Calibration;
use bifrost_core::agent::stop::{next_stop, stop_all, stopped};
use bifrost_core::agent::ticket::PendingCall;
use bifrost_core::agent::turn::{self, Card, Decision, Frontend, Patch, RoundNumbers};
use bifrost_core::agent::workspace;
use bifrost_core::chat::{self, Message, Options, StreamChunk};
use bifrost_core::{check, claude, config, edit, instructions, ollama, store, vram};

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
/// channel so the window can say what the wait is. A tool turn does not come through
/// here: `bifrost_core::agent::turn` says why its first round waits, and the round
/// starts Ollama itself.
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
    let id = next_stop();

    // Every bifrost_core call goes off the async worker. It is blocking by design,
    // and calling it from an async command drops a tokio runtime inside an async
    // context and kills the worker — which already cost Vidar a request that
    // hung forever on "Reading your screen…".
    let cfg = tauri::async_runtime::spawn_blocking(config::load)
        .await
        .unwrap_or_default();
    let model = model.unwrap_or(cfg.model);

    start_ollama(Some(&on_event)).await?;

    // Cold? Say so before the wait, not after. Checked rather than assumed — the
    // model may have been warmed from Vidar's tray, from `bifrost warm`, or simply
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
    // why bifrost_core::chat::stream takes a callback rather than being async.
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

/// What the stream callback returns to cancel, so `send` can tell a stop from
/// a real error.
const STOPPED: &str = "stopped";

/// Stop the answer that is streaming now.
///
/// Takes effect at the next token. The window stops showing the answer at once
/// either way, so a model still loading — which cannot be interrupted — only
/// costs the load, not a wait.
///
/// `page` and `request` name the request being stopped (see [`STOPPED_REQUEST`]),
/// so a tool turn whose `run_turn` has not reached Rust yet is stopped all the same.
#[tauri::command]
fn stop(page: Option<String>, request: Option<u64>) {
    // Named before anything else, so a turn that starts after this finds it.
    if let (Some(page), Some(request)) = (page, request) {
        name_stopped(page, request);
    }
    stop_all();
    // A tool turn waiting on a card is answered with a refusal, which the loop —
    // seeing Stop — sends nowhere. What the webview's own `resolve(false)` did when
    // the loop was still there.
    answer_waiting();
}

/// The last request a Stop named: the page that sent it — a name each page load
/// makes for itself — and that page's number for the request.
///
/// **A Stop can reach Rust before the turn it stops has begun.** The window frees at
/// once, while a `run_turn` sent a moment earlier may still be on its way, and a turn
/// that takes its place on the Stop counter after the Stop never sees it on the
/// counter — it would run on with nobody watching, its cards still answerable. So a
/// turn also asks whether its own request was named, which holds whichever of the
/// two messages arrives first. The page is part of the name because a reload starts
/// its numbering again.
static STOPPED_REQUEST: std::sync::Mutex<Option<(String, u64)>> = std::sync::Mutex::new(None);

/// Write down that `page`'s `request` was stopped. A page's requests only count up,
/// so the highest one stopped is all there is to remember.
fn name_stopped(page: String, request: u64) {
    let mut last = STOPPED_REQUEST.lock().unwrap_or_else(|e| e.into_inner());
    if !matches!(&*last, Some((p, r)) if *p == page && *r >= request) {
        *last = Some((page, request));
    }
}

fn request_stopped(page: &str, request: u64) -> bool {
    matches!(
        &*STOPPED_REQUEST.lock().unwrap_or_else(|e| e.into_inner()),
        Some((p, r)) if p == page && *r >= request
    )
}

/// Answer the card a tool turn is waiting on, if one is, with a refusal.
fn answer_waiting() {
    if let Some((_, decision)) = waiting().take() {
        let _ = decision.send(Decision::Deny);
    }
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
    /// What the family is known for, from the tag alone — see [`ollama::best_at`].
    /// Empty for a family it does not recognise.
    best_at: Vec<String>,
    /// Which apps follow this tag: "Vidar" for the shared model, "Hugin" for the
    /// coding model (which is also Mimir's coding offer). Can be both.
    roles: Vec<String>,
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
                let mut roles = Vec::new();
                if same_tag(&m.name, &cfg.model) {
                    roles.push("Vidar".to_string());
                }
                if same_tag(&m.name, &cfg.coding_model) {
                    roles.push("Hugin".to_string());
                }
                ModelInfo {
                    best_at: ollama::best_at(&m.name).into_iter().map(String::from).collect(),
                    roles,
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
/// "current model", so writing it here is what lets the CLI and (from G3) Vidar
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
/// The config is written *before* the load: Vidar decides whether to
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
// Storage lives in bifrost_core::store, one JSON file per conversation beside
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

/// What a tool turn tells the window, as it happens, in the order the loop does
/// things. Sent through a `Channel` like [`StreamEvent`], and applied by the
/// window strictly one after another.
#[derive(Serialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "event", content = "data")]
enum TurnEvent {
    /// What the turn is waiting for, such as `Starting Ollama…`; empty clears it.
    Status { text: String },
    /// What a round cost: the meter, the calibration the next turn starts from, and
    /// how many old results were set aside to fit **this** round.
    Round { prompt_eval_count: u32, prompt_chars: u64, set_aside: usize, fits: bool },
    /// The model's words.
    Answer { content: String, prompt_eval_count: u32, eval_count: u32 },
    /// Something to tell the person: why the turn stopped, or the bound it hit.
    Error { text: String },
    /// A tool card to show. `id` is how `Update` and [`decide`] name it.
    /// Boxed: a card is far larger than every other event.
    Card { id: usize, card: Box<Card> },
    /// A change to a card shown earlier.
    Update { id: usize, patch: Patch },
    /// Save the thread as it now stands: a Stop landed with a card on screen that
    /// the Stop's own save may have missed.
    Save,
    /// The turn is over and nothing more will come. Sent last of all, so the window
    /// can wait for it before it saves: the channel keeps its order, but a large
    /// event can still be on its way when the command itself returns.
    End,
}

/// The card a tool turn is waiting on, and where to send what the person decides.
/// One for the whole process, like the ticket it guards: a turn waits on one card
/// at a time.
static WAITING: std::sync::Mutex<Option<(usize, std::sync::mpsc::Sender<Decision>)>> =
    std::sync::Mutex::new(None);

/// Card ids, for the whole process rather than per turn, so a click on a card from
/// an earlier turn can never answer a new one.
static CARD_IDS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn waiting() -> std::sync::MutexGuard<'static, Option<(usize, std::sync::mpsc::Sender<Decision>)>> {
    WAITING.lock().unwrap_or_else(|e| e.into_inner())
}

/// The window, as the tool turn's [`Frontend`]: everything the loop shows goes up the
/// channel, and a decision comes back through [`decide`].
struct Webview {
    events: Channel<TurnEvent>,
    /// The Stop number taken when the turn began: a Stop pressed since has stopped it.
    stop: u64,
    /// The page that started the turn, and its number for the request: a Stop that
    /// named them stopped it, even one that reached Rust first.
    page: String,
    request: u64,
}

impl Frontend for Webview {
    fn stopped(&self) -> bool {
        stopped(self.stop) || request_stopped(&self.page, self.request)
    }

    fn status(&mut self, text: &str) {
        let _ = self.events.send(TurnEvent::Status { text: text.to_string() });
    }

    fn round(&mut self, numbers: RoundNumbers) {
        if numbers.recovered {
            println!("[mimir] a malformed tool call was recovered through the raw retry");
        }
        if numbers.fed_back {
            println!("[mimir] a tool call Ollama could not read was sent back to the model");
        }
        let _ = self.events.send(TurnEvent::Round {
            prompt_eval_count: numbers.prompt_eval_count,
            prompt_chars: numbers.prompt_chars,
            set_aside: numbers.set_aside,
            fits: numbers.fits,
        });
    }

    fn answer(&mut self, content: &str, prompt_eval_count: u32, eval_count: u32) {
        let _ = self.events.send(TurnEvent::Answer { content: content.to_string(), prompt_eval_count, eval_count });
    }

    fn error(&mut self, message: &str) {
        let _ = self.events.send(TurnEvent::Error { text: message.to_string() });
    }

    fn card(&mut self, card: Card) -> usize {
        let id = CARD_IDS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let _ = self.events.send(TurnEvent::Card { id, card: Box::new(card) });
        id
    }

    fn update(&mut self, card: usize, patch: Patch) {
        let _ = self.events.send(TurnEvent::Update { id: card, patch });
    }

    fn decide(&mut self, card: usize, _call: &PendingCall) -> Decision {
        let (send, receive) = std::sync::mpsc::channel();
        *waiting() = Some((card, send));
        // A Stop that landed before the slot was filled found nothing to answer.
        if self.stopped() {
            waiting().take();
            return Decision::Deny;
        }
        // A sender dropped without a word is a refusal, never an approval.
        receive.recv().unwrap_or(Decision::Deny)
    }

    fn save(&mut self) {
        let _ = self.events.send(TurnEvent::Save);
    }
}

/// Run one tool turn: the loop in `bifrost_core::agent::turn`, shared with Hugin and
/// the probes, with this window as its frontend.
///
/// The model is asked, a call it makes is shown as a card, a call that needs a person
/// waits for [`decide`], and what ran goes back to the model — round after round,
/// until it answers, a bound is hit or Stop is pressed. Everything the window shows
/// comes up `on_event` as it happens. `Err` is a request that could not be made or a
/// call that could not be run at all; the window shows it, as it shows any failed
/// request.
///
/// `page` and `request` are the window's names for this request, which a Stop
/// repeats ([`STOPPED_REQUEST`]).
#[tauri::command]
async fn run_turn(
    messages: Vec<Message>,
    model: Option<String>,
    calibration: Option<Calibration>,
    page: String,
    request: u64,
    on_event: Channel<TurnEvent>,
) -> Result<(), String> {
    // Taken before anything is awaited, so a Stop pressed at once still reaches it.
    let stop = next_stop();
    tauri::async_runtime::spawn_blocking(move || {
        let mut window = Webview { events: on_event, stop, page, request };
        let mut convo = messages;
        let done = turn::run_turn(&mut convo, model.as_deref(), calibration, &mut window);
        let _ = window.events.send(TurnEvent::End);
        done
    })
    .await
    .map_err(|e| format!("tool turn failed: {e}"))?
}

/// Answer the card a tool turn is waiting on. A click on any other card — one from an
/// earlier turn, or one already answered — changes nothing.
#[tauri::command]
fn decide(card: usize, allow: bool) {
    let mut slot = waiting();
    if !matches!(&*slot, Some((id, _)) if *id == card) {
        return;
    }
    if let Some((_, decision)) = slot.take() {
        let _ = decision.send(if allow { Decision::Allow } else { Decision::Deny });
    }
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
        app.dialog().file().set_title("Choose the folder Mimir may edit").blocking_pick_folder()
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

// ── Check with Claude ───────────────────────────────────────────────────────

/// What the settings dialog and the turn's button need to know about Check with Claude.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeStatus {
    /// The switch. Off until the user turns it on.
    checks: bool,
    /// Whether the `claude` command is there and signed in. Only when asked for
    /// (`probe`) **and the switch is on**: it runs `claude auth status`, which makes no
    /// model call but starts a process of the user's, and nothing about `claude` is
    /// looked at while the feature is off.
    command: Option<claude::Status>,
}

/// The switch, and — with `probe`, when the switch is on — the state of the `claude`
/// command.
#[tauri::command]
async fn claude_status(probe: bool) -> Result<ClaudeStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let cfg = config::load();
        let command = (probe && cfg.claude_checks).then(|| claude::status(cfg.claude_command.as_deref()));
        ClaudeStatus { checks: cfg.claude_checks, command }
    })
    .await
    .map_err(|e| format!("reading the Claude status failed: {e}"))
}

/// Turn Check with Claude on or off. **Off by default.** Takes only the switch.
#[tauri::command]
async fn set_claude_checks(on: bool) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || config::update(|cfg| cfg.claude_checks = on))
        .await
        .map_err(|e| format!("changing the Claude setting failed: {e}"))?
}

/// What Check with Claude would send for a turn, shown before it is sent.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ClaudePreview {
    files: usize,
    /// The size of everything sent — the question and the diff.
    bytes: usize,
    /// The question was longer than one check sends and is cut.
    question_cut: bool,
    /// A fingerprint of the exact text, which [`claude_check`] must be given back.
    hash: String,
}

/// What [`claude_check`] would send for this question and these changes: the prompt
/// is built here for the preview and again at send time, from the same function.
fn prepare_claude(question: &str, ids: &[String]) -> Result<claude::Prepared, String> {
    if ids.len() > MAX_TURN_UNDO {
        return Err(format!("a turn of more than {MAX_TURN_UNDO} edits cannot be checked at once"));
    }
    let backups = edit::backups_dir().ok_or("APPDATA is not set")?;
    let _edits = edits_lock();
    claude::prepare(question, ids, &backups)
}

/// How much [`claude_check`] would send for this question and these changes.
#[tauri::command]
async fn claude_preview(question: String, undo_ids: Vec<String>) -> Result<ClaudePreview, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let p = prepare_claude(&question, &undo_ids)?;
        Ok(ClaudePreview { files: p.files, bytes: p.bytes, question_cut: p.question_cut, hash: p.hash })
    })
    .await
    .map_err(|e| format!("building the preview failed: {e}"))?
}

/// Stop for Check with Claude, **apart from the Stop counter** a streamed answer or a
/// tool turn is on: pressing Stop on a turn must not end a check, nor Stop on a check
/// end a turn.
///
/// A check is named by its page and the page's number for it, as a turn's Stop is
/// ([`STOPPED_REQUEST`]), so a Stop that reaches Rust before its check has begun still
/// stops it. The epoch is raised when a page loads: a check belonged to the page before.
static CLAUDE_STOPPED: std::sync::Mutex<Option<(String, u64)>> = std::sync::Mutex::new(None);
static CLAUDE_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn claude_name_stopped(page: String, request: u64) {
    let mut last = CLAUDE_STOPPED.lock().unwrap_or_else(|e| e.into_inner());
    if !matches!(&*last, Some((p, r)) if *p == page && *r >= request) {
        *last = Some((page, request));
    }
}

fn claude_request_stopped(page: &str, request: u64) -> bool {
    matches!(
        &*CLAUDE_STOPPED.lock().unwrap_or_else(|e| e.into_inner()),
        Some((p, r)) if p == page && *r >= request
    )
}

/// **The one call to the cloud:** ask Claude to check a turn's changes against the
/// question that began it. Reached only by the button, and refused if the switch is
/// off. `Ok(None)` is a Stop.
///
/// `hash` is the fingerprint the preview returned. The prompt is built again here, and
/// if the files have changed since the user was shown what would go, nothing is sent.
#[tauri::command]
async fn claude_check(
    question: String,
    undo_ids: Vec<String>,
    hash: String,
    page: String,
    request: u64,
) -> Result<Option<String>, String> {
    let epoch = CLAUDE_EPOCH.load(std::sync::atomic::Ordering::SeqCst);
    tauri::async_runtime::spawn_blocking(move || {
        let stopped = || {
            claude_request_stopped(&page, request) || CLAUDE_EPOCH.load(std::sync::atomic::Ordering::SeqCst) != epoch
        };
        if stopped() {
            return Ok(None);
        }
        let cfg = config::load();
        if !cfg.claude_checks {
            return Err("Claude checks are off. Turn them on in the settings first.".to_string());
        }
        let prepared = prepare_claude(&question, &undo_ids)?;
        if prepared.hash != hash {
            return Err("The files changed since the preview — check again.".to_string());
        }
        match claude::run(cfg.claude_command.as_deref(), &prepared.text, stopped) {
            Ok(answer) => Ok(Some(answer)),
            Err(claude::RunError::Stopped) => Ok(None),
            Err(claude::RunError::Failed(why)) => Err(why),
        }
    })
    .await
    .map_err(|e| format!("the Claude check failed: {e}"))?
}

/// Stop the Claude check `request` of `page`, running or not yet begun.
#[tauri::command]
fn claude_stop(page: String, request: u64) {
    claude_name_stopped(page, request);
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
/// to write to — without this, everything printed vanishes. Vidar's `lib.rs`
/// has the twin of this function writing `vidar.log`, for the same reason.
///
/// **Two copies of twenty lines, deliberately.** It is app-level plumbing: each
/// app picks its own filename and its own answer to whether it is windowless at
/// all. `bifrost-core`'s stated rule is that nothing in it prints, and giving it a
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

fn report_migration(m: bifrost_core::paths::Migration) {
    use bifrost_core::paths::Migration;
    match m {
        Migration::Nothing => {}
        Migration::Moved => println!("[mimir] moved %APPDATA%\\odin to %APPDATA%\\bifrost"),
        Migration::BothExist => eprintln!(
            "[mimir] WARNING: %APPDATA%\\odin still holds data but %APPDATA%\\bifrost already exists; using bifrost, nothing moved"
        ),
        Migration::Blocked(e) => {
            eprintln!("[mimir] WARNING: could not move %APPDATA%\\odin yet ({e}); still using it")
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    if let Some(log) = redirect_log_to_file() {
        println!("[mimir] log: {}", log.display());
    }
    // Before anything reads the config: the data folder moved from
    // %APPDATA%\odin to %APPDATA%\bifrost at the rename, and the first app to
    // start moves it. After the log redirect, so what happened is written down.
    report_migration(bifrost_core::paths::migrate());

    tauri::Builder::default()
        // Used only from Rust, by choose_project_folder. The capability file grants
        // the webview no dialog permission, so a page cannot open a picker itself.
        .plugin(tauri_plugin_dialog::init())
        // A page that loads — at start, or after a reload — owns nothing that was
        // running: a turn, a stream or a check belonged to the page before it, whose
        // channel is gone. Each is stopped, and a card left waiting is refused, as
        // Stop does. Without this a turn waiting on a card would wait for ever, and
        // the next turn's card would take its place and set it going again unwatched.
        .on_page_load(|_, payload| {
            if matches!(payload.event(), tauri::webview::PageLoadEvent::Started) {
                stop_all();
                answer_waiting();
                // A check belonged to the page before, which is gone.
                CLAUDE_EPOCH.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        })
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
            run_turn,
            decide,
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
            claude_status,
            set_claude_checks,
            claude_preview,
            claude_check,
            claude_stop,
            stop
        ])
        .run(tauri::generate_context!())
        .expect("error while running Mimir");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undoing_a_turn_of_more_than_the_limit_is_refused_before_anything_runs() {
        let ids: Vec<String> = (0..=MAX_TURN_UNDO).map(|i| format!("{i:x}")).collect();
        let err = tauri::async_runtime::block_on(undo_turn(ids)).unwrap_err();
        assert!(err.contains("more than 64"), "{err}");
    }

    /// A Stop that reaches Rust before its turn does still stops it: the turn finds
    /// its own request named. The page's next request, and another page's, are not.
    #[test]
    fn a_stop_that_arrives_before_its_turn_still_stops_it() {
        name_stopped("page-a".into(), 3);
        assert!(request_stopped("page-a", 3) && request_stopped("page-a", 2));
        assert!(!request_stopped("page-a", 4), "the request after the stopped one");
        assert!(!request_stopped("page-b", 3), "another page's numbering");
        // An earlier request named late does not take back a later one.
        name_stopped("page-a".into(), 1);
        assert!(request_stopped("page-a", 3));
        // After a reload the new page starts its numbering again.
        name_stopped("page-b".into(), 1);
        assert!(request_stopped("page-b", 1) && !request_stopped("page-b", 2));
    }

    /// A check's Stop is its own: it reaches a check that has not begun, and the turns'
    /// Stop counter does not touch it.
    #[test]
    fn a_check_has_a_stop_of_its_own() {
        claude_name_stopped("page-c".into(), 2);
        assert!(claude_request_stopped("page-c", 2) && claude_request_stopped("page-c", 1));
        assert!(!claude_request_stopped("page-c", 3) && !claude_request_stopped("page-d", 2));
        stop_all();
        assert!(!claude_request_stopped("page-c", 3), "a turn's Stop does not stop a check");
        let n = next_stop();
        claude_name_stopped("page-c".into(), 3);
        assert!(!stopped(n), "a check's Stop does not stop a turn");
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
