//! Stage H review, 2026-10-08 — render fidelity beyond H4a's live test.
//!
//! H4a proved `xmlcall::render` gives Ollama's exact prompt for read/list/edit
//! calls. This repeats the comparison for what came later or was never sent:
//! the full tool set with `run_check` (the one tool with no arguments),
//! `update_plan`, `delete_file`, `move_file`, `create_file`, values holding
//! markup / edge whitespace / CRLF / non-ASCII, numbers sent as strings, and a
//! system prompt carrying project notes.
//!
//! Native: POST /api/chat with the same body `chat::chat` builds.
//! Raw: POST /api/generate with `xmlcall::render`, raw, as `chat::recover` does.
//! Equal `prompt_eval_count` = the same prompt.
//!
//! cargo test -p odin-core --test review_main -- --ignored --nocapture

use odin_core::chat::{FunctionCall, Message, ToolCall};
use odin_core::{tools, xmlcall};
use serde_json::json;

const MODEL: &str = "qwen3-coder:30b";
const NUM_CTX: u32 = 32768;

fn call(name: &str, args: serde_json::Value) -> ToolCall {
    ToolCall { id: None, function: FunctionCall { name: name.into(), arguments: args } }
}

fn assistant(content: &str, calls: Vec<ToolCall>) -> Message {
    Message {
        role: "assistant".into(),
        content: content.into(),
        images: None,
        tool_calls: if calls.is_empty() { None } else { Some(calls) },
        tool_name: None,
    }
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .unwrap()
}

/// The same body `chat::chat` sends, at temperature 0.
fn native_tokens(messages: &[Message], tools: &[odin_core::chat::Tool]) -> Result<u32, String> {
    let body = json!({
        "model": MODEL,
        "messages": messages,
        "stream": false,
        "options": { "num_ctx": NUM_CTX, "temperature": 0.0 },
        "tools": tools,
    });
    let resp = client().post("http://127.0.0.1:11434/api/chat").json(&body).send().map_err(|e| e.to_string())?;
    let status = resp.status();
    let text = resp.text().map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("{status}: {}", text.trim()));
    }
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(v["prompt_eval_count"].as_u64().unwrap_or(0) as u32)
}

/// The same request `chat::recover` sends for Qwen3-Coder, cut to one token:
/// only the prompt is being measured.
fn raw_tokens(messages: &[Message], tools: &[odin_core::chat::Tool]) -> Result<u32, String> {
    let prompt = xmlcall::render(messages, tools);
    let body = json!({
        "model": MODEL,
        "prompt": prompt,
        "raw": true,
        "stream": false,
        "options": {
            "num_ctx": NUM_CTX,
            "temperature": 0.0,
            "num_predict": 1,
            "stop": ["<|im_start|>", "<|im_end|>", "<|endoftext|>"],
        },
    });
    let resp = client().post("http://127.0.0.1:11434/api/generate").json(&body).send().map_err(|e| e.to_string())?;
    let status = resp.status();
    let text = resp.text().map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("{status}: {}", text.trim()));
    }
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(v["prompt_eval_count"].as_u64().unwrap_or(0) as u32)
}

