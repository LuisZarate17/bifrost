//! Independent review of what goes to, and comes back from, the model: H4a (`xmlcall`),
//! H7 (`compact`), H8 (ranged reads), H10 (`update_plan`, project notes).
//!
//! Integration tests against odin-core's public API only. **A test that FAILS here is a defect
//! the review found**; its message says what was observed. A test that passes documents a
//! guarantee that holds. Nothing here calls Ollama.

use std::fs;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use odin_core::chat::{FunctionCall, Message, Tool, ToolCall};
use odin_core::edit::Workspace;
use odin_core::tools::{self, Lines, PlanStatus, Request};
use odin_core::{compact, instructions, toolcall, xmlcall};
use serde_json::{json, Value};

// ---------------------------- helpers ----------------------------

fn call(name: &str, args: Value) -> ToolCall {
    ToolCall { id: None, function: FunctionCall { name: name.to_string(), arguments: args } }
}

/// The assistant turn the app sends back: only the call that ran, no words.
fn asked(c: &ToolCall) -> Message {
    Message { role: "assistant".into(), content: String::new(), images: None, tool_calls: Some(vec![c.clone()]), tool_name: None }
}

fn app_tools() -> Vec<Tool> {
    tools::available(true, Some("cargo check"))
}

/// One Qwen3-Coder tool call, as the model writes it.
fn xml_call(name: &str, params: &[(&str, &str)]) -> String {
    let mut s = format!("<tool_call>\n<function={name}>");
    for (k, v) in params {
        s.push_str(&format!("\n<parameter={k}>\n{v}\n</parameter>"));
    }
    s.push_str("\n</function>\n</tool_call>");
    s
}

fn extract(content: &str) -> (String, Vec<ToolCall>, Vec<String>) {
    xmlcall::extract_calls(content, &app_tools())
}

fn arg_str(c: &ToolCall, key: &str) -> Option<String> {
    c.function.arguments.get(key).and_then(Value::as_str).map(str::to_string)
}

// ---------------------- xmlcall: the parser ----------------------

/// A value that holds `</tool_call>`, `</parameter>`, `</function>` or `<parameter=` text
/// (the model editing XML, or this very parser) must come back exactly, whichever parameter
/// holds it and in whichever order the model wrote the parameters.
#[test]
fn xml_values_holding_closing_tags_come_back_exactly_in_every_position() {
    let values = [
        "println!(\"</tool_call>\");",
        "</parameter>",
        "x </parameter>\ny",
        "a\n</function>\nb",
        "x </parameter>\n<parameter=zzz>\ny",
        "<function=read_file>\n<parameter=path>\nfoo\n</parameter>\n</function>\n</tool_call>",
        "if a < b && c > d { /* <parameter=old_text> */ }",
        "</parameter>\n</function>\n</tool_call>\n",
        "tail </parameter>",
    ];
    let orders: [[&str; 3]; 6] = [
        ["path", "old_text", "new_text"],
        ["path", "new_text", "old_text"],
        ["old_text", "path", "new_text"],
        ["old_text", "new_text", "path"],
        ["new_text", "path", "old_text"],
        ["new_text", "old_text", "path"],
    ];
    let mut bad = Vec::new();
    for v in values {
        for target in ["old_text", "new_text"] {
            for order in orders {
                let params: Vec<(&str, &str)> = order
                    .iter()
                    .map(|k| {
                        let val = if *k == target {
                            v
                        } else {
                            match *k {
                                "path" => "a.rs",
                                "old_text" => "OLD",
                                _ => "NEW",
                            }
                        };
                        (*k, val)
                    })
                    .collect();
                let (_, calls, failures) = extract(&xml_call("edit_file", &params));
                let got = calls.first().and_then(|c| arg_str(c, target));
                if got.as_deref() != Some(v) || calls.len() != 1 || !failures.is_empty() {
                    bad.push(format!("{target} in order {order:?}, value {v:?}\n    got {got:?} calls={} failures={failures:?}", calls.len()));
                }
            }
        }
    }
    assert!(bad.is_empty(), "{} of {} cases wrong:\n{}", bad.len(), values.len() * 12, bad.join("\n"));
}

/// The model does add parameters a tool does not have (saved Qwen3-Coder logs: `read_file`
/// with `range` and `pattern`). Such a parameter after a known one must not be swallowed into
/// the known one's value.
#[test]
fn xml_unrecognised_parameter_after_a_known_one_does_not_corrupt_the_known_value() {
    let content = xml_call("read_file", &[("path", "src/a.rs"), ("range", "1-50")]);
    let (_, calls, failures) = extract(&content);
    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(calls.len(), 1);
    assert_eq!(
        arg_str(&calls[0], "path").as_deref(),
        Some("src/a.rs"),
        "the unknown <parameter=range> was folded into path: {:?}",
        calls[0].function.arguments
    );
}

/// The same, for an edit: a trailing hallucinated `replace_all` must not end up inside `new_text`
/// (which is then written into the user's file if the diff is approved).
#[test]
fn xml_unrecognised_trailing_parameter_does_not_leak_into_new_text() {
    let content = xml_call("edit_file", &[("path", "a.rs"), ("old_text", "x"), ("new_text", "y"), ("replace_all", "true")]);
    let (_, calls, _) = extract(&content);
    assert_eq!(calls.len(), 1);
    assert_eq!(
        arg_str(&calls[0], "new_text").as_deref(),
        Some("y"),
        "new_text swallowed the unknown parameter: {:?}",
        calls[0].function.arguments["new_text"]
    );
}

/// An unknown parameter written FIRST is fine (the next known one is the boundary).
#[test]
fn xml_unrecognised_parameter_before_a_known_one_is_harmless() {
    let content = xml_call("read_file", &[("mode", "fast"), ("path", "a.rs")]);
    let (_, calls, failures) = extract(&content);
    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(arg_str(&calls[0], "path").as_deref(), Some("a.rs"));
}

/// A repeated parameter must not merge the two values into one.
#[test]
fn xml_a_repeated_parameter_does_not_merge_its_values() {
    let content = xml_call("read_file", &[("path", "a.rs"), ("path", "b.rs")]);
    let (_, calls, _) = extract(&content);
    assert_eq!(calls.len(), 1);
    let p = arg_str(&calls[0], "path").unwrap_or_default();
    assert!(p == "a.rs" || p == "b.rs", "two path values merged into {p:?}");
}

/// The module doc says a value is split only at a `<tool_call>` followed by an OFFERED tool's
/// name. `<function=example_function_name>` is the example in Qwen3-Coder's own system prompt,
/// so a model quoting its format reproduces it; it is not an offered tool.
#[test]
fn xml_a_quoted_example_function_tag_inside_a_value_is_not_a_call_boundary() {
    let v = "format:\n<tool_call>\n<function=example_function_name>\nvalue";
    let content = xml_call("edit_file", &[("path", "doc.md"), ("old_text", "x"), ("new_text", v)]);
    let (_, calls, failures) = extract(&content);
    assert!(
        failures.is_empty() && calls.len() == 1 && arg_str(&calls[0], "new_text").as_deref() == Some(v),
        "calls={calls:?} failures={failures:?}"
    );
}

