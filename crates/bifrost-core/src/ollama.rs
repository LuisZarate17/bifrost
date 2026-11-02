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
    /// Bytes the loaded model takes in all, on the card and off it.
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub digest: Option<String>,
    #[serde(default)]
    pub size_vram: Option<u64>,
    #[serde(default)]
    pub expires_at: Option<String>,
}

impl LoadedModel {
    pub fn is(&self, tag: &str) -> bool {
        self.name.as_deref() == Some(tag) || self.model.as_deref() == Some(tag)
    }

    /// The share of the model that sits off the card, in `0.0..=1.0`: `(size - size_vram) / size`.
    /// `None` when either number is missing or `size` is 0.
    pub fn cpu_share(&self) -> Option<f64> {
        cpu_share(self.size?, self.size_vram?)
    }

    pub fn size_vram_mib(&self) -> Option<u64> {
        self.size_vram.map(|b| b / (1024 * 1024))
    }

    /// Whether the model is pinned — loaded with `keep_alive: -1`, as [`load`]
    /// does — rather than due to unload a few minutes after its last request.
    ///
    /// Ollama reports a pin as an expiry roughly 290 years out (2318 on
    /// 2026-09-27), and an ordinary request as minutes from now. A chat that loads
    /// the model leaves it resident but not pinned, so "resident" alone is not
    /// enough to skip warming: that is how `bifrost warm` and the tray came to say a
    /// model was ready while it was five minutes from being dropped.
    pub fn pinned(&self) -> bool {
        self.expires_at
            .as_deref()
            .and_then(|s| s.get(..4))
            .and_then(|year| year.parse::<u32>().ok())
            .is_some_and(|year| year >= 2200)
    }
}

