//! G0 — can a local model emit reliable tool calls?
//!
//!   cargo run -p bench --bin toolprobe
//!   cargo run -p bench --bin toolprobe -- --model llama3.1:8b
//!   cargo run -p bench --bin toolprobe -- --recover
//!
//! **This gates G6.** If no local model can be trusted to call a tool when one is
//! needed and to leave it alone when one is not, the tool-calling chunk does not
//! get built — and it is half an hour to find out rather than a wasted sitting.
//!
//! `bench`'s own harness cannot run this: it goes through `/api/generate`, which
//! cannot carry a `tools` array at all. It lives here anyway because this is the
//! measurement crate and this is a measurement.
//!
//! **The negative cases are the point.** A model that calls a tool every time is
//! as useless as one that never does, and nothing but reading the output will
//! tell you which you have — a spurious call is a perfectly well-formed reply.
//! Four of the ten cases below must produce no call at all.

use odin_core::chat::{self, Message, Options, Tool};
use odin_core::config;

/// What the model is offered. Small, unambiguous, and covering the three shapes
/// that matter: no arguments, one string argument, two numeric arguments.
fn tools() -> Vec<Tool> {
    vec![
        Tool::function(
            "get_time",
            "Get the current date and time on this machine.",
            serde_json::json!({ "type": "object", "properties": {}, "required": [] }),
        ),
        Tool::function(
            "read_file",
            "Read the contents of a file from disk and return it as text.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute or relative path to the file" }
                },
                "required": ["path"]
            }),
        ),
        Tool::function(
            "add",
            "Add two numbers and return their sum.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "a": { "type": "number", "description": "first addend" },
                    "b": { "type": "number", "description": "second addend" }
                },
                "required": ["a", "b"]
            }),
        ),
    ]
}

/// One case: the prompt, the tool that should be called (`None` = none should
/// be), and the arguments that would be correct.
struct Case {
    prompt: &'static str,
    want: Option<&'static str>,
    check: fn(&serde_json::Value) -> bool,
}

fn any(_: &serde_json::Value) -> bool {
    true
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = config::load();
    let model = match args.iter().position(|a| a == "--model") {
        Some(i) => args.get(i + 1).cloned().unwrap_or(cfg.model),
        None => cfg.model,
    };
    // Default stays native so G0/H4 numbers stay comparable; --recover goes
    // through the app's own path (native first, then the raw retry).
    let recover = args.iter().any(|a| a == "--recover");

    let cases: Vec<Case> = vec![
        // ---- should call a tool -------------------------------------------
        Case { prompt: "What time is it right now?", want: Some("get_time"), check: any },
        Case {
            prompt: "Add 847 and 1259 for me.",
            want: Some("add"),
            check: |a| a["a"].as_f64() == Some(847.0) && a["b"].as_f64() == Some(1259.0),
        },
        Case {
            prompt: "What is 12 plus 30?",
            want: Some("add"),
            check: |a| a["a"].as_f64() == Some(12.0) && a["b"].as_f64() == Some(30.0),
        },
        Case {
            prompt: "Read the file at C:\\temp\\notes.txt and tell me what it says.",
            want: Some("read_file"),
            check: |a| a["path"].as_str().is_some_and(|p| p.contains("notes.txt")),
        },
        Case {
            prompt: "Please show me the contents of README.md.",
            want: Some("read_file"),
            check: |a| a["path"].as_str().is_some_and(|p| p.contains("README.md")),
        },
        Case { prompt: "What is today's date?", want: Some("get_time"), check: any },

        // ---- must NOT call a tool -----------------------------------------
        // A model that reaches for a tool here is unusable: every one of these
        // is answerable from what it already knows, and a spurious call costs a
        // round trip and puts made-up data in the conversation.
        Case { prompt: "What is the capital of France?", want: None, check: any },
        Case {
            prompt: "Explain in one sentence what a borrow checker does.",
            want: None,
            check: any,
        },
        Case { prompt: "Write a haiku about rain.", want: None, check: any },
        Case {
            prompt: "What does the Rust keyword 'impl' mean? Answer in one sentence.",
            want: None,
            check: any,
        },
    ];

    println!("G0 — tool-call probe");
    println!("model:  {model}");
    println!("path:   {}", if recover { "recover (the app's: native, then the raw retry)" } else { "native" });
    println!("ctx:    {}", cfg.num_ctx);
    println!("temp:   0 - a verdict that moves between runs is not a measurement");
    println!("cases:  {} ({} expect a call, {} expect none)\n",
        cases.len(),
        cases.iter().filter(|c| c.want.is_some()).count(),
        cases.iter().filter(|c| c.want.is_none()).count(),
    );

    let tools = tools();
    let (mut correct, mut wrong_tool, mut bad_args, mut missed, mut spurious, mut errors, mut recovered) =
        (0, 0, 0, 0, 0, 0, 0);

    for (i, case) in cases.iter().enumerate() {
        let messages = [Message::user(case.prompt)];
        let outcome = if recover {
            chat::chat_tools(&model, &messages, &tools, Options::measured(cfg.num_ctx))
        } else {
            chat::chat(&model, &messages, &tools, Options::measured(cfg.num_ctx))
        };
        let reply = match outcome {
            Ok(r) => r,
            Err(e) => {
                errors += 1;
                println!("{:>2}. ERROR  {}\n    {e}", i + 1, case.prompt);
                continue;
            }
        };
        if reply.recovered {
            recovered += 1;
        }

        let calls = reply.tool_calls();
        let got = calls.first();
        let verdict = match (case.want, got) {
            // Wanted none, got none.
            (None, None) => { correct += 1; "ok    no call, correctly".to_string() }
            // Wanted none, got one. The failure that nothing else catches.
            (None, Some(c)) => {
                spurious += 1;
                format!("SPURIOUS  called {} when none was needed", c.function.name)
            }
            // Wanted one, got none. Usually answered in prose instead.
            (Some(w), None) => {
                missed += 1;
                let prose = reply.message.content.trim().replace('\n', " ");
                format!("MISSED    wanted {w}, answered in prose: \"{}\"",
                    prose.chars().take(60).collect::<String>())
            }
            (Some(w), Some(c)) if c.function.name != w => {
                wrong_tool += 1;
                format!("WRONG     wanted {w}, called {}", c.function.name)
            }
            (Some(w), Some(c)) if !(case.check)(&c.function.arguments) => {
                bad_args += 1;
                format!("BAD ARGS  {w} called with {}", c.function.arguments)
            }
            (Some(w), Some(_)) => { correct += 1; format!("ok    {w}, correct arguments") }
        };

        println!("{:>2}. {verdict}", i + 1);
        println!("    prompt: {}", case.prompt);
        if calls.len() > 1 {
            println!("    note:   {} calls returned; only the first was judged", calls.len());
        }
    }

    let total = cases.len();
    println!("\n─── {model} ───");
    println!("  correct        {correct}/{total}");
    println!("  wrong tool     {wrong_tool}");
    println!("  bad arguments  {bad_args}");
    println!("  missed a call  {missed}");
    println!("  spurious call  {spurious}     <- the one that makes a model unusable");
    if errors > 0 {
        println!("  errors         {errors}");
    }
    if recovered > 0 {
        println!("  recovered      {recovered}     <- needed the raw retry or a passed-through call read back out");
    }
    println!("\n  pass rate: {:.0}%", 100.0 * correct as f64 / total as f64);
}