/// An answer that only MENTIONS the wrapper tags (the b05 shape behind a bare EOF error from
/// Ollama) comes back through the recovery as the user's answer; it must keep the words it is about.
#[test]
fn xml_prose_mentioning_the_wrapper_tags_comes_back_unchanged() {
    let prose = "Each call is wrapped in <tool_call> and </tool_call> tags.";
    let (text, calls, failures) = extract(prose);
    assert!(calls.is_empty() && failures.is_empty());
    assert_eq!(text, prose, "the tags were deleted from the prose");
}

/// `null` (any case) is turned into JSON null for ANY declared type, string included.
#[test]
fn xml_string_parameter_holding_the_word_null_stays_text() {
    for word in ["null", "NULL", "Null"] {
        let (_, calls, _) = extract(&xml_call("search", &[("text", word)]));
        assert_eq!(calls.len(), 1);
        assert!(
            calls[0].function.arguments["text"].is_string(),
            "search text {word:?} became {}",
            calls[0].function.arguments["text"]
        );
    }
}

/// End to end: what the app does with it. Replacing code with the word `null` must not become a
/// deletion, and searching for `null` must not be answered "called without text".
#[test]
fn xml_replacing_text_with_the_word_null_is_not_a_deletion_and_searching_for_it_works() {
    let (_, calls, _) = extract(&xml_call("edit_file", &[("path", "a.py"), ("old_text", "None"), ("new_text", "null")]));
    match tools::interpret(&calls[0]) {
        Ok(Request::Edit { new_text, .. }) => assert_eq!(new_text, "null", "new_text null arrived as {new_text:?} (a deletion)"),
        other => panic!("{other:?}"),
    }
    let (_, calls, _) = extract(&xml_call("search", &[("text", "null")]));
    let r = tools::interpret(&calls[0]);
    assert!(matches!(r, Ok(Request::Search { ref text, .. }) if text == "null"), "search for null: {r:?}");
}

/// parse(render(v)) == v for string values, through the real renderer: the parser must strip
/// exactly the one newline each side that the renderer adds.
#[test]
fn xml_render_then_parse_round_trips_string_values() {
    let values = [
        "", " ", "\n", "\n\n", "a\n", "\na", "\n\na\n\n", "  indented\n", "tab\there", "x\r\ny", "a & b < c > d",
        "line1\nline2", "123", "true", "[1, 2]", "{\"a\": 1}", "\u{e9}\u{65e5}\u{672c}\u{8a9e}\u{1f600}", "</parameter>", "null", "NULL",
    ];
    let mut bad = Vec::new();
    for v in values {
        let msgs = vec![Message::user("go"), asked(&call("edit_file", json!({ "path": "a.rs", "old_text": "o", "new_text": v })))];
        let prompt = xmlcall::render(&msgs, &[]);
        let (start, end) = (prompt.find("<tool_call>").unwrap(), prompt.rfind("</tool_call>").unwrap() + "</tool_call>".len());
        let (_, calls, failures) = extract(&prompt[start..end]);
        let got = calls.first().map(|c| c.function.arguments["new_text"].clone());
        if got != Some(json!(v)) || !failures.is_empty() {
            bad.push(format!("{v:?} -> {got:?} {failures:?}"));
        }
    }
    assert!(bad.is_empty(), "round trip lost:\n{}", bad.join("\n"));
}

/// Exactly one newline each side goes; spaces and further newlines stay.
#[test]
fn xml_only_one_newline_each_side_is_stripped() {
    let cases = [
        ("<parameter=new_text>\n\n</parameter>", ""),
        ("<parameter=new_text>\n</parameter>", ""),
        ("<parameter=new_text></parameter>", ""),
        ("<parameter=new_text>\n\n\n</parameter>", "\n"),
        ("<parameter=new_text>\n  x  \n</parameter>", "  x  "),
        ("<parameter=new_text>\n\nx\n\n</parameter>", "\nx\n"),
        ("<parameter=new_text>x</parameter>", "x"),
    ];
    for (param, want) in cases {
        let content = format!("<tool_call>\n<function=edit_file>\n<parameter=path>\na.rs\n</parameter>\n<parameter=old_text>\no\n</parameter>\n{param}\n</function>\n</tool_call>");
        let (_, calls, failures) = extract(&content);
        assert!(failures.is_empty(), "{param:?}: {failures:?}");
        assert_eq!(arg_str(&calls[0], "new_text").as_deref(), Some(want), "{param:?}");
    }
}

/// A reply cut off anywhere: never a panic (multi-byte text included), and a half-written
/// value is never accepted as the whole value.
#[test]
fn xml_a_reply_cut_off_anywhere_never_panics_and_never_keeps_half_a_value() {
    let want = [("path", "src/\u{e9}.rs"), ("old_text", "fn \u{e4}() {}"), ("new_text", "fn \u{f6}() { \u{65e5}\u{672c}\u{8a9e} \u{1f600} }")];
    let full = xml_call("edit_file", &want);
    let mut kept_half = Vec::new();
    for cut in full.char_indices().map(|(i, _)| i).chain(std::iter::once(full.len())) {
        let (_, calls, _) = extract(&full[..cut]);
        for c in &calls {
            for (k, w) in want {
                if let Some(got) = arg_str(c, k) {
                    if got != w {
                        kept_half.push(format!("cut at {cut}: {k} = {got:?}"));
                    }
                }
            }
        }
    }
    assert!(kept_half.is_empty(), "{}", kept_half.join("\n"));
}

/// A cut-off reply is accepted as a complete call when what was written so far parses, even if
/// a REQUIRED parameter has not been written. For `edit_file`, a missing `new_text` is a deletion.
#[test]
fn xml_a_reply_cut_off_before_a_required_parameter_is_not_accepted_as_a_call() {
    let full = xml_call("edit_file", &[("path", "a.rs"), ("old_text", "let keep = 1;"), ("new_text", "let keep = 2;")]);
    let required = ["path", "old_text", "new_text"];
    let mut accepted = Vec::new();
    for cut in full.char_indices().map(|(i, _)| i) {
        let (_, calls, _) = extract(&full[..cut]);
        if let Some(c) = calls.first() {
            let missing: Vec<&str> = required.iter().copied().filter(|k| c.function.arguments.get(*k).is_none()).collect();
            if !missing.is_empty() {
                let tail: String = full[..cut].chars().rev().take(24).collect::<Vec<_>>().into_iter().rev().collect();
                accepted.push(format!("cut at {cut} (...{tail:?}): accepted without {missing:?}; interpret -> {:?}", tools::interpret(c)));
            }
        }
    }
    assert!(accepted.is_empty(), "{}", accepted.join("\n"));
}

