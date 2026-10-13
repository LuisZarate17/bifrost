//! A budget for the prompt inside one tool turn: what to set aside when the
//! conversation outgrows the model's context.
//!
//! **Why it exists (measured 2026-10-02, Ollama 0.33.3, `qwen3-coder:30b`).**
//! Inside a tool turn every result is appended to the conversation and nothing
//! counts it. When the prompt exceeds `num_ctx`, Ollama does **not** error — it
//! cuts, and cuts from the middle:
//!
//! - a 14k-token prompt at `num_ctx` 4096 came back with `prompt_eval_count` 43:
//!   the system message and the last message survived, everything between went;
//! - a turn shaped like Mimir's (system, the user's request, a read of `a.rs` at
//!   11k tokens, a read of `b.rs` at 11k) at `num_ctx` 8192 came back with
//!   `prompt_eval_count` 4098: only the tail of the last file was kept, and the
//!   model answered about the code instead of the request.
//!
//! The user's request was lost without a word. Since H5 reads inside the folder
//! run without asking, so nothing makes a turn piling up 64 KB reads visible.
//!
//! **The rule is deterministic and oldest-first.** Old tool results and old long
//! arguments (a `create_file` content, a big `new_text`) are replaced by a one
//! line stub until the estimate fits. Messages are never dropped, so every call
//! stays paired with its result, and the stub names the tool and path so the
//! model can ask again. What is never touched: the system prompt, any user
//! message, the last message, the newest tool result, and anything already set
//! aside — which is what makes it one-way, and keeps the prefix of the next
//! request identical for Ollama's prompt cache. **The newest read of each file
//! (and range: lines 1–100 and lines 300–400 of one file are two reads, since H8)
//! goes last**: a first pass leaves them alone, and only if the turn still does
//! not fit does a second pass take them too, oldest first. Protecting them
//! outright would protect every file read once — five different files read in a
//! turn, the very case this exists for, would set nothing aside.
//!
//! **The estimate is characters over a calibrated characters-per-token.** The
//! dense file in the measurement above came to about 2 characters a token;
//! ordinary code is 3 to 3.5. Without a previous round to learn from, the guess is
//! the conservative 2.5; with one, it is that round's own `chars / tokens`
//! (Ollama reports `prompt_eval_count`), clamped so that a round Ollama cut — its
//! count far too low — cannot make the next estimate absurdly generous.
//!
//! Pure on purpose: no I/O, so the app and the probe share the one rule and it is
//! testable without a model.

use std::collections::HashMap;

use crate::chat::{Message, Tool, ToolCall};
use crate::tools;

/// How much of `num_ctx` the request may use. The rest is left for the reply.
/// The same 0.75 the app's per-send trimming uses.
pub const BUDGET: f64 = 0.75;

/// Past this share of `num_ctx` the request is said not to fit. Above
/// [`BUDGET`] but below this, results are set aside as far as the rule allows
/// and the turn goes on: it still fits, with less room for the reply.
/// **Measured 2026-10-02** (`tools/e2e/mimir-context.mjs`): at `num_ctx` 8192 a
/// turn whose newest read alone kept it at 6,663 tokens — 81% — answered
/// correctly with nothing cut, yet a warning at 75% told the user it no longer
/// fit. The warning is for when the request really is at risk.
pub const HARD: f64 = 0.9;

/// Characters a token is assumed to cost before there is a round to learn from.
/// Conservative: the densest file measured was ~2, ordinary code 3 to 3.5.
pub const DEFAULT_CHARS_PER_TOKEN: f64 = 2.5;

/// Clamp for the calibrated ratio. Below 1.5 would mean a token per letter; above
/// 5.0 is longer than any English or code measured.
const MIN_CHARS_PER_TOKEN: f64 = 1.5;
const MAX_CHARS_PER_TOKEN: f64 = 5.0;

/// A tool result this long or shorter is not worth setting aside: the stub would
/// save little and the model loses what it said.
pub const RESULT_LIMIT: usize = 600;

/// A string argument this long or shorter stays. Past it, it is the content of a
/// file the model wrote or a big replacement text.
pub const ARG_LIMIT: usize = 2000;

