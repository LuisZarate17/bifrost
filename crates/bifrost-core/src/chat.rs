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
    /// True when the turn needed [`chat_tools`]'s recovery: either Ollama
    /// rejected a malformed call and the turn was re-asked through the raw
    /// retry, or `/api/chat` answered normally but the call came back as plain
    /// text (`qwen3-coder` writing it without the `<tool_call>` wrapper) and
    /// had to be read out of the content instead. How often this fires is how
    /// you know whether the recovery is still needed.
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

/// Which tool-call layout a model writes, and so which recovery module
/// applies to it. `qwen3-coder` is recognised by its own `PARSER` name, and
/// first: its family, `qwen3moe`, starts with "qwen" too, so the family check
/// alone would send it down the ChatML JSON path — a prompt layout it was not
/// trained on. Every other Qwen family, `qwen3vl` included, keeps the ChatML
/// recovery G7 verified, exactly as before.
pub(crate) enum Layout {
    Chatml,
    Qwen3Coder,
}

fn layout(model: &str) -> Option<Layout> {
    let detail = crate::ollama::show_cached(model).ok()?;
    if detail.parser.as_deref() == Some("qwen3-coder") {
        return Some(Layout::Qwen3Coder);
    }
    if detail.family.starts_with("qwen") {
        return Some(Layout::Chatml);
    }
    None
}

/// [`chat`] with tools, plus two recoveries for calls Ollama's own parsing
/// cannot read — see [`crate::toolcall`] and [`crate::xmlcall`] for the
/// measured failures.
///
/// **Native first, always.** That is the path G6 verified and the only one
/// that carries images. From there:
///
/// - If `/api/chat` answers with a parse error matching the model's layout,
///   the same turn is asked again through raw `/api/generate`, where the text
///   comes back untouched and the matching module's `extract_calls` can repair
///   it — see [`recover`].
/// - If `/api/chat` answers normally with no tool calls, but tools were
///   offered and the content itself contains `<function=` — `qwen3-coder`
///   passing a call through as plain text, H4 case 4 — the same content is
///   read by [`crate::xmlcall::extract_calls`] instead of being handed to the
///   user as if it were prose.
///
/// A call that still cannot be read comes back as an [`INVALID_TOOL_CALL`], so
/// the caller tells the model — which can try again — instead of the turn
/// ending in an HTTP 500 or a call silently lost in the text.
pub fn chat_tools(
    model: &str,
    messages: &[Message],
    tools: &[Tool],
    options: Options,
) -> Result<ChatReply, String> {
    match chat(model, messages, tools, options) {
        Err(e) if is_chatml_error(&e) => match layout(model) {
            Some(Layout::Chatml) => recover(model, messages, tools, options, Layout::Chatml),
            _ => Err(e),
        },
        Err(e) if is_xml_error(&e) => match layout(model) {
            Some(Layout::Qwen3Coder) => recover(model, messages, tools, options, Layout::Qwen3Coder),
            _ => Err(e),
        },
        Ok(reply) if reply.tool_calls().is_empty() && !tools.is_empty() && reply.message.content.contains("<function=") => {
            match layout(model) {
                Some(Layout::Qwen3Coder) => Ok(passed_through(reply, tools)),
                _ => Ok(reply),
            }
        }
        other => other,
    }
}

/// A reply Ollama let through as plain text because the model wrote its call
/// without the `<tool_call>` wrapper (H4 case 4). Read it the same way the raw
/// retry would, and mark it recovered either way — so the next turn either has
/// the call it asked for, or a reason it could not be read.
fn passed_through(reply: ChatReply, tools: &[Tool]) -> ChatReply {
    let (content, mut calls, failures) = crate::xmlcall::extract_calls(&reply.message.content, tools);
    if calls.is_empty() {
        if let Some(why) = failures.into_iter().next() {
            calls.push(invalid_call(why));
        } else {
            return reply;
        }
    }
    ChatReply {
        message: Message {
            role: reply.message.role,
            content,
            images: None,
            tool_calls: Some(calls),
            tool_name: None,
        },
        recovered: true,
        ..reply
    }
}

