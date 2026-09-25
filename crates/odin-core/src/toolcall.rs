//! Recovering the tool calls Ollama throws away — because the model gets one
//! thing wrong every time, and Ollama's parser drops the whole call for it.
//!
//! **The failure this exists for, measured at G7.** Asked to edit a line like
//! `println!("{}", greet("world"));`, `qwen3-vl:8b` writes the call as
//!
//! ```text
//! "old_text": "    println!("{}", greet(\"world\"));"
//! ```
//!
//! It escapes the quote before `world` and **never the quote before `{`**. The
//! JSON is invalid, Ollama cannot parse the call, and `/api/chat` answers
//! `HTTP 500 invalid character '{' after object key:value pair`. It is not a
//! slip: it failed 6 runs out of 6, and a sentence in the tool description
//! telling it to escape those quotes changed nothing (0 of 5 either way). `"{`
//! behaves like one unit the model has rarely seen written as `\"{`. It hits
//! any edit touching `format!("{x}")`, `f"{name}"` or a JSON literal — so, most
//! code.
//!
//! **The text cannot be had from `/api/chat` at all.** Ollama's renderer for
//! this model scans every reply for `<tool_call>` — measured: HTTP 500 again
//! with no `tools` declared — so asking it to "just return the text" fails the
//! same way. Only `/api/generate` in raw mode leaves the output alone.
//!
//! **So this is a recovery path, not a replacement.** Native tool calling stays
//! the normal route — it is what G6 verified, and it is the only one that
//! carries images. When, and only when, Ollama rejects a call it could not
//! parse, the same turn is asked again through raw `/api/generate` with the
//! conversation laid out in Qwen's own chat format ([`render_chatml`]), and the
//! call is read by [`parse_call`]: strict JSON first, then a repair that uses
//! what we know and Ollama does not — **the names of the arguments this tool
//! takes.** Anchored on those, a bare `"` inside a value is just a character.
//!
//! The layout was checked by reproduction: rendered this way at temperature 0,
//! the model produced the identical broken call Ollama had rejected — so this
//! is the same prompt it saw, not a similar one.

use serde_json::{Map, Value};

use crate::chat::{FunctionCall, Message, Tool, ToolCall};

/// The tool section of the system prompt, in the format Qwen's chat template
/// uses — which is what the model saw in training whenever it was given tools.
pub fn tools_section(tools: &[Tool]) -> String {
    let defs: Vec<String> = tools
        .iter()
        .map(|t| serde_json::to_value(t).map(|v| spaced(&v)).unwrap_or_default())
        .collect();
    format!(
        "# Tools\n\n\
         You may call one or more functions to assist with the user query.\n\n\
         You are provided with function signatures within <tools></tools> XML tags:\n\
         <tools>\n{}\n</tools>\n\n\
         For each function call, return a json object with function name and arguments \
         within <tool_call></tool_call> XML tags:\n\
         <tool_call>\n{{\"name\": <function-name>, \"arguments\": <args-json-object>}}\n</tool_call>",
        defs.join("\n")
    )
}