#[test]
fn xml_a_call_without_its_opening_tag_is_found_only_at_a_line_start() {
    let body = "<function=read_file>\n<parameter=path>\na.rs\n</parameter>\n</function>";
    // The case H4 measured: no opening tag, a stray closing one.
    let (text, calls, failures) = extract(&format!("{body}\n</tool_call>"));
    assert!((text.as_str(), calls.len(), failures.len()) == ("", 1, 0), "{text:?} {calls:?} {failures:?}");
    // Prose before it on its own line, and indentation.
    let (text, calls, _) = extract(&format!("Reading it.\n  {body}"));
    assert_eq!((text.as_str(), calls.len()), ("Reading it.", 1));
    // Mid-line is prose.
    let (text, calls, _) = extract(&format!("I will call {body}"));
    assert!(calls.is_empty() && text.contains("<function=read_file>"));
}

/// Several calls in one reply, wrapped.
#[test]
fn xml_several_wrapped_calls_in_one_reply_are_all_found_in_order() {
    let a = xml_call("read_file", &[("path", "a.rs")]);
    let b = xml_call("search", &[("text", "needle"), ("pattern", "*.rs")]);
    let c = xml_call("run_check", &[]);
    let (text, calls, failures) = extract(&format!("First.\n{a}\nThen.\n{b}\nLast.\n{c}\nEnd."));
    assert!(failures.is_empty(), "{failures:?}");
    let names: Vec<&str> = calls.iter().map(|c| c.function.name.as_str()).collect();
    assert_eq!(names, ["read_file", "search", "run_check"]);
    assert_eq!(arg_str(&calls[1], "pattern").as_deref(), Some("*.rs"));
    for word in ["First.", "Then.", "Last.", "End."] {
        assert!(text.contains(word), "{word} lost from {text:?}");
    }
}

/// Two calls with no wrapper tag at all (the shape H4 case 4 had): the value of the first call
/// must not run on into the second call.
#[test]
fn xml_two_calls_without_wrappers_do_not_run_into_each_other() {
    let content = "<function=read_file>\n<parameter=path>\na.rs\n</parameter>\n</function>\n<function=read_file>\n<parameter=path>\nb.rs\n</parameter>\n</function>";
    let (_, calls, failures) = extract(content);
    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(arg_str(&calls[0], "path").as_deref(), Some("a.rs"), "first call: {:?}", calls[0].function.arguments);
}

#[test]
fn xml_an_unknown_tool_is_a_failure_and_a_good_call_beside_it_is_kept() {
    let bad = xml_call("rm_rf", &[("path", "/")]);
    let good = xml_call("read_file", &[("path", "a.rs")]);
    let (_, calls, failures) = extract(&format!("{bad}\n{good}"));
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].function.name, "read_file");
    assert_eq!(failures.len(), 1);
    assert!(failures[0].contains("no such tool: rm_rf"), "{failures:?}");
}

#[test]
fn xml_a_call_with_no_parameters_and_non_ascii_values() {
    let (_, calls, failures) = extract(&xml_call("run_check", &[]));
    assert!(failures.is_empty());
    assert_eq!(calls[0].function.arguments, json!({}));
    let v = "caf\u{e9} \u{65e5}\u{672c}\u{8a9e} \u{1f600} \u{2014} <b>&amp;</b>";
    let (_, calls, _) = extract(&xml_call("create_file", &[("path", "n\u{fc}.txt"), ("content", v)]));
    assert_eq!(arg_str(&calls[0], "content").as_deref(), Some(v));
    assert_eq!(arg_str(&calls[0], "path").as_deref(), Some("n\u{fc}.txt"));
}

/// Every spelling of a line number the model might write, from XML to the range the tool reads.
#[test]
fn xml_line_number_spellings_reach_the_range_correctly() {
    let range = |start: &str, end: Option<&str>| -> Result<Option<Lines>, String> {
        let mut params = vec![("path", "a.rs"), ("start_line", start)];
        if let Some(e) = end {
            params.push(("end_line", e));
        }
        let (_, calls, failures) = extract(&xml_call("read_file", &params));
        assert!(failures.is_empty(), "{failures:?}");
        match tools::interpret(&calls[0])? {
            Request::Read { range, .. } => Ok(range),
            other => panic!("{other:?}"),
        }
    };
    let l = |start, end| Ok(Some(Lines { start, end }));
    assert_eq!(range("120", Some("180")), l(120, Some(180)));
    assert_eq!(range(" 120 ", Some(" 180 ")), l(120, Some(180)));
    assert_eq!(range("2.0", Some("9.0")), l(2, Some(9)));
    assert_eq!(range("0", Some("5")), l(1, Some(5)));
    assert_eq!(range("1e3", None), l(1000, None));
    assert_eq!(range("null", None), Ok(None));
    assert_eq!(range("", None), Ok(None));
    for bad in ["120.5", "-5", "abc", "0x10", "1,000", "\u{663}", "12abc", "NaN", "inf", "1e300"] {
        assert!(range(bad, None).is_err(), "{bad:?} -> {:?}", range(bad, None));
    }
    assert!(range("90", Some("20")).is_err());
}

/// Typed values by schema, through the real parser (a throwaway tool with every type).
#[test]
fn xml_typed_values_follow_the_schema() {
    let tools = vec![Tool::function(
        "t",
        "t",
        json!({"type":"object","properties":{
            "i":{"type":"integer"},"n":{"type":"number"},"b":{"type":"boolean"},
            "a":{"type":"array"},"o":{"type":"object"},"s":{"type":"string"}}}),
    )];
    let run = |k: &str, v: &str| {
        let content = format!("<tool_call>\n<function=t>\n<parameter={k}>\n{v}\n</parameter>\n</function>\n</tool_call>");
        xmlcall::extract_calls(&content, &tools).1[0].function.arguments[k].clone()
    };
    assert_eq!(run("i", "12"), json!(12));
    assert_eq!(run("i", "12.0"), json!("12.0"));
    assert_eq!(run("n", "1.5"), json!(1.5));
    assert_eq!(run("n", "2.0"), json!(2));
    assert_eq!(run("b", "TRUE"), json!(true));
    assert_eq!(run("b", "false"), json!(false));
    assert_eq!(run("a", "[1, 2]"), json!([1, 2]));
    assert_eq!(run("a", "[1, 2"), json!("[1, 2"));
    assert_eq!(run("o", "{\"k\": [1]}"), json!({"k": [1]}));
    assert_eq!(run("s", "12"), json!("12"));
    assert_eq!(run("zzz", "12"), json!("12"));
}

// ---------------------- xmlcall: the renderer ----------------------

