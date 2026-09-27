//! `/api/chat` — messages with roles, and the tool calls that ride on them.
//!
//! Separate from [`crate::ollama`], which speaks `/api/generate`. That endpoint
//! takes one prompt and one set of images and has nowhere to put a role, a
//! conversation, or a tool call. Everything Stage G builds above this — history,
//! attachments, tool calling — is a field on a message, so the messages endpoint
//! is the substrate (PLAN §0b, STAGE-G decision #1).
//!
//! Written at G0 rather than G1 on purpose: the tool-call probe needs a client,
//! and a client is far easier to get right with a probe depending on it than with
//! a whole app.
//!
//! **Non-streaming.** For a tool call that is the right shape anyway — you cannot
//! act on a call until the whole `tool_calls` array has arrived. G1 adds a
//! streaming variant for the token-by-token case, reusing B2's `Channel` pattern.
//!
//! Blocking, like the rest of this crate. Tauri callers go through
//! `spawn_blocking`.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::ollama;

/// Long enough for a cold model load plus a real answer. `ollama::WARM_TIMEOUT`
/// is 120s for the same reason: A1 saw 18s reading a model off cold disk.
const CHAT_TIMEOUT: Duration = Duration::from_secs(300);

/// One turn of the conversation.
///
/// Carries the fields it does not use yet — `images`, `tool_calls`, `tool_name`
/// — because STAGE-G decision #2 is that adding them later means migrating a
/// database already holding real conversations. They serialize away when empty,
/// so a plain text turn is still `{"role":..., "content":...}` on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,

    /// Raw base64, **no `data:` prefix** — measured across 40+ requests at A2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<String>>,

    /// Present on an assistant turn that asked for a tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,

    /// Set on a `role: "tool"` turn, naming which tool produced `content`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self::plain("user", content)
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self::plain("system", content)
    }

    /// The result of running a tool, handed back for the model to continue from.
    pub fn tool_result(name: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".to_string(),
            content: content.into(),
            images: None,
            tool_calls: None,
            tool_name: Some(name.into()),
        }
    }

    fn plain(role: &str, content: impl Into<String>) -> Self {
        Self {
            role: role.to_string(),
            content: content.into(),
            images: None,
            tool_calls: None,
            tool_name: None,
        }
    }

    /// Attach images to a user turn.
    pub fn with_images(mut self, images: Vec<String>) -> Self {
        self.images = Some(images);
        self
    }
}

/// A tool the model asked to run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Ollama supplies one; it is absent on some models, so it stays optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// **A JSON object, not a string.** Measured 2026-09-19 against Ollama
    /// 0.33.3: `{"a": 847, "b": 1259}` arrives parsed. This differs from
    /// OpenAI's API, which serializes arguments to a string the caller must
    /// parse again — code ported from there will double-parse and fail.
    #[serde(default)]
    pub arguments: serde_json::Value,
}

/// A tool offered to the model, as JSON Schema.
///
/// Build with [`Tool::function`] rather than by hand: the nesting
/// (`type`/`function`/`parameters`) is easy to get subtly wrong, and a malformed
/// schema is accepted silently and simply never called.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ToolFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFunction {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

impl Tool {
    pub fn function(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        Self {
            kind: "function".to_string(),
            function: ToolFunction {
                name: name.into(),
                description: description.into(),
                parameters,
            },
        }
    }
}

/// Sampling and context settings.
///
/// `num_ctx` is never left to Ollama on purpose: it derives its own from VRAM
/// and lands at 4k on this card, and **it keys a loaded runner by model *and*
/// context**, so asking at a value you did not warm at silently tears the runner
/// down and reloads — measured 2026-09-19 at roughly 3s each way.
///
/// `temperature` exists for the same reason `evals/` pins its prompt files:
/// **a verdict that moves between runs is not a measurement.** Use
/// [`Options::measured`] for anything whose result gets written down, and
/// [`Options::new`] in the apps, where Ollama's own sampling is what you want.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    pub num_ctx: u32,
    pub temperature: Option<f32>,
}

impl Options {
    /// Ollama's own sampling, at the given context. What the apps use.
    pub fn new(num_ctx: u32) -> Self {
        Self {
            num_ctx,
            temperature: None,
        }
    }

    /// Deterministic. What a probe or a benchmark uses.
    pub fn measured(num_ctx: u32) -> Self {
        Self {
            num_ctx,
            temperature: Some(0.0),
        }
    }
}