/// Starts every stub, so a set-aside message is recognisable and never redone.
pub const STUB_MARKER: &str = "[Set aside to fit";

/// What the prompt may cost, and how many characters a token is taken to be.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Budget {
    pub num_ctx: u32,
    pub chars_per_token: f64,
}

impl Budget {
    /// `calibration` is the previous round's `(characters sent, prompt_eval_count)`.
    /// Ignored when either is zero.
    ///
    /// **A ratio above the ceiling is not trusted, it is thrown away.** More than
    /// five characters a token is what a round Ollama cut looks like — measured:
    /// 14,000 tokens of prompt reported as 43 — and clamping it to the ceiling
    /// would still make the next estimate twice as generous as the default, so
    /// the next round would be cut again. A ratio below the floor is clamped: a
    /// low ratio only makes the estimate stricter.
    pub fn new(num_ctx: u32, calibration: Option<(u64, u32)>) -> Self {
        let chars_per_token = match calibration {
            Some((chars, tokens)) if chars > 0 && tokens > 0 => {
                let ratio = chars as f64 / tokens as f64;
                if ratio > MAX_CHARS_PER_TOKEN { DEFAULT_CHARS_PER_TOKEN } else { ratio.max(MIN_CHARS_PER_TOKEN) }
            }
            _ => DEFAULT_CHARS_PER_TOKEN,
        };
        Self { num_ctx, chars_per_token }
    }

    /// The most tokens the request may be estimated at.
    pub fn allowed_tokens(&self) -> u32 {
        (self.num_ctx as f64 * BUDGET) as u32
    }

    /// Past this, the request is said not to fit: see [`HARD`].
    pub fn hard_tokens(&self) -> u32 {
        (self.num_ctx as f64 * HARD) as u32
    }

    /// Characters to tokens, rounded up.
    pub fn tokens(&self, chars: u64) -> u32 {
        (chars as f64 / self.chars_per_token).ceil() as u32
    }
}

/// What [`compact`] decided.
#[derive(Debug, Clone, PartialEq)]
pub struct Compaction {
    /// Indices into the `messages` given, oldest first.
    pub set_aside: Vec<usize>,
    /// Whether the estimate is within [`HARD`] once those are set aside. It may
    /// be over [`BUDGET`] and still fit.
    pub fits: bool,
    /// The estimate once they are.
    pub estimated_tokens: u32,
}

fn message_chars(m: &Message) -> u64 {
    let mut n = m.content.chars().count() as u64;
    for c in m.tool_calls.as_deref().unwrap_or(&[]) {
        n += c.function.name.chars().count() as u64;
        n += c.function.arguments.to_string().chars().count() as u64;
    }
    n
}

/// Characters the request carries: every message's content, the name and
/// arguments of every call, and the tool schemas — sent every round, about 3 KB.
/// Images are not counted; a picture costs tokens by its size, not its base64.
pub fn prompt_chars(messages: &[Message], tools: &[Tool]) -> u64 {
    let schemas = serde_json::to_string(tools).map(|s| s.chars().count() as u64).unwrap_or(0);
    messages.iter().map(message_chars).sum::<u64>() + schemas
}

/// The `path` of the first call on an assistant message, tidied for comparing.
/// The path a call names, in one spelling: `./src/A.rs`, `src\a.rs` and
/// `src/a.rs` are one file on Windows, and a second spelling would make an
/// older read look like the newest copy of its own file.
fn call_path(call: &ToolCall) -> Option<String> {
    call.function.arguments["path"].as_str().map(|p| {
        let p = p.trim().replace('\\', "/");
        let p = p.strip_prefix("./").unwrap_or(&p).to_string();
        if cfg!(windows) { p.to_lowercase() } else { p }
    })
}