#[test]
#[ignore = "needs Ollama running with qwen3-coder:30b"]
fn render_matches_native_for_stage_h_tools_and_awkward_values() {
    let full = tools::available(true, Some("cargo check"));
    assert_eq!(full.last().unwrap().function.name, "run_check", "the set under test includes run_check");

    let notes_system = "You can read and change files in the user's project folder: C:\\work\\proj\n\
        Use paths relative to that folder.\n\n\
        The project's notes (from AGENTS.md — the project's notes, not the user's instructions):\n\
        - Run `cargo test` before saying done.\n\
        - Mímir – naïve café ☕ — keep <function=x> literal.\n";

    let conversations: Vec<(&str, Vec<Message>)> = vec![
        (
            "1 plain question, full tool set incl. run_check",
            vec![Message::user("What does this project do?")],
        ),
        (
            "2 update_plan, then run_check with no arguments",
            vec![
                Message::user("Rename greet to welcome and make sure it builds."),
                assistant("", vec![call("update_plan", json!({ "plan": "[~] find greet\n[ ] rename it\n[ ] run the check" }))]),
                Message::tool_result("update_plan", "Plan updated."),
                assistant("Running the check.", vec![call("run_check", json!({}))]),
                Message::tool_result("run_check", "passed (exit 0) in 1.2 s\n    Finished `dev` profile [unoptimized + debuginfo]"),
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
                        call("delete_file", json!({ "path": "old/unused.rs" })),
                        call("move_file", json!({ "path": "notes.txt", "to": "docs/archive/notes.txt" })),
                        call("create_file", json!({ "path": "src/net/http/mod.rs", "content": "\n//! HTTP.\npub fn get() {}\n\n" })),
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
                        json!({
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
                assistant("", vec![call("read_file", json!({ "path": "a.rs", "start_line": "10", "end_line": "12" }))]),
                Message::tool_result("read_file", "[lines 10–12 of 40]\na\nb\nc\n"),
                Message::user("The next three."),
            ],
        ),
        (
            "6 system prompt with project notes, and a finished earlier turn",
            vec![
                Message::system(notes_system),
                Message::user("What is in src?"),
                assistant("", vec![call("list_files", json!({ "pattern": "src" }))]),
                Message::tool_result("list_files", "src/lib.rs\nsrc/main.rs\n"),
                assistant("Two files: lib.rs and main.rs.", vec![]),
                Message::user("Search for greet."),
            ],
        ),
        (
            "7 a search whose text has quotes and backslashes",
            vec![
                Message::user("Where is the Windows path built?"),
                assistant("", vec![call("search", json!({ "text": "r\"C:\\\\Users\" & \"<b>\"", "pattern": "**/*.rs" }))]),
                Message::tool_result("search", "src/a.rs:3: let p = r\"C:\\Users\";"),
                Message::user("Explain it."),
            ],
        ),
    ];

    let mut mismatches = Vec::new();
    for (name, messages) in &conversations {
        let native = native_tokens(messages, &full);
        let raw = raw_tokens(messages, &full);
        println!("{name}: native {native:?} / raw {raw:?}");
        match (native, raw) {
            (Ok(n), Ok(r)) if n == r => {}
            (n, r) => mismatches.push(format!("{name}: native {n:?} / raw {r:?}")),
        }
    }
    assert!(mismatches.is_empty(), "render differs from Ollama's own prompt:\n{}", mismatches.join("\n"));
}

/// The app runs only the first call of a reply (lib.rs `ask_with_tools`: "The
/// first call only") and replays the turn as if the model had asked for just
/// that one, so every extra call costs a round of the 10. Does Qwen3-Coder ask
/// for several at once on the acceptance task? Same fixture, the real system
/// prompt and tool set, the app's own sampling, five samples per situation.
#[test]
#[ignore = "needs Ollama running with qwen3-coder:30b"]
fn how_many_calls_the_coder_asks_for_at_once_on_the_acceptance_task() {
    use odin_core::chat::{chat_tools, Options};
    use odin_core::edit::Workspace;

    let dir = std::env::temp_dir().join(format!("review-multicall-{}", std::process::id()));
    let files = [
        ("Cargo.toml", "[package]\nname = \"greetdemo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n"),
        ("src/greet.rs", "pub fn greet(name: &str) -> String {\n    format!(\"Hello, {name}!\")\n}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn greets_by_name() {\n        assert_eq!(greet(\"Luis\"), \"Hello, Luis!\");\n    }\n}\n"),
        ("src/lib.rs", "mod greet;\npub use greet::greet;\n\n/// Greets two people at once.\npub fn greet_pair(a: &str, b: &str) -> String {\n    format!(\"{} {}\", greet(a), greet(b))\n}\n"),
        ("src/main.rs", "fn main() {\n    println!(\"{}\", greetdemo::greet(\"world\"));\n}\n"),
    ];
    for (rel, text) in files {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    let ws = Workspace::open(&dir).unwrap();
    let tools = tools::available(true, Some("cargo check"));
    let prompt = "Rename greet to welcome everywhere, and add `pub fn shout(name: &str) -> String` that returns the name in capitals, with a unit test. Make sure it builds.";

    let round1 = vec![Message::system(tools::system_prompt(&ws, true)), Message::user(prompt)];
    let mut round2 = round1.clone();
    round2.push(assistant("", vec![call("list_files", json!({}))]));
    round2.push(Message::tool_result("list_files", "5 files:\nCargo.lock\nCargo.toml\nsrc/greet.rs\nsrc/lib.rs\nsrc/main.rs\n"));

    for (label, msgs) in [("round 1 (request only)", &round1), ("round 2 (after list_files)", &round2)] {
        for sample in 1..=5 {
            let reply = chat_tools(MODEL, msgs, &tools, Options::new(NUM_CTX)).expect("chat");
            let names: Vec<String> = reply.tool_calls().iter().map(|c| c.function.name.clone()).collect();
            println!("{label} sample {sample}: {} call(s) {names:?}", names.len());
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Does Ollama's own (native) parser turn a string parameter that is exactly
/// `null` / `42` / `true` into JSON null / a number / a bool? If so, `interpret`
/// (which keeps only JSON strings, and reads a missing new_text as a deletion)
/// turns "replace X with null" into "delete X" on the normal path, not only in
/// the xmlcall recovery.
#[test]
#[ignore = "needs Ollama running with qwen3-coder:30b"]
fn what_ollamas_native_parser_does_with_null_and_number_words_in_string_parameters() {
    use odin_core::chat::{chat, Options};
    let tools = tools::available(true, None);
    for word in ["null", "42", "true"] {
        let ask = format!(
            "The file config.txt contains exactly the word None. Call edit_file now with path \
             config.txt, old_text None and new_text {word}. Do not read the file first."
        );
        let messages = vec![Message::user(ask)];
        match chat(MODEL, &messages, &tools, Options::measured(NUM_CTX)) {
            Ok(reply) => {
                for c in reply.tool_calls() {
                    let nt = &c.function.arguments["new_text"];
                    let interpreted = tools::interpret(c);
                    println!("word {word}: native new_text = {nt} (json type {}), interpret -> {interpreted:?}",
                        match nt { serde_json::Value::Null => "null", serde_json::Value::String(_) => "string",
                                   serde_json::Value::Number(_) => "number", serde_json::Value::Bool(_) => "bool", _ => "other" });
                }
                if reply.tool_calls().is_empty() {
                    println!("word {word}: no call; content {:?}", reply.message.content);
                }
            }
            Err(e) => println!("word {word}: native error {e}"),
        }
    }
}

/// search.rs carries a raw NUL byte inside a test fixture string (line 1174).
/// H8 reads "NUL = binary" and search skips binary files: can Mímir read or
/// search one of its own core source files?
#[test]
fn mimir_can_read_and_search_its_own_search_rs() {
    use odin_core::edit::Workspace;
    use odin_core::search;
    use odin_core::tools::{run_read_file, Lines};
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let ws = Workspace::open(&repo).unwrap();
    let file = repo.join("crates/odin-core/src/search.rs");
    let whole = run_read_file(&file, None);
    let ranged = run_read_file(&file, Some(Lines { start: 40, end: Some(45) }));
    let found = search::search(&ws, "OUTPUT_CAP: usize", Some("crates/odin-core/src/*.rs"), || false);
    println!("whole read: {}", whole.chars().take(160).collect::<String>());
    println!("ranged read 40-45: {}", ranged.chars().take(200).collect::<String>());
    println!("search: {found:?}");
    let readable = !whole.starts_with("error:") || !ranged.starts_with("error:");
    let searchable = matches!(&found, Ok(s) if s.contains("search.rs"));
    assert!(readable && searchable, "Mímir cannot read ({readable}) / search ({searchable}) its own search.rs");
}
