//! Talking to Ollama: what is loaded, and loading or releasing it.
//!
//! The logic here is a port of `evals/bench.mjs`, which has been driving real
//! benchmark runs since A2 — the unload-and-poll dance in particular was worked
//! out empirically there and is not worth rediscovering.
//!
//! Nothing in this module prints. It returns data and lets `main` format it, so
//! C4's tray can call the same functions without a stdout to write to.

use std::time::{Duration, Instant};

use serde::Deserialize;

/// The base, not an endpoint. `/api/ps` needs reaching as well as `/api/generate`,
/// and a second hard-coded URL string is how those two drift apart.
pub const OLLAMA_BASE: &str = "http://127.0.0.1:11434";

/// `bench.mjs` uses 10s for `/api/ps` and 30s for the unload request.
const SHORT_TIMEOUT: Duration = Duration::from_secs(10);
const UNLOAD_TIMEOUT: Duration = Duration::from_secs(30);

/// A2 measured a 3.7s cold load for the chosen model, but A1 saw 18s reading a
/// model off cold disk after a reboot. 120s is slack, not an expectation.
const WARM_TIMEOUT: Duration = Duration::from_secs(120);

/// How long to wait for Ollama to admit the model is gone, and how often to ask.
/// Both from `bench.mjs` (UNLOAD_POLL_TIMEOUT_MS / UNLOAD_POLL_INTERVAL_MS).
const UNLOAD_POLL_TIMEOUT: Duration = Duration::from_secs(30);
const UNLOAD_POLL_INTERVAL: Duration = Duration::from_millis(1000);

pub fn url(path: &str) -> String {
    format!("{OLLAMA_BASE}{path}")
}

/// One entry from `/api/ps`.
///
/// Ollama is inconsistent about whether the tag lands in `name` or `model`, so
/// both are optional and both are checked — `bench.mjs` matches
/// `m.name === model || m.model === model` for exactly this reason.
#[derive(Deserialize, Debug)]
pub struct LoadedModel {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub size_vram: Option<u64>,
    #[serde(default)]
    pub expires_at: Option<String>,
}

impl LoadedModel {
    pub fn is(&self, tag: &str) -> bool {
        self.name.as_deref() == Some(tag) || self.model.as_deref() == Some(tag)
    }

    pub fn size_vram_mib(&self) -> Option<u64> {
        self.size_vram.map(|b| b / (1024 * 1024))
    }
}

#[derive(Deserialize)]
struct PsResponse {
    #[serde(default)]
    models: Vec<LoadedModel>,
}

fn client(timeout: Duration) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| format!("could not build HTTP client: {e}"))
}

/// Turn a transport failure into the one message that actually helps.
fn unreachable(e: reqwest::Error) -> String {
    if e.is_timeout() {
        format!("Ollama did not respond in time ({e})")
    } else {
        format!("cannot reach Ollama at {OLLAMA_BASE} — is `ollama serve` running? ({e})")
    }
}

/// What Ollama believes it has loaded. Its own claim, not the card's — see
/// `crate::vram` for the second opinion.
pub fn ps() -> Result<Vec<LoadedModel>, String> {
    let response = client(SHORT_TIMEOUT)?
        .get(url("/api/ps"))
        .send()
        .map_err(unreachable)?;

    if !response.status().is_success() {
        return Err(format!("Ollama returned HTTP {} from /api/ps", response.status()));
    }

    response
        .json::<PsResponse>()
        .map(|r| r.models)
        .map_err(|e| format!("could not parse /api/ps: {e}"))
}

pub fn find<'a>(models: &'a [LoadedModel], tag: &str) -> Option<&'a LoadedModel> {
    models.iter().find(|m| m.is(tag))
}

/// Load the model and pin it. An empty prompt is Ollama's load-only request.
///
/// CRITICAL: `options` must match what the capture path sends. Ollama keys a
/// loaded runner by the model *and* the options that change its memory layout,
/// `num_ctx` among them — so warming at the default context and then asking at
/// 8192 tears the runner down and loads it again. The warm would cost 3.7s and
/// 8GB and buy nothing, silently. `capture` printing `load 0.00s` afterwards is
/// the proof that this matched.
pub fn load(tag: &str, num_ctx: u32) -> Result<Duration, String> {
    let body = serde_json::json!({
        "model": tag,
        "prompt": "",
        "keep_alive": -1,
        "stream": false,
        "options": { "num_ctx": num_ctx },
    });

    let started = Instant::now();
    let response = client(WARM_TIMEOUT)?
        .post(url("/api/generate"))
        .json(&body)
        .send()
        .map_err(unreachable)?;

    if !response.status().is_success() {
        return Err(format!("Ollama returned HTTP {} while loading", response.status()));
    }

    Ok(started.elapsed())
}

/// Release the model, then wait until Ollama stops listing it.
///
/// Returns whether the unload was *confirmed*. A timeout is information, not a
/// crash — `bench.mjs` returns a boolean here too and lets the caller decide.
pub fn unload(tag: &str) -> Result<bool, String> {
    let body = serde_json::json!({
        "model": tag,
        "prompt": "",
        "keep_alive": 0,
        "stream": false,
    });

    let response = client(UNLOAD_TIMEOUT)?
        .post(url("/api/generate"))
        .json(&body)
        .send()
        .map_err(unreachable)?;

    if !response.status().is_success() {
        return Err(format!("Ollama returned HTTP {} while unloading", response.status()));
    }

    // The POST returns as soon as Ollama accepts it; the runner exits after.
    let deadline = Instant::now() + UNLOAD_POLL_TIMEOUT;
    while Instant::now() < deadline {
        if find(&ps()?, tag).is_none() {
            return Ok(true);
        }
        std::thread::sleep(UNLOAD_POLL_INTERVAL);
    }
    Ok(false)
}
