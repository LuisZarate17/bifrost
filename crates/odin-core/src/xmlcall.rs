//! Recovering tool calls in `qwen3-coder:30b`'s own layout — a different shape
//! from [`crate::toolcall`]'s Qwen ChatML, and a different robustness problem.
//!
//! **Two failures, measured at H4 (`docs/results/H4-results.md`, raw logs
//! under `evals/out/H4-2026-10-01/`).**
//!
//! 1. 6 of 48 runs died with `/api/chat returned 500 Internal Server Error:
//!    {"error":"XML syntax error on line N: unexpected EOF"}`. Every one was an
//!    edit touching `toolcall.rs`, whose own source contains the literal text
//!    `</tool_call>` — and Ollama's parser for this model ends the call at the
//!    FIRST `</tool_call>` it sees, even one sitting inside a parameter value.
//!    What follows is not valid XML on its own, and Go's XML parser hits EOF.
//! 2. Tool probe case 4: the model wrote the call *without* the opening
//!    `<tool_call>` tag, so Ollama passed it through as plain `content` with no
//!    `tool_calls` at all. The fixture below is reconstructed from the probe's
//!    60-char preview (which showed newlines as spaces) — everything after
//!    `</pa` is reconstructed, not captured verbatim.
//!
//! **So, like `toolcall.rs`, this is a recovery path, never a replacement.**
//! Native tool calling stays the normal route. The anchor this parser uses
//! instead of Ollama's own (which stops at the first `</tool_call>`/
//! `</parameter>`) is **the tools we offered and their argument names** — the
//! same idea `toolcall.rs` uses for the JSON layout, ported to this one.
//!
//! **The remaining ambiguity, stated honestly:** a value that itself contains
//! `</parameter>` at the end of a line followed by `<parameter=ANY_NAME>` that has a
//! `</parameter>` of its own further on, or a later `<tool_call>` followed by
//! `<function=OFFERED_TOOL>`, is still split there (and, in a call with no
//! `<tool_call>` wrapper, so is a line-start `<function=OFFERED_TOOL>`). This is the same class of limit `toolcall.rs`'s key-position
//! anchoring has — and, as there, every call still goes to the user for
//! approval before anything runs.

use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::chat::{FunctionCall, Message, Tool, ToolCall};

const IM_START: &str = "<|im_start|>";
const IM_END: &str = "<|im_end|>";

/// The default system text Ollama's `Qwen3CoderRenderer` substitutes when
/// tools are offered and the conversation has no system message of its own.
const DEFAULT_SYSTEM: &str =
    "You are Qwen, a helpful AI assistant that can interact with a computer to solve tasks.";

/// JSON with Go's `encoding/json` escaping of `<`, `>` and `&` as `\u003c`,
/// `\u003e`, `\u0026` — Ollama's renderer uses `json.Marshal` for every
/// compact-JSON fragment it writes, and a literal `<` in a `required` array or
/// an argument value would otherwise land unescaped, which is not the prompt
/// the model was trained on.
fn go_json(v: &Value) -> String {
    serde_json::to_string(v)
        .unwrap_or_default()
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
}

/// `formatToolDefinitionType`: a bare type as-is, one element of an array as
/// that element, several as compact JSON.
fn format_type(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) if a.len() == 1 => a[0].as_str().map(str::to_string).unwrap_or_default(),
        Value::Array(a) if a.is_empty() => "[]".to_string(),
        other => go_json(other),
    }
}

/// `formatToolCallArgument`: string raw, null as the word "null", an array or
/// object as compact JSON, everything else by its `Display`.
fn format_arg(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::String(s) => s.clone(),
        Value::Array(_) | Value::Object(_) => go_json(v),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
    }
}

/// `renderAdditionalKeys`, restricted to the keys Ollama's own schema types
/// keep (everything else was dropped before the renderer ever saw it — see
/// `api-types.go`'s `ToolProperty`/`ToolFunctionParameters`). Null is skipped,
/// an empty array or object is treated as absent, and — since Go's map
/// iteration order is random and ours is not — the keys are written in the
/// order given here rather than a map's; none of this app's tools has more
/// than one additional key, so the difference is never observable.
fn additional_keys(obj: &Value, allowed: &[&str]) -> String {
    let mut out = String::new();
    let Some(map) = obj.as_object() else { return out };
    for key in allowed {
        let Some(value) = map.get(*key) else { continue };
        match value {
            Value::Null => continue,
            Value::Array(a) if a.is_empty() => continue,
            Value::Object(o) if o.is_empty() => continue,
            Value::Array(_) | Value::Object(_) => out.push_str(&format!("\n<{key}>{}</{key}>", go_json(value))),
            Value::String(s) => out.push_str(&format!("\n<{key}>{s}</{key}>")),
            Value::Bool(b) => out.push_str(&format!("\n<{key}>{b}</{key}>")),
            Value::Number(n) => out.push_str(&format!("\n<{key}>{n}</{key}>")),
        }
    }
    out
}

/// The tool-call arguments, in order, parsing them out of a JSON string first
/// if that is how they arrived — the same tolerance `toolcall::from_value`
/// has. Falls back to no parameters rather than failing the whole render.
fn call_args(v: &Value) -> Vec<(String, Value)> {
    let obj = match v {
        Value::String(s) => serde_json::from_str::<Value>(s).ok().and_then(|p| p.as_object().cloned()),
        Value::Object(o) => Some(o.clone()),
        _ => None,
    };
    obj.map(|o| o.into_iter().collect()).unwrap_or_default()
}