/// What came back, plus the numbers worth recording.
#[derive(Debug, Clone)]
pub struct ChatReply {
    pub message: Message,
    /// `~0.00s` after a real warm. Anything else means the runner was rebuilt —
    /// usually because `num_ctx` did not match what it was loaded with.
    pub load: Duration,
    pub total: Duration,
    /// Time to the first fragment. `None` from [`chat`], which waits for the
    /// whole reply and so has no first token to time.
    pub ttft: Option<Duration>,
    pub prompt_eval_count: u32,
    pub eval_count: u32,
    /// True when Ollama rejected a malformed tool call and the turn was
    /// recovered through [`chat_tools`]'s raw retry. How often this fires is
    /// how you know whether the recovery is still needed.
    pub recovered: bool,
}

impl ChatReply {
    /// The calls the model asked for, or an empty slice.
    pub fn tool_calls(&self) -> &[ToolCall] {
        self.message.tool_calls.as_deref().unwrap_or(&[])
    }
}

#[derive(Deserialize)]
struct RawReply {
    message: Message,
    #[serde(default)]
    load_duration: u64,
    #[serde(default)]
    total_duration: u64,
    #[serde(default)]
    prompt_eval_count: u32,
    #[serde(default)]
    eval_count: u32,
}

/// Send a conversation, optionally offering tools, and wait for the whole reply.
///
/// `num_ctx` is always set explicitly — Ollama derives its default from VRAM and
/// lands at 4k on this card, and **it keys a loaded runner by model *and*
/// context**, so asking at a different value than you warmed at silently tears
/// the runner down and reloads (measured 2026-09-19 at ~3s each way). Pass
/// `config::DEFAULT_NUM_CTX` unless you have a reason not to.
pub fn chat(
    model: &str,
    messages: &[Message],
    tools: &[Tool],
    options: Options,
) -> Result<ChatReply, String> {
    let mut opts = serde_json::json!({ "num_ctx": options.num_ctx });
    if let Some(t) = options.temperature {
        opts["temperature"] = serde_json::json!(t);
    }
    let mut body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": false,
        "options": opts,
    });
    // Omitted entirely rather than sent empty: a model with no `tools`
    // capability should see a request identical to one that never wanted them.
    if !tools.is_empty() {
        body["tools"] = serde_json::to_value(tools).map_err(|e| format!("encoding tools: {e}"))?;
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(CHAT_TIMEOUT)
        .build()
        .map_err(|e| format!("building client: {e}"))?;

    let response = client
        .post(ollama::url("/api/chat"))
        .json(&body)
        .send()
        .map_err(|e| format!("POST /api/chat: {e}"))?;

    let status = response.status();
    let text = response
        .text()
        .map_err(|e| format!("reading /api/chat body: {e}"))?;

    if !status.is_success() {
        // Ollama reports a model that cannot take tools here rather than by
        // refusing the connection, so the body is the interesting part.
        return Err(format!("/api/chat returned {status}: {}", text.trim()));
    }

    let raw: RawReply =
        serde_json::from_str(&text).map_err(|e| format!("parsing /api/chat reply: {e}"))?;

    Ok(ChatReply {
        message: raw.message,
        load: Duration::from_nanos(raw.load_duration),
        total: Duration::from_nanos(raw.total_duration),
        // Nothing to time: this waited for the whole reply.
        ttft: None,
        prompt_eval_count: raw.prompt_eval_count,
        eval_count: raw.eval_count,
        recovered: false,
    })
}

/// [`chat`] with tools, plus a recovery for the one malformed call Ollama
/// cannot parse — see [`crate::toolcall`] for the measured failure.
///
/// **Native first, always.** That is the path G6 verified and the only one that
/// carries images. Only when Ollama answers with a JSON parse error for the
/// model's call — and only for a model that speaks Qwen's chat format — is the
/// same turn asked again through raw `/api/generate`, where the text comes back
/// untouched and [`crate::toolcall::parse_call`] can repair it.
///
/// A call that still cannot be read comes back as an [`INVALID_TOOL_CALL`], so
/// the caller tells the model — which can try again — instead of the turn ending
/// in an HTTP 500.
pub fn chat_tools(
    model: &str,
    messages: &[Message],
    tools: &[Tool],
    options: Options,
) -> Result<ChatReply, String> {
    match chat(model, messages, tools, options) {
        Err(e) if is_malformed_call(&e) && speaks_chatml(model) => recover(model, messages, tools, options),
        other => other,
    }
}