/// What a read read, for deciding which is the newest copy: the file and the
/// lines. **Two ranges of one file are two reads** — lines 1–100 and lines
/// 300–400 do not replace each other, and the second would otherwise let the
/// first be set aside, making the model read what it already had. The range goes
/// through [`tools::read_range`], so `120` and `"120"` are one key.
///
/// **A range that cannot be read has no key (R-22).** It was refused before it ran,
/// so it returned no content; falling back to the whole-file key made it look like
/// the newest copy of the file and put the one good read first in line to go.
fn read_key(call: &ToolCall) -> Option<String> {
    let path = call_path(call)?;
    Some(match tools::read_range(&call.function.arguments).ok()? {
        Some(r) => format!("{path}#{}-{}", r.start, r.end.map_or(String::new(), |e| e.to_string())),
        None => path,
    })
}

/// How a range is said in a stub: `lines 120–180`, or `from line 120`.
fn describe_range(range: tools::Lines) -> String {
    match range.end {
        Some(e) => format!("lines {}–{e}", range.start),
        None => format!("from line {}", range.start),
    }
}

/// The call that produced the tool result at `i`: the assistant message before.
fn paired_call(messages: &[Message], i: usize) -> Option<&ToolCall> {
    let before = messages.get(i.checked_sub(1)?)?;
    if before.role != "assistant" {
        return None;
    }
    before.tool_calls.as_deref()?.first()
}

fn has_long_argument(m: &Message) -> bool {
    m.tool_calls.as_deref().unwrap_or(&[]).iter().any(|c| {
        c.function
            .arguments
            .as_object()
            .is_some_and(|o| o.values().any(|v| v.as_str().is_some_and(|s| s.chars().count() > ARG_LIMIT)))
    })
}

/// Which messages, oldest first, to set aside so the prompt fits `budget`.
pub fn compact(messages: &[Message], tools: &[Tool], budget: &Budget) -> Compaction {
    let mut total = prompt_chars(messages, tools);
    let allowed = budget.allowed_tokens();
    let mut set = Vec::new();

    if budget.tokens(total) > allowed {
        let newest_tool = messages.iter().rposition(|m| m.role == "tool");
        let mut newest_read: HashMap<String, usize> = HashMap::new();
        for (i, m) in messages.iter().enumerate() {
            if m.role != "tool" {
                continue;
            }
            let call = paired_call(messages, i);
            let name = call.map(|c| c.function.name.as_str()).or(m.tool_name.as_deref());
            // Only a read that returned content is a copy of the file: one that
            // failed (`error: …`, by [`tools`]'s contract) is a few words and must
            // not stand in for the good read of the same file.
            if name == Some("read_file")
                && !m.content.trim_start().starts_with("error:")
                && let Some(key) = call.and_then(read_key)
            {
                newest_read.insert(key, i);
            }
        }
        let last = messages.len().saturating_sub(1);
        let candidate = |i: usize, m: &Message| {
            let long = match m.role.as_str() {
                "tool" => m.content.chars().count() > RESULT_LIMIT && !is_stub(&m.content),
                "assistant" => has_long_argument(m),
                _ => false,
            };
            long && i != last && Some(i) != newest_tool
        };
        let newest_of_its_file = |i: usize, m: &Message| {
            m.role == "tool"
                && paired_call(messages, i).and_then(read_key).is_some_and(|k| newest_read.get(&k) == Some(&i))
        };

        // Two passes, oldest first in each. The first leaves alone the newest read
        // of every file, since that is the copy the model works from. Only if the
        // turn still does not fit do those go too — otherwise a turn that reads
        // five different files once each, the very case this exists for, would
        // protect all five and set aside nothing. The last message and the newest
        // result are never set aside.
        for keep_newest_reads in [true, false] {
            for (i, m) in messages.iter().enumerate() {
                if budget.tokens(total) <= allowed {
                    break;
                }
                if !candidate(i, m) || set.contains(&i) || (keep_newest_reads && newest_of_its_file(i, m)) {
                    continue;
                }
                let mut stubbed = m.clone();
                set_aside(&mut stubbed, paired_call(messages, i));
                total = total - message_chars(m) + message_chars(&stubbed);
                set.push(i);
            }
        }
        set.sort_unstable();
    }

    let estimated_tokens = budget.tokens(total);
    Compaction { set_aside: set, fits: estimated_tokens <= budget.hard_tokens(), estimated_tokens }
}

