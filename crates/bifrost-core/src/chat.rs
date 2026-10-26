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
    /// True when Ollama answered a tools request with a parse error for the
    /// model's call and [`chat_tools`] has no reply to give, so this one carries
    /// a stand-in [`INVALID_TOOL_CALL`] with Ollama's message. The caller sends
    /// it back to the model, which can try again. Separate from `recovered`:
    /// that one means the call was read; this one means it was not.
    pub fed_back: bool,
    /// Time Ollama spent reading the prompt, and writing the reply. Zero when it
    /// did not say, as on a reply that never came from the model.
    pub prompt_eval_duration: Duration,
    pub eval_duration: Duration,
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
    #[serde(default)]
    prompt_eval_duration: u64,
    #[serde(default)]
    eval_duration: u64,
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
        fed_back: false,
        prompt_eval_duration: Duration::from_nanos(raw.prompt_eval_duration),
        eval_duration: Duration::from_nanos(raw.eval_duration),
    })
}

/// Which tool-call layout a model writes, and so which recovery module
/// applies to it. `qwen3-coder` is recognised by its own `PARSER` name, and
/// first: its family, `qwen3moe`, starts with "qwen" too, so the family check
/// alone would send it down the ChatML JSON path — a prompt layout it was not
/// trained on. Every other Qwen family, `qwen3vl` included, keeps the ChatML
/// recovery G7 verified, exactly as before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
///
/// **That holds for every model, not only Qwen.** A parse error that no layout
/// recovers — the model has none, or the retry failed too — is turned into the
/// same stand-in call, marked [`ChatReply::fed_back`]; see [`settle`]. Only an
/// error that looks like the model's own unreadable call is: the runner dying,
/// running out of memory or timing out still ends the turn
/// ([`is_tool_parse_error`]).
pub fn chat_tools(
    model: &str,
    messages: &[Message],
    tools: &[Tool],
    options: Options,
) -> Result<ChatReply, String> {
    let reply = match chat(model, messages, tools, options) {
        Err(e) if is_chatml_error(&e) || is_xml_error(&e) || (!tools.is_empty() && is_tool_parse_error(&e)) => {
            after_failure(e, !tools.is_empty(), layout(model), |l| recover(model, messages, tools, options, l))
        }
        Ok(reply) if reply.tool_calls().is_empty() && !tools.is_empty() && reply.message.content.contains("<function=") => {
            match layout(model) {
                Some(Layout::Qwen3Coder) => Ok(passed_through(reply, tools)),
                _ => Ok(reply),
            }
        }
        other => other,
    };
    reply.map(|mut r| {
        r.message.content = without_dangling_markup(&r.message.content).to_string();
        r
    })
}