/// `(size - size_vram) / size`, clamped to `0.0..=1.0`; `None` for a `size` of 0.
pub fn cpu_share(size: u64, size_vram: u64) -> Option<f64> {
    if size == 0 {
        return None;
    }
    Some((size.saturating_sub(size_vram) as f64 / size as f64).clamp(0.0, 1.0))
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

/// Everything one generate call is worth knowing about.
///
/// Time to first token is measured **here**, not taken from Ollama, because it
/// is the number the product lives or dies by: how long the screen sits blank
/// after you press the hotkey. Ollama's own `total_duration` does not answer
/// that.
#[derive(Debug, Clone)]
pub struct Measured {
    pub answer: String,
    pub ttft: Duration,
    pub total: Duration,
    pub prompt_eval_count: u32,
    pub eval_count: u32,
    /// Nanoseconds Ollama spent loading the model for this request. Nonzero here
    /// means the run paid a cold load, which makes its timings incomparable with
    /// a warm one — the benchmark's job is to notice that, not to hide it.
    pub load_ns: u64,
    pub done_reason: String,
}

impl Measured {
    /// Tokens per second of decode, which is what "how fast does it write"
    /// actually means. Prefill is excluded — that is what `ttft` covers.
    pub fn decode_tps(&self) -> f64 {
        let secs = self.total.as_secs_f64() - self.ttft.as_secs_f64();
        if secs > 0.0 { self.eval_count as f64 / secs } else { 0.0 }
    }
}

#[derive(Deserialize)]
struct StreamChunk {
    #[serde(default)]
    response: String,
    #[serde(default)]
    done: bool,
    #[serde(default)]
    prompt_eval_count: Option<u32>,
    #[serde(default)]
    eval_count: Option<u32>,
    #[serde(default)]
    load_duration: Option<u64>,
    #[serde(default)]
    done_reason: Option<String>,
}

/// Run one generate and measure it, optionally against an image.
///
/// Streams rather than waiting for the whole answer, because time-to-first-token
/// cannot be measured any other way — a non-streaming call only tells you when
/// it finished.
pub fn generate_measured(
    tag: &str,
    prompt: &str,
    image_png_base64: Option<&str>,
    num_ctx: u32,
    timeout: Duration,
) -> Result<Measured, String> {
    use std::io::{BufRead, BufReader};

    let mut body = serde_json::json!({
        "model": tag,
        "prompt": prompt,
        "stream": true,
        "options": { "num_ctx": num_ctx },
    });
    // `images` sits at the top level while `num_ctx` goes inside `options`.
    // Either in the wrong place fails silently — the model simply never sees the
    // picture and answers about nothing.
    if let Some(b64) = image_png_base64 {
        body["images"] = serde_json::json!([b64]);
    }

    let started = Instant::now();
    let response = client(timeout)?
        .post(url("/api/generate"))
        .json(&body)
        .send()
        .map_err(unreachable)?;

    if !response.status().is_success() {
        return Err(format!("Ollama returned HTTP {}", response.status()));
    }

    let mut answer = String::new();
    let mut ttft: Option<Duration> = None;
    // Ollama sends its metrics only in the final done object. If the stream ends
    // without one - seen when a model degenerates into a repetition loop and the
    // connection drops mid-answer - every metric stays at zero, and a benchmark
    // that writes those zeros down reports a model at 0 tok/s as though it had
    // measured that. Track it, and refuse.
    let mut saw_done = false;
    let mut m = Measured {
        answer: String::new(),
        ttft: Duration::ZERO,
        total: Duration::ZERO,
        prompt_eval_count: 0,
        eval_count: 0,
        load_ns: 0,
        done_reason: String::new(),
    };

    for line in BufReader::new(response).lines() {
        let line = line.map_err(|e| format!("read failed: {e}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let chunk: StreamChunk =
            serde_json::from_str(&line).map_err(|e| format!("bad JSON from Ollama: {e}"))?;

        if !chunk.response.is_empty() && ttft.is_none() {
            ttft = Some(started.elapsed());
        }
        answer.push_str(&chunk.response);

        if chunk.done {
            saw_done = true;
            m.prompt_eval_count = chunk.prompt_eval_count.unwrap_or(0);
            m.eval_count = chunk.eval_count.unwrap_or(0);
            m.load_ns = chunk.load_duration.unwrap_or(0);
            m.done_reason = chunk.done_reason.unwrap_or_default();
        }
    }

    if !saw_done {
        return Err(format!(
            "stream ended without the final metrics object after {} chars - the run produced no usable numbers",
            answer.len()
        ));
    }

    m.answer = answer;
    m.ttft = ttft.unwrap_or_default();
    m.total = started.elapsed();
    Ok(m)
}

/// What Ollama itself timed for one generate: its own durations, in nanoseconds, so prefill and
/// decode speed can be told apart without a stream. See [`generate_timed`].
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct Timed {
    #[serde(default)]
    pub prompt_eval_count: u32,
    #[serde(default)]
    pub prompt_eval_duration: u64,
    #[serde(default)]
    pub eval_count: u32,
    #[serde(default)]
    pub eval_duration: u64,
    #[serde(default)]
    pub load_duration: u64,
    #[serde(default)]
    pub total_duration: u64,
}

impl Timed {
    /// Prompt tokens read per second, from Ollama's own prompt-eval clock.
    pub fn prefill_tps(&self) -> f64 {
        per_second(self.prompt_eval_count, self.prompt_eval_duration)
    }

    /// Tokens written per second, from Ollama's own eval clock.
    pub fn decode_tps(&self) -> f64 {
        per_second(self.eval_count, self.eval_duration)
    }
}

fn per_second(count: u32, nanos: u64) -> f64 {
    if nanos == 0 { 0.0 } else { count as f64 / (nanos as f64 / 1e9) }
}

/// One non-streaming generate, at temperature 0 and `num_predict` tokens at most, answered with
/// Ollama's own durations. For a speed measurement: the time to first token is not needed, and
/// the prefill and decode clocks the final object carries are the ones to trust.
pub fn generate_timed(tag: &str, prompt: &str, num_ctx: u32, num_predict: u32, timeout: Duration) -> Result<Timed, String> {
    let body = serde_json::json!({
        "model": tag,
        "prompt": prompt,
        "stream": false,
        "options": { "num_ctx": num_ctx, "num_predict": num_predict, "temperature": 0 },
    });

    let response = client(timeout)?
        .post(url("/api/generate"))
        .json(&body)
        .send()
        .map_err(unreachable)?;

    if !response.status().is_success() {
        return Err(format!("Ollama returned HTTP {}", response.status()));
    }

    response.json::<Timed>().map_err(|e| format!("could not parse the generate reply: {e}"))
}

// The prompt the app ships with moved to `config::DEFAULT_PROMPT` at Before-G0
// step 3, when the shared config gained somewhere better to keep it. The reason
// it belonged in this crate at all is unchanged: the benchmark has to measure the
// same words the product uses, and A2 found the prompt moved answer quality more
// than the choice between a 4B and an 8B model did.

/// How long to wait for a freshly started Ollama to answer.
///
/// A1 measured a cold start well under this; the slack is for a machine that has
/// just booted and is still busy.
const START_TIMEOUT: Duration = Duration::from_secs(30);
const START_POLL: Duration = Duration::from_millis(400);

/// Is Ollama answering?
pub fn is_running() -> bool {
    client(Duration::from_secs(2))
        .map(|c| c.get(url("/api/tags")).send().is_ok())
        .unwrap_or(false)
}

/// A model on disk, whether or not it is loaded.
///
/// Distinct from [`LoadedModel`], which is what `/api/ps` reports about the
/// runners currently in memory. This is the catalogue; that is the inventory.
#[derive(Debug, Clone, Deserialize)]
pub struct InstalledModel {
    pub name: String,
    /// Bytes on disk. Not what it costs resident — the same model is 6,233 MiB
    /// at `num_ctx` 8192 and 9,737 MiB at 32768 (measured 2026-09-19).
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub details: ModelDetails,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ModelDetails {
    #[serde(default)]
    pub family: String,
    #[serde(default)]
    pub parameter_size: String,
    #[serde(default)]
    pub quantization_level: String,
}

/// What `/api/show` says about one model.
#[derive(Debug, Clone, Default)]
pub struct ModelDetail {
    /// Typically some of `completion`, `vision`, `tools`, `embedding`,
    /// `thinking`. **Declared, not demonstrated** — G0 measured `llama3.1:8b`
    /// reporting `tools` and then firing a spurious call on every negative case.
    /// Good enough to label a picker and to refuse cleanly; not evidence of
    /// competence.
    pub capabilities: Vec<String>,
    /// The largest context the model itself supports, which is usually far more
    /// than is affordable: `qwen3-vl:8b` reports 262,144 against the 32,768 that
    /// fits on this card. Useful for telling "this model cannot do that" apart
    /// from "this card cannot afford it".
    pub max_context: Option<u64>,
    pub family: String,
    pub parameter_size: String,
    pub quantization_level: String,
    /// The word after `PARSER ` in the model's modelfile — `qwen3-coder` for
    /// that model, `qwen3-vl-instruct` for the vision ones. Tells
    /// [`crate::chat::chat_tools`] which tool-call layout the model writes,
    /// since `family` alone does not: qwen3-coder's family is `qwen3moe`, the
    /// same shape a family-only check would send down the wrong recovery path.
    pub parser: Option<String>,
}

impl ModelDetail {
    pub fn has(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|c| c == capability)
    }
}

/// What a model is best at, read off its tag alone.
///
/// **Only what the name makes plain.** A family this does not recognise gets an
/// empty list, never a guess: a wrong "best at" is worse than none. Matching is
/// by substring, case-insensitive, so `qwen3-vl:8b-instruct-q8_0` and
/// `Qwen3-VL:8B` read the same. "General chat" is the fallback for the plain
/// chat families and is claimed only when nothing more specific matched.
pub fn best_at(tag: &str) -> Vec<&'static str> {
    let t = tag.to_lowercase();
    let any = |needles: &[&str]| needles.iter().any(|n| t.contains(n));

    let mut out = Vec::new();
    if any(&["coder", "coding", "devstral", "codestral", "codellama", "starcoder"]) {
        out.push("Coding");
    }
    if any(&["-vl", "llava", "vision", "minicpm-v", "moondream"]) {
        out.push("Screenshots");
    }
    if any(&["gpt-oss", "deepseek-r1", "qwq", "magistral"]) {
        out.push("Reasoning");
    }
    // By how the family starts, so `dolphin3` is not taken for `phi`.
    let family = t.split(':').next().unwrap_or("");
    if out.is_empty() && ["llama", "mistral", "gemma", "phi", "qwen3", "qwen2.5"].iter().any(|f| family.starts_with(f)) {
        out.push("General chat");
    }
    out
}

/// The word after the first `PARSER ` line in a modelfile, or `None` if there
/// is no such line. Pulled into its own function so it is testable without a
/// running Ollama.
fn parser_from_modelfile(modelfile: &str) -> Option<String> {
    modelfile
        .lines()
        .find_map(|line| line.strip_prefix("PARSER "))
        .map(|rest| rest.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Ask Ollama what a model can do.
///
/// One round trip per model, so a caller listing several pays for each. **Cache
/// it per tag where that matters** — a model's capabilities do not change between
/// calls, and G3 needs the answer on every summon without spending a round trip
/// on it.
pub fn show(tag: &str) -> Result<ModelDetail, String> {
    #[derive(Deserialize)]
    struct Shown {
        #[serde(default)]
        capabilities: Vec<String>,
        #[serde(default)]
        details: ModelDetails,
        #[serde(default)]
        model_info: serde_json::Map<String, serde_json::Value>,
        #[serde(default)]
        modelfile: String,
    }

    let response = client(SHORT_TIMEOUT)?
        .post(url("/api/show"))
        .json(&serde_json::json!({ "model": tag }))
        .send()
        .map_err(unreachable)?;

    if !response.status().is_success() {
        return Err(format!("/api/show returned {} for {tag}", response.status()));
    }

    let shown: Shown = response
        .json()
        .map_err(|e| format!("parsing /api/show for {tag}: {e}"))?;

    // The context key is namespaced by architecture — `qwen3vl.context_length`,
    // `llama.context_length` — so it is found by suffix rather than by name.
    // Hard-coding one family would silently return None for every other model.
    let max_context = shown
        .model_info
        .iter()
        .find(|(k, _)| k.ends_with(".context_length"))
        .and_then(|(_, v)| v.as_u64());

    Ok(ModelDetail {
        capabilities: shown.capabilities,
        max_context,
        family: shown.details.family,
        parameter_size: shown.details.parameter_size,
        quantization_level: shown.details.quantization_level,
        parser: parser_from_modelfile(&shown.modelfile),
    })
}

/// [`show`], remembered per tag.
///
/// **A model's capabilities do not change between calls**, and Vidar asks on
/// every summon — where the popup's whole appeal is that it is instant, and an
/// extra round trip before the screenshot even goes out is exactly the wrong
/// place to spend one.
///
/// Never expires inside a process. Re-pulling a tag while an app is running
/// could in principle change what it reports; that is rare enough, and cheap
/// enough to fix by restarting, that an invalidation story would cost more than
/// it is worth.
///
/// **Failures are not cached.** Ollama being down is temporary, and remembering
/// "unknown" would outlive the outage that caused it.
pub fn show_cached(tag: &str) -> Result<ModelDetail, String> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    static CACHE: OnceLock<Mutex<HashMap<String, ModelDetail>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));

    if let Ok(map) = cache.lock()
        && let Some(hit) = map.get(tag) {
            return Ok(hit.clone());
        }

    let detail = show(tag)?;
    if let Ok(mut map) = cache.lock() {
        map.insert(tag.to_string(), detail.clone());
    }
    Ok(detail)
}

/// Every model installed, newest-sorted by Ollama itself.
///
/// `/api/tags` was already being pinged as a liveness check in [`is_running`];
/// this actually reads what it returns. **It reports nothing about capability** —
/// whether a model can see images or call tools comes from `/api/show`, which is
/// a separate round trip per model and belongs to the labelled picker at G2.
pub fn tags() -> Result<Vec<InstalledModel>, String> {
    #[derive(Deserialize)]
    struct Tags {
        #[serde(default)]
        models: Vec<InstalledModel>,
    }

    let response = client(SHORT_TIMEOUT)?
        .get(url("/api/tags"))
        .send()
        .map_err(unreachable)?;

    if !response.status().is_success() {
        return Err(format!("/api/tags returned {}", response.status()));
    }

    response
        .json::<Tags>()
        .map(|t| t.models)
        .map_err(|e| format!("parsing /api/tags: {e}"))
}

/// Start Ollama if it is not already up, and wait until it answers.
///
/// Returns how long the start took, or `None` if it was already running.
///
/// **`ollama.exe serve`, never `ollama app.exe`** — the latter opens a blocking
/// first-run GUI and the caller would wait forever on a window nobody asked for.
/// Recorded in PLAN.md §8 after it cost real time.
///
/// Detached deliberately: Vidar may be closed while Ollama keeps serving, and a
/// server that dies with the thing that happened to start it is worse than one
/// you started yourself.
pub fn ensure_running() -> Result<Option<Duration>, String> {
    if is_running() {
        return Ok(None);
    }

    let started = Instant::now();
    spawn_detached()?;

    while started.elapsed() < START_TIMEOUT {
        if is_running() {
            return Ok(Some(started.elapsed()));
        }
        std::thread::sleep(START_POLL);
    }

    Err(format!(
        "started `ollama serve` but it did not answer within {}s",
        START_TIMEOUT.as_secs()
    ))
}

#[cfg(windows)]
fn spawn_detached() -> Result<(), String> {
    use std::os::windows::process::CommandExt;

    // CREATE_NO_WINDOW alone. The first version also passed DETACHED_PROCESS,
    // and MSDN is explicit that these two and CREATE_NEW_CONSOLE are **mutually
    // exclusive** — passing more than one is undefined, and what it did in
    // practice was put a console window on screen, which is the one thing this
    // flag existed to prevent.
    //
    // Nothing is lost by dropping DETACHED_PROCESS: Vidar is a windows-subsystem
    // application with no console of its own, so there is none for the child to
    // inherit or be tied to.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    // PATH first; the default install location is the fallback for a shell that
    // was started before Ollama was installed.
    // PATH first; the default install location is the fallback for a shell that
    // was started before Ollama was installed. Built with `join` rather than a
    // literal, so there are no path separators to escape.
    let fallback = std::env::var("LOCALAPPDATA").ok().map(|p| {
        std::path::Path::new(&p)
            .join("Programs")
            .join("Ollama")
            .join("ollama.exe")
            .to_string_lossy()
            .to_string()
    });
    let mut candidates = vec!["ollama".to_string()];
    candidates.extend(fallback);

    let mut last = String::new();
    for exe in candidates.iter().filter(|c| !c.is_empty()) {
        match std::process::Command::new(exe)
            .arg("serve")
            // Ollama outlives whatever started it, and it would otherwise inherit that
            // process's working directory — the repo, for an app launched from its shortcut.
            // It held the repo open for a week and blocked renaming the folder (2026-09-25),
            // and so did every model runner it spawned, which inherit it in turn.
            .current_dir(std::env::temp_dir())
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(_) => return Ok(()),
            Err(e) => last = format!("{exe}: {e}"),
        }
    }
    Err(format!("could not start Ollama ({last}) — install it, or run `ollama serve` yourself"))
}