#[test]
fn render_app_conversation_has_ollamas_layout() {
    let tools = app_tools();
    let msgs = vec![
        Message::system("SYS"),
        Message::user("Fix it"),
        asked(&call("read_file", json!({"path":"src/a.rs","start_line":120,"end_line":180}))),
        Message::tool_result("read_file", "[lines 120\u{2013}180 of 2140]\nfn a() {}\n"),
        asked(&call("update_plan", json!({"plan":"[x] a\n[ ] b"}))),
        Message::tool_result("update_plan", "Plan updated: 1 of 2 done."),
        asked(&call("run_check", json!({}))),
        Message::tool_result("run_check", "passed"),
    ];
    let p = xmlcall::render(&msgs, &tools);
    assert!(p.starts_with("<|im_start|>system\nSYS\n\n# Tools\n\nYou have access to the following functions:\n\n<tools>\n<function>\n<name>read_file</name>\n<description>"), "{}", &p[..200]);
    assert!(p.ends_with("<|im_start|>assistant\n"));
    // An empty-content assistant turn leaves one blank line before the call; numbers are bare.
    assert!(p.contains("<|im_start|>assistant\n\n<tool_call>\n<function=read_file>\n<parameter=end_line>\n180\n</parameter>\n<parameter=path>\nsrc/a.rs\n</parameter>\n<parameter=start_line>\n120\n</parameter>\n</function>\n</tool_call><|im_end|>\n"), "{p}");
    assert!(p.contains("<|im_start|>user\n<tool_response>\n[lines 120\u{2013}180 of 2140]\nfn a() {}\n\n</tool_response><|im_end|>\n"));
    assert!(p.contains("<function=run_check>\n</function>\n</tool_call><|im_end|>\n"));
    assert!(p.contains("<function=update_plan>\n<parameter=plan>\n[x] a\n[ ] b\n</parameter>\n</function>"));
    // Every offered tool, in the order offered (not sorted), each with its required list.
    let order: Vec<usize> = tools.iter().map(|t| p.find(&format!("<name>{}</name>", t.function.name)).unwrap()).collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
    assert!(p.contains("<required>[\"plan\"]</required>") && p.contains("<required>[\"path\"]</required>"));
}

/// Consecutive tool results share one user block; a tool result after an assistant turn opens
/// its own; a message list that starts with a tool result still renders.
#[test]
fn render_consecutive_tool_results_share_one_user_block() {
    let t = vec![
        Message::tool_result("read_file", "R1"),
        Message::tool_result("read_file", "R2"),
        asked(&call("read_file", json!({"path":"x"}))),
        Message::tool_result("read_file", "R3"),
        Message::user("next"),
    ];
    let p = xmlcall::render(&t, &[]);
    assert!(p.starts_with("<|im_start|>user\n<tool_response>\nR1\n</tool_response>\n<tool_response>\nR2\n</tool_response><|im_end|>\n<|im_start|>assistant\n\n<tool_call>"), "{p}");
    assert!(p.contains("</tool_call><|im_end|>\n<|im_start|>user\n<tool_response>\nR3\n</tool_response><|im_end|>\n<|im_start|>user\nnext<|im_end|>\n<|im_start|>assistant\n"), "{p}");
}

#[test]
fn render_escapes_json_fragments_the_way_go_does_and_leaves_strings_raw() {
    let msgs = vec![Message::user("go"), asked(&call("t", json!({"items": ["<a>", "b&c"], "s": "<raw> & text", "n": null, "f": 2.5, "b": true})))];
    let p = xmlcall::render(&msgs, &[]);
    assert!(p.contains("<parameter=items>\n[\"\\u003ca\\u003e\",\"b\\u0026c\"]\n</parameter>"), "{p}");
    assert!(p.contains("<parameter=s>\n<raw> & text\n</parameter>"));
    assert!(p.contains("<parameter=n>\nnull\n</parameter>") && p.contains("<parameter=f>\n2.5\n</parameter>") && p.contains("<parameter=b>\ntrue\n</parameter>"));
}

// ---------------------- tool arguments: ranged reads (H8) ----------------------

fn line(v: Value) -> Result<Option<usize>, String> {
    tools::line_arg(&json!({ "start_line": v }), "start_line")
}

#[test]
fn args_every_spelling_of_a_line_number() {
    let ok = |v: Value, want: Option<usize>| assert_eq!(line(v.clone()), Ok(want), "{v}");
    ok(json!(120), Some(120));
    ok(json!("120"), Some(120));
    ok(json!(" 180 "), Some(180));
    ok(json!(120.0), Some(120));
    ok(json!("120.0"), Some(120));
    ok(json!("1e2"), Some(100));
    ok(json!(1e2), Some(100));
    ok(json!(0), Some(0));
    ok(json!("0"), Some(0));
    ok(json!("+12"), Some(12));
    ok(json!(null), None);
    ok(json!(""), None);
    ok(json!("   "), None);
    ok(json!(u64::MAX), Some(u64::MAX as usize));
    ok(json!(999_999_999_999u64), Some(999_999_999_999));
    for bad in [
        json!(-3), json!(-0.5), json!("abc"), json!(12.5), json!("12.5"), json!("-1"), json!(true), json!([1]),
        json!({"a": 1}), json!(1e300), json!("1e300"), json!("NaN"), json!("inf"), json!("-inf"), json!("1_000"),
        json!("12abc"), json!("\u{663}"), json!("1e12"),
    ] {
        assert!(line(bad.clone()).is_err(), "{bad} was accepted as {:?}", line(bad.clone()));
    }
}

#[test]
fn args_read_range_rules() {
    let r = |s: Value, e: Value| tools::read_range(&json!({ "start_line": s, "end_line": e }));
    let lines = |start, end| Ok(Some(Lines { start, end }));
    assert_eq!(r(json!(null), json!(null)), Ok(None));
    assert_eq!(r(json!(5), json!(5)), lines(5, Some(5)));
    assert_eq!(r(json!(null), json!(40)), lines(1, Some(40)));
    assert_eq!(r(json!(0), json!(null)), lines(1, None));
    assert_eq!(r(json!(3), json!(null)), lines(3, None));
    assert_eq!(r(json!(u64::MAX), json!(null)), lines(usize::MAX, None));
    assert_eq!(r(json!(u64::MAX), json!(u64::MAX)), lines(usize::MAX, Some(usize::MAX)));
    for (s, e) in [(json!(5), json!(4)), (json!(0), json!(0)), (json!(null), json!(0)), (json!("5"), json!("3")), (json!(2), json!(1))] {
        assert!(r(s.clone(), e.clone()).is_err(), "{s} {e} -> {:?}", r(s.clone(), e.clone()));
    }
}

fn hdr(a: usize, b: usize, of: usize) -> String {
    format!("[lines {a}\u{2013}{b} of {of}]\n")
}