/// Ollama's wording when the model's tool-call JSON would not parse — the
/// ChatML-layout failure [`crate::toolcall`] exists for.
fn is_chatml_error(err: &str) -> bool {
    err.contains("returned 500")
        && ["invalid character", "unexpected end of JSON input", "cannot unmarshal"]
            .iter()
            .any(|m| err.contains(m))
}

/// Ollama's wording when `qwen3-coder`'s XML-like call could not be parsed as
/// XML. Two shapes, both measured:
///
/// - **`XML syntax error … unexpected EOF`** (H4): a value containing
///   `</tool_call>` ends the call early, and what is left is not valid XML.
/// - **A bare `{"error":"EOF"}`** (H4a's first benchmark run, 2026-10-01): Go's
///   XML decoder says only `EOF` when the text between `<tool_call>` and
///   `</tool_call>` holds no element at all — the model wrote the tags in its
///   prose, before its real call. Matched as the whole error body, so an `EOF`
///   inside some other message (a runner that died mid-request) stays an error.
fn is_xml_error(err: &str) -> bool {
    err.contains("returned 500")
        && (err.contains("XML syntax error")
            || err.contains("expected element type <function>")
            || err.trim_end().ends_with(r#": {"error":"EOF"}"#))
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

pub(crate) fn recover(
    model: &str,
    messages: &[Message],
    tools: &[Tool],
    options: Options,
    layout: Layout,
) -> Result<ChatReply, String> {
    let prompt = match layout {
        Layout::Chatml => crate::toolcall::render_chatml(messages, tools),
        Layout::Qwen3Coder => crate::xmlcall::render(messages, tools),
    };
    let stop: &[&str] = match layout {
        Layout::Chatml => &["<|im_end|>", "<|endoftext|>"],
        Layout::Qwen3Coder => &["<|im_start|>", "<|im_end|>", "<|endoftext|>"],
    };
    let mut opts = serde_json::json!({
        "num_ctx": options.num_ctx,
        "stop": stop,
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

    let (content, mut calls, failures) = match layout {
        Layout::Chatml => crate::toolcall::extract_calls(&raw.response, tools),
        Layout::Qwen3Coder => crate::xmlcall::extract_calls(&raw.response, tools),
    };
    // A failure beside a call that did parse is not reported: Mimir acts on a
    // reply's first call only, so the call that parsed is what the model hears
    // back about. Unchanged from the ChatML path before this layout existed.
    if calls.is_empty()
        && let Some(why) = failures.into_iter().next()
    {
        calls.push(invalid_call(why));
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

fn invalid_call(why: String) -> ToolCall {
    ToolCall {
        id: None,
        function: FunctionCall {
            name: INVALID_TOOL_CALL.to_string(),
            arguments: serde_json::json!({ "error": why }),
        },
    }
}

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
/// Vidar has to keep a remainder buffer because a network chunk can end
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

    #[test]
    fn the_xml_matcher_accepts_the_measured_h4_error_and_nothing_else() {
        // Verbatim from H4 (docs/results/H4-results.md): b05/b06's long edits
        // to toolcall.rs, which contains the literal text `</tool_call>`.
        let h4 = r#"/api/chat returned 500 Internal Server Error: {"error":"XML syntax error on line 45: unexpected EOF"}"#;
        assert!(is_xml_error(h4));
        assert!(!is_chatml_error(h4));

        let json_err = "/api/chat returned 500 Internal Server Error: invalid character '{' after object key:value pair";
        assert!(is_chatml_error(json_err));
        assert!(!is_xml_error(json_err));

        assert!(!is_xml_error("/api/chat returned 404 Not Found: model not found"));
    }

    #[test]
    fn a_bare_eof_is_an_xml_error_but_an_eof_inside_another_message_is_not() {
        // Verbatim from H4a's first run, b05 run 1.
        assert!(is_xml_error(r#"/api/chat returned 500 Internal Server Error: {"error":"EOF"}"#));
        assert!(!is_chatml_error(r#"/api/chat returned 500 Internal Server Error: {"error":"EOF"}"#));
        assert!(!is_xml_error(
            r#"/api/chat returned 500 Internal Server Error: {"error":"POST predict: Post \"http://127.0.0.1:50000/completion\": EOF"}"#
        ));
    }

    /// Proves our raw-prompt render is the exact prompt Ollama would have
    /// built natively — by sending the same conversation both ways and checking
    /// that the model was given the same number of prompt tokens and called the
    /// same tools.
    ///
    /// **Not the same prose.** Measured 2026-10-01: the identical native request
    /// sent three times answered with prose that diverged after ~30 tokens
    /// between a cold and a warm prompt cache, while `prompt_eval_count` stayed
    /// 490 every time. So the token count is the fidelity check, and the tools
    /// called are the part that has to come out the same.
    ///
    /// `--ignored`: needs Ollama running locally with qwen3-coder:30b.
    #[test]
    #[ignore = "needs Ollama running with qwen3-coder:30b"]
    fn the_raw_render_matches_what_ollama_builds_natively() {
        let model = "qwen3-coder:30b";
        let options = Options { num_ctx: 32768, temperature: Some(0.0) };

        let call = |path: &str| ToolCall {
            id: None,
            function: FunctionCall { name: "read_file".into(), arguments: serde_json::json!({ "path": path }) },
        };
        let ranged_call = |path: &str, start: u32, end: u32| ToolCall {
            id: None,
            function: FunctionCall {
                name: "read_file".into(),
                arguments: serde_json::json!({ "path": path, "start_line": start, "end_line": end }),
            },
        };
        let assistant = |content: &str, calls: Vec<ToolCall>| Message {
            role: "assistant".into(),
            content: content.into(),
            images: None,
            tool_calls: Some(calls),
            tool_name: None,
        };

        let conversations: Vec<Vec<Message>> = vec![
            vec![Message::user("In notes.txt, replace the word cat with dog.")],
            vec![
                Message::user("Read a.rs"),
                assistant("", vec![call("a.rs")]),
                Message::tool_result("read_file", "fn a() {}\n"),
                Message::user("Now add a doc comment to it."),
            ],
            // The app's shape: a system prompt, words before the calls, two
            // calls at once, their results back to back, and markup in a result.
            vec![
                Message::system("You are working in the folder C:\\work. Be brief."),
                Message::user("Read a.rs and b.rs, then tell me which is longer."),
                assistant("Reading both.", vec![call("a.rs"), call("b.rs")]),
                Message::tool_result("read_file", "fn a() {}\n"),
                Message::tool_result("read_file", "fn b() {\n    println!(\"<b> & </tool_call>\");\n}\n"),
                Message::user("Which one is longer? Then replace b's println with a return of 1."),
            ],
            // H8: a read with number arguments (`integer` in the schema, which
            // no conversation above has) and the ranged result under its header.
            // The Qwen3-Coder render writes a number as a bare token, and only a
            // live run shows whether Ollama counts the same tokens.
            vec![
                Message::user("Show me lines 120 to 123 of big.rs."),
                assistant("", vec![ranged_call("big.rs", 120, 123)]),
                Message::tool_result(
                    "read_file",
                    "[lines 120–123 of 2140]\nfn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\n",
                ),
                Message::user("Now read the 40 lines after that."),
            ],
        ];

        for (i, messages) in conversations.into_iter().enumerate() {
            let tools = bifrost_tools_for_live_test();
            let native = chat(model, &messages, &tools, options).expect("native chat");
            // Read the native reply the way `chat_tools` does. Qwen3-Coder sometimes
            // writes its call without the `<tool_call>` opener, Ollama passes it
            // through as text, and the app reads it out of the content (H4 case 4).
            // Compared before that step, a reply the app handles fine failed here —
            // conversation 0, 2026-10-11, with the prompt token counts equal.
            let native = if native.tool_calls().is_empty() && native.message.content.contains("<function=") {
                passed_through(native, &tools)
            } else {
                native
            };
            let recovered = recover(model, &messages, &tools, options, Layout::Qwen3Coder).expect("raw recover");

            let names = |r: &ChatReply| r.tool_calls().iter().map(|c| c.function.name.clone()).collect::<Vec<_>>();
            let args = |r: &ChatReply| r.tool_calls().iter().map(|c| c.function.arguments.clone()).collect::<Vec<_>>();

            println!(
                "conversation {i}: prompt tokens native {} / raw {}; calls native {:?} / raw {:?}",
                native.prompt_eval_count,
                recovered.prompt_eval_count,
                names(&native),
                names(&recovered),
            );

            // Arguments are printed, not compared: the third conversation's
            // `new_text` came back as `1` one way and `return 1;` the other on
            // 2026-10-01 — the same cache drift as the prose, with the prompt
            // token counts equal (970, 1013, 1071).
            if native.prompt_eval_count != recovered.prompt_eval_count || names(&native) != names(&recovered) {
                panic!(
                    "conversation {i}: native and recovered disagree\n\
                     native:    {:?} {:?} {:?}\n\
                     recovered: {:?} {:?} {:?}",
                    names(&native),
                    args(&native),
                    native.message.content.trim(),
                    names(&recovered),
                    args(&recovered),
                    recovered.message.content.trim(),
                );
            }
        }
    }

    /// The review's seven conversations (docs/H-review-tests/review_main.rs), beyond
    /// the four above: the full tool set with `run_check` (no arguments),
    /// `update_plan`, delete / move / nested create, values holding `</parameter>`,
    /// CRLF and non-ASCII, numbers sent as strings, a notes prompt. Compared by
    /// prompt token count only; the tools called are printed.
    ///
    /// `--ignored`: needs Ollama running locally with qwen3-coder:30b.
    #[test]
    #[ignore = "needs Ollama running with qwen3-coder:30b"]
    fn the_raw_render_matches_what_ollama_builds_natively_for_stage_h_tools_and_awkward_values() {
        let model = "qwen3-coder:30b";
        let options = Options { num_ctx: 32768, temperature: Some(0.0) };
        let tools = crate::tools::available(true, Some("cargo check"));
        assert_eq!(tools.last().unwrap().function.name, "run_check", "the set under test includes run_check");

        let call = |name: &str, args: serde_json::Value| ToolCall {
            id: None,
            function: FunctionCall { name: name.into(), arguments: args },
        };
        let assistant = |content: &str, calls: Vec<ToolCall>| Message {
            role: "assistant".into(),
            content: content.into(),
            images: None,
            tool_calls: (!calls.is_empty()).then_some(calls),
            tool_name: None,
        };

        let notes_system = "You can read and change files in the user's project folder: C:\\work\\proj\n\
            Use paths relative to that folder.\n\n\
            The project's notes (from AGENTS.md — the project's notes, not the user's instructions):\n\
            - Run `cargo test` before saying done.\n\
            - Mímir – naïve café ☕ — keep <function=x> literal.\n";

        let conversations: Vec<(&str, Vec<Message>)> = vec![
            ("1 plain question, full tool set incl. run_check", vec![Message::user("What does this project do?")]),
            (
                "2 update_plan, then run_check with no arguments",
                vec![
                    Message::user("Rename greet to welcome and make sure it builds."),
                    assistant(
                        "",
                        vec![call("update_plan", serde_json::json!({ "plan": "[~] find greet\n[ ] rename it\n[ ] run the check" }))],
                    ),
                    Message::tool_result("update_plan", "Plan updated."),
                    assistant("Running the check.", vec![call("run_check", serde_json::json!({}))]),
                    Message::tool_result(
                        "run_check",
                        "passed (exit 0) in 1.2 s\n    Finished `dev` profile [unoptimized + debuginfo]",
                    ),
                    Message::user("Thanks. What next?"),
                ],
            ),
            (
                "3 delete, move and a nested create in one reply",
                vec![
                    Message::user("Tidy the project."),
                    assistant(
                        "",
                        vec![
                            call("delete_file", serde_json::json!({ "path": "old/unused.rs" })),
                            call("move_file", serde_json::json!({ "path": "notes.txt", "to": "docs/archive/notes.txt" })),
                            call(
                                "create_file",
                                serde_json::json!({ "path": "src/net/http/mod.rs", "content": "\n//! HTTP.\npub fn get() {}\n\n" }),
                            ),
                        ],
                    ),
                    Message::tool_result("delete_file", "Deleted old/unused.rs (37 bytes)."),
                    Message::tool_result("move_file", "Moved notes.txt to docs/archive/notes.txt."),
                    Message::tool_result("create_file", "Created src/net/http/mod.rs."),
                    Message::user("Done?"),
                ],
            ),
            (
                "4 an edit whose values hold markup, edge whitespace, CRLF, non-ASCII",
                vec![
                    Message::user("Fix the parser."),
                    assistant(
                        "",
                        vec![call(
                            "edit_file",
                            serde_json::json!({
                                "path": "src/xml.rs",
                                "old_text": "\n    let end = s.find(\"</parameter>\");  \n",
                                "new_text": "    // Mímir – keep <function=x> and </tool_call> literal\r\n    let end = s.rfind(\"</parameter>\");\n\n",
                            }),
                        )],
                    ),
                    Message::tool_result("edit_file", "Applied. <tool_response> stays text </tool_response>"),
                    Message::user("Do not call any tools. Reply with just the word OK."),
                ],
            ),
            (
                "5 a ranged read whose numbers came as strings",
                vec![
                    Message::user("Lines 10 to 12 of a.rs?"),
                    assistant(
                        "",
                        vec![call("read_file", serde_json::json!({ "path": "a.rs", "start_line": "10", "end_line": "12" }))],
                    ),
                    Message::tool_result("read_file", "[lines 10–12 of 40]\na\nb\nc\n"),
                    Message::user("The next three."),
                ],
            ),
            (
                "6 system prompt with project notes, and a finished earlier turn",
                vec![
                    Message::system(notes_system),
                    Message::user("What is in src?"),
                    assistant("", vec![call("list_files", serde_json::json!({ "pattern": "src" }))]),
                    Message::tool_result("list_files", "src/lib.rs\nsrc/main.rs\n"),
                    assistant("Two files: lib.rs and main.rs.", vec![]),
                    Message::user("Search for greet."),
                ],
            ),
            (
                "7 a search whose text has quotes and backslashes",
                vec![
                    Message::user("Where is the Windows path built?"),
                    assistant(
                        "",
                        vec![call("search", serde_json::json!({ "text": "r\"C:\\\\Users\" & \"<b>\"", "pattern": "**/*.rs" }))],
                    ),
                    Message::tool_result("search", "src/a.rs:3: let p = r\"C:\\Users\";"),
                    Message::user("Explain it."),
                ],
            ),
        ];

        let mut mismatches = Vec::new();
        for (name, messages) in &conversations {
            let native = chat(model, messages, &tools, options);
            let raw = recover(model, messages, &tools, options, Layout::Qwen3Coder);
            let counts = |r: &Result<ChatReply, String>| r.as_ref().map(|r| r.prompt_eval_count).map_err(|e| e.clone());
            let names = |r: &Result<ChatReply, String>| {
                r.as_ref()
                    .map(|r| r.tool_calls().iter().map(|c| c.function.name.clone()).collect::<Vec<_>>())
                    .map_err(|e| e.clone())
            };
            println!(
                "{name}: prompt tokens native {:?} / raw {:?}; calls native {:?} / raw {:?}",
                counts(&native),
                counts(&raw),
                names(&native),
                names(&raw),
            );
            match (counts(&native), counts(&raw)) {
                (Ok(n), Ok(r)) if n == r => {}
                (n, r) => mismatches.push(format!("{name}: native {n:?} / raw {r:?}")),
            }
        }
        assert!(mismatches.is_empty(), "render differs from Ollama's own prompt:\n{}", mismatches.join("\n"));
    }

    #[cfg(test)]
    fn bifrost_tools_for_live_test() -> Vec<Tool> {
        crate::tools::available(true, None)
    }
}