/// What a long argument becomes once it has been sent: its length, and a short
/// fingerprint of the text so that an identical resend can still be recognised
/// ([`same_argument`]). The fingerprint is 32 bits of FNV-1a — stable across Rust
/// versions, like the undo hash — and means nothing to the model.
fn argument_stub(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("[{} characters, already sent, #{:08x}]", text.chars().count(), h as u32)
}

/// Whether `kept`, an argument as the conversation holds it now, is `sent`, the
/// same call's argument as the model sends it again. **They are equal, or `kept`
/// is the stub ([`set_aside`]) of exactly `sent`'s text (R-21):** once an old long
/// argument is set aside, an identical resend no longer equals the history, and the
/// repeat rule ([`tools::prior_failures`]) would start counting again from nothing —
/// the model could resend one impossible edit for ever. The stub keeps the length
/// **and a fingerprint**: length alone would make a retry with one character fixed
/// (same length, different text) count as the same failed call, and end a turn
/// that was making progress.
pub fn same_argument(kept: &serde_json::Value, sent: &serde_json::Value) -> bool {
    if kept == sent {
        return true;
    }
    match (kept.as_str(), sent.as_str()) {
        (Some(k), Some(s)) => s.chars().count() > ARG_LIMIT && k == argument_stub(s),
        _ => false,
    }
}

/// Whether a tool result is already a stub from [`set_aside`]: it starts with
/// [`STUB_MARKER`], after the `error: ` that a stubbed failure keeps.
fn is_stub(content: &str) -> bool {
    let c = content.trim_start();
    c.strip_prefix(FAILED_PREFIX).map_or(c, str::trim_start).starts_with(STUB_MARKER)
}

/// How a failed tool result begins ([`tools`]'s contract), and so how its stub does.
const FAILED_PREFIX: &str = "error:";