#[test]
fn read_header_numbers_are_right_for_odd_files() {
    let l = |start, end| Some(Lines { start, end });
    // No final newline: the last line is a line, and is not given a newline.
    assert_eq!(tools::read_text("f", b"a\nb\nc", l(1, Some(2))), format!("{}a\nb\n", hdr(1, 2, 3)));
    assert_eq!(tools::read_text("f", b"a\nb\nc", l(3, None)), format!("{}c", hdr(3, 3, 3)));
    // CRLF with no final newline, and a BOM.
    assert_eq!(tools::read_text("f", b"\xEF\xBB\xBFa\r\nb\r\nc", l(2, Some(3))), format!("{}b\nc", hdr(2, 3, 3)));
    // A file that is one newline is one empty line.
    assert_eq!(tools::read_text("f", b"\n", l(1, Some(1))), format!("{}\n", hdr(1, 1, 1)));
    // Empty, and BOM only.
    assert_eq!(tools::read_text("f", b"", None), "");
    for bytes in [&b""[..], &b"\xEF\xBB\xBF"[..]] {
        assert_eq!(tools::read_text("f", bytes, l(1, None)), "error: f is empty, so it has no lines to read");
    }
    // The end of the file: the last line, one past it, and an end far past it.
    assert_eq!(tools::read_text("f", b"a\nb\n", l(2, Some(10))), format!("{}b\n", hdr(2, 2, 2)));
    assert_eq!(tools::read_text("f", b"a\nb\n", l(3, None)), "error: f has 2 lines; start_line 3 is past the end");
    assert_eq!(
        tools::read_text("f", b"a\nb\n", l(usize::MAX, Some(usize::MAX))),
        format!("error: f has 2 lines; start_line {} is past the end", usize::MAX)
    );
    // Invalid UTF-8 is replaced, not dropped, and does not change the count.
    assert_eq!(tools::read_text("f", b"a\n\xff\xfe\nb\n", l(1, None)), format!("{}a\n\u{fffd}\u{fffd}\nb\n", hdr(1, 3, 3)));
}

#[test]
fn read_size_limits_sit_exactly_on_the_documented_bytes() {
    // Whole: 65536 bytes is read, 65537 is refused with the size and the line count.
    let exact = vec![120u8; tools::MAX_READ_BYTES as usize];
    assert_eq!(tools::read_text("f", &exact, None).len(), exact.len());
    let over = vec![120u8; tools::MAX_READ_BYTES as usize + 1];
    let out = tools::read_text("f", &over, None);
    assert!(out.starts_with("error: f is 65537 bytes and 1 lines, over the 65536-byte limit"), "{out}");
    // Ranged: 64 lines of exactly 1024 bytes fit; a 65th is cut, and the header says where to go on.
    let one = format!("{}\n", "y".repeat(1023));
    let sixty_four = one.repeat(64);
    let out = tools::read_text("f", sixty_four.as_bytes(), Some(Lines { start: 1, end: None }));
    assert_eq!(out, format!("{}{}", hdr(1, 64, 64), sixty_four));
    let sixty_five = one.repeat(65);
    let out = tools::read_text("f", sixty_five.as_bytes(), Some(Lines { start: 1, end: None }));
    let head = "[lines 1\u{2013}64 of 65; cut at 65536 bytes \u{2014} read on from line 65]\n";
    assert!(out.starts_with(head), "{}", &out[..80]);
    assert_eq!(out.len(), head.len() + 65536);
    // A NUL in the first 8 KB is binary, whole or ranged, before any size rule.
    let mut bin = vec![97u8; 100_000];
    bin[8191] = 0;
    for range in [None, Some(Lines { start: 1, end: Some(1) })] {
        assert!(tools::read_text("f", &bin, range).contains("looks like a binary"), "{range:?}");
    }
}