/// JSON with `": "` and `", "` separators — how Qwen's template writes it with
/// `tojson`, as opposed to serde's compact form. Keeps the recovered prompt
/// character-for-character like the one the model saw.
pub fn spaced(v: &Value) -> String {
    match v {
        Value::Object(m) => {
            let parts: Vec<String> = m
                .iter()
                .map(|(k, v)| format!("{}: {}", Value::String(k.clone()), spaced(v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        Value::Array(a) => format!("[{}]", a.iter().map(spaced).collect::<Vec<_>>().join(", ")),
        other => other.to_string(),
    }
}

/// A conversation as the raw prompt Qwen's chat template produces, ending with
/// the assistant's turn open for the model to write.
///
/// - the tool section follows the system message (one is added if there is none)
/// - an assistant turn that called a tool carries the call as `<tool_call>` text
/// - a tool result is a user turn wrapped in `<tool_response>`
///
/// **Images cannot ride along in raw mode**, so a turn that had them says so in
/// words. This only runs to recover a malformed call, where the turn is already
/// about text in a file — but the model is told rather than left to wonder.
pub fn render_chatml(messages: &[Message], tools: &[Tool]) -> String {
    let section = tools_section(tools);
    let mut out = String::new();
    let mut have_system = false;

    for m in messages {
        match m.role.as_str() {
            "system" if !have_system => {
                have_system = true;
                out.push_str(&format!("<|im_start|>system\n{}\n\n{section}<|im_end|>\n", m.content));
            }
            "assistant" => {
                let mut text = m.content.trim().to_string();
                for call in m.tool_calls.as_deref().unwrap_or_default() {
                    let args = match &call.function.arguments {
                        Value::String(s) => s.clone(),
                        v => spaced(v),
                    };
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&format!(
                        "<tool_call>\n{{\"name\": {}, \"arguments\": {args}}}\n</tool_call>",
                        Value::String(call.function.name.clone())
                    ));
                }
                out.push_str(&format!("<|im_start|>assistant\n{text}<|im_end|>\n"));
            }
            "tool" => out.push_str(&format!(
                "<|im_start|>user\n<tool_response>\n{}\n</tool_response><|im_end|>\n",
                m.content.trim_end_matches('\n')
            )),
            role => {
                let images = m.images.as_ref().map_or(0, Vec::len);
                let note = if images > 0 {
                    format!(
                        "[{images} image{} attached here, not shown in this retry]\n",
                        if images == 1 { "" } else { "s" }
                    )
                } else {
                    String::new()
                };
                let role = if role == "system" { "user" } else { role };
                out.push_str(&format!("<|im_start|>{role}\n{note}{}<|im_end|>\n", m.content));
            }
        }
    }
    if !have_system {
        out.insert_str(0, &format!("<|im_start|>system\n{section}<|im_end|>\n"));
    }
    out.push_str("<|im_start|>assistant\n");
    out
}

/// Find the tool calls in a reply.
///
/// Returns the text with the calls removed, the calls that parsed, and a
/// description of any block that could not be read — which goes back to the
/// model as an error rather than being dropped in silence.
pub fn extract_calls(content: &str, tools: &[Tool]) -> (String, Vec<ToolCall>, Vec<String>) {
    const OPEN: &str = "<tool_call>";
    const CLOSE: &str = "</tool_call>";

    let mut text = String::new();
    let mut calls = Vec::new();
    let mut failures = Vec::new();
    let mut rest = content;

    while let Some(start) = rest.find(OPEN) {
        text.push_str(&rest[..start]);
        let after = &rest[start + OPEN.len()..];
        // A reply cut off mid-call has no closing tag; take what there is.
        let (block, next) = match after.find(CLOSE) {
            Some(end) => (&after[..end], &after[end + CLOSE.len()..]),
            None => (after, ""),
        };
        match parse_call(block, tools) {
            Ok(c) => calls.push(c),
            Err(e) => failures.push(e),
        }
        rest = next;
    }
    text.push_str(rest);
    (text.trim().to_string(), calls, failures)
}

/// One `<tool_call>` body → a call. Strict JSON first; if that fails, a repair
/// anchored on the tool's own argument names.
pub fn parse_call(block: &str, tools: &[Tool]) -> Result<ToolCall, String> {
    let block = block.trim();
    if let Ok(v) = serde_json::from_str::<Value>(block)
        && let Some(call) = from_value(&v)
    {
        return Ok(call);
    }
    repair(block, tools)
}

fn from_value(v: &Value) -> Option<ToolCall> {
    let name = v.get("name")?.as_str()?.to_string();
    let arguments = match v.get("arguments") {
        // Some models send the arguments as a JSON string of JSON.
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Object(Map::new())),
        Some(a) => a.clone(),
        None => Value::Object(Map::new()),
    };
    Some(ToolCall { id: None, function: FunctionCall { name, arguments } })
}

/// The tool's argument names, from its JSON schema.
fn arg_names<'a>(tools: &'a [Tool], name: &str) -> Option<Vec<&'a str>> {
    let tool = tools.iter().find(|t| t.function.name == name)?;
    let props = tool.function.parameters.get("properties")?.as_object()?;
    Some(props.keys().map(String::as_str).collect())
}

