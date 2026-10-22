//! G7 — can a local model change files correctly?
//!
//!   cargo run -p bench --bin editprobe
//!   cargo run -p bench --bin editprobe -- --model llama3.1:8b --runs 3
//!   cargo run -p bench --bin editprobe -- --only t10 --format replace
//!
//! **This decides how Mimir edits files, and whether it should at all.**
//! Reading a file wrong costs one bad answer; writing one wrong corrupts it. So
//! before the feature is trusted, it is measured on the model that will do it.
//!
//! Two ways of editing, on the same ten tasks:
//!
//! - **replace** — `read_file`, `edit_file` (one exact piece of text, which must
//!   occur once) and `create_file`. **What the app ships**, with the exact tool
//!   descriptions and system prompt it uses, from `bifrost_core::tools`.
//! - **rewrite** — `read_file`, `write_file` (send back the whole file) and
//!   `create_file`. The obvious alternative, measured so the choice is evidence.
//!
//! Each at the app's own sampling settings, several runs, and at temperature 0
//! once. **Every run starts from a fresh copy of a small project whose files
//! have CRLF line endings** — because every file in this repo does
//! (`core.autocrlf = true`), and a probe run on LF files would test the easy case.
//!
//! Scored **byte for byte** against the expected result. `NEAR` is a separate
//! verdict for whitespace-only differences — worth knowing about, but not a pass.
//! One task must change nothing at all: a model that edits when asked a question
//! is unusable however well it edits.
//!
//! The loop mirrors the app's: only the first tool call in a reply is run, the
//! assistant turn that asked for it goes back first, and every call is approved
//! automatically — the probe measures the model, not the person. The loop itself
//! is `probe.rs`, shared with every later probe since H1.

#[path = "../probe.rs"]
mod probe;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use bifrost_core::chat::{Message, Options, Tool};
use bifrost_core::edit::Workspace;
use bifrost_core::tools;
use bifrost_core::{config, ollama};

use probe::{Setup, Transport};

type Files = BTreeMap<String, String>;

// ── the project every run starts from ───────────────────────────────────────

const CONFIG_RS: &str = "pub const PORT: u16 = 8080;
pub const HOST: &str = \"127.0.0.1\";