/// The reply's text without a tool-call tag left dangling at its end: an opener the
/// model wrote and then stopped, or a closer whose call is already gone (J0c, a
/// `hugin --ask` answer ending in `<tool_call>`). A tag is taken only from a line of
/// its own at the very end, and only with no partner, so an answer that talks about
/// the tags or shows a whole example keeps them (R-17).
fn without_dangling_markup(original: &str) -> &str {
    let mut text = original.trim_end();
    let mut cut = false;
    loop {
        let last_line = text.rsplit('\n').next().unwrap_or(text);
        let unmatched = match last_line.trim() {
            "<tool_call>" => true,
            "</tool_call>" => text.matches("<tool_call>").count() < text.matches("</tool_call>").count(),
            "</function>" => text.matches("<function=").count() < text.matches("</function>").count(),
            _ => false,
        };
        if !unmatched {
            // Untouched when there was nothing to cut, trailing whitespace and all.
            return if cut { text } else { original };
        }
        text = text[..text.len() - last_line.len()].trim_end();
        cut = true;
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

/// What a request that failed for a reason of the machine's, not the model's,
/// says. Lower case, matched against a lower-cased error:
///
/// - **`wsarecv`, `POST predict`**: Ollama's server lost the runner
///   mid-request, so the error is a socket's, often ending in a bare `EOF`.
/// - **`runner process has terminated`, `out of memory`, `cuda error`**: the
///   runner died or could not load.
/// - **connection and timeout wordings, `not found`**: no server, no answer in
///   time, or no such model — a 404, not a 500, but listed so the answer is the
///   same whatever status a newer Ollama puts on them.
///
/// Checked first, because a runner that dies halfway through a reply can leave
/// `unexpected end of JSON input` in the same message.
const INFRASTRUCTURE_ERRORS: [&str; 11] = [
    "wsarecv",
    "post predict",
    "runner process has terminated",
    "out of memory",
    "cuda error",
    "connection refused",
    "connection reset",
    "model not found",
    "try pulling it first",
    "timed out",
    "deadline exceeded",
];

/// What Ollama says when it could not read the model's tool call, lower case:
/// the ChatML JSON wordings (`invalid character`, `unexpected end of JSON
/// input`, `unmarshal`), the Qwen3-Coder XML ones (`XML syntax error`, `expected
/// element type <function>`), and the generic ones other parsers use (Mistral's
/// and gpt-oss's `error parsing tool call`, `failed to parse tool call`,
/// `invalid tool call`).
const TOOL_CALL_PARSE_ERRORS: [&str; 8] = [
    "error parsing tool call",
    "failed to parse tool call",
    "invalid tool call",
    "unmarshal",
    "xml syntax error",
    "expected element type <function>",
    "invalid character",
    "unexpected end of json input",
];

/// `err` without the model's call that Ollama echoes in `error parsing tool call:
/// raw='…', err=…`: from `raw='` to the last `', err=`, or to the end if that is not
/// there. What is left is Ollama's own words.
fn without_echoed_call(err: &str) -> String {
    let Some(start) = err.find("raw='") else { return err.to_string() };
    match err[start..].rfind("', err=") {
        Some(end) => format!("{}{}", &err[..start], &err[start + end + 2..]),
        None => err[..start].to_string(),
    }
}

/// Whether a failed request is the model's own tool call being unreadable —
/// so the model can be told and try again — rather than the machine failing.
///
/// An HTTP 500 that says so in one of Ollama's wordings
/// ([`TOOL_CALL_PARSE_ERRORS`]), and says nothing of the machine
/// ([`INFRASTRUCTURE_ERRORS`]). **A bare `{"error":"EOF"}` is not one**: for a
/// model whose layout is known to produce it, [`is_xml_error`] recognises it; for
/// any other it is a runner that went away.
fn is_tool_parse_error(err: &str) -> bool {
    if !err.contains("returned 500") {
        return false;
    }
    // Not the call the error echoes: code that says "timed out" is not a timeout.
    let err = without_echoed_call(err).to_lowercase();
    !INFRASTRUCTURE_ERRORS.iter().any(|m| err.contains(m)) && TOOL_CALL_PARSE_ERRORS.iter().any(|m| err.contains(m))
}

/// Which layout's raw retry to try for a failed request, given the layout the model
/// writes (`None` for a model with none): decided from the **model's** layout first,
/// since the error's wording alone can mislead — Go's XML decoder says `invalid
/// character entity` for a `qwen3-coder` call, which is also ChatML's wording.
///
/// - `qwen3-coder`: its XML retry, for an XML-shaped error ([`is_xml_error`]);
/// - the other Qwen families: the ChatML retry, for a JSON-shaped one
///   ([`is_chatml_error`]);
/// - anything else, or an error worded for the other layout: none, and the call is
///   fed back instead.
fn retry_layout(err: &str, model_layout: Option<Layout>) -> Option<Layout> {
    match model_layout? {
        Layout::Qwen3Coder if is_xml_error(err) => Some(Layout::Qwen3Coder),
        Layout::Chatml if is_chatml_error(err) => Some(Layout::Chatml),
        _ => None,
    }
}

/// What a request that failed in a way that looked like the model's call comes to:
/// the layout's raw retry if [`retry_layout`] picks one (`retry` runs it), then
/// [`settle`]. Everything [`chat_tools`] does after its first request fails, apart
/// from the requests themselves.
fn after_failure(
    err: String,
    offered: bool,
    model_layout: Option<Layout>,
    retry: impl FnOnce(Layout) -> Result<ChatReply, String>,
) -> Result<ChatReply, String> {
    let tried = retry_layout(&err, model_layout).map(retry);
    settle(err, offered, tried)
}

/// What a parse error comes to, given whether the layout's retry was tried and
/// what it gave. The decision [`chat_tools`] makes after the first request failed
/// in a way that looked like the model's call:
///
/// - the retry read it: that reply;
/// - the retry failed for a reason of the machine: that error, the turn ends;
/// - the retry failed with a parse error of its own: the stand-in call
///   ([`fed_back`]), with the first error's words if they were a parse error, else
///   the retry's — a bare `EOF` from the first request says nothing the model can use;
/// - no retry, with tools offered and an error worded as a parse error: the
///   stand-in call;
/// - otherwise the error as it came.
pub(crate) fn settle(
    err: String,
    offered: bool,
    retry: Option<Result<ChatReply, String>>,
) -> Result<ChatReply, String> {
    match retry {
        Some(Ok(reply)) => Ok(reply),
        Some(Err(retry_err)) if !(offered && is_tool_parse_error(&retry_err)) => Err(retry_err),
        Some(Err(retry_err)) => Ok(fed_back(if is_tool_parse_error(&err) { &err } else { &retry_err })),
        None if offered && is_tool_parse_error(&err) => Ok(fed_back(&err)),
        None => Err(err),
    }
}

/// The most of Ollama's message that goes back to the model. A parse error can
/// echo the whole call, which may be a whole file; the model has it already.
const MAX_FED_BACK: usize = 1500;

/// `text` cut to [`MAX_FED_BACK`] bytes at a character boundary, saying how much went.
fn capped(text: String) -> String {
    if text.len() <= MAX_FED_BACK {
        return text;
    }
    let mut end = MAX_FED_BACK;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [{} more bytes cut]", &text[..end], text.len() - end)
}

/// The reply that stands in for a call Ollama could not read: no words, one
/// [`INVALID_TOOL_CALL`] carrying Ollama's own message (capped), and `fed_back` set.
fn fed_back(err: &str) -> ChatReply {
    ChatReply {
        message: Message {
            role: "assistant".to_string(),
            content: String::new(),
            images: None,
            tool_calls: Some(vec![invalid_call(capped(ollama_message(err)))]),
            tool_name: None,
        },
        load: Duration::ZERO,
        total: Duration::ZERO,
        ttft: None,
        prompt_eval_count: 0,
        eval_count: 0,
        recovered: false,
        fed_back: true,
        prompt_eval_duration: Duration::ZERO,
        eval_duration: Duration::ZERO,
    }
}

/// What Ollama said, out of `/api/chat returned 500 Internal Server Error:
/// {"error":"…"}`: the `error` field if the body is that JSON, else the body.
fn ollama_message(err: &str) -> String {
    let body = err
        .split_once("returned 500")
        .and_then(|(_, rest)| rest.split_once(": "))
        .map_or(err, |(_, body)| body)
        .trim();
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["error"].as_str().map(str::to_string))
        .unwrap_or_else(|| body.to_string())
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
    #[serde(default)]
    prompt_eval_duration: u64,
    #[serde(default)]
    eval_duration: u64,
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
        fed_back: false,
        prompt_eval_duration: Duration::from_nanos(raw.prompt_eval_duration),
        eval_duration: Duration::from_nanos(raw.eval_duration),
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
        fed_back: false,
        prompt_eval_duration: Duration::ZERO,
        eval_duration: Duration::ZERO,
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
            reply.prompt_eval_duration = Duration::from_nanos(v["prompt_eval_duration"].as_u64().unwrap_or(0));
            reply.eval_duration = Duration::from_nanos(v["eval_duration"].as_u64().unwrap_or(0));
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
            fed_back: false,
            prompt_eval_duration: Duration::ZERO,
            eval_duration: Duration::ZERO,
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

    /// Each wording Ollama has been seen to use, or plausibly will, and which side of
    /// the line it falls on: the model's call that would not parse (`true`, so it is
    /// fed back to the model), or the machine failing (`false`, so the turn ends).
    #[test]
    fn the_classifier_tells_a_call_that_would_not_parse_from_a_machine_that_failed() {
        let in500 = |body: &str| format!("/api/chat returned 500 Internal Server Error: {body}");
        let table: Vec<(String, bool, &str)> = vec![
            // The model's call: Ollama 0.33.3's measured wordings.
            (in500("invalid character '{' after object key:value pair"), true, "G7 chatml"),
            (in500(r#"{"error":"unexpected end of JSON input"}"#), true, "chatml, truncated"),
            (in500(r#"{"error":"json: cannot unmarshal string into Go struct field .arguments of type map[string]interface {}"}"#), true, "chatml, wrong type"),
            (in500(r#"{"error":"XML syntax error on line 45: unexpected EOF"}"#), true, "H4 xml"),
            (in500(r#"{"error":"expected element type <function> but have <tool_call>"}"#), true, "xml, no function"),
            // Other parsers, and plausible newer wordings.
            (in500(r#"{"error":"error parsing tool call: raw='{\"name\": \"read_file\", \"arguments\": {'; err=unexpected end of JSON input"}"#), true, "mistral / gpt-oss"),
            (in500(r#"{"error":"Error parsing tool call: raw='<call>'"}"#), true, "capitalised"),
            (in500(r#"{"error":"failed to parse tool call arguments"}"#), true, "failed to parse"),
            (in500(r#"{"error":"invalid tool call"}"#), true, "invalid tool call"),
            (in500(r#"{"error":"XML SYNTAX ERROR on line 3"}"#), true, "xml, shouted"),
            // The machine failing.
            (in500(r#"{"error":"EOF"}"#), false, "bare EOF: a runner that went away, unless the layout says otherwise"),
            (in500(r#"{"error":"POST predict: Post \"http://127.0.0.1:50000/completion\": EOF"}"#), false, "predict, EOF"),
            (in500(r#"{"error":"POST predict: Post \"http://127.0.0.1:50000/completion\": read tcp 127.0.0.1:1->127.0.0.1:2: wsarecv: An existing connection was forcibly closed by the remote host."}"#), false, "wsarecv"),
            (in500(r#"{"error":"llama runner process has terminated: exit status 2"}"#), false, "runner terminated"),
            (in500(r#"{"error":"llama runner process has terminated: unexpected end of JSON input"}"#), false, "terminated, with a parse wording beside it"),
            (in500(r#"{"error":"CUDA error: out of memory"}"#), false, "cuda oom"),
            (in500(r#"{"error":"model requires more system memory (12.0 GiB) than is available (8.0 GiB)"}"#), false, "memory, no marker"),
            (in500(r#"{"error":"CUDA error: an illegal memory access was encountered"}"#), false, "cuda error"),
            (in500(r#"{"error":"internal server error"}"#), false, "unlabelled 500"),
            (r#"/api/chat returned 404 Not Found: {"error":"model \"x\" not found, try pulling it first"}"#.to_string(), false, "404"),
            ("POST /api/chat: error sending request for url (http://localhost:11434/api/chat): connection refused".to_string(), false, "connection refused"),
            ("POST /api/chat: error sending request for url (http://localhost:11434/api/chat): connection reset by peer".to_string(), false, "connection reset"),
            ("POST /api/chat: error sending request for url (http://localhost:11434/api/chat): operation timed out".to_string(), false, "timeout"),
            // A parse wording off a status that is not 500 is not this.
            (r#"/api/chat returned 400 Bad Request: {"error":"invalid character"}"#.to_string(), false, "400"),
        ];
        for (err, want, what) in &table {
            assert_eq!(is_tool_parse_error(err), *want, "{what}: {err}");
        }
    }

    #[test]
    fn a_parse_error_becomes_the_stand_in_call_carrying_ollamas_message() {
        let err = r#"/api/chat returned 500 Internal Server Error: {"error":"error parsing tool call: raw='{', err=unexpected end of JSON input"}"#;
        let reply = settle(err.to_string(), true, None).expect("fed back");
        assert!(reply.fed_back && !reply.recovered);
        let calls = reply.tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, INVALID_TOOL_CALL);
        assert_eq!(
            calls[0].function.arguments["error"],
            "error parsing tool call: raw='{', err=unexpected end of JSON input"
        );

        // A body that is not JSON is the message as it stands.
        let plain = "/api/chat returned 500 Internal Server Error: invalid tool call";
        assert_eq!(settle(plain.to_string(), true, None).unwrap().tool_calls()[0].function.arguments["error"], "invalid tool call");
    }

    #[test]
    fn what_a_failed_request_comes_to_after_the_layouts_retry() {
        let parse = r#"/api/chat returned 500 Internal Server Error: {"error":"XML syntax error on line 3: unexpected EOF"}"#;
        let machine = r#"/api/chat returned 500 Internal Server Error: {"error":"POST predict: Post \"http://x\": EOF"}"#;
        let retried = ChatReply { fed_back: false, recovered: true, ..fed_back(parse) };

        // Not a Qwen model, no retry: fed back with tools offered, an error without.
        assert!(settle(parse.to_string(), true, None).unwrap().fed_back);
        assert_eq!(settle(parse.to_string(), false, None).unwrap_err(), parse);
        // The machine failing ends the turn either way.
        assert_eq!(settle(machine.to_string(), true, None).unwrap_err(), machine);
        // The retry read the call: its reply, not fed back.
        let ok = settle(parse.to_string(), true, Some(Ok(retried))).unwrap();
        assert!(ok.recovered && !ok.fed_back);
        // The retry failed because the machine did: that error ends the turn.
        let died = "POST /api/generate (tool-call recovery): connection refused".to_string();
        assert_eq!(settle(parse.to_string(), true, Some(Err(died.clone()))).unwrap_err(), died);
        // The retry failed on a parse error of its own: fed back with the first message.
        let again = r#"tool-call recovery: /api/generate returned 500 Internal Server Error: {"error":"invalid tool call"}"#;
        let fed = settle(parse.to_string(), true, Some(Err(again.to_string()))).unwrap();
        assert!(fed.fed_back);
        assert_eq!(fed.tool_calls()[0].function.arguments["error"], "XML syntax error on line 3: unexpected EOF");
        // A bare EOF whose retry failed stays the retry's error, as before.
        let bare = r#"/api/chat returned 500 Internal Server Error: {"error":"EOF"}"#;
        assert_eq!(settle(bare.to_string(), true, Some(Err(died.clone()))).unwrap_err(), died);
        assert_eq!(settle(bare.to_string(), true, None).unwrap_err(), bare);
        // A bare EOF whose retry failed with a parse error: the retry's words go back to the model.
        let fed = settle(bare.to_string(), true, Some(Err(again.to_string()))).unwrap();
        assert!(fed.fed_back);
        assert_eq!(fed.tool_calls()[0].function.arguments["error"], "invalid tool call");
        // With no tools offered it is still an error.
        assert_eq!(settle(bare.to_string(), false, Some(Err(again.to_string()))).unwrap_err(), again);
    }

    /// Which retry is tried, from the model's layout and the error's wording: the model's
    /// layout decides first, so qwen3-coder's XML error that says `invalid character` still
    /// gets the XML retry.
    #[test]
    fn the_retry_follows_the_models_layout_and_then_the_errors_wording() {
        let in500 = |body: &str| format!("/api/chat returned 500 Internal Server Error: {body}");
        let xml_entity = in500(r#"{"error":"XML syntax error on line 12: invalid character entity &foo; (no semicolon)"}"#);
        let xml_eof = in500(r#"{"error":"XML syntax error on line 45: unexpected EOF"}"#);
        let xml_element = in500(r#"{"error":"expected element type <function> but have <tool_call>"}"#);
        let bare = in500(r#"{"error":"EOF"}"#);
        let json = in500("invalid character '{' after object key:value pair");
        let json_end = in500(r#"{"error":"unexpected end of JSON input"}"#);
        let unmarshal = in500(r#"{"error":"json: cannot unmarshal string into Go struct field"}"#);
        let generic = in500(r#"{"error":"error parsing tool call: raw='{', err=unexpected end of JSON input"}"#);
        use Layout::{Chatml, Qwen3Coder};
        let table: Vec<(&str, Option<Layout>, Option<Layout>)> = vec![
            // qwen3-coder: XML errors, including the one worded like ChatML's.
            (&xml_entity, Some(Qwen3Coder), Some(Qwen3Coder)),
            (&xml_eof, Some(Qwen3Coder), Some(Qwen3Coder)),
            (&xml_element, Some(Qwen3Coder), Some(Qwen3Coder)),
            (&bare, Some(Qwen3Coder), Some(Qwen3Coder)),
            // qwen3-coder with an error that is not XML's: nothing to retry.
            (&json, Some(Qwen3Coder), None),
            (&generic, Some(Qwen3Coder), None),
            // The other Qwens: JSON errors.
            (&json, Some(Chatml), Some(Chatml)),
            (&json_end, Some(Chatml), Some(Chatml)),
            (&unmarshal, Some(Chatml), Some(Chatml)),
            (&xml_eof, Some(Chatml), None),
            (&bare, Some(Chatml), None),
            // Its tail is ChatML's wording, so a ChatML model gets the retry; an XML one does not.
            (&generic, Some(Chatml), Some(Chatml)),
            // Any other model, whatever the wording: fed back, not retried.
            (&json, None, None),
            (&xml_entity, None, None),
            (&bare, None, None),
            (&generic, None, None),
        ];
        for (err, model_layout, want) in table {
            assert_eq!(retry_layout(err, model_layout), want, "{model_layout:?}: {err}");
        }
    }

    /// `chat_tools`' own wiring after a failed request, with the retry scripted: which layout
    /// it is asked for, and what comes of the answer.
    #[test]
    fn what_chat_tools_does_after_a_failed_request_is_the_retry_for_the_models_layout_then_feeding_back() {
        let entity = r#"/api/chat returned 500 Internal Server Error: {"error":"XML syntax error on line 12: invalid character entity &x;"}"#;
        let read = || ChatReply { recovered: true, ..fed_back("") };
        let tried = std::cell::RefCell::new(Vec::new());
        let retry = |l: Layout| {
            tried.borrow_mut().push(l);
            Ok(ChatReply { fed_back: false, ..read() })
        };

        // qwen3-coder: the XML retry runs, and its reply stands.
        let reply = after_failure(entity.to_string(), true, Some(Layout::Qwen3Coder), retry).unwrap();
        assert!(reply.recovered && !reply.fed_back);
        assert_eq!(*tried.borrow(), [Layout::Qwen3Coder]);

        // Another model: no retry at all, and the call is fed back.
        tried.borrow_mut().clear();
        let reply = after_failure(entity.to_string(), true, None, retry).unwrap();
        assert!(reply.fed_back && tried.borrow().is_empty());

        // qwen3-coder whose retry then fails on the machine: that error ends the turn.
        let died = "POST /api/generate (tool-call recovery): connection refused".to_string();
        let ended = after_failure(entity.to_string(), true, Some(Layout::Qwen3Coder), |_| Err(died.clone()));
        assert_eq!(ended.unwrap_err(), died);

        // No tools offered: the error as it came, with or without a layout to retry.
        assert_eq!(after_failure(entity.to_string(), false, None, retry).unwrap_err(), entity);
    }

    /// A parse error echoes the model's own call, and a call may say anything.
    #[test]
    fn code_in_the_echoed_call_is_not_taken_for_a_machine_failing() {
        for said in [
            "timed out", "connection refused", "connection reset", "out of memory", "CUDA error",
            "POST predict", "wsarecv", "runner process has terminated", "deadline exceeded",
        ] {
            let err = format!(
                r#"/api/chat returned 500 Internal Server Error: {{"error":"error parsing tool call: raw='{{\"name\":\"edit_file\",\"arguments\":{{\"new_text\":\"// {said}\"}}}}', err=invalid character 'x' looking for beginning of value"}}"#
            );
            assert!(is_tool_parse_error(&err), "{said}: {err}");
        }
        // Without the terminator the echo runs to the end, and Ollama's prefix still says what it is.
        let cut = r#"/api/chat returned 500 Internal Server Error: {"error":"error parsing tool call: raw='out of memory"#;
        assert!(is_tool_parse_error(cut));
        // A real machine failure after an echo is still one.
        let real = r#"/api/chat returned 500 Internal Server Error: {"error":"error parsing tool call: raw='x', err=POST predict: EOF"}"#;
        assert!(!is_tool_parse_error(real));
    }

    #[test]
    fn what_goes_back_to_the_model_is_capped_with_the_cut_said() {
        let whole = "x".repeat(10_000);
        let err = format!(r#"/api/chat returned 500 Internal Server Error: {{"error":"error parsing tool call: raw='{whole}', err=invalid character"}}"#);
        let reply = settle(err, true, None).unwrap();
        let sent = reply.tool_calls()[0].function.arguments["error"].as_str().unwrap().to_string();
        assert!(sent.starts_with("error parsing tool call: raw='xxx"), "{sent:.60}");
        assert!(sent.len() < MAX_FED_BACK + 40, "{}", sent.len());
        assert!(sent.ends_with(" more bytes cut]"), "{sent}");
        // A short message is untouched, and a cut never splits a character.
        assert_eq!(capped("short".to_string()), "short");
        let wide = format!("a{}", "é".repeat(MAX_FED_BACK));
        assert!(capped(wide).ends_with("… [1502 more bytes cut]"));
    }

    #[test]
    fn a_tag_left_dangling_at_the_end_of_a_reply_is_cut_and_one_that_is_talked_about_is_kept() {
        let cases = [
            ("The file is a.rs.\n<tool_call>", "The file is a.rs.", "an opener with nothing after"),
            ("The file is a.rs.\n\n<tool_call>\n", "The file is a.rs.", "and a blank line before it"),
            ("Done.\n</function>\n</tool_call>", "Done.", "closers whose call is gone"),
            ("<tool_call>", "", "nothing but the opener"),
            ("Each call is wrapped in <tool_call> and </tool_call>", "Each call is wrapped in <tool_call> and </tool_call>", "R-17: talked about"),
            ("The opening tag is\n`<tool_call>`", "The opening tag is\n`<tool_call>`", "quoted as code"),
            (
                "Like this:\n<tool_call>\n<function=read_file>\n</function>\n</tool_call>",
                "Like this:\n<tool_call>\n<function=read_file>\n</function>\n</tool_call>",
                "a whole example",
            ),
            ("Plain answer.\n", "Plain answer.\n", "no markup, left as it was"),
        ];
        for (text, want, why) in cases {
            assert_eq!(without_dangling_markup(text), want, "{why}");
        }
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
    /// called are the part that has to come out the same — from the same cache
    /// state, which the warm-up below sets.
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
            // Both sides are asked with this exact prompt already cached. Ollama 0.40's
            // llama-server reuses its prompt cache, and the tool chosen at temperature 0
            // follows the cache: 2026-10-26, conversation 0 called `list_files` both ways
            // after another conversation, and `read_file` both ways on an exact hit. Asked
            // native-then-raw with no warm-up, native got the prefix hit and raw the exact
            // one, and they disagreed on every run after the first.
            chat(model, &messages, &tools, options).expect("warm-up chat");
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