/// An exact port of Ollama's `Qwen3CoderRenderer.Render` (no thinking). See the
/// module doc for why this exists at all, and `renderers-qwen3coder.go` for
/// the source this mirrors line for line.
pub fn render(messages: &[Message], tools: &[Tool]) -> String {
    let mut out = String::new();

    // Filter out system messages; the first non-empty one wins.
    let mut system_message = String::new();
    let mut filtered: Vec<&Message> = Vec::new();
    for m in messages {
        if m.role != "system" {
            filtered.push(m);
            continue;
        }
        if system_message.is_empty() {
            system_message = m.content.clone();
        }
    }

    if !system_message.is_empty() || !tools.is_empty() {
        out.push_str(IM_START);
        out.push_str("system\n");
        if system_message.is_empty() {
            system_message = DEFAULT_SYSTEM.to_string();
        }
        out.push_str(&system_message);

        if !tools.is_empty() {
            out.push_str("\n\n# Tools\n\nYou have access to the following functions:\n\n<tools>");
            for tool in tools {
                out.push('\n');
                out.push_str("<function>\n");
                out.push_str(&format!("<name>{}</name>", tool.function.name));
                if !tool.function.description.is_empty() {
                    out.push_str(&format!("\n<description>{}</description>", tool.function.description));
                }
                out.push_str("\n<parameters>");
                if let Some(props) = tool.function.parameters.get("properties").and_then(Value::as_object) {
                    for (name, prop) in props {
                        out.push_str("\n<parameter>");
                        out.push_str(&format!("\n<name>{name}</name>"));
                        if let Some(t) = prop.get("type") {
                            out.push_str(&format!("\n<type>{}</type>", format_type(t)));
                        }
                        if let Some(d) = prop.get("description").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                            out.push_str(&format!("\n<description>{d}</description>"));
                        }
                        out.push_str(&additional_keys(prop, &["anyOf", "items", "enum", "properties", "required"]));
                        out.push_str("\n</parameter>");
                    }
                }
                out.push_str(&additional_keys(&tool.function.parameters, &["$defs", "items", "required"]));
                out.push_str("\n</parameters>");
                out.push_str("\n</function>");
            }
            out.push_str("\n</tools>");
            out.push_str(
                "\n\nIf you choose to call a function ONLY reply in the following format with NO suffix:\n\n\
                 <tool_call>\n<function=example_function_name>\n<parameter=example_parameter_1>\nvalue_1\n\
                 </parameter>\n<parameter=example_parameter_2>\nThis is the value for the second parameter\n\
                 that can span\nmultiple lines\n</parameter>\n</function>\n</tool_call>\n\n<IMPORTANT>\n\
                 Reminder:\n\
                 - Function calls MUST follow the specified format: an inner <function=...></function> block \
                 must be nested within <tool_call></tool_call> XML tags\n\
                 - Required parameters MUST be specified\n\
                 - You may provide optional reasoning for your function call in natural language BEFORE the \
                 function call, but NOT after\n\
                 - If there is no function call available, answer the question like normal with your current \
                 knowledge and do not tell the user about function calls\n\
                 </IMPORTANT>",
            );
        }
        out.push_str(IM_END);
        out.push('\n');
    }

    let n = filtered.len();
    for (i, m) in filtered.iter().enumerate() {
        let last = i == n - 1;
        let prefill = last && m.role == "assistant";
        match m.role.as_str() {
            "assistant" => {
                let calls = m.tool_calls.as_deref().filter(|c| !c.is_empty());
                if let Some(calls) = calls {
                    out.push_str(IM_START);
                    out.push_str("assistant\n");
                    if !m.content.is_empty() {
                        out.push_str(&m.content);
                        out.push('\n');
                    }
                    for call in calls {
                        out.push_str(&format!("\n<tool_call>\n<function={}>", call.function.name));
                        for (name, value) in call_args(&call.function.arguments) {
                            out.push_str(&format!("\n<parameter={name}>\n{}\n</parameter>", format_arg(&value)));
                        }
                        out.push_str("\n</function>\n</tool_call>");
                    }
                    out.push_str(IM_END);
                    out.push('\n');
                } else {
                    out.push_str(IM_START);
                    out.push_str("assistant\n");
                    out.push_str(&m.content);
                    if !prefill {
                        out.push_str(IM_END);
                        out.push('\n');
                    }
                }
            }
            "tool" => {
                if i == 0 || filtered[i - 1].role != "tool" {
                    out.push_str(IM_START);
                    out.push_str("user");
                }
                out.push_str("\n<tool_response>\n");
                out.push_str(&m.content);
                out.push_str("\n</tool_response>");
                if i == n - 1 || filtered[i + 1].role != "tool" {
                    out.push_str(IM_END);
                    out.push('\n');
                }
            }
            role => {
                out.push_str(&format!("{IM_START}{role}\n"));
                out.push_str(&m.content);
                out.push_str(IM_END);
                out.push('\n');
            }
        }
        if last && !prefill {
            out.push_str(IM_START);
            out.push_str("assistant\n");
        }
    }

    out
}