fn skip_ws(s: &[u8], mut i: usize) -> usize {
    while i < s.len() && s[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// Where `"key"<ws>:<ws>"` starts and where its value starts, for an
/// occurrence preceded (ignoring whitespace) by `{` or `,` — i.e. where a key can
/// actually be, rather than the same characters inside some value.
fn find_key(s: &str, key: &str, from: usize) -> Option<(usize, usize)> {
    let b = s.as_bytes();
    let needle = format!("\"{key}\"");
    let mut search = from;
    while let Some(off) = s[search..].find(&needle) {
        let at = search + off;
        let mut before = at;
        while before > 0 && b[before - 1].is_ascii_whitespace() {
            before -= 1;
        }
        let key_position = before > 0 && matches!(b[before - 1], b'{' | b',');
        let mut i = skip_ws(b, at + needle.len());
        if key_position && i < b.len() && b[i] == b':' {
            i = skip_ws(b, i + 1);
            if i < b.len() && b[i] == b'"' {
                return Some((at, i + 1));
            }
        }
        search = at + needle.len();
    }
    None
}

/// Undo JSON escapes, and leave everything else — a bare `"` included — as it is.
fn unescape_tolerant(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('/') => out.push('/'),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    Some(ch) => out.push(ch),
                    None => {
                        out.push_str("\\u");
                        out.push_str(&hex);
                    }
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn repair(block: &str, tools: &[Tool]) -> Result<ToolCall, String> {
    let fail = |why: &str| {
        let preview: String = block.chars().take(160).collect();
        format!("the tool call could not be read ({why}): {preview}")
    };

    let (_, name_start) = find_key(block, "name", 0).ok_or_else(|| fail("no tool name"))?;
    let name_end = block[name_start..].find('"').ok_or_else(|| fail("unterminated tool name"))? + name_start;
    let name = block[name_start..name_end].to_string();
    let keys = arg_names(tools, &name).ok_or_else(|| fail(&format!("no such tool: {name}")))?;

    let args_at = block.find("\"arguments\"").ok_or_else(|| fail("no arguments"))?;
    let brace = block[args_at..].find('{').ok_or_else(|| fail("arguments are not an object"))? + args_at;

    // Every argument we can find, in the order it appears.
    let mut found: Vec<(&str, usize, usize)> = keys
        .iter()
        .filter_map(|k| find_key(block, k, brace).map(|(ks, vs)| (*k, ks, vs)))
        .collect();
    found.sort_by_key(|(_, ks, _)| *ks);
    if found.is_empty() {
        return Err(fail("no recognisable arguments"));
    }

    let b = block.as_bytes();
    let mut args = Map::new();
    for (i, (key, _, value_start)) in found.iter().enumerate() {
        // A value ends at the quote before the next key's `,` — or, for the last
        // one, at the quote before the closing `}}`.
        let end_quote = if let Some((_, next_key, _)) = found.get(i + 1) {
            let mut j = *next_key;
            while j > 0 && b[j - 1].is_ascii_whitespace() {
                j -= 1;
            }
            if j == 0 || b[j - 1] != b',' {
                return Err(fail("arguments are not separated by commas"));
            }
            j -= 1;
            while j > 0 && b[j - 1].is_ascii_whitespace() {
                j -= 1;
            }
            j.checked_sub(1).filter(|q| b[*q] == b'"')
        } else {
            let mut j = block.len();
            for _ in 0..2 {
                while j > 0 && b[j - 1].is_ascii_whitespace() {
                    j -= 1;
                }
                if j == 0 || b[j - 1] != b'}' {
                    return Err(fail("the call is not closed with }}"));
                }
                j -= 1;
            }
            while j > 0 && b[j - 1].is_ascii_whitespace() {
                j -= 1;
            }
            j.checked_sub(1).filter(|q| b[*q] == b'"')
        }
        .ok_or_else(|| fail(&format!("{key} is not a string")))?;

        if end_quote < *value_start {
            return Err(fail(&format!("{key} has no value")));
        }
        args.insert(
            key.to_string(),
            Value::String(unescape_tolerant(&block[*value_start..end_quote])),
        );
    }

    Ok(ToolCall { id: None, function: FunctionCall { name, arguments: Value::Object(args) } })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tools() -> Vec<Tool> {
        crate::tools::available(true)
    }

    fn arg(c: &ToolCall, k: &str) -> String {
        c.function.arguments[k].as_str().unwrap().to_string()
    }

    /// Verbatim from qwen3-vl:8b-instruct-q4_K_M at temperature 0, captured at
    /// G7 — the call Ollama answered with HTTP 500.
    const CAPTURED: &str = r#"{"name": "edit_file", "arguments": {"path": "src/main.rs", "old_text": "    println!("{}", greet(\"world\"));", "new_text": "    println!("{}", greet(\"Luis\"));"}}"#;

    #[test]
    fn the_captured_failure_is_invalid_json() {
        // The premise. If this ever parses, the repair is no longer what saves it.
        assert!(serde_json::from_str::<Value>(CAPTURED).is_err());
    }

    #[test]
    fn the_captured_failure_is_repaired_exactly() {
        let c = parse_call(CAPTURED, &tools()).unwrap();
        assert_eq!(c.function.name, "edit_file");
        assert_eq!(arg(&c, "path"), "src/main.rs");
        assert_eq!(arg(&c, "old_text"), r#"    println!("{}", greet("world"));"#);
        assert_eq!(arg(&c, "new_text"), r#"    println!("{}", greet("Luis"));"#);
    }

    #[test]
    fn valid_json_takes_the_strict_path() {
        let c = parse_call(r#"{"name": "read_file", "arguments": {"path": "a b/c.rs"}}"#, &tools()).unwrap();
        assert_eq!(arg(&c, "path"), "a b/c.rs");
    }

    #[test]
    fn repairs_unescaped_quotes_in_every_argument() {
        let raw = r#"{"name": "create_file", "arguments": {"path": "x.json", "content": "{"a": "b"}\n"}}"#;
        let c = parse_call(raw, &tools()).unwrap();
        assert_eq!(arg(&c, "content"), "{\"a\": \"b\"}\n");
    }

    #[test]
    fn repair_handles_arguments_in_any_order_and_spacing() {
        let raw = "{\"name\":\"edit_file\",\"arguments\":{ \"new_text\" : \"say(\"hi\")\" ,\n \"old_text\": \"say(\"yo\")\", \"path\":\"a.py\" } }";
        let c = parse_call(raw, &tools()).unwrap();
        assert_eq!(arg(&c, "old_text"), "say(\"yo\")");
        assert_eq!(arg(&c, "new_text"), "say(\"hi\")");
        assert_eq!(arg(&c, "path"), "a.py");
    }

    #[test]
    fn escapes_that_were_written_are_honoured() {
        let raw = r#"{"name": "edit_file", "arguments": {"path": "a", "old_text": "a\tb\\c "{", "new_text": "éé\n"}}"#;
        let c = parse_call(raw, &tools()).unwrap();
        assert_eq!(arg(&c, "old_text"), "a\tb\\c \"{");
        assert_eq!(arg(&c, "new_text"), "éé\n");
    }

    #[test]
    fn a_key_name_inside_a_value_is_not_mistaken_for_a_key() {
        // "path" appears inside old_text, but not where a key can be.
        let raw = r#"{"name": "edit_file", "arguments": {"path": "cfg.json", "old_text": "{"path": "x"}", "new_text": "{"path": "y"}"}}"#;
        let c = parse_call(raw, &tools()).unwrap();
        assert_eq!(arg(&c, "path"), "cfg.json");
        assert_eq!(arg(&c, "old_text"), r#"{"path": "x"}"#);
    }

    #[test]
    fn an_unknown_tool_is_not_repaired_into_existence() {
        let raw = r#"{"name": "delete_everything", "arguments": {"path": "/"x"}}"#;
        assert!(parse_call(raw, &tools()).unwrap_err().contains("no such tool"));
    }

    #[test]
    fn garbage_is_reported_rather_than_guessed() {
        assert!(parse_call("not a call at all", &tools()).is_err());
        assert!(parse_call(r#"{"name": "edit_file", "arguments": {"path": "a", "old_text": "x""#, &tools()).is_err());
    }

    #[test]
    fn arguments_sent_as_a_string_of_json_are_unwrapped() {
        let c = parse_call(r#"{"name": "read_file", "arguments": "{\"path\": \"a.rs\"}"}"#, &tools()).unwrap();
        assert_eq!(arg(&c, "path"), "a.rs");
    }

    #[test]
    fn calls_are_found_in_the_reply_and_removed_from_its_text() {
        let content = format!("Let me change that.\n<tool_call>\n{CAPTURED}\n</tool_call>");
        let (text, calls, failures) = extract_calls(&content, &tools());
        assert_eq!(text, "Let me change that.");
        assert_eq!(calls.len(), 1);
        assert!(failures.is_empty());
    }

    #[test]
    fn a_reply_without_calls_is_just_text() {
        let (text, calls, failures) = extract_calls("The port is 8080.", &tools());
        assert_eq!((text.as_str(), calls.len(), failures.len()), ("The port is 8080.", 0, 0));
    }

    #[test]
    fn an_unreadable_call_is_reported_not_dropped() {
        let (_, calls, failures) = extract_calls("<tool_call>\n{broken\n</tool_call>", &tools());
        assert!(calls.is_empty());
        assert_eq!(failures.len(), 1);
    }

    #[test]
    fn a_call_cut_off_before_its_closing_tag_is_still_read() {
        let (_, calls, _) =
            extract_calls("<tool_call>\n{\"name\": \"read_file\", \"arguments\": {\"path\": \"a\"}}", &tools());
        assert_eq!(calls.len(), 1);
    }

    fn read_call(path: &str) -> Message {
        Message {
            role: "assistant".into(),
            content: String::new(),
            images: None,
            tool_calls: Some(vec![ToolCall {
                id: None,
                function: FunctionCall { name: "read_file".into(), arguments: json!({ "path": path }) },
            }]),
            tool_name: None,
        }
    }

    #[test]
    fn the_conversation_is_rendered_the_way_qwens_template_does() {
        let history = vec![
            Message::system("be careful"),
            Message::user("read a.rs"),
            read_call("a.rs"),
            Message::tool_result("read_file", "fn a() {}\n"),
        ];
        let prompt = render_chatml(&history, &tools());
        let section = tools_section(&tools());
        assert_eq!(
            prompt,
            format!(
                "<|im_start|>system\nbe careful\n\n{section}<|im_end|>\n\
                 <|im_start|>user\nread a.rs<|im_end|>\n\
                 <|im_start|>assistant\n<tool_call>\n{{\"name\": \"read_file\", \"arguments\": {{\"path\": \"a.rs\"}}}}\n</tool_call><|im_end|>\n\
                 <|im_start|>user\n<tool_response>\nfn a() {{}}\n</tool_response><|im_end|>\n\
                 <|im_start|>assistant\n"
            )
        );
    }

    #[test]
    fn a_system_turn_is_added_when_there_is_none() {
        let prompt = render_chatml(&[Message::user("hi")], &tools());
        assert!(prompt.starts_with("<|im_start|>system\n# Tools"));
        assert!(prompt.ends_with("<|im_start|>user\nhi<|im_end|>\n<|im_start|>assistant\n"));
    }

    #[test]
    fn images_are_mentioned_rather_than_silently_lost() {
        let m = Message::user("what is this?").with_images(vec!["AAAA".into(), "BBBB".into()]);
        let prompt = render_chatml(&[m], &tools());
        assert!(prompt.contains("[2 images attached here, not shown in this retry]\nwhat is this?"));
        assert!(!prompt.contains("AAAA"), "raw mode cannot carry image bytes");
    }

    #[test]
    fn every_tool_is_described_in_the_prompt() {
        let section = tools_section(&tools());
        for name in ["read_file", "edit_file", "create_file"] {
            assert!(section.contains(&format!("\"name\": \"{name}\"")), "{name} missing");
        }
    }

    #[test]
    fn spaced_json_matches_how_the_template_writes_it() {
        assert_eq!(spaced(&json!({ "a": 1, "b": [true, null], "c": { "d": "e" } })), r#"{"a": 1, "b": [true, null], "c": {"d": "e"}}"#);
        assert_eq!(spaced(&json!("q\"uote")), r#""q\"uote""#);
    }
}