/// read_text and run_read_file are public and take any Lines; the app builds them with
/// read_range, which never gives start 0 or an end before the start. Other callers may.
#[test]
fn read_text_does_not_panic_or_lie_on_lines_that_read_range_would_never_make() {
    let file = b"a\nb\nc\nd\ne\n";
    let mut bad = Vec::new();
    for r in [
        Lines { start: 0, end: None },
        Lines { start: 0, end: Some(3) },
        Lines { start: 3, end: Some(1) },
        Lines { start: 5, end: Some(2) },
        Lines { start: 2, end: Some(1) },
    ] {
        match catch_unwind(AssertUnwindSafe(|| tools::read_text("f", file, Some(r)))) {
            Err(_) => bad.push(format!("{r:?}: panicked")),
            Ok(out) if out.contains("over the 65536-byte limit") => bad.push(format!("{r:?}: answered {out:?}")),
            Ok(_) => {}
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

// ---------------------- the repeat rule and the refusals the app sends back ----------------------

/// The app sends a refusal from interpret() back to the model as the bare message (lib.rs reject(),
/// App.tsx pushes out.error as the tool message). prior_failures only counts results that start
/// with error:, so the third identical bad call is never stopped by the repeat rule.
#[test]
fn repeat_rule_counts_a_bad_range_refused_by_interpret() {
    let bad = call("read_file", json!({ "path": "a.rs", "start_line": "abc" }));
    let why = tools::interpret(&bad).unwrap_err();
    let history = vec![Message::user("go"), asked(&bad), Message::tool_result("read_file", why.clone())];
    assert_eq!(tools::prior_failures(&history, &bad), 1, "refusal {why:?} is not counted as a failure");
}

#[test]
fn repeat_rule_counts_the_seventh_plan_update_refusal() {
    let plan = call("update_plan", json!({ "plan": "[ ] a" }));
    let history = vec![Message::user("go"), asked(&plan), Message::tool_result("update_plan", tools::PLAN_LIMIT_NOTE)];
    assert_eq!(tools::prior_failures(&history, &plan), 1, "the plan-limit refusal is not counted as a failure");
}

/// H7 replaces an old long argument with a stub in the webview copy of the conversation, so an
/// identical resend no longer equals the call in the history and the repeat rule starts again.
#[test]
fn repeat_rule_survives_an_old_long_argument_being_set_aside() {
    let edit = call("edit_file", json!({ "path": "a.rs", "old_text": "x".repeat(3000), "new_text": "y" }));
    let mut history = vec![Message::user("go"), asked(&edit), Message::tool_result("edit_file", "error: old_text was not found in a.rs")];
    assert_eq!(tools::prior_failures(&history, &edit), 1);
    compact::set_aside(&mut history[1], None);
    assert_eq!(tools::prior_failures(&history, &edit), 1, "after set_aside the identical resend is not recognised");
}

// ---------------------- compact (H7) ----------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn random_conversation(rng: &mut Rng) -> Vec<Message> {
    let mut m = vec![Message::system("s".repeat(200 + rng.below(3000))), Message::user("u".repeat(1 + rng.below(400)))];
    for _ in 0..rng.below(16) {
        match rng.below(8) {
            0..=2 => {
                let path = ["a.rs".to_string(), "b.rs".to_string(), "./A.rs".to_string(), format!("src{}a.rs", char::from(92u8)), format!("C:{0}proj{0}a.rs", char::from(92u8))][rng.below(5)].clone();
                let mut args = json!({ "path": path });
                if rng.below(3) == 0 {
                    let s = 1 + rng.below(40);
                    args["start_line"] = if rng.below(2) == 0 { json!(s) } else { json!(s.to_string()) };
                    args["end_line"] = json!(s + 5 + rng.below(40));
                }
                m.push(asked(&call("read_file", args)));
                m.push(Message::tool_result("read_file", "x".repeat(rng.below(9000))));
            }
            3 => {
                m.push(asked(&call("search", json!({ "text": "needle" }))));
                m.push(Message::tool_result("search", "src/a.rs:1: needle\n".repeat(rng.below(400))));
            }
            4 => {
                let (o, n) = ("o".repeat(rng.below(4000)), "n".repeat(rng.below(4000)));
                m.push(asked(&call("edit_file", json!({ "path": "a.rs", "old_text": o, "new_text": n }))));
                m.push(Message::tool_result("edit_file", "Edited a.rs: 1 line removed, 1 added."));
            }
            5 => {
                m.push(asked(&call("create_file", json!({ "path": "n.rs", "content": "c".repeat(rng.below(6000)) }))));
                m.push(Message::tool_result("create_file", "Created n.rs (10 lines)."));
            }
            6 => {
                let steps = 1 + rng.below(12);
                let plan: String = (0..steps).map(|i| format!("[ ] step {i} {}\n", "w".repeat(rng.below(100)))).collect();
                m.push(asked(&call("update_plan", json!({ "plan": plan }))));
                m.push(Message::tool_result("update_plan", "Plan updated: 0 of 3 done."));
            }
            _ => {
                m.push(asked(&call("run_check", json!({}))));
                m.push(Message::tool_result("run_check", "e".repeat(rng.below(5000))));
            }
        }
    }
    m
}

/// Exactly what the app does with a plan (compact_for_send in apps/mimir/src-tauri/src/lib.rs).
fn apply(messages: &mut [Message], set_aside: &[usize]) {
    for &i in set_aside {
        let c = i.checked_sub(1).and_then(|p| messages[p].tool_calls.as_deref()?.first().cloned());
        compact::set_aside(&mut messages[i], c.as_ref());
    }
}

fn has_long_arg(m: &Message) -> bool {
    m.tool_calls.as_deref().unwrap_or(&[]).iter().any(|c| {
        c.function
            .arguments
            .as_object()
            .is_some_and(|o| o.values().any(|v| v.as_str().is_some_and(|s| s.chars().count() > compact::ARG_LIMIT)))
    })
}

fn is_plan_call(m: &Message) -> bool {
    m.tool_calls.as_deref().and_then(|c| c.first()).is_some_and(|c| c.function.name == "update_plan")
}

/// The part of a conversation compaction must never change: roles, call names and paths.
fn shape(v: &[Message]) -> Vec<(String, Vec<(String, Value)>)> {
    v.iter()
        .map(|m| {
            let calls = m.tool_calls.as_deref().unwrap_or(&[]);
            (m.role.clone(), calls.iter().map(|c| (c.function.name.clone(), c.function.arguments["path"].clone())).collect())
        })
        .collect()
}

/// Check every invariant on one random conversation; returns whether compaction acted, and the
/// violations found.
fn check_seed(seed: u64, tools: &[Tool]) -> (bool, Vec<String>) {
    let mut bad: Vec<String> = Vec::new();
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let before = random_conversation(&mut rng);
    let ctx = [2048u32, 4096, 8192, 32768][rng.below(4)];
    let calib = if rng.below(2) == 0 {
        None
    } else {
        let chars = 5000 + rng.below(60_000) as u64;
        Some((chars, (chars as f64 / (1.2 + rng.below(50) as f64 / 10.0)) as u32))
    };
    let budget = compact::Budget::new(ctx, calib);
    let plan = compact::compact(&before, tools, &budget);
    let tag = format!("seed {seed} ctx {ctx} calib {calib:?}");

    // Ascending, unique, in range.
    if !plan.set_aside.windows(2).all(|w| w[0] < w[1]) || plan.set_aside.iter().any(|&i| i >= before.len()) {
        return (true, vec![format!("{tag}: indices not ascending or out of range {:?}", plan.set_aside)]);
    }
    // Never the system prompt, a user message, the last message or the newest result; only a
    // long result or a long argument; never a task list.
    let last = before.len() - 1;
    let newest_tool = before.iter().rposition(|m| m.role == "tool");
    for &i in &plan.set_aside {
        let m = &before[i];
        let eligible = match m.role.as_str() {
            "tool" => m.content.chars().count() > compact::RESULT_LIMIT && !m.content.starts_with(compact::STUB_MARKER),
            "assistant" => has_long_arg(m),
            _ => false,
        };
        if !eligible {
            bad.push(format!("{tag}: index {i} ({}) should never be set aside", m.role));
        }
        if i == last || Some(i) == newest_tool {
            bad.push(format!("{tag}: index {i} is the last message or the newest result"));
        }
        if is_plan_call(m) || (i > 0 && is_plan_call(&before[i - 1])) {
            bad.push(format!("{tag}: an update_plan call or its result (index {i}) was set aside"));
        }
    }
    // Applying it changes content only.
    let mut after = before.clone();
    apply(&mut after, &plan.set_aside);
    if shape(&before) != shape(&after) {
        bad.push(format!("{tag}: a message, a call or a path changed"));
    }
    for i in 0..before.len() {
        let set = plan.set_aside.contains(&i);
        if !set && before[i] != after[i] {
            bad.push(format!("{tag}: message {i} changed but was not in the plan"));
        }
        if set && before[i].role == "tool" && !after[i].content.starts_with(compact::STUB_MARKER) {
            bad.push(format!("{tag}: result {i} is not a stub"));
        }
        if set && before[i].role == "assistant" && has_long_arg(&after[i]) {
            bad.push(format!("{tag}: call {i} still has a long argument"));
        }
    }
    // The estimate the plan reports is the estimate of what was produced.
    let recomputed = budget.tokens(compact::prompt_chars(&after, tools));
    if recomputed != plan.estimated_tokens {
        bad.push(format!("{tag}: plan says {} tokens, the result is {recomputed}", plan.estimated_tokens));
    }
    if plan.fits != (plan.estimated_tokens <= budget.hard_tokens()) {
        bad.push(format!("{tag}: fits flag disagrees with the hard limit"));
    }
    // Doing it twice does nothing more; under budget, nothing is touched.
    let again = compact::compact(&after, tools, &budget);
    if !again.set_aside.is_empty() {
        bad.push(format!("{tag}: a second pass set aside {:?} more", again.set_aside));
    }
    if budget.tokens(compact::prompt_chars(&before, tools)) <= budget.allowed_tokens() && !plan.set_aside.is_empty() {
        bad.push(format!("{tag}: set something aside while under budget"));
    }
    (!plan.set_aside.is_empty(), bad)
}

#[test]
fn compact_invariants_hold_on_random_conversations() {
    let tools = app_tools();
    let mut bad: Vec<String> = Vec::new();
    let mut acted = 0;
    for seed in 1..=800u64 {
        let (did, found) = check_seed(seed, &tools);
        acted += did as usize;
        bad.extend(found);
    }
    assert!(acted > 100, "the generator rarely exercised compaction ({acted} of 800)");
    assert!(bad.is_empty(), "{} violations, first 12:\n{}", bad.len(), bad.iter().take(12).cloned().collect::<Vec<_>>().join("\n"));
}

fn read_pair(path: &str, args_extra: Value, chars: usize) -> [Message; 2] {
    let mut args = json!({ "path": path });
    if let Value::Object(extra) = args_extra {
        for (k, v) in extra {
            args[k] = v;
        }
    }
    [asked(&call("read_file", args)), Message::tool_result("read_file", "x".repeat(chars))]
}

fn search_pair(chars: usize) -> [Message; 2] {
    [asked(&call("search", json!({ "text": "q" }))), Message::tool_result("search", "y".repeat(chars))]
}

fn tight(allowed_tokens: u32) -> compact::Budget {
    compact::Budget::new((allowed_tokens as f64 / compact::BUDGET).ceil() as u32 + 1, None)
}

/// A rejected read (a range that ends before it starts, which interpret refuses in a few words)
/// shares the whole-file key of the earlier good read, so it becomes the newest copy of the file
/// and the only copy of the content is set aside first.
#[test]
fn compact_a_refused_read_does_not_demote_the_good_read_of_the_same_file() {
    let mut m = vec![Message::system("sys"), Message::user("go")];
    m.extend(read_pair("a.rs", json!({}), 5000));
    let refused = [
        asked(&call("read_file", json!({ "path": "a.rs", "start_line": 90, "end_line": 20 }))),
        Message::tool_result("read_file", "end_line is the last line to read, not a count"),
    ];
    m.extend(refused);
    m.extend(search_pair(5000));
    m.extend(read_pair("b.rs", json!({}), 5000));
    // Over by one result: the search (index 7) is the one to go; the only copy of a.rs (index 3) must stay.
    let c = compact::compact(&m, &[], &tight(5000));
    assert_eq!(c.set_aside, vec![7], "set aside {:?}: the only content of a.rs went before the search result", c.set_aside);
}

/// After an edit of a file, the read before it is stale; compaction still treats it as the
/// newest read of that file. Documented here as observed behaviour, not asserted as a defect.
#[test]
fn compact_a_read_made_stale_by_an_edit_is_still_kept_ahead_of_a_search() {
    let mut m = vec![Message::system("sys"), Message::user("go")];
    m.extend(read_pair("a.rs", json!({}), 5000));
    m.extend([asked(&call("edit_file", json!({ "path": "a.rs", "old_text": "o", "new_text": "n" }))), Message::tool_result("edit_file", "Edited a.rs: 1 line removed, 1 added.")]);
    m.extend(search_pair(5000));
    m.extend(read_pair("b.rs", json!({}), 5000));
    let c = compact::compact(&m, &[], &tight(5000));
    assert_eq!(c.set_aside, vec![7], "{:?}", c.set_aside);
}

/// The stubs say what was set aside, which range, and how to get it back.
#[test]
fn compact_stub_text_is_honest_about_what_was_dropped() {
    let stub = |name: &str, args: Value, result: &str| {
        let c = call(name, args);
        let mut m = Message::tool_result(name, result);
        compact::set_aside(&mut m, Some(&c));
        m.content
    };
    let s = stub("read_file", json!({ "path": "src/x.rs", "start_line": 120, "end_line": 180 }), "a\nb\n");
    assert!(s.starts_with(compact::STUB_MARKER) && s.contains("read_file src/x.rs lines 120\u{2013}180 (2 lines)") && s.contains("Read it again"), "{s}");
    let s = stub("read_file", json!({ "path": "src/x.rs", "start_line": 5 }), "a\n");
    assert!(s.contains("src/x.rs from line 5 (1 line)"), "{s}");
    let s = stub("search", json!({ "text": "q" }), "a\nb\nc");
    assert!(s.contains("the earlier result of search (3 lines)") && s.contains("Run it again"), "{s}");
    let s = stub("run_check", json!({}), "x");
    assert!(s.contains("the earlier result of run_check") && s.contains("Run it again"), "{s}");
    let mut orphan = Message::user("not a result");
    orphan.role = "tool".into();
    compact::set_aside(&mut orphan, None);
    assert!(orphan.content.contains("an earlier result"), "{}", orphan.content);
}

/// A task list within its caps is never set aside, even as the oldest thing in an over-budget
/// turn; one far over the caps (the model sent it, the parser would trim it) is.
#[test]
fn compact_leaves_a_task_list_within_its_caps_alone() {
    let steps: Vec<String> = (0..tools::MAX_PLAN_STEPS).map(|i| format!("[ ] step {i} {}", "w".repeat(tools::MAX_PLAN_STEP_CHARS - 12))).collect();
    let plan = steps.join("\n");
    let oversize = (0..30).map(|i| format!("[ ] step {i} {}", "w".repeat(200))).collect::<Vec<_>>().join("\n");
    for (text, expect_aside) in [(plan, false), (oversize, true)] {
        let mut m = vec![Message::system("sys"), Message::user("go")];
        m.extend([asked(&call("update_plan", json!({ "plan": text }))), Message::tool_result("update_plan", "Plan updated: 0 of 12 done.")]);
        m.extend(read_pair("a.rs", json!({}), 9000));
        m.extend(read_pair("b.rs", json!({}), 9000));
        let c = compact::compact(&m, &app_tools(), &tight(1500));
        assert_eq!(c.set_aside.contains(&2), expect_aside, "plan call set aside = {:?}", c.set_aside);
        assert!(!c.set_aside.contains(&3), "the plan result is never set aside");
    }
}

/// Budgets built from nonsense never panic or divide by zero.
#[test]
fn compact_extreme_calibration_and_budgets_never_panic() {
    let mut rng = Rng(7);
    let m = random_conversation(&mut rng);
    for calib in [None, Some((0, 0)), Some((u64::MAX, 1)), Some((1, u32::MAX)), Some((u64::MAX, u32::MAX)), Some((5, 0)), Some((0, 5))] {
        for ctx in [0u32, 1, 100, u32::MAX] {
            let b = compact::Budget::new(ctx, calib);
            assert!(b.chars_per_token.is_finite() && b.chars_per_token >= 1.5 && b.chars_per_token <= 5.0, "{calib:?} -> {}", b.chars_per_token);
            let _ = compact::compact(&m, &app_tools(), &b);
            let _ = b.tokens(u64::MAX);
        }
    }
    for cpt in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let b = compact::Budget { num_ctx: 8192, chars_per_token: cpt };
        let _ = compact::compact(&m, &app_tools(), &b);
    }
}

// ---------------------- update_plan (H10) ----------------------

fn step(text: &str, status: PlanStatus) -> tools::PlanStep {
    tools::PlanStep { text: text.to_string(), status }
}

#[test]
fn plan_malformed_input_never_panics_and_is_read_sensibly() {
    use PlanStatus::*;
    let p = tools::parse_plan;
    assert_eq!(p("[ ] a\n\n\n[x] b"), [step("a", Pending), step("b", Done)]);
    assert_eq!(p("no markers\nat all"), [step("no markers", Pending), step("at all", Pending)]);
    assert_eq!(p("[ ]a\n[X] B\n  [~]   c  "), [step("a", Pending), step("B", Done), step("c", Doing)]);
    assert!(p("[x]").is_empty() && p("[ ]\n[~]  \n- [x]").is_empty() && p("").is_empty() && p("\n\n").is_empty());
    // Unrecognised marks are part of the text, still a pending step.
    assert_eq!(p("[-] skipped\n[] none"), [step("[-] skipped", Pending), step("[] none", Pending)]);
    // A bare carriage return does not split lines; CRLF does.
    assert_eq!(p("[ ] a\r\n[ ] b"), [step("a", Pending), step("b", Pending)]);
    // 12 steps pass whole, the 13th is dropped; a very long line is cut at 120 characters.
    let twelve: String = (1..=12).map(|i| format!("[ ] s{i}\n")).collect();
    assert_eq!(p(&twelve).len(), 12);
    assert_eq!(p(&format!("{twelve}[ ] s13")).len(), 12);
    let long = format!("[x] {}", "z".repeat(10_000));
    assert_eq!(p(&long)[0].text.chars().count(), tools::MAX_PLAN_STEP_CHARS);
    // Multi-byte text at every position of the marker never splits a character.
    for s in ["[\u{e9}] x", "[\u{65e5}] x", "\u{1f600}[ ] x", "[ \u{1f600}] x", "[x\u{e9}", "1.\u{e9}", "-\u{1f600}", "9\u{663}) x"] {
        let _ = p(s);
    }
    // Arbitrary bytes, decoded lossily.
    let mut rng = Rng(99);
    for _ in 0..500 {
        let bytes: Vec<u8> = (0..rng.below(80)).map(|_| rng.below(256) as u8).collect();
        let _ = p(&String::from_utf8_lossy(&bytes));
    }
}

/// The model is told "Plan updated: N of M done." after an update. When the list is cut to the
/// caps, M is the cut size and nothing says the rest was dropped.
#[test]
fn plan_a_list_cut_to_the_caps_is_not_silently_cut() {
    let twenty: String = (1..=20).map(|i| format!("[ ] step {i}\n")).collect();
    let steps = tools::parse_plan(&twenty);
    let said = tools::plan_message(&steps);
    assert!(
        said.contains("12") && (said.contains("only") || said.contains("cut") || said.contains("first") || said.contains("dropped") || said.contains("20")),
        "the model sent 20 steps and was told only {said:?}"
    );
}

/// A model of the loop in App.tsx (askWithTools): a plan update that Rust accepts gives its round
/// back; everything else, a refused update included, costs one. Bounds both.
fn app_loop(mut next: impl FnMut(&[Message]) -> ToolCall) -> (usize, usize, usize) {
    let mut convo = vec![Message::user("go")];
    let (mut step, mut requests, mut accepted, mut rounds_paid) = (0i64, 0usize, 0usize, 0usize);
    while step < tools::MAX_ROUNDS as i64 && requests < tools::MAX_ROUNDS + tools::MAX_PLAN_UPDATES {
        let c = next(&convo);
        let outcome = match tools::interpret(&c) {
            Err(e) => (e, false),
            Ok(Request::Plan { steps }) if tools::plan_updates(&convo) >= tools::MAX_PLAN_UPDATES => {
                let _ = steps;
                (tools::PLAN_LIMIT_NOTE.to_string(), false)
            }
            Ok(Request::Plan { steps }) => (tools::plan_message(&steps), true),
            Ok(_) => ("ok".to_string(), false),
        };
        convo.push(asked(&c));
        convo.push(Message::tool_result(&c.function.name, outcome.0));
        if outcome.1 {
            accepted += 1;
            step -= 1;
        } else {
            rounds_paid += 1;
        }
        step += 1;
        requests += 1;
    }
    (requests, accepted, rounds_paid)
}

#[test]
fn plan_no_model_can_keep_a_turn_running_with_task_lists() {
    let plan = |t: &str| call("update_plan", json!({ "plan": t }));
    let read = call("read_file", json!({ "path": "a.rs" }));
    let limit = tools::MAX_ROUNDS + tools::MAX_PLAN_UPDATES;
    // Only valid updates, the same or different each time.
    let (req, acc, paid) = app_loop(|_| plan("[ ] a"));
    assert!(req <= limit && acc == tools::MAX_PLAN_UPDATES && paid == tools::MAX_ROUNDS, "{req} {acc} {paid}");
    let mut n = 0;
    let (req, acc, _) = app_loop(|_| {
        n += 1;
        plan(&format!("[ ] step {n}"))
    });
    assert!(req <= limit && acc == tools::MAX_PLAN_UPDATES, "{req} {acc}");
    // Malformed updates cost rounds like any call.
    let (req, acc, _) = app_loop(|_| plan("   "));
    assert!(req == tools::MAX_ROUNDS && acc == 0, "{req} {acc}");
    // Alternating with reads.
    let mut flip = false;
    let (req, acc, _) = app_loop(|_| {
        flip = !flip;
        if flip { plan("[ ] a") } else { read.clone() }
    });
    assert!(req <= limit && acc <= tools::MAX_PLAN_UPDATES, "{req} {acc}");
}

/// A task list reaches update_plan unharmed through each recovery path.
#[test]
fn plan_survives_the_xml_and_json_recovery_paths() {
    let want = [step("read it", PlanStatus::Done), step("say \"hi\"", PlanStatus::Doing), step("run the check", PlanStatus::Pending)];
    let text = "[x] read it\n[~] say \"hi\"\n[ ] run the check";
    // Qwen3-Coder layout: a multi-line value.
    let (_, calls, failures) = extract(&xml_call("update_plan", &[("plan", text)]));
    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(tools::interpret(&calls[0]), Ok(Request::Plan { steps: want.to_vec() }));
    // ChatML layout: valid JSON, and the repair for unescaped quotes.
    let good = r#"{"name": "update_plan", "arguments": {"plan": "[x] read it\n[~] say \"hi\"\n[ ] run the check"}}"#;
    let bad = r#"{"name": "update_plan", "arguments": {"plan": "[x] read it\n[~] say "hi"\n[ ] run the check"}}"#;
    for raw in [good, bad] {
        let c = toolcall::parse_call(raw, &app_tools()).unwrap_or_else(|e| panic!("{raw}: {e}"));
        assert_eq!(tools::interpret(&c), Ok(Request::Plan { steps: want.to_vec() }), "{raw}");
    }
}