/// Ollama's wording when the model's tool-call JSON would not parse. Matched
/// narrowly on purpose: every other error should surface exactly as it was.
fn is_malformed_call(err: &str) -> bool {
    err.contains("returned 500")
        && ["invalid character", "unexpected end of JSON input", "cannot unmarshal"]
            .iter()
            .any(|m| err.contains(m))
}

/// Qwen-family models use the ChatML layout [`crate::toolcall::render_chatml`]
/// writes. Anything else keeps Ollama's own error rather than getting a prompt
/// in a format it was not trained on.
fn speaks_chatml(model: &str) -> bool {
    crate::ollama::show_cached(model)
        .map(|d| d.family.starts_with("qwen"))
        .unwrap_or(false)
}

#[derive(Deserialize)]
struct RawGenerate {
    #[serde(default)]
    response: String,
    #[serde(default)]
    load_duration: u64,
    #[serde(default)]
    total_duration: u64,
    #[serde(default)]
    prompt_eval_count: u32,
    #[serde(default)]
    eval_count: u32,
}

fn recover(model: &str, messages: &[Message], tools: &[Tool], options: Options) -> Result<ChatReply, String> {
    let prompt = crate::toolcall::render_chatml(messages, tools);
    let mut opts = serde_json::json!({
        "num_ctx": options.num_ctx,
        "stop": ["<|im_end|>", "<|endoftext|>"],
    });
    if let Some(t) = options.temperature {
        opts["temperature"] = serde_json::json!(t);
    }
    let body = serde_json::json!({
        "model": model,
        "prompt": prompt,
        "raw": true,
        "stream": false,
        "options": opts,
    });

    let client = reqwest::blocking::Client::builder()
        .timeout(CHAT_TIMEOUT)
        .build()
        .map_err(|e| format!("building client: {e}"))?;
    let response = client
        .post(ollama::url("/api/generate"))
        .json(&body)
        .send()
        .map_err(|e| format!("POST /api/generate (tool-call recovery): {e}"))?;
    let status = response.status();
    let text = response.text().map_err(|e| format!("reading /api/generate body: {e}"))?;
    if !status.is_success() {
        return Err(format!("tool-call recovery: /api/generate returned {status}: {}", text.trim()));
    }
    let raw: RawGenerate =
        serde_json::from_str(&text).map_err(|e| format!("parsing /api/generate reply: {e}"))?;

    let (content, mut calls, failures) = crate::toolcall::extract_calls(&raw.response, tools);
    if calls.is_empty()
        && let Some(why) = failures.into_iter().next()
    {
        calls.push(ToolCall {
            id: None,
            function: FunctionCall {
                name: INVALID_TOOL_CALL.to_string(),
                arguments: serde_json::json!({ "error": why }),
            },
        });
    }

    Ok(ChatReply {
        message: Message {
            role: "assistant".to_string(),
            content,
            images: None,
            tool_calls: if calls.is_empty() { None } else { Some(calls) },
            tool_name: None,
        },
        load: Duration::from_nanos(raw.load_duration),
        total: Duration::from_nanos(raw.total_duration),
        ttft: None,
        prompt_eval_count: raw.prompt_eval_count,
        eval_count: raw.eval_count,
        recovered: true,
    })
}

/// The name [`chat_tools`] gives a call it could not read even after repair,
/// so the caller hands the reason back to the model rather than running
/// anything.
pub const INVALID_TOOL_CALL: &str = "invalid_tool_call";