/// Replace what `message` carries with a stub.
///
/// A tool result becomes one line naming the tool and path (from `call`, the
/// assistant call that produced it, when known). **A result that was a failure
/// (`error: …`) keeps that prefix on its stub**: [`tools::prior_failures`] counts
/// failures by it, and a failed edit's "closest text" error is long enough to be set
/// aside along with its `old_text`, after which the repeat rule would otherwise stop
/// seeing the failure and let the model resend the same impossible edit for longer. An assistant call keeps its name
/// and every argument, but a long string argument becomes
/// `"[N characters, already sent, #fingerprint]"`.
pub fn set_aside(message: &mut Message, call: Option<&ToolCall>) {
    if message.role == "tool" {
        let lines = message.content.lines().count();
        let name = call.map(|c| c.function.name.clone()).or_else(|| message.tool_name.clone());
        let path = call.and_then(|c| c.function.arguments["path"].as_str().map(str::to_string));
        let range = call
            .filter(|c| c.function.name == "read_file")
            .and_then(|c| tools::read_range(&c.function.arguments).ok().flatten());
        let path = match (path, range) {
            (Some(p), Some(r)) => Some(format!("{p} {}", describe_range(r))),
            (p, _) => p,
        };
        let what = match (&name, &path) {
            (Some(n), Some(p)) => format!("the earlier result of {n} {p}"),
            (Some(n), None) => format!("the earlier result of {n}"),
            _ => "an earlier result".to_string(),
        };
        let again = if name.as_deref() == Some("read_file") { "Read it again" } else { "Run it again" };
        let failed = if message.content.trim_start().starts_with(FAILED_PREFIX) { "error: " } else { "" };
        message.content = format!(
            "{failed}{STUB_MARKER} the model's context: {what} ({lines} line{}). {again} if you still need it.]",
            if lines == 1 { "" } else { "s" }
        );
        return;
    }
    for c in message.tool_calls.as_deref_mut().unwrap_or(&mut []) {
        if let Some(args) = c.function.arguments.as_object_mut() {
            for v in args.values_mut() {
                if let Some(s) = v.as_str()
                    && s.chars().count() > ARG_LIMIT
                {
                    *v = serde_json::Value::String(argument_stub(s));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::FunctionCall;
    use serde_json::json;

    fn call(name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall { id: None, function: FunctionCall { name: name.to_string(), arguments: args } }
    }

    fn asks(name: &str, args: serde_json::Value) -> Message {
        Message {
            role: "assistant".into(),
            content: String::new(),
            images: None,
            tool_calls: Some(vec![call(name, args)]),
            tool_name: None,
        }
    }

    /// A read and its result: two messages, as the app puts them in the convo.
    fn read(path: &str, chars: usize) -> [Message; 2] {
        [asks("read_file", json!({ "path": path })), Message::tool_result("read_file", "x".repeat(chars))]
    }

    /// Characters a token costs by default: 2.5, so 1000 characters is 400 tokens.
    fn tight(allowed_tokens: u32) -> Budget {
        // 0.75 of num_ctx is the allowance.
        Budget::new((allowed_tokens as f64 / BUDGET).ceil() as u32 + 1, None)
    }

    /// A search and its result. Unlike a read, nothing protects an old one.
    fn found(chars: usize) -> [Message; 2] {
        [asks("search", json!({ "text": "x" })), Message::tool_result("search", "x".repeat(chars))]
    }

    /// A system prompt, a request, then a read of each path given — or, for the
    /// path `"?"`, a search — with a result of that many characters.
    fn turn(results: &[(&str, usize)]) -> Vec<Message> {
        let mut m = vec![Message::system("sys"), Message::user("summarise these")];
        for (p, n) in results {
            m.extend(if *p == "?" { found(*n) } else { read(p, *n) });
        }
        m
    }

    #[test]
    fn nothing_is_set_aside_under_budget() {
        let m = turn(&[("a.rs", 5000), ("b.rs", 5000)]);
        let c = compact(&m, &[], &Budget::new(32768, None));
        assert!(c.set_aside.is_empty());
        assert!(c.fits);
    }

    #[test]
    fn the_oldest_result_goes_first_and_only_as_many_as_needed() {
        let m = turn(&[("?", 5000), ("?", 5000), ("?", 5000), ("?", 5000)]);
        // 20000 chars = 8000 tokens; allow 5500, so one stub (saves 2000) is not
        // enough but two are.
        let c = compact(&m, &[], &tight(5500));
        // Indices: 3 and 5 are the first two results. The third is not needed
        // and the last is the newest result.
        assert_eq!(c.set_aside, vec![3, 5]);
        assert!(c.fits);
    }

    #[test]
    fn system_and_user_messages_are_never_set_aside() {
        let mut m = vec![Message::system("s".repeat(9000)), Message::user("u".repeat(9000))];
        m.extend(read("a.rs", 5000));
        let c = compact(&m, &[], &tight(100));
        assert!(!c.set_aside.contains(&0) && !c.set_aside.contains(&1));
        assert!(!c.fits);
    }

    #[test]
    fn an_older_read_of_a_file_goes_before_the_newest_read_of_any_file() {
        // a.rs read twice: its first read (index 3) goes first, and that is enough.
        let m = turn(&[("a.rs", 5000), ("b.rs", 5000), ("a.rs", 5000), ("c.rs", 5000)]);
        let c = compact(&m, &[], &tight(7000));
        assert_eq!(c.set_aside, vec![3]);
        assert!(c.fits);
    }

    #[test]
    fn the_newest_read_of_a_file_goes_too_when_nothing_else_is_left() {
        // Still over after the old read of a.rs: b.rs's only read (index 5) goes
        // next, oldest first — but never c.rs, the newest result.
        let m = turn(&[("a.rs", 5000), ("b.rs", 5000), ("a.rs", 5000), ("c.rs", 5000)]);
        let c = compact(&m, &[], &tight(5000));
        assert_eq!(c.set_aside, vec![3, 5]);
        assert!(c.fits);
        let c = compact(&m, &[], &tight(100));
        assert_eq!(c.set_aside, vec![3, 5, 7]);
        assert!(!c.fits);
    }

    #[test]
    fn over_the_budget_but_under_the_hard_limit_still_fits() {
        // One read alone, newest and protected: nothing can go. At 1,000 tokens
        // of allowance the hard limit is 1,200; a 2,800-character result is about
        // 1,120 tokens with the rest — over the budget, under the limit.
        let m = turn(&[("a.rs", 2800)]);
        let c = compact(&m, &[], &tight(1000));
        assert!(c.set_aside.is_empty());
        assert!(c.estimated_tokens > 1000 && c.fits, "{}", c.estimated_tokens);
    }

    #[test]
    fn one_file_spelt_two_ways_is_still_one_file() {
        // `./src/A.rs` then `src\a.rs`: the first is the older read of the same
        // file, so the first pass may take it.
        let m = turn(&[("./src/A.rs", 5000), ("src\\a.rs", 5000), ("b.rs", 5000)]);
        let c = compact(&m, &[], &tight(5000));
        assert_eq!(c.set_aside, vec![3]);
    }

    #[test]
    fn two_files_read_once_each_make_room_as_measured() {
        // The case measured against Ollama: a request, then two large reads of
        // different files. The first read goes; the second, newest, stays.
        let m = turn(&[("a.rs", 20000), ("b.rs", 20000)]);
        let c = compact(&m, &[], &tight(9000));
        assert_eq!(c.set_aside, vec![3]);
        assert!(c.fits);
    }

    #[test]
    fn a_long_create_file_argument_is_set_aside_with_its_result_kept() {
        let mut m = vec![Message::system("sys"), Message::user("write it")];
        m.push(asks("create_file", json!({ "path": "n.rs", "content": "y".repeat(8000) })));
        m.push(Message::tool_result("create_file", "Created n.rs (200 lines)."));
        m.extend(read("a.rs", 5000));
        let c = compact(&m, &[], &tight(2000));
        assert_eq!(c.set_aside, vec![2]);
        let mut stubbed = m[2].clone();
        set_aside(&mut stubbed, None);
        let args = &stubbed.tool_calls.unwrap()[0].function.arguments;
        let stub = args["content"].as_str().unwrap();
        assert!(stub.starts_with("[8000 characters, already sent, #"), "{stub}");
        assert_eq!(args["path"], "n.rs");
    }

    /// A failed edit whose error and long `old_text` have both been set aside is
    /// still a failure to the repeat rule when the model resends it identically.
    #[test]
    fn a_set_aside_failure_is_still_counted_by_the_repeat_rule() {
        let old = "x".repeat(3000);
        let edit = call("edit_file", json!({ "path": "a.rs", "old_text": old, "new_text": "y" }));
        let mut m = vec![
            Message::user("change it"),
            asks("edit_file", edit.function.arguments.clone()),
            Message::tool_result("edit_file", format!("error: old_text was not found. Closest text: {}", "z".repeat(2000))),
        ];
        assert_eq!(tools::prior_failures(&m, &edit), 1);
        let call = paired_call(&m, 2).cloned();
        set_aside(&mut m[2], call.as_ref());
        set_aside(&mut m[1], None);
        assert!(m[2].content.starts_with("error: [Set aside"), "{}", m[2].content);
        assert!(is_stub(&m[2].content));
        assert_eq!(tools::prior_failures(&m, &edit), 1, "a stubbed failure stopped counting");
    }

    /// **A retry with one character changed is not the call that failed (R-21).**
    /// Same length, different text: the stub's fingerprint tells them apart, so the
    /// repeat rule does not end a turn that is making progress. The identical text
    /// still matches.
    #[test]
    fn a_stub_matches_only_the_text_it_replaced_not_another_of_the_same_length() {
        let first = "x".repeat(3000);
        let mut retry = first.clone();
        retry.replace_range(1500..1501, "y");
        let stub = serde_json::Value::String(argument_stub(&first));
        assert!(same_argument(&stub, &json!(first)));
        assert!(!same_argument(&stub, &json!(retry)), "a same-length retry counted as the same call");
    }

    #[test]
    fn compacting_twice_sets_nothing_new_aside() {
        let mut m = turn(&[("?", 5000), ("?", 5000), ("?", 5000)]);
        let budget = tight(4000);
        let first = compact(&m, &[], &budget);
        assert!(first.fits && !first.set_aside.is_empty());
        for &i in &first.set_aside {
            let call = paired_call(&m, i).cloned();
            set_aside(&mut m[i], call.as_ref());
        }
        let again = compact(&m, &[], &budget);
        assert!(again.set_aside.is_empty());
        assert!(again.fits);
        assert_eq!(again.estimated_tokens, first.estimated_tokens);
    }

    #[test]
    fn a_stub_is_never_set_aside_again_even_when_still_over_budget() {
        let mut m = turn(&[("?", 5000), ("?", 5000)]);
        let call = paired_call(&m, 3).cloned();
        set_aside(&mut m[3], call.as_ref());
        assert!(m[3].content.starts_with(STUB_MARKER));
        let c = compact(&m, &[], &tight(10));
        assert!(c.set_aside.is_empty());
        assert!(!c.fits);
    }

    #[test]
    fn it_does_not_fit_when_only_protected_messages_remain() {
        let m = turn(&[("a.rs", 20000)]);
        let c = compact(&m, &[], &tight(1000));
        assert!(c.set_aside.is_empty());
        assert!(!c.fits);
    }

    #[test]
    fn the_stub_names_the_tool_and_path_and_counts_lines() {
        let mut m = Message::tool_result("read_file", "a\nb\nc\n");
        set_aside(&mut m, Some(&call("read_file", json!({ "path": "src/x.rs" }))));
        assert_eq!(
            m.content,
            "[Set aside to fit the model's context: the earlier result of read_file src/x.rs (3 lines). \
             Read it again if you still need it.]"
        );
    }

    #[test]
    fn a_ranged_stub_names_the_lines() {
        let mut m = Message::tool_result("read_file", "a\nb\nc\n");
        let ask = call("read_file", json!({ "path": "src/x.rs", "start_line": "120", "end_line": 180 }));
        set_aside(&mut m, Some(&ask));
        assert_eq!(
            m.content,
            "[Set aside to fit the model's context: the earlier result of read_file src/x.rs lines 120–180 (3 lines). \
             Read it again if you still need it.]"
        );
        let mut m = Message::tool_result("read_file", "a\n");
        set_aside(&mut m, Some(&call("read_file", json!({ "path": "src/x.rs", "start_line": 120 }))));
        assert!(m.content.contains("src/x.rs from line 120 (1 line)"), "{}", m.content);
    }

    /// A read of some lines of a file, and its result.
    fn ranged(path: &str, start: serde_json::Value, end: u32, chars: usize) -> [Message; 2] {
        [
            asks("read_file", json!({ "path": path, "start_line": start, "end_line": end })),
            Message::tool_result("read_file", "x".repeat(chars)),
        ]
    }

    #[test]
    fn two_ranges_of_one_file_are_both_the_newest_of_their_own_read() {
        // First pass: neither range of a.rs is "the older read of a.rs", so the
        // search between them and the last read is what goes — not the first range.
        let mut m = vec![Message::system("sys"), Message::user("go")];
        m.extend(ranged("a.rs", json!(1), 100, 5000));
        m.extend(ranged("a.rs", json!(300), 400, 5000));
        m.extend(found(5000));
        m.extend(read("b.rs", 5000));
        let c = compact(&m, &[], &tight(7000));
        assert_eq!(c.set_aside, vec![7]);
        assert!(c.fits);
    }

    #[test]
    fn a_repeat_of_the_same_range_is_the_older_read_and_goes_first() {
        let mut m = vec![Message::system("sys"), Message::user("go")];
        m.extend(ranged("a.rs", json!(10), 20, 5000));
        m.extend(ranged("a.rs", json!(10), 20, 5000));
        m.extend(read("b.rs", 5000));
        let c = compact(&m, &[], &tight(5000));
        assert_eq!(c.set_aside, vec![3]);
    }

    #[test]
    fn a_range_sent_as_a_string_is_the_same_range() {
        let mut m = vec![Message::system("sys"), Message::user("go")];
        m.extend(ranged("a.rs", json!("10"), 20, 5000));
        m.extend(ranged("a.rs", json!(10), 20, 5000));
        m.extend(read("b.rs", 5000));
        let c = compact(&m, &[], &tight(5000));
        assert_eq!(c.set_aside, vec![3]);
    }

    #[test]
    fn calibration_is_used_and_clamped() {
        assert_eq!(Budget::new(8192, None).chars_per_token, 2.5);
        assert_eq!(Budget::new(8192, Some((3000, 1000))).chars_per_token, 3.0);
        assert_eq!(Budget::new(8192, Some((100, 1000))).chars_per_token, 1.5);
        // Over five characters a token is a cut round, not a measurement.
        assert_eq!(Budget::new(8192, Some((90000, 1000))).chars_per_token, 2.5);
        assert_eq!(Budget::new(8192, Some((5000, 1000))).chars_per_token, 5.0);
        assert_eq!(Budget::new(8192, Some((0, 10))).chars_per_token, 2.5);
        assert_eq!(Budget::new(8192, Some((10, 0))).chars_per_token, 2.5);
        assert_eq!(Budget::new(8192, None).allowed_tokens(), 6144);
    }

    #[test]
    fn a_refused_read_does_not_demote_the_good_read_of_the_same_file() {
        let mut m = vec![Message::system("sys"), Message::user("go")];
        m.extend(read("a.rs", 5000));
        m.extend([
            asks("read_file", json!({ "path": "a.rs", "start_line": 90, "end_line": 20 })),
            Message::tool_result("read_file", "end_line is the last line to read, not a count"),
        ]);
        m.extend(found(5000));
        m.extend(read("b.rs", 5000));
        // The search (index 7) goes; the only copy of a.rs (index 3) stays.
        assert_eq!(compact(&m, &[], &tight(5000)).set_aside, vec![7]);
    }

    #[test]
    fn a_read_that_failed_is_not_the_newest_copy_of_its_file() {
        let mut m = vec![Message::system("sys"), Message::user("go")];
        m.extend(read("a.rs", 5000));
        m.extend([
            asks("read_file", json!({ "path": "a.rs" })),
            Message::tool_result("read_file", format!("error: cannot read a.rs{}", " x".repeat(2500))),
        ]);
        m.extend(found(5000));
        m.extend(read("b.rs", 5000));
        // The failed result is long here, but not content: the good read (3) stays
        // ahead of it, and the oldest of the rest goes first.
        let c = compact(&m, &[], &tight(5000));
        assert!(!c.set_aside.contains(&3), "{:?}", c.set_aside);
    }

    #[test]
    fn a_stubbed_argument_is_the_same_argument_as_the_text_it_replaced() {
        let long = "x".repeat(ARG_LIMIT + 1);
        let mut stubbed = asks("edit_file", json!({ "old_text": long }));
        set_aside(&mut stubbed, None);
        let kept = &stubbed.tool_calls.as_ref().unwrap()[0].function.arguments["old_text"];
        assert!(same_argument(kept, &json!(long)));
        assert!(!same_argument(kept, &json!("x".repeat(ARG_LIMIT + 2))));
        assert!(!same_argument(kept, &json!("short")));
        assert!(same_argument(&json!("short"), &json!("short")));
        assert!(!same_argument(&json!(5), &json!(long)));
    }

    #[test]
    fn prompt_chars_counts_arguments_and_the_schemas() {
        let plain = vec![Message::user("abcd")];
        // An empty tool list still serializes, as `[]`.
        assert_eq!(prompt_chars(&plain, &[]), 4 + 2);

        let with_call = vec![Message::user("abcd"), asks("read_file", json!({ "path": "a.rs" }))];
        let args = json!({ "path": "a.rs" }).to_string().chars().count() as u64;
        assert_eq!(prompt_chars(&with_call, &[]), 4 + 2 + "read_file".len() as u64 + args);

        let tools = crate::tools::available(false, None);
        let schemas = serde_json::to_string(&tools).unwrap().chars().count() as u64;
        assert!(schemas > 100);
        assert_eq!(prompt_chars(&plain, &tools), 4 + schemas);
    }
}