#[cfg(not(windows))]
fn spawn_detached() -> Result<(), String> {
    std::process::Command::new("ollama")
        .arg("serve")
        .current_dir(std::env::temp_dir()) // see the Windows version above
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("could not start Ollama: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded(expires_at: Option<&str>) -> LoadedModel {
        LoadedModel {
            name: Some("qwen3-vl:8b-instruct-q4_K_M".into()),
            model: None,
            size: None,
            digest: None,
            size_vram: None,
            expires_at: expires_at.map(String::from),
        }
    }

    /// The shape `/api/ps` has in current Ollama releases.
    const PS_BODY: &str = r#"{"models":[{"name":"qwen3-coder:30b","model":"qwen3-coder:30b",
        "size":21474836480,"digest":"06c1097efce0431c2045fe7b2e5108366e43bee1b4603a7aded8f21689e90bca",
        "details":{"parent_model":"","format":"gguf","family":"qwen3moe","families":["qwen3moe"],
        "parameter_size":"30.5B","quantization_level":"Q4_K_M"},
        "expires_at":"2318-01-06T22:13:05.123-08:00","size_vram":17179869184,"context_length":32768}]}"#;

    #[test]
    fn ps_reports_size_and_digest() {
        let ps: PsResponse = serde_json::from_str(PS_BODY).unwrap();
        let m = &ps.models[0];
        assert!(m.is("qwen3-coder:30b"));
        assert_eq!(m.size, Some(21_474_836_480));
        assert_eq!(m.digest.as_deref(), Some("06c1097efce0431c2045fe7b2e5108366e43bee1b4603a7aded8f21689e90bca"));
        assert_eq!(m.size_vram, Some(17_179_869_184));
        assert!(m.pinned());
        assert_eq!(m.cpu_share(), Some(0.2));
    }

    #[test]
    fn ps_without_size_or_digest_still_parses() {
        let ps: PsResponse = serde_json::from_str(r#"{"models":[{"name":"a:b","size_vram":5,"expires_at":"2026-01-01T00:00:00Z"}]}"#).unwrap();
        let m = &ps.models[0];
        assert_eq!((m.size, m.digest.as_deref(), m.size_vram), (None, None, Some(5)));
        assert_eq!(m.cpu_share(), None);
        assert!(serde_json::from_str::<PsResponse>("{}").unwrap().models.is_empty());
    }

    #[test]
    fn timed_speeds_come_from_ollamas_own_clocks() {
        let t: Timed = serde_json::from_str(
            r#"{"response":"x","done":true,"prompt_eval_count":2000,"prompt_eval_duration":500000000,"eval_count":128,"eval_duration":2000000000}"#,
        )
        .unwrap();
        assert_eq!(t.prefill_tps(), 4000.0);
        assert_eq!(t.decode_tps(), 64.0);
        // No clock, no speed (rather than infinity).
        assert_eq!(Timed::default().decode_tps(), 0.0);
    }

    #[test]
    fn the_cpu_share_is_what_sits_off_the_card() {
        assert_eq!(cpu_share(100, 100), Some(0.0));
        assert_eq!(cpu_share(100, 75), Some(0.25));
        assert_eq!(cpu_share(100, 0), Some(1.0));
        // A card figure above the total (rounding in Ollama's own report) is not a negative share.
        assert_eq!(cpu_share(100, 120), Some(0.0));
        assert_eq!(cpu_share(0, 0), None);
    }

    #[test]
    fn a_pin_is_an_expiry_centuries_out() {
        // Both read from /api/ps on 2026-09-27: after `load`, and after a chat.
        assert!(loaded(Some("2319-01-06T22:13:05.123-08:00")).pinned());
        assert!(!loaded(Some("2026-09-26T23:31:09.5835022-07:00")).pinned());
    }

    #[test]
    fn no_expiry_is_not_a_pin() {
        assert!(!loaded(None).pinned());
        assert!(!loaded(Some("")).pinned());
    }

    #[test]
    fn best_at_reads_the_family_off_the_tag() {
        assert_eq!(best_at("gpt-oss:20b"), ["Reasoning"]);
        assert_eq!(best_at("qwen3-coder:30b"), ["Coding"]);
        assert_eq!(best_at("qwen3-vl:8b-instruct-q8_0"), ["Screenshots"]);
        assert_eq!(best_at("Qwen3-VL:8B"), ["Screenshots"]);
        assert_eq!(best_at("llama3.1:8b"), ["General chat"]);
        assert_eq!(best_at("qwen3:8b"), ["General chat"]);
    }

    #[test]
    fn best_at_never_guesses() {
        assert!(best_at("ornith-1.5:9b").is_empty());
        assert!(best_at("").is_empty());
        // A name that only contains a family's letters is not that family.
        assert!(best_at("dolphin3:8b").is_empty());
    }

    #[test]
    fn the_parser_line_names_the_tool_call_layout() {
        assert_eq!(
            parser_from_modelfile("FROM qwen3-coder:30b\nRENDERER qwen3-coder\nPARSER qwen3-coder\n"),
            Some("qwen3-coder".to_string())
        );
        assert_eq!(parser_from_modelfile("PARSER qwen3-vl-instruct"), Some("qwen3-vl-instruct".to_string()));
        assert_eq!(parser_from_modelfile("FROM llama3.1:8b\n"), None);
        assert_eq!(parser_from_modelfile(""), None);
    }
}