/// One thing that arrived while the model was answering.
pub enum StreamChunk<'a> {
    /// A fragment of the answer. Not a word or a line — whatever Ollama sent.
    Token(&'a str),
    /// The final object, with the metrics that only arrive at the end.
    Done(&'a ChatReply),
}

/// Stream a reply, handing each fragment to `on` as it lands.
///
/// **Blocking, and deliberately so**, like everything else in this crate — the
/// caller drives it from a thread of its own, and Tauri callers already go
/// through `spawn_blocking` as a standing rule. That buys two things: no async
/// dependencies (this crate carries `reqwest`'s `blocking` feature only), and a
/// much simpler loop. `reqwest::blocking::Response` implements [`std::io::Read`],
/// so `BufReader::lines()` does the NDJSON splitting; the async version in
/// Heimdall has to keep a remainder buffer because a network chunk can end
/// mid-line.
///
/// Returning `on`'s error stops the stream — that is how a caller cancels, and
/// how a dead IPC channel stops a request nobody is listening to any more.
///
/// The accumulated text is on the returned [`ChatReply`]'s message. **Append it
/// to the conversation**: the model has to be told what it already said, or the
/// next turn is a first question again.
pub fn stream(
    model: &str,
    messages: &[Message],
    tools: &[Tool],
    options: Options,
    mut on: impl FnMut(StreamChunk) -> Result<(), String>,
) -> Result<ChatReply, String> {
    use std::io::{BufRead, BufReader};

    let mut opts = serde_json::json!({ "num_ctx": options.num_ctx });
    if let Some(t) = options.temperature {
        opts["temperature"] = serde_json::json!(t);
    }
    let mut body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "options": opts,
    });
    if !tools.is_empty() {
        body["tools"] = serde_json::to_value(tools).map_err(|e| format!("encoding tools: {e}"))?;
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(CHAT_TIMEOUT)
        .build()
        .map_err(|e| format!("building client: {e}"))?;

    // Before `send`, not after. A blocking `send` does not return until the
    // response has started, and a cold model spends seconds loading first — so
    // timing from after it would report microseconds and mean nothing. This is
    // what makes the figure comparable to A4's measured 1.19s to first token.
    let started = std::time::Instant::now();

    let response = client
        .post(ollama::url("/api/chat"))
        .json(&body)
        .send()
        .map_err(|e| format!("POST /api/chat: {e}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().unwrap_or_default();
        return Err(format!("/api/chat returned {status}: {}", detail.trim()));
    }

    let mut text = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut ttft: Option<Duration> = None;
    let mut reply = ChatReply {
        message: Message::plain("assistant", ""),
        load: Duration::ZERO,
        total: Duration::ZERO,
        ttft: None,
        prompt_eval_count: 0,
        eval_count: 0,
        recovered: false,
    };

    for line in BufReader::new(response).lines() {
        let line = line.map_err(|e| format!("reading /api/chat stream: {e}"))?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let v: serde_json::Value =
            serde_json::from_str(line).map_err(|e| format!("bad JSON from Ollama: {e}"))?;

        // `/api/chat` puts the text under `message.content`; `/api/generate` used
        // `response`. Reading the wrong one streams nothing, silently.
        if let Some(fragment) = v["message"]["content"].as_str()
            && !fragment.is_empty() {
                if ttft.is_none() {
                    ttft = Some(started.elapsed());
                }
                text.push_str(fragment);
                on(StreamChunk::Token(fragment))?;
            }

        // Tool calls arrive whole rather than a fragment at a time, but they can
        // land on any object in the stream, not only the last one.
        if let Some(calls) = v["message"]["tool_calls"].as_array() {
            for call in calls {
                if let Ok(parsed) = serde_json::from_value::<ToolCall>(call.clone()) {
                    tool_calls.push(parsed);
                }
            }
        }

        if v["done"].as_bool().unwrap_or(false) {
            reply.load = Duration::from_nanos(v["load_duration"].as_u64().unwrap_or(0));
            reply.total = Duration::from_nanos(v["total_duration"].as_u64().unwrap_or(0));
            reply.prompt_eval_count = v["prompt_eval_count"].as_u64().unwrap_or(0) as u32;
            reply.eval_count = v["eval_count"].as_u64().unwrap_or(0) as u32;
        }
    }

    reply.ttft = ttft;
    reply.message.content = text;
    reply.message.tool_calls = (!tool_calls.is_empty()).then_some(tool_calls);
    on(StreamChunk::Done(&reply))?;
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_turn_serializes_without_the_unused_fields() {
        // Decision #2 keeps the fields on the struct; the wire format should not
        // pay for them until something fills them in.
        let json = serde_json::to_string(&Message::user("hello")).unwrap();
        assert_eq!(json, r#"{"role":"user","content":"hello"}"#);
    }

    #[test]
    fn tool_call_arguments_arrive_as_an_object() {
        // Guards against porting OpenAI-shaped code that expects a string here.
        let reply: Message = serde_json::from_str(
            r#"{"role":"assistant","content":"",
                "tool_calls":[{"id":"call_1","function":{"name":"add","arguments":{"a":1,"b":2}}}]}"#,
        )
        .unwrap();
        let calls = reply.tool_calls.unwrap();
        assert_eq!(calls[0].function.name, "add");
        assert_eq!(calls[0].function.arguments["a"], 1);
    }

    #[test]
    fn a_reply_with_no_tool_calls_yields_an_empty_slice() {
        let reply = ChatReply {
            message: Message::user("hi"),
            load: Duration::ZERO,
            total: Duration::ZERO,
            ttft: None,
            prompt_eval_count: 0,
            eval_count: 0,
            recovered: false,
        };
        assert!(reply.tool_calls().is_empty());
    }
}