pub fn address() -> String {
    format!(\"{HOST}:{PORT}\")
}

pub fn debug_mode() -> bool {
    false
}
";

const MAIN_RS: &str = "mod config;

fn greet(name: &str) -> String {
    format!(\"Hello, {name}!\")
}

fn main() {
    println!(\"{}\", greet(\"world\"));
    println!(\"listening on {}\", config::address());
}
";

const CARGO_TOML: &str = "[package]
name = \"demo\"
version = \"0.1.0\"
edition = \"2021\"

[dependencies]
serde = \"1\"
";

const README_MD: &str = "# Demo

A small demo server. It listens on port 8080 by default.

## Running

Run it with `cargo run`.
";

const APP_TS: &str = "export function total(prices: number[]): number {
  let sum = 0;
  for (const p of prices) {
    sum += p;
  }
  return sum;
}
";

fn baseline() -> Files {
    [
        ("src/config.rs", CONFIG_RS),
        ("src/main.rs", MAIN_RS),
        ("Cargo.toml", CARGO_TOML),
        ("README.md", README_MD),
        ("web/app.ts", APP_TS),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

fn crlf(s: &str) -> Vec<u8> {
    s.replace('\n', "\r\n").into_bytes()
}

// ── the tasks ───────────────────────────────────────────────────────────────

enum Expect {
    /// These files must end up exactly so; every other file must be untouched.
    Files(Vec<(&'static str, String)>),
    /// Checked by a function, for tasks with more than one right answer — a new
    /// function or file the model writes itself. `touches` may change; nothing
    /// else may.
    Judge {
        touches: &'static [&'static str],
        check: fn(&Files) -> Result<(), String>,
    },
    /// Nothing may change, and the answer must contain this.
    Unchanged { answer_mentions: &'static str },
}

struct Task {
    id: &'static str,
    prompt: &'static str,
    expect: Expect,
}

fn tasks() -> Vec<Task> {
    let with = |base: &str, from: &str, to: &str| {
        assert!(base.contains(from), "fixture typo: {from:?}");
        base.replacen(from, to, 1)
    };
    vec![
        Task {
            id: "t01-constant",
            prompt: "In src/config.rs, change the port from 8080 to 3000.",
            expect: Expect::Files(vec![("src/config.rs", with(CONFIG_RS, "8080", "3000"))]),
        },
        Task {
            id: "t02-rename",
            prompt: "In src/main.rs, rename the function greet to welcome, and update the place it is called.",
            expect: Expect::Files(vec![(
                "src/main.rs",
                with(&with(MAIN_RS, "fn greet(", "fn welcome("), "greet(\"world\")", "welcome(\"world\")"),
            )]),
        },
        Task {
            id: "t03-toml",
            prompt: "Add a dependency on anyhow version 1 to Cargo.toml, on the line after serde.",
            expect: Expect::Files(vec![(
                "Cargo.toml",
                with(CARGO_TOML, "serde = \"1\"\n", "serde = \"1\"\nanyhow = \"1\"\n"),
            )]),
        },
        Task {
            id: "t04-markdown",
            prompt: "In README.md, change the Running section so it tells people to run `cargo run --release` instead of `cargo run`.",
            expect: Expect::Files(vec![(
                "README.md",
                with(README_MD, "Run it with `cargo run`.", "Run it with `cargo run --release`."),
            )]),
        },
        Task {
            id: "t05-string",
            prompt: "In src/main.rs, make main greet \"Luis\" instead of \"world\".",
            expect: Expect::Files(vec![("src/main.rs", with(MAIN_RS, "greet(\"world\")", "greet(\"Luis\")"))]),
        },
        Task {
            id: "t06-delete",
            prompt: "Delete the debug_mode function from src/config.rs — nothing uses it.",
            expect: Expect::Files(vec![(
                "src/config.rs",
                with(CONFIG_RS, "\npub fn debug_mode() -> bool {\n    false\n}\n", ""),
            )]),
        },
        Task {
            id: "t07-add-fn",
            prompt: "In web/app.ts, add an exported function average(prices: number[]): number after total, that returns total(prices) / prices.length.",
            expect: Expect::Judge {
                touches: &["web/app.ts"],
                check: |f| {
                    let s = f.get("web/app.ts").ok_or("web/app.ts is gone")?;
                    if !s.starts_with(APP_TS.trim_end()) {
                        return Err("the existing total() function was changed".into());
                    }
                    let at = s
                        .find("export function average(prices: number[]): number")
                        .ok_or("no exported average(prices: number[]): number")?;
                    if !s[at..].contains("total(prices) / prices.length") {
                        return Err("average does not return total(prices) / prices.length".into());
                    }
                    Ok(())
                },
            },
        },
        Task {
            id: "t08-create",
            prompt: "Create a file CHANGELOG.md in the project folder with a '# Changelog' heading followed by a bullet point '- Initial release'.",
            expect: Expect::Judge {
                touches: &["CHANGELOG.md"],
                check: |f| {
                    let s = f.get("CHANGELOG.md").ok_or("CHANGELOG.md was not created")?;
                    let lines: Vec<&str> = s.lines().map(str::trim_end).collect();
                    let h = lines.iter().position(|l| *l == "# Changelog").ok_or("no '# Changelog' heading")?;
                    let b = lines.iter().position(|l| *l == "- Initial release").ok_or("no '- Initial release' bullet")?;
                    if b < h {
                        return Err("the bullet comes before the heading".into());
                    }
                    Ok(())
                },
            },
        },
        Task {
            id: "t09-readonly",
            prompt: "What port does the server listen on? Just tell me, don't change anything.",
            expect: Expect::Unchanged { answer_mentions: "8080" },
        },
        Task {
            id: "t10-two-files",
            prompt: "Change the port to 9090 everywhere it is mentioned: src/config.rs and README.md.",
            expect: Expect::Files(vec![
                ("src/config.rs", with(CONFIG_RS, "8080", "9090")),
                ("README.md", with(README_MD, "8080", "9090")),
            ]),
        },
    ]
}

// ── running one task ────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum Format {
    Replace,
    Rewrite,
}

impl Format {
    fn name(self) -> &'static str {
        match self {
            Format::Replace => "replace",
            Format::Rewrite => "rewrite",
        }
    }

    fn tools(self) -> Vec<Tool> {
        let mut t = tools::available(true, None);
        if self == Format::Rewrite {
            t.retain(|t| t.function.name != "edit_file");
            t.insert(1, tools::rewrite_tool());
        }
        t
    }

    fn system(self, ws: &Workspace) -> String {
        let s = tools::system_prompt(ws, false);
        match self {
            Format::Replace => s,
            Format::Rewrite => s.replace(
                "then use edit_file with text copied exactly from what you read",
                "then use write_file to send back the complete file with your change made",
            ),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Verdict {
    Pass,
    Near,
    Fail,
    Error,
}

impl Verdict {
    fn label(self) -> &'static str {
        match self {
            Verdict::Pass => "PASS",
            Verdict::Near => "NEAR",
            Verdict::Fail => "FAIL",
            Verdict::Error => "ERROR",
        }
    }
}

struct Outcome {
    verdict: Verdict,
    why: String,
    calls: Vec<String>,
    edit_errors: usize,
    rounds: usize,
    hit_bound: bool,
    secs: f64,
    /// Each call and what came back, for reading a failure rather than guessing at it.
    log: Vec<String>,
    recovered: usize,
}

/// Every file under `dir`, as bytes, keyed by forward-slash relative path.
fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(b) = std::fs::read(&p) {
                let rel = p.strip_prefix(dir).unwrap().to_string_lossy().replace('\\', "/");
                out.insert(rel, b);
            }
        }
    }
    out
}

/// Whitespace-insensitive form, for telling "almost" from "wrong".
fn loose(s: &str) -> String {
    s.replace("\r\n", "\n")
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn first_difference(got: &str, want: &str) -> String {
    for (i, (g, w)) in got.lines().zip(want.lines()).enumerate() {
        if g != w {
            return format!("line {}: got {g:?}, want {w:?}", i + 1);
        }
    }
    format!("got {} lines, want {}", got.lines().count(), want.lines().count())
}

fn judge(task: &Task, before: &Files, after: &BTreeMap<String, Vec<u8>>, answer: &str) -> (Verdict, String) {
    let text = |b: &[u8]| String::from_utf8_lossy(b).replace("\r\n", "\n");
    let after_text: Files = after.iter().map(|(k, v)| (k.clone(), text(v))).collect();

    let may_change: Vec<&str> = match &task.expect {
        Expect::Files(fs) => fs.iter().map(|(p, _)| *p).collect(),
        Expect::Judge { touches, .. } => touches.to_vec(),
        Expect::Unchanged { .. } => vec![],
    };

    // Collateral damage first: anything outside what the task names must be
    // exactly as it was, byte for byte.
    for (path, content) in before {
        if may_change.contains(&path.as_str()) {
            continue;
        }
        match after.get(path) {
            None => return (Verdict::Fail, format!("{path} was deleted")),
            Some(b) if *b != crlf(content) => {
                return (Verdict::Fail, format!("{path} was changed but should not have been"));
            }
            _ => {}
        }
    }
    for path in after.keys() {
        if !before.contains_key(path) && !may_change.contains(&path.as_str()) {
            return (Verdict::Fail, format!("an unexpected file {path} was created"));
        }
    }

    match &task.expect {
        Expect::Unchanged { answer_mentions } => {
            if answer.contains(answer_mentions) {
                (Verdict::Pass, String::new())
            } else {
                (Verdict::Fail, format!("changed nothing, but the answer never said {answer_mentions}"))
            }
        }
        Expect::Judge { check, .. } => match check(&after_text) {
            Ok(()) => (Verdict::Pass, String::new()),
            Err(e) => (Verdict::Fail, e),
        },
        Expect::Files(want) => {
            let mut near = None;
            for (path, content) in want {
                let Some(got) = after.get(*path) else {
                    return (Verdict::Fail, format!("{path} is missing"));
                };
                if *got == crlf(content) {
                    continue;
                }
                let got_s = text(got);
                if loose(&got_s) == loose(content) {
                    near.get_or_insert_with(|| {
                        if !got.windows(2).any(|w| w == b"\r\n") {
                            format!("{path}: right content, but CRLF line endings were lost")
                        } else {
                            format!("{path}: whitespace differs — {}", first_difference(&got_s, content))
                        }
                    });
                    continue;
                }
                return (Verdict::Fail, format!("{path}: {}", first_difference(&got_s, content)));
            }
            match near {
                Some(why) => (Verdict::Near, why),
                None => (Verdict::Pass, String::new()),
            }
        }
    }
}

fn run(model: &str, task: &Task, format: Format, transport: Transport, opts: &Options, scratch: &Path) -> Outcome {
    let started = Instant::now();
    let dir = scratch.join("project");
    let backups = scratch.join("backups");
    let _ = std::fs::remove_dir_all(scratch);
    let before = baseline();
    for (rel, content) in &before {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, crlf(content)).unwrap();
    }
    let ws = Workspace::open(&dir).expect("fixture folder");
    let tools = format.tools();

    let mut messages = vec![Message::system(format.system(&ws)), Message::user(task.prompt)];
    let setup = Setup { ws: &ws, backups: &backups, check: None, confine_reads: false, compact: true, commands: None, unattended: false };
    let r = probe::run_loop(model, &mut messages, &tools, *opts, transport, tools::MAX_ROUNDS, &setup);
    let secs = started.elapsed().as_secs_f64();

    if let Some(e) = r.error {
        return Outcome {
            verdict: Verdict::Error,
            why: e,
            calls: r.calls,
            edit_errors: r.edit_errors,
            rounds: r.rounds,
            hit_bound: false,
            secs,
            log: r.log,
            recovered: r.recovered,
        };
    }

    let (mut verdict, mut why) = judge(task, &before, &tree(&dir), &r.answer);
    if r.stopped && verdict != Verdict::Pass {
        why = format!("stopped after repeating a failing call — {why}");
    }
    if r.hit_bound && verdict == Verdict::Pass {
        // Right files, but the app would have stopped it with an error.
        verdict = Verdict::Fail;
        why = format!("still calling tools after {} rounds", tools::MAX_ROUNDS);
    }
    Outcome {
        verdict,
        why,
        calls: r.calls,
        edit_errors: r.edit_errors,
        rounds: r.rounds,
        hit_bound: r.hit_bound,
        secs,
        log: r.log,
        recovered: r.recovered,
    }
}

// ── main ────────────────────────────────────────────────────────────────────

fn arg(args: &[String], key: &str) -> Option<String> {
    args.iter().position(|a| a == key).and_then(|i| args.get(i + 1)).cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = config::load();
    let model = arg(&args, "--model").unwrap_or(cfg.model.clone());
    let runs: usize = arg(&args, "--runs").and_then(|s| s.parse().ok()).unwrap_or(3);
    let only = arg(&args, "--only");
    let formats: Vec<Format> = match arg(&args, "--format").as_deref() {
        Some("replace") => vec![Format::Replace],
        Some("rewrite") => vec![Format::Rewrite],
        _ => vec![Format::Replace, Format::Rewrite],
    };
    let transports: Vec<Transport> = match arg(&args, "--transport").as_deref() {
        Some("native") => vec![Transport::Native],
        Some("recover") => vec![Transport::Recover],
        _ => vec![Transport::Native, Transport::Recover],
    };
    let tasks: Vec<Task> = tasks()
        .into_iter()
        .filter(|t| only.as_deref().is_none_or(|o| t.id.starts_with(o)))
        .collect();

    println!("G7 — edit probe");
    println!("model:  {model}");
    println!("ctx:    {}", cfg.num_ctx);
    println!("tasks:  {} · files have CRLF endings, like every file in this repo", tasks.len());
    println!("runs:   {runs} at the app's own settings, plus 1 at temperature 0\n");

    // Warm at the app's context, so the first task is not timing a model load.
    match ollama::load(&model, cfg.num_ctx) {
        Ok(d) => println!("warmed in {:.1}s\n", d.as_secs_f64()),
        Err(e) => {
            eprintln!("could not load {model}: {e}");
            std::process::exit(1);
        }
    }

    let scratch_root = std::env::temp_dir().join(format!("bifrost-editprobe-{}", std::process::id()));
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();

    let settings: Vec<(&str, Options, usize)> = vec![
        ("app", Options::new(cfg.num_ctx), runs),
        ("t=0", Options::measured(cfg.num_ctx), 1),
    ];

    let mut rows = Vec::new();
    let mut json = Vec::new();

    for &format in &formats {
      for &transport in &transports {
        for (label, opts, n) in &settings {
            let mut tally = [0usize; 4];
            let mut total_secs = 0.0;
            let mut edit_errors = 0;
            let mut calls_total = 0;
            let mut recovered_total = 0;
            println!("── {} · {} · {} ──", format.name(), transport.name(), label);
            for task in &tasks {
                let mut line = String::new();
                for i in 0..*n {
                    let scratch = scratch_root.join(format!("{}-{}-{}-{}-{i}", format.name(), transport.name(), label.replace('=', ""), task.id));
                    let o = run(&model, task, format, transport, opts, &scratch);
                    let _ = std::fs::remove_dir_all(&scratch);
                    tally[o.verdict as usize] += 1;
                    total_secs += o.secs;
                    edit_errors += o.edit_errors;
                    calls_total += o.calls.len();
                    recovered_total += o.recovered;
                    line.push_str(&format!("{:<5} ", o.verdict.label()));
                    if o.verdict != Verdict::Pass {
                        println!("    {} run {}: {} — {}", task.id, i + 1, o.verdict.label(), o.why);
                        for l in &o.log {
                            println!("      {l}");
                        }
                    }
                    json.push(serde_json::json!({
                        "format": format.name(), "transport": transport.name(), "setting": label, "log": o.log, "task": task.id, "run": i + 1,
                        "verdict": o.verdict.label(), "why": o.why, "calls": o.calls,
                        "editErrors": o.edit_errors, "recovered": o.recovered, "rounds": o.rounds, "hitBound": o.hit_bound,
                        "secs": (o.secs * 10.0).round() / 10.0,
                    }));
                }
                println!("  {:<14} {line}", task.id);
            }
            let runs_done: usize = tally.iter().sum();
            let pct = 100.0 * tally[0] as f64 / runs_done.max(1) as f64;
            println!(
                "  → {pass}/{runs_done} pass ({pct:.0}%) · {near} near · {fail} fail · {err} error · {edit_errors} rejected edits · {:.1}s avg\n",
                total_secs / runs_done.max(1) as f64,
                pass = tally[0], near = tally[1], fail = tally[2], err = tally[3],
            );
            if recovered_total > 0 {
                println!("  ({recovered_total} malformed calls recovered)");
            }
            rows.push(format!(
                "| {} | {} | {} | {}/{} ({pct:.0}%) | {} | {} | {} | {edit_errors} | {:.1} | {:.1}s |",
                format.name(), transport.name(), label, tally[0], runs_done, tally[1], tally[2], tally[3],
                calls_total as f64 / runs_done.max(1) as f64,
                total_secs / runs_done.max(1) as f64,
            ));
        }
      }
    }

    println!("─── {model} ───");
    println!("| format | transport | setting | pass | near | fail | error | rejected edits | calls/run | time/run |");
    println!("|---|---|---|---|---|---|---|---|---|---|");
    for r in &rows {
        println!("{r}");
    }

    let out = PathBuf::from("evals/out").join(format!("G7-{stamp}"));
    if std::fs::create_dir_all(&out).is_ok() {
        let _ = std::fs::write(
            out.join("results.json"),
            serde_json::to_string_pretty(&serde_json::json!({ "model": model, "runs": json })).unwrap(),
        );
        println!("\nwrote {}", out.join("results.json").display());
    }
    let _ = std::fs::remove_dir_all(&scratch_root);
}