fn skip_ws(s: &str, mut i: usize) -> usize {
    let b = s.as_bytes();
    while i < s.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

struct FormA {
    tc_at: usize,
    tag_end: usize,
    name: String,
}

/// The next place `<tool_call>` is followed (ignoring whitespace) by
/// `<function=NAME>`, from `from`. `NAME` may or may not be one we offered —
/// that distinction is made by the caller.
fn find_form_a(s: &str, from: usize) -> Option<FormA> {
    let mut search_from = from;
    loop {
        let rel = s[search_from..].find("<tool_call>")?;
        let tc_at = search_from + rel;
        let after_tc = tc_at + "<tool_call>".len();
        let ws_end = skip_ws(s, after_tc);
        if let Some(rest) = s[ws_end..].strip_prefix("<function=")
            && let Some(gt) = rest.find('>')
        {
            let name_start = ws_end + "<function=".len();
            let name_end = name_start + gt;
            return Some(FormA { tc_at, tag_end: name_end + 1, name: s[name_start..name_end].to_string() });
        }
        search_from = after_tc;
    }
}

struct FormB {
    at: usize,
    tag_end: usize,
    name: String,
}

/// The next `<function=NAME>` that sits at the start of a line — only spaces
/// or tabs precede it on that line, or it is the start of the text — with
/// `NAME` one we offered. Used only for the first call, and only when no
/// `<tool_call>` tag has been found anywhere.
fn find_form_b(s: &str, from: usize, names: &HashSet<&str>) -> Option<FormB> {
    let mut search_from = from;
    while let Some(rel) = s[search_from..].find("<function=") {
        let at = search_from + rel;
        let line_start = s[..at].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let prefix = &s[line_start..at];
        if prefix.bytes().all(|b| b == b' ' || b == b'\t')
            && let Some(gt) = s[at + "<function=".len()..].find('>')
        {
            let name_start = at + "<function=".len();
            let name_end = name_start + gt;
            let name = &s[name_start..name_end];
            if names.contains(name) {
                return Some(FormB { at, tag_end: name_end + 1, name: name.to_string() });
            }
        }
        search_from = at + "<function=".len();
    }
    None
}

/// Whether `name` is shaped like a parameter name: letters, digits, `_` and `-`.
fn is_identifier(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

struct ValueEnd {
    value_end: usize,
    next_pos: usize,
}

/// Where a parameter value ends — robust to a value containing
/// `</tool_call>`, `</parameter>` or `</function>`, which is the whole point:
/// Ollama's own parser is not. See the module doc for the anchoring idea, and
/// the brief's A/B rule this ports.
fn find_value_end(region: &str, value_start: usize, key: &str, arg_names: &[&str], taken: &HashSet<&str>) -> Option<ValueEnd> {
    const CLOSE: &str = "</parameter>";
    let mut occurrences = Vec::new();
    let mut search = value_start;
    while let Some(rel) = region[search..].find(CLOSE) {
        let at = search + rel;
        occurrences.push(at);
        search = at + CLOSE.len();
    }
    if occurrences.is_empty() {
        return None;
    }

    // A value written the way the format asks starts on its own line, and its
    // closing tag then sits on a line of its own too. A value written inline
    // (`<parameter=path>src/a.rs</parameter>`) has no such shape to check.
    let own_lines = region[value_start..].starts_with('\n');

    let mut a = None;
    for &at in &occurrences {
        let ws_end = skip_ws(region, at + CLOSE.len());
        if let Some(rest) = region[ws_end..].strip_prefix("<parameter=")
            && let Some(gt) = rest.find('>')
        {
            let k2 = &rest[..gt];
            // A tool's own other parameter that is still to come ends the value
            // wherever the closing tag sits: that is the anchor this module is
            // built on.
            let expected = k2 != key && arg_names.contains(&k2) && !taken.contains(k2);
            // Any other identifier-shaped name (one the tool lacks, one already
            // given, this very one again) ends it only when it is a real
            // parameter, that is, it has a `</parameter>` of its own further on,
            // and the closing tag looks like a close rather than code that quotes
            // one: on a line of its own when the value began on its own line.
            // Without that last test a value such as `x </parameter>` followed by
            // `<parameter=zzz>` in a code sample could never come back exact.
            let tag_at = ws_end + "<parameter=".len() + gt + 1;
            let real = is_identifier(k2)
                && region[tag_at..].contains(CLOSE)
                && (!own_lines || (region[..at].ends_with('\n') && at > value_start + 1));
            if expected || real {
                a = Some(at);
                break;
            }
        }
    }

    let mut b = None;
    for &at in occurrences.iter().rev() {
        let ws_end = skip_ws(region, at + CLOSE.len());
        if ws_end >= region.len() || region[ws_end..].starts_with("</function>") {
            b = Some(at);
            break;
        }
    }

    let chosen = match (a, b) {
        (Some(a), Some(b)) => {
            if a < b {
                a
            } else {
                b
            }
        }
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => return None,
    };
    Some(ValueEnd { value_end: chosen, next_pos: chosen + CLOSE.len() })
}

/// `parseValue`: one leading and one trailing newline removed, if present.
fn trim_value(raw: &str) -> String {
    let raw = raw.strip_prefix('\n').unwrap_or(raw);
    let raw = raw.strip_suffix('\n').unwrap_or(raw);
    raw.to_string()
}

/// `parseTypedToolValue`, ported with its single-type fallbacks intact: null
/// is checked first, and when a value fails to parse as the one type it was
/// given, it falls back to the raw string rather than being lost.
///
/// **Except that a parameter declared `string` keeps the word `null` as text.**
/// Ollama's parser turns it into JSON null, and the tool layer then could not tell
/// "replace `None` with `null`" from "delete `None`".
fn typed_value(raw: &str, prop_type: Option<&Value>) -> Value {
    let raw = trim_value(raw);

    let types: Vec<String> = match prop_type {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
        _ => vec![],
    };
    if raw.to_lowercase() == "null" && types != ["string"] {
        return Value::Null;
    }
    if types.is_empty() {
        return Value::String(raw);
    }
    let has = |t: &str| types.iter().any(|x| x == t);
    let single = types.len() == 1;

    if has("boolean") {
        match raw.to_lowercase().as_str() {
            "true" => return Value::Bool(true),
            "false" => return Value::Bool(false),
            _ => {}
        }
        if single {
            return Value::Bool(false);
        }
    }
    if has("integer") {
        if let Ok(i) = raw.parse::<i64>() {
            return Value::Number(i.into());
        }
        if single {
            return Value::String(raw);
        }
    }
    if has("number") {
        if let Ok(f) = raw.parse::<f64>() {
            if f == f.trunc() {
                return Value::Number((f as i64).into());
            }
            if let Some(n) = serde_json::Number::from_f64(f) {
                return Value::Number(n);
            }
        }
        if single {
            return Value::String(raw);
        }
    }
    if has("array") {
        if let Ok(v) = serde_json::from_str::<Value>(&raw)
            && v.is_array()
        {
            return v;
        }
        if single {
            return Value::String(raw);
        }
    }
    if has("object") {
        if let Ok(v) = serde_json::from_str::<Value>(&raw)
            && v.is_object()
        {
            return v;
        }
        if single {
            return Value::String(raw);
        }
    }
    if has("string") {
        return Value::String(raw);
    }
    Value::String(raw)
}

/// One call's parameters, read from the region between its `<function=NAME>`
/// tag and the next call start (or the end of the text). Returns the
/// arguments and how far into the region the call's own closing tags reached.
fn parse_params(region: &str, tool_name: &str, tools: &[Tool]) -> Result<(Map<String, Value>, usize), String> {
    let schema = tools.iter().find(|t| t.function.name == tool_name);
    let props = schema.and_then(|t| t.function.parameters.get("properties")).and_then(Value::as_object);
    let arg_names: Vec<&str> = props.map(|m| m.keys().map(String::as_str).collect()).unwrap_or_default();

    let fail = |why: &str| {
        let preview: String = region.chars().take(160).collect();
        format!("the tool call could not be read ({why}): {preview}")
    };

    let mut args = Map::new();
    let mut i = 0usize;
    loop {
        i = skip_ws(region, i);
        if i >= region.len() || region[i..].starts_with("</function>") {
            break;
        }
        if let Some(rest) = region[i..].strip_prefix("<parameter=") {
            let Some(gt) = rest.find('>') else {
                return Err(fail("a <parameter=...> tag is not closed"));
            };
            let key_start = i + "<parameter=".len();
            let key_end = key_start + gt;
            let key = region[key_start..key_end].to_string();
            let value_start = key_end + 1;

            let taken: HashSet<&str> = args.keys().map(String::as_str).collect();
            let end = find_value_end(region, value_start, &key, &arg_names, &taken)
                .ok_or_else(|| fail(&format!("{key} is not closed with </parameter>")))?;

            let raw_value = &region[value_start..end.value_end];
            // A parameter the tool does not have (the model does send them:
            // `range` on `read_file`) is kept as a string, as Ollama's own parser
            // does; the tools read only the names they know, so it does no harm,
            // and what matters is that it no longer bleeds into its neighbour's
            // value. A repeated one keeps its last value, as a map insert does.
            let prop_type = props.and_then(|m| m.get(&key)).and_then(|p| p.get("type"));
            args.insert(key, typed_value(raw_value, prop_type));
            i = end.next_pos;
        } else {
            return Err(fail("unexpected text in the call"));
        }
    }

    if region[i..].starts_with("</function>") {
        i += "</function>".len();
    }
    i = skip_ws(region, i);
    if region[i..].starts_with("</tool_call>") {
        i += "</tool_call>".len();
    }
    Ok((args, i))
}

/// Find the tool calls Qwen's own layout wrote. Same contract as
/// [`crate::toolcall::extract_calls`]: the remaining text (trimmed), the calls
/// that parsed, and a description of every block that could not be read.
///
/// See the module doc for the two anchoring rules this uses in place of
/// Ollama's own first-match parsing, and for the ambiguity that is still left.
pub fn extract_calls(content: &str, tools: &[Tool]) -> (String, Vec<ToolCall>, Vec<String>) {
    let names: HashSet<&str> = tools.iter().map(|t| t.function.name.as_str()).collect();

    let mut text = String::new();
    let mut calls = Vec::new();
    let mut failures = Vec::new();
    let mut pos = 0usize;
    let mut seen_call_start = false;
    // Whether the call just read had no `<tool_call>` wrapper. A reply of
    // several bare calls is read call by call; once a wrapped call has been
    // seen, a bare `<function=...>` line afterwards is prose again.
    let mut last_bare = false;

    loop {
        let form_a = find_form_a(content, pos);
        let form_b = if !seen_call_start || last_bare { find_form_b(content, pos, &names) } else { None };

        let (start, tag_end, name, bare) = match (form_a, form_b) {
            (Some(a), Some(b)) if a.tc_at <= b.at => (a.tc_at, a.tag_end, a.name, false),
            (Some(a), None) => (a.tc_at, a.tag_end, a.name, false),
            (_, Some(b)) => (b.at, b.tag_end, b.name, true),
            (None, None) => {
                text.push_str(&content[pos..]);
                break;
            }
        };

        text.push_str(&content[pos..start]);
        seen_call_start = true;
        last_bare = bare;

        if !names.contains(name.as_str()) {
            // Worded like every other failure here and in `toolcall`, since it
            // goes back to the model the same way.
            let preview: String = content[start..].chars().take(160).collect();
            failures.push(format!("the tool call could not be read (no such tool: {name}): {preview}"));
            pos = match content[tag_end..].find("</tool_call>") {
                Some(rel) => tag_end + rel + "</tool_call>".len(),
                None => content.len(),
            };
            continue;
        }

        // A wrapped call ends where the next `<tool_call>` starts. A bare one has
        // no wrapper to look for, so it also ends at the next `<function=...>`
        // of an offered tool at the start of a line; a wrapped call does not
        // stop there, since its values may quote such a line as code.
        let mut region_end = find_form_a(content, tag_end).map(|a| a.tc_at).unwrap_or(content.len());
        if bare && let Some(b) = find_form_b(content, tag_end, &names) {
            region_end = region_end.min(b.at);
        }
        let region = &content[tag_end..region_end];

        match parse_params(region, &name, tools) {
            Ok((args, consumed)) => {
                calls.push(ToolCall { id: None, function: FunctionCall { name, arguments: Value::Object(args) } });
                pos = tag_end + consumed;
            }
            Err(e) => {
                failures.push(e);
                pos = region_end;
            }
        }
    }

    let cleaned = text.replace("<tool_call>", "").replace("</tool_call>", "");
    (cleaned.trim().to_string(), calls, failures)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn small_tools() -> Vec<Tool> {
        vec![
            Tool::function(
                "read_file",
                "Read a file.",
                json!({ "type": "object", "properties": { "path": { "type": "string", "description": "Path" } }, "required": ["path"] }),
            ),
            Tool::function(
                "edit_file",
                "Edit a file.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path" },
                        "old_text": { "type": "string", "description": "Old" },
                        "new_text": { "type": "string", "description": "New" }
                    },
                    "required": ["path", "old_text", "new_text"]
                }),
            ),
        ]
    }

    fn add_tools() -> Vec<Tool> {
        vec![Tool::function(
            "add",
            "Add two numbers.",
            json!({
                "type": "object",
                "properties": {
                    "a": { "type": "number", "description": "first" },
                    "b": { "type": "number", "description": "second" }
                },
                "required": ["a", "b"]
            }),
        )]
    }

    fn edit_call(path: &str, old: &str, new: &str) -> Message {
        Message {
            role: "assistant".into(),
            content: "Let me look.".into(),
            images: None,
            tool_calls: Some(vec![ToolCall {
                id: None,
                function: FunctionCall {
                    name: "edit_file".into(),
                    arguments: json!({ "new_text": new, "old_text": old, "path": path }),
                },
            }]),
            tool_name: None,
        }
    }

    /// A small tool list with no `required`, so the exact-string render test
    /// below is not also exercising `additional_keys` for the parameters
    /// object — that is covered separately by the `<required>` test.
    fn render_test_tools() -> Vec<Tool> {
        vec![
            Tool::function(
                "read_file",
                "Read a file.",
                json!({ "type": "object", "properties": { "path": { "type": "string", "description": "Path to read" } } }),
            ),
            Tool::function(
                "edit_file",
                "Edit a file.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path" },
                        "old_text": { "type": "string", "description": "Old" },
                        "new_text": { "type": "string", "description": "New" }
                    }
                }),
            ),
        ]
    }

    #[test]
    fn the_conversation_is_rendered_the_way_ollamas_renderer_does() {
        let history = vec![
            Message::system("be careful"),
            Message::user("edit it"),
            edit_call("a.rs", "x", "y"),
            Message::tool_result("edit_file", "Edited a.rs"),
            Message::tool_result("edit_file", "ok"),
            Message::user("thanks"),
        ];
        let prompt = render(&history, &render_test_tools());
        let expected = "<|im_start|>system\nbe careful\n\n\
             # Tools\n\nYou have access to the following functions:\n\n<tools>\n\
             <function>\n<name>read_file</name>\n<description>Read a file.</description>\n\
             <parameters>\n<parameter>\n<name>path</name>\n<type>string</type>\n<description>Path to read</description>\n</parameter>\n\
             </parameters>\n</function>\n\
             <function>\n<name>edit_file</name>\n<description>Edit a file.</description>\n\
             <parameters>\n\
             <parameter>\n<name>new_text</name>\n<type>string</type>\n<description>New</description>\n</parameter>\n\
             <parameter>\n<name>old_text</name>\n<type>string</type>\n<description>Old</description>\n</parameter>\n\
             <parameter>\n<name>path</name>\n<type>string</type>\n<description>Path</description>\n</parameter>\n\
             </parameters>\n</function>\n\
             </tools>\n\n\
             If you choose to call a function ONLY reply in the following format with NO suffix:\n\n\
             <tool_call>\n<function=example_function_name>\n<parameter=example_parameter_1>\nvalue_1\n</parameter>\n\
             <parameter=example_parameter_2>\nThis is the value for the second parameter\nthat can span\n\
             multiple lines\n</parameter>\n</function>\n</tool_call>\n\n<IMPORTANT>\nReminder:\n\
             - Function calls MUST follow the specified format: an inner <function=...></function> block must \
             be nested within <tool_call></tool_call> XML tags\n\
             - Required parameters MUST be specified\n\
             - You may provide optional reasoning for your function call in natural language BEFORE the function \
             call, but NOT after\n\
             - If there is no function call available, answer the question like normal with your current \
             knowledge and do not tell the user about function calls\n\
             </IMPORTANT><|im_end|>\n\
             <|im_start|>user\nedit it<|im_end|>\n\
             <|im_start|>assistant\nLet me look.\n\
             \n<tool_call>\n<function=edit_file>\n\
             <parameter=new_text>\ny\n</parameter>\n\
             <parameter=old_text>\nx\n</parameter>\n\
             <parameter=path>\na.rs\n</parameter>\n\
             </function>\n</tool_call><|im_end|>\n\
             <|im_start|>user\n<tool_response>\nEdited a.rs\n</tool_response>\n\
             <tool_response>\nok\n</tool_response><|im_end|>\n\
             <|im_start|>user\nthanks<|im_end|>\n\
             <|im_start|>assistant\n";
        assert_eq!(prompt, expected);
    }

    #[test]
    fn the_default_system_text_is_used_when_there_is_none() {
        let prompt = render(&[Message::user("hi")], &small_tools());
        assert!(prompt.starts_with("<|im_start|>system\nYou are Qwen, a helpful AI assistant"));
    }

    #[test]
    fn no_system_block_at_all_without_tools_or_a_system_message() {
        let prompt = render(&[Message::user("hi")], &[]);
        assert!(!prompt.contains("<|im_start|>system"));
        assert_eq!(prompt, "<|im_start|>user\nhi<|im_end|>\n<|im_start|>assistant\n");
    }

    #[test]
    fn only_the_first_system_message_wins() {
        let prompt = render(&[Message::system("first"), Message::system("second"), Message::user("hi")], &[]);
        assert!(prompt.contains("first"));
        assert!(!prompt.contains("second"));
    }

    #[test]
    fn a_trailing_assistant_turn_is_a_prefill_with_no_opener_or_closer() {
        let partial = Message {
            role: "assistant".into(),
            content: "partial".into(),
            images: None,
            tool_calls: None,
            tool_name: None,
        };
        let prompt = render(&[Message::user("hi"), partial], &[]);
        assert!(prompt.ends_with("<|im_start|>assistant\npartial"));
        assert!(!prompt.contains("partial<|im_end|>"));
    }

    #[test]
    fn the_compact_json_helper_escapes_the_way_go_does() {
        assert_eq!(go_json(&json!(["a<b", "c>d", "e&f"])), r#"["a\u003cb","c\u003ed","e\u0026f"]"#);
    }

    fn call_args_tool() -> Vec<Tool> {
        vec![Tool::function(
            "x",
            "x",
            json!({ "type": "object", "properties": {}, "required": ["<weird>"] }),
        )]
    }

    #[test]
    fn a_required_list_with_a_lt_is_go_escaped_in_the_rendered_tool() {
        let prompt = render(&[Message::user("hi")], &call_args_tool());
        assert!(prompt.contains(r#"<required>["\u003cweird\u003e"]</required>"#));
    }

    #[test]
    fn a_plain_call_with_prose_before_it_is_extracted() {
        let content = "Sure, here goes.\n<tool_call>\n<function=read_file>\n<parameter=path>\na.rs\n</parameter>\n</function>\n</tool_call>";
        let (text, calls, failures) = extract_calls(content, &small_tools());
        assert_eq!(text, "Sure, here goes.");
        assert_eq!(calls.len(), 1);
        assert!(failures.is_empty());
        assert_eq!(calls[0].function.name, "read_file");
        assert_eq!(calls[0].function.arguments["path"], "a.rs");
    }

    #[test]
    fn tags_mentioned_in_prose_before_the_real_call_are_not_a_call() {
        // The shape behind Ollama's bare `{"error":"EOF"}` (H4a, b05): it takes
        // the text between the two mentioned tags as a call and finds nothing.
        let content = "Each call goes between `<tool_call>` and `</tool_call>` tags.\n<tool_call>\n<function=read_file>\n<parameter=path>\na.rs\n</parameter>\n</function>\n</tool_call>";
        let (text, calls, failures) = extract_calls(content, &small_tools());
        assert!(failures.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.arguments["path"], "a.rs");
        assert!(text.starts_with("Each call goes between"));
    }

    #[test]
    fn the_toolprobe_case_4_shape_without_tool_call_is_still_read() {
        // Reconstructed from the probe's 60-char preview; only the part up to
        // `</pa` was actually captured.
        let content = "<function=read_file>\n<parameter=path>\nC:\\temp\\notes.txt\n</parameter>\n</function>\n</tool_call>";
        let (text, calls, failures) = extract_calls(content, &small_tools());
        assert!(failures.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.arguments["path"], r"C:\temp\notes.txt");
        assert_eq!(text, "");
    }

    #[test]
    fn a_new_text_containing_a_fake_tool_call_block_is_extracted_exactly() {
        let new_text = "\"<tool_call>\\n{\\\"name\\\": ...}\\n</tool_call>\"";
        let content = format!(
            "<tool_call>\n<function=edit_file>\n<parameter=path>\na.rs\n</parameter>\n\
             <parameter=old_text>\nold\n</parameter>\n<parameter=new_text>\n{new_text}\n</parameter>\n\
             </function>\n</tool_call>"
        );
        let (_, calls, failures) = extract_calls(&content, &small_tools());
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.arguments["new_text"], new_text);
    }

    #[test]
    fn a_value_containing_parameter_and_function_close_tags_is_still_found_by_rule_b() {
        let content = "<tool_call>\n<function=edit_file>\n<parameter=path>\na.rs\n</parameter>\n\
             <parameter=old_text>\nx\n</parameter>\n\
             <parameter=new_text>\nsome </parameter>\n</function> text\n</parameter>\n</function>\n</tool_call>";
        let (_, calls, failures) = extract_calls(content, &small_tools());
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.arguments["new_text"], "some </parameter>\n</function> text");
    }

    #[test]
    fn prose_after_the_call_is_kept() {
        let content = "<tool_call>\n<function=read_file>\n<parameter=path>\na.rs\n</parameter>\n</function>\n</tool_call>\nDone.";
        let (text, calls, _) = extract_calls(content, &small_tools());
        assert_eq!(text, "Done.");
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn two_calls_are_both_found() {
        let content = "<tool_call>\n<function=read_file>\n<parameter=path>\na.rs\n</parameter>\n</function>\n</tool_call>\n\
            <tool_call>\n<function=read_file>\n<parameter=path>\nb.rs\n</parameter>\n</function>\n</tool_call>";
        let (_, calls, _) = extract_calls(content, &small_tools());
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].function.arguments["path"], "a.rs");
        assert_eq!(calls[1].function.arguments["path"], "b.rs");
    }

    #[test]
    fn an_unknown_tool_inside_tool_call_is_a_failure() {
        let content = "<tool_call>\n<function=delete_everything>\n<parameter=path>\n/\n</parameter>\n</function>\n</tool_call>";
        let (_, calls, failures) = extract_calls(content, &small_tools());
        assert!(calls.is_empty());
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("no such tool: delete_everything"));
    }

    #[test]
    fn an_unknown_tool_at_a_line_start_stays_text() {
        let content = "<function=delete_everything>\nsome prose";
        let (text, calls, failures) = extract_calls(content, &small_tools());
        assert!(calls.is_empty());
        assert!(failures.is_empty());
        assert_eq!(text, content);
    }

    #[test]
    fn a_function_tag_mid_line_in_prose_stays_text_with_no_calls() {
        let content = "I would use <function=read_file> here if I could.";
        let (text, calls, failures) = extract_calls(content, &small_tools());
        assert!(calls.is_empty());
        assert!(failures.is_empty());
        assert_eq!(text, content);
    }

    #[test]
    fn an_unclosed_parameter_is_a_failure() {
        let content = "<tool_call>\n<function=read_file>\n<parameter=path>\na.rs\n</function>\n</tool_call>";
        let (_, calls, failures) = extract_calls(content, &small_tools());
        assert!(calls.is_empty());
        assert_eq!(failures.len(), 1);
    }

    #[test]
    fn a_call_cut_off_after_its_last_parameter_is_still_accepted() {
        let content = "<tool_call>\n<function=read_file>\n<parameter=path>\na.rs\n</parameter>";
        let (_, calls, failures) = extract_calls(content, &small_tools());
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.arguments["path"], "a.rs");
    }

    #[test]
    fn only_one_leading_and_trailing_newline_is_stripped() {
        let content = "<tool_call>\n<function=read_file>\n<parameter=path>\n\nx\n\n</parameter>\n</function>\n</tool_call>";
        let (_, calls, _) = extract_calls(content, &small_tools());
        assert_eq!(calls[0].function.arguments["path"], "\nx\n");
    }

    #[test]
    fn typed_values_are_coerced_by_the_tools_schema() {
        let content = "<tool_call>\n<function=add>\n<parameter=a>\n847\n</parameter>\n<parameter=b>\n1.5\n</parameter>\n</function>\n</tool_call>";
        let (_, calls, _) = extract_calls(content, &add_tools());
        assert_eq!(calls[0].function.arguments["a"], json!(847));
        assert_eq!(calls[0].function.arguments["b"], json!(1.5));
    }

    #[test]
    fn a_string_parameter_holding_the_word_null_stays_text() {
        let content = "<tool_call>
<function=read_file>
<parameter=path>
null
</parameter>
</function>
</tool_call>";
        let (_, calls, _) = extract_calls(content, &small_tools());
        assert_eq!(calls[0].function.arguments["path"], "null");
    }

    #[test]
    fn a_null_typed_value_becomes_json_null() {
        let content = "<tool_call>\n<function=add>\n<parameter=a>\nnull\n</parameter>\n<parameter=b>\n1\n</parameter>\n</function>\n</tool_call>";
        let (_, calls, _) = extract_calls(content, &add_tools());
        assert!(calls[0].function.arguments["a"].is_null());
    }

    #[test]
    fn a_string_typed_param_keeps_a_numeric_looking_value_as_a_string() {
        let content = "<tool_call>\n<function=read_file>\n<parameter=path>\n123\n</parameter>\n</function>\n</tool_call>";
        let (_, calls, _) = extract_calls(content, &small_tools());
        assert_eq!(calls[0].function.arguments["path"], json!("123"));
    }

    #[test]
    fn a_line_number_parameter_becomes_a_number_by_the_real_read_schema() {
        let content = "<tool_call>\n<function=read_file>\n<parameter=path>\nsrc/a.rs\n</parameter>\n<parameter=start_line>\n120\n</parameter>\n<parameter=end_line>\n180\n</parameter>\n</function>\n</tool_call>";
        let (_, calls, failures) = extract_calls(content, &crate::tools::available(false, None));
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(calls[0].function.arguments["path"], json!("src/a.rs"));
        assert_eq!(calls[0].function.arguments["start_line"], json!(120));
        assert_eq!(calls[0].function.arguments["end_line"], json!(180));
        assert!(calls[0].function.arguments["start_line"].is_number());
    }

    #[test]
    fn an_unknown_parameter_after_a_known_one_does_not_join_its_value() {
        let content = "<tool_call>\n<function=read_file>\n<parameter=path>\nsrc/a.rs\n</parameter>\n<parameter=range>\n1-50\n</parameter>\n</function>\n</tool_call>";
        let (_, calls, failures) = extract_calls(content, &small_tools());
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(calls[0].function.arguments["path"], json!("src/a.rs"));
        assert_eq!(calls[0].function.arguments["range"], json!("1-50"));
    }

    #[test]
    fn an_inline_value_followed_by_an_unknown_parameter_ends_at_its_own_close() {
        let content = "<tool_call>\n<function=read_file>\n<parameter=path>src/a.rs</parameter>\n<parameter=range>1-50</parameter>\n</function>\n</tool_call>";
        let (_, calls, _) = extract_calls(content, &small_tools());
        assert_eq!(calls[0].function.arguments["path"], json!("src/a.rs"));
    }

    #[test]
    fn a_trailing_unknown_parameter_stays_out_of_new_text() {
        let content = "<tool_call>\n<function=edit_file>\n<parameter=path>\na.rs\n</parameter>\n<parameter=old_text>\nx\n</parameter>\n<parameter=new_text>\ny\n</parameter>\n<parameter=replace_all>\ntrue\n</parameter>\n</function>\n</tool_call>";
        let (_, calls, _) = extract_calls(content, &small_tools());
        assert_eq!(calls[0].function.arguments["new_text"], json!("y"));
    }

    #[test]
    fn a_repeated_parameter_keeps_its_last_value() {
        let content = "<tool_call>\n<function=read_file>\n<parameter=path>\na.rs\n</parameter>\n<parameter=path>\nb.rs\n</parameter>\n</function>\n</tool_call>";
        let (_, calls, _) = extract_calls(content, &small_tools());
        assert_eq!(calls[0].function.arguments["path"], json!("b.rs"));
    }

    #[test]
    fn an_unknown_parameter_tag_quoted_in_a_value_mid_line_stays_in_the_value() {
        // The closing tag sits mid-line, so this is code quoting the format.
        let v = "x </parameter>\n<parameter=zzz>\ny";
        let content = format!("<tool_call>\n<function=edit_file>\n<parameter=path>\na.rs\n</parameter>\n<parameter=old_text>\no\n</parameter>\n<parameter=new_text>\n{v}\n</parameter>\n</function>\n</tool_call>");
        let (_, calls, failures) = extract_calls(&content, &small_tools());
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(calls[0].function.arguments["new_text"], json!(v));
    }

    #[test]
    fn calls_without_wrappers_are_read_one_by_one() {
        let content = "<function=read_file>\n<parameter=path>\na.rs\n</parameter>\n</function>\n<function=read_file>\n<parameter=path>\nb.rs\n</parameter>\n</function>";
        let (text, calls, failures) = extract_calls(content, &small_tools());
        assert!(failures.is_empty(), "{failures:?}");
        let paths: Vec<_> = calls.iter().map(|c| c.function.arguments["path"].clone()).collect();
        assert_eq!(paths, vec![json!("a.rs"), json!("b.rs")]);
        assert_eq!(text, "");
    }

    #[test]
    fn a_wrapped_value_quoting_a_function_line_is_not_split() {
        let v = "<function=read_file>\n<parameter=path>\nfoo\n</parameter>\n</function>";
        let content = format!("<tool_call>\n<function=edit_file>\n<parameter=path>\na.rs\n</parameter>\n<parameter=old_text>\no\n</parameter>\n<parameter=new_text>\n{v}\n</parameter>\n</function>\n</tool_call>");
        let (_, calls, failures) = extract_calls(&content, &small_tools());
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.arguments["new_text"], json!(v));
    }
}
