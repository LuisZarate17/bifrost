//! H2 — can the model find code nobody pointed it at?
//!
//!   cargo run -p bench --bin findprobe
//!   cargo run -p bench --bin findprobe -- --runs 1 --only q09 --variant search
//!
//! Until H2, `read_file` was only any use when someone named the file. H1's fix
//! probe watched the model read an error that could only be in `main.rs` and go
//! on editing `config.rs`. This measures whether `list_files` and `search` fix
//! that — on a real codebase, this one (then named odin), as a detached git worktree at a pinned
//! commit so the answers cannot move.
//!
//! Eighteen questions, three kinds:
//!
//! - **Named** (8): the question names the thing — `DEFAULT_NUM_CTX`, `prior_failures`.
//!   Grep's home ground.
//! - **Vague** (7): it does not — "where is an edit outside the folder refused?".
//!   The model has to guess words to search for. This is the half that decides
//!   whether a repo map or embeddings are ever worth building (H10).
//! - **No tool** (3): a haiku, a sum, a keyword. Passing means answering without
//!   calling anything — a model that searches for everything is as useless as one
//!   that never does (G0's lesson).
//!
//! A question passes when the answer names the right file **and** states the
//! fact. Reading the file is not required: a search hit can be the whole answer.
//! Only `read_file`, `list_files` and `search` are offered (R-09). Every run
//! shares one worktree, so a model that edited, created or deleted a file would
//! change what every later run reads — which happened when the edit tools were
//! on. A "no tool needed" question is still scored by whether the model called
//! any tool at all, so offering fewer tools does not change that test. As a
//! second guard the worktree is reset after every run (`git checkout -- .` and
//! `git clean -fdq`); if that fails the run is booked ERROR with git's message,
//! never as the model's verdict.
//!
//! Each runs **with** list_files and search, and **without** — the tools the app
//! offered before H2 — at the app's sampling and once at temperature 0.

#[path = "../probe.rs"]
mod probe;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use bifrost_core::chat::{Message, Options, Tool};
use bifrost_core::edit::Workspace;
use bifrost_core::{config, ollama, search, tools};

use probe::{Setup, Transport};

/// The commit every question is about. H1's, so `check.rs` exists.
const PIN: &str = "f95365f";

/// Past the app's own bound, to see how many rounds finding really takes.
/// Passes after the app's bound are counted apart.
const PROBE_ROUNDS: usize = 20;

/// The system-prompt sentence about the finding tools, removed for the variant
/// that does not have them. Checked, so a change of wording cannot slip past.
const FIND_SENTENCE: &str =
    " To find where something is, use search or list_files rather than guessing a path.";

/// The only tools findprobe offers: what a find question needs. The edit, create,
/// delete and move tools were offered once, and models used them — in
/// `H2-find-1791487078` q12 run 2 edited `lib.rs`, and the runs after it read the
/// edited file (R-09).
const FIND_TOOLS: [&str; 3] = ["read_file", "list_files", "search"];

// ── the questions ───────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Named,
    Vague,
    NoTool,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Named => "named",
            Kind::Vague => "vague",
            Kind::NoTool => "no tool",
        }
    }
}

struct Question {
    id: &'static str,
    kind: Kind,
    ask: &'static str,
    /// The answer must name one of these (the end of a path is enough).
    files: &'static [&'static str],
    /// And state one of these, as a whole word. Empty: the file is the fact.
    facts: &'static [&'static str],
}

fn questions() -> Vec<Question> {
    use Kind::*;
    vec![
        Question {
            id: "q01-num-ctx",
            kind: Named,
            ask: "Where is DEFAULT_NUM_CTX defined, and what is its value?",
            files: &["config.rs"],
            facts: &["32768", "32,768"],
        },
        Question {
            id: "q02-read-bytes",
            kind: Named,
            ask: "What is MAX_READ_BYTES set to, and in which file?",
            files: &["tools.rs"],
            facts: &["64 * 1024", "65536", "65,536", "64 KB", "64KB", "64 KiB"],
        },
        Question {
            id: "q03-prior-failures",
            kind: Named,
            ask: "Which file defines the function prior_failures?",
            files: &["tools.rs"],
            facts: &[],
        },
        Question {
            id: "q04-identical",
            kind: Named,
            ask: "What is MAX_IDENTICAL_FAILURES set to, and in which file?",
            files: &["tools.rs"],
            facts: &["3", "three"],
        },
        Question {
            id: "q05-fingerprint",
            kind: Named,
            ask: "Which file has the function fingerprint, and what kind of hash does it compute?",
            files: &["edit.rs"],
            facts: &["FNV", "FNV-1a"],
        },
        Question {
            id: "q06-check-timeout",
            kind: Named,
            ask: "Where is CHECK_TIMEOUT defined, and how long is it?",
            files: &["check.rs"],
            facts: &["180"],
        },
        Question {
            id: "q07-recent",
            kind: Named,
            ask: "What is RECENT_FOLDERS set to, and in which file?",
            files: &["mimir/src-tauri/src/lib.rs"],
            facts: &["5", "five"],
        },
        Question {
            id: "q08-default-model",
            kind: Named,
            ask: "Where is DEFAULT_MODEL defined, and which model does it name?",
            files: &["config.rs"],
            facts: &["qwen3-vl:8b-instruct-q4_K_M"],
        },
        Question {
            id: "q09-outside",
            kind: Vague,
            ask: "Where is the rule that stops an edit to a file outside the project folder? Name the file and the function.",
            files: &["edit.rs"],
            facts: &["contain", "contain_with", "resolve_existing", "resolve_new"],
        },
        Question {
            id: "q10-rounds",
            kind: Vague,
            ask: "How many times may the model call tools in one answer before the app stops it, and where is that limit set?",
            files: &["tools.rs", "App.tsx"],
            facts: &["10", "ten"],
        },
        Question {
            id: "q11-conversations",
            kind: Vague,
            ask: "Which file decides where chat history is saved on disk, and under which folder?",
            files: &["store.rs"],
            facts: &["APPDATA", "AppData"],
        },
        Question {
            id: "q12-capture",
            kind: Vague,
            ask: "How does Heimdall's popup stay out of screen shares and recordings? Name the file and the Windows call.",
            files: &["heimdall/src-tauri/src/lib.rs"],
            facts: &["WDA_EXCLUDEFROMCAPTURE", "SetWindowDisplayAffinity"],
        },
        Question {
            id: "q13-console",
            kind: Vague,
            ask: "When Ollama is started automatically, how is a console window kept from appearing? Name the file.",
            files: &["ollama.rs"],
            facts: &["CREATE_NO_WINDOW", "0x0800_0000", "0x08000000"],
        },
        Question {
            id: "q14-tool-list",
            kind: Vague,
            ask: "Which file decides which tools the model is offered, and what is the function called?",
            files: &["tools.rs"],
            facts: &["available"],
        },
        Question {
            id: "q15-output-cap",
            kind: Vague,
            ask: "When a check command prints a lot, how much of the start of its output is kept, and where is that set?",
            files: &["check.rs"],
            facts: &["6 * 1024", "6144", "6 KB", "6KB", "6 KiB"],
        },
        Question {
            id: "q16-impl",
            kind: NoTool,
            ask: "In one sentence, what does the Rust keyword impl do?",
            files: &[],
            facts: &[],
        },
        Question {
            id: "q17-haiku",
            kind: NoTool,
            ask: "Write a haiku about rain.",
            files: &[],
            facts: &[],
        },
        Question {
            id: "q18-sum",
            kind: NoTool,
            ask: "What is 17 times 23?",
            files: &[],
            facts: &["391"],
        },
    ]
}

/// `fact` in `text` as a whole word: not a digit inside a longer number, not
/// `contain` inside `container`. Case does not matter.
fn has_word(text: &str, fact: &str) -> bool {
    let (t, f) = (text.to_lowercase(), fact.to_lowercase());
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    t.match_indices(&f).any(|(at, _)| {
        let before = t[..at].chars().next_back();
        let after = t[at + f.len()..].chars().next();
        !word(before) && !word(after)
    })
}

/// Does the answer pass? `Err` says why not.
fn judge(q: &Question, answer: &str, calls: &[String]) -> Result<(), String> {
    if answer.trim().is_empty() {
        return Err("no answer".into());
    }
    if q.kind == Kind::NoTool {
        if !calls.is_empty() {
            return Err(format!("called {} for a question that needed no tool", calls.join(", ")));
        }
    } else {
        let said = answer.to_lowercase().replace('\\', "/");
        if !q.files.iter().any(|f| said.contains(&f.to_lowercase())) {
            return Err(format!("did not name {}", q.files.join(" or ")));
        }
    }
    if !q.facts.is_empty() && !q.facts.iter().any(|f| has_word(answer, f)) {
        return Err(format!("did not say {}", q.facts.join(" / ")));
    }
    Ok(())
}

// ── running ─────────────────────────────────────────────────────────────────

/// What the map may take of the system prompt, in characters: about 2k tokens
/// of the 32k context.
const MAP_BUDGET: usize = 7000;

#[derive(Clone, Copy, PartialEq)]
enum Variant {
    Search,
    ReadOnly,
    /// Search, plus a map of the code files with their first comment line in
    /// the system prompt. Only runs when asked for with `--variant map`.
    Map,
}

impl Variant {
    fn name(self) -> &'static str {
        match self {
            Variant::Search => "with search",
            Variant::ReadOnly => "read only",
            Variant::Map => "with map",
        }
    }

    fn tools(self) -> Vec<Tool> {
        let mut t = tools::available(true, None);
        t.retain(|t| FIND_TOOLS.contains(&t.function.name.as_str()));
        if self == Variant::ReadOnly {
            t.retain(|t| t.function.name != "list_files" && t.function.name != "search");
        }
        t
    }

    fn system(self, ws: &Workspace) -> String {
        let s = tools::system_prompt(ws, false);
        assert!(s.contains(FIND_SENTENCE), "the finding sentence changed; update FIND_SENTENCE");
        match self {
            Variant::Search => s,
            Variant::Map => format!(
                "{s}

A map of the project's code files, each with its first comment line:
{}",
                search::repo_map(ws, MAP_BUDGET, || false)
            ),
            Variant::ReadOnly => s.replace(FIND_SENTENCE, ""),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Verdict {
    Pass,
    Late,
    Fail,
    Error,
}

impl Verdict {
    fn label(self) -> &'static str {
        match self {
            Verdict::Pass => "PASS",
            Verdict::Late => "LATE",
            Verdict::Fail => "FAIL",
            Verdict::Error => "ERROR",
        }
    }
}

struct Outcome {
    verdict: Verdict,
    why: String,
    calls: Vec<String>,
    rounds: usize,
    asks: usize,
    asks_every_call: usize,
    secs: f64,
    log: Vec<String>,
}

fn run(
    model: &str,
    q: &Question,
    variant: Variant,
    opts: Options,
    ws: &Workspace,
    tree: &Worktree,
    backups: &Path,
) -> Outcome {
    let started = Instant::now();
    let mut messages = vec![Message::system(variant.system(ws)), Message::user(q.ask)];
    let setup = Setup { ws, backups, check: None, confine_reads: false, compact: true, commands: None, unattended: false };
    let r = probe::run_loop(model, &mut messages, &variant.tools(), opts, Transport::Recover, PROBE_ROUNDS, &setup);
    // Whatever happened, the next run must read the pinned tree. A reset that
    // fails means this run, and maybe the next, measured nothing.
    let reset = tree.reset();
    let (verdict, why) = if let Err(e) = reset {
        (Verdict::Error, format!("could not reset the worktree after the run: {e}"))
    } else if let Some(e) = r.error {
        (Verdict::Error, e)
    } else if r.hit_bound {
        (Verdict::Fail, format!("still calling tools after {PROBE_ROUNDS} rounds"))
    } else {
        match judge(q, &r.answer, &r.calls) {
            Ok(()) if r.rounds > tools::MAX_ROUNDS => (Verdict::Late, format!("right, but after {} rounds", r.rounds)),
            Ok(()) => (Verdict::Pass, String::new()),
            Err(e) => (Verdict::Fail, e),
        }
    };
    Outcome {
        verdict,
        why,
        calls: r.calls,
        rounds: r.rounds,
        asks: r.asks,
        asks_every_call: r.asks_every_call,
        secs: started.elapsed().as_secs_f64(),
        log: r.log,
    }
}

/// The pinned commit as a detached worktree, removed when dropped.
struct Worktree {
    path: PathBuf,
}

impl Worktree {
    fn add(at: &Path) -> Result<Self, String> {
        // A worktree left by a run that was killed would refuse the new one.
        let _ = Command::new("git").args(["worktree", "prune"]).status();
        let _ = std::fs::remove_dir_all(at);
        let out = Command::new("git")
            .args(["worktree", "add", "--detach"])
            .arg(at)
            .arg(PIN)
            .output()
            .map_err(|e| format!("could not run git: {e}"))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).into_owned());
        }
        Ok(Self { path: at.to_path_buf() })
    }

    /// Put the tree back exactly as the pinned commit has it: undo changes to
    /// tracked files, then remove anything untracked.
    fn reset(&self) -> Result<(), String> {
        for args in [&["checkout", "--", "."][..], &["clean", "-fdq"][..]] {
            let out = Command::new("git")
                .arg("-C")
                .arg(&self.path)
                .args(args)
                .output()
                .map_err(|e| format!("could not run git: {e}"))?;
            if !out.status.success() {
                return Err(format!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
            }
        }
        Ok(())
    }
}

impl Drop for Worktree {
    fn drop(&mut self) {
        let _ = Command::new("git").args(["worktree", "remove", "--force"]).arg(&self.path).status();
        let _ = Command::new("git").args(["worktree", "prune"]).status();
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
    let variants: Vec<Variant> = match arg(&args, "--variant").as_deref() {
        Some("search") => vec![Variant::Search],
        Some("read") => vec![Variant::ReadOnly],
        Some("map") => vec![Variant::Map],
        _ => vec![Variant::Search, Variant::ReadOnly],
    };
    let questions: Vec<Question> =
        questions().into_iter().filter(|q| only.as_deref().is_none_or(|o| q.id.starts_with(o))).collect();

    let scratch = std::env::temp_dir().join(format!("bifrost-findprobe-{}", std::process::id()));
    // Started if it is not running, as the apps and the CLI do, and before the
    // worktree exists: a failure here then has nothing to clean up.
    if let Err(e) = ollama::ensure_running() {
        eprintln!("could not start Ollama: {e}");
        std::process::exit(1);
    }
    let tree = match Worktree::add(&scratch.join("bifrost")) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("could not check out {PIN}: {e}");
            std::process::exit(1);
        }
    };
    let ws = Workspace::open(&tree.path).expect("the worktree");
    let backups = scratch.join("backups");

    println!("H2 — find probe");
    println!("model:     {model}");
    println!("ctx:       {}", cfg.num_ctx);
    println!("codebase:  bifrost at {PIN}, {} questions", questions.len());
    println!("runs:      {runs} at the app's own settings, plus 1 at temperature 0");
    println!("bound:     {PROBE_ROUNDS} rounds (the app stops at {})", tools::MAX_ROUNDS);
    if variants.contains(&Variant::Map) {
        let map = search::repo_map(&ws, MAP_BUDGET, || false);
        println!("map:       {} chars, {} lines", map.chars().count(), map.lines().count());
    }
    println!();

    match ollama::load(&model, cfg.num_ctx) {
        Ok(d) => println!("warmed in {:.1}s\n", d.as_secs_f64()),
        Err(e) => {
            eprintln!("could not load {model}: {e}");
            // `exit` runs no destructors, so the worktree would be left behind
            // (R-38): let it go first.
            drop(ws);
            drop(tree);
            let _ = std::fs::remove_dir_all(&scratch);
            std::process::exit(1);
        }
    }

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let settings: Vec<(&str, Options, usize)> =
        vec![("app", Options::new(cfg.num_ctx), runs), ("t=0", Options::measured(cfg.num_ctx), 1)];
    let kinds = [Kind::Named, Kind::Vague, Kind::NoTool];

    let mut rows = Vec::new();
    let mut json = Vec::new();

    for &variant in &variants {
        for (label, opts, n) in &settings {
            // Per kind: passes within the app's bound, and runs.
            let mut tally = [(0usize, 0usize); 3];
            let (mut late, mut errors, mut calls, mut secs) = (0, 0, 0, 0.0);
            let (mut asks, mut asks_every_call) = (0usize, 0usize);
            println!("── {} · {} ──", variant.name(), label);
            for q in &questions {
                let k = kinds.iter().position(|&k| k == q.kind).unwrap();
                let mut line = String::new();
                for i in 0..*n {
                    let o = run(&model, q, variant, *opts, &ws, &tree, &backups);
                    tally[k].1 += 1;
                    match o.verdict {
                        Verdict::Pass => tally[k].0 += 1,
                        Verdict::Late => late += 1,
                        Verdict::Error => errors += 1,
                        Verdict::Fail => {}
                    }
                    calls += o.calls.len();
                    asks += o.asks;
                    asks_every_call += o.asks_every_call;
                    secs += o.secs;
                    line.push_str(&format!("{:<5} ", o.verdict.label()));
                    if o.verdict != Verdict::Pass {
                        println!("    {} run {}: {} — {}", q.id, i + 1, o.verdict.label(), o.why);
                        for l in &o.log {
                            println!("      {l}");
                        }
                    }
                    json.push(serde_json::json!({
                        "variant": variant.name(), "setting": label, "question": q.id, "kind": q.kind.name(),
                        "run": i + 1, "verdict": o.verdict.label(), "why": o.why, "calls": o.calls,
                        "rounds": o.rounds, "asks": o.asks, "asksEveryCall": o.asks_every_call, "log": o.log, "secs": (o.secs * 10.0).round() / 10.0,
                    }));
                }
                println!("  {:<20} {line}", q.id);
            }
            let (pass, total): (usize, usize) = tally.iter().fold((0, 0), |(p, t), (a, b)| (p + a, t + b));
            let per = |i: usize| format!("{}/{}", tally[i].0, tally[i].1);
            println!(
                "  → {pass}/{total} pass within the app's {} rounds · named {} · vague {} · no tool {} · {late} late · \
                 {errors} error · {:.1} calls/run · {:.1} asks/run ({:.1} asking every call) · {:.1}s avg\n",
                tools::MAX_ROUNDS,
                per(0),
                per(1),
                per(2),
                calls as f64 / total.max(1) as f64,
                asks as f64 / total.max(1) as f64,
                asks_every_call as f64 / total.max(1) as f64,
                secs / total.max(1) as f64,
            );
            rows.push(format!(
                "| {} | {} | {pass}/{total} | {} | {} | {} | {late} | {errors} | {:.1} | {:.1} | {:.1}s |",
                variant.name(),
                label,
                per(0),
                per(1),
                per(2),
                calls as f64 / total.max(1) as f64,
                asks as f64 / total.max(1) as f64,
                secs / total.max(1) as f64,
            ));
        }
    }

    println!("─── {model} ───");
    println!("| variant | setting | pass | named | vague | no tool | late | error | calls/run | asks/run | time/run |");
    println!("|---|---|---|---|---|---|---|---|---|---|---|");
    for r in &rows {
        println!("{r}");
    }

    drop(tree);
    let _ = std::fs::remove_dir_all(&scratch);
    let out = PathBuf::from("evals/out").join(format!("H2-find-{stamp}"));
    if std::fs::create_dir_all(&out).is_ok() {
        let _ = std::fs::write(
            out.join("results.json"),
            serde_json::to_string_pretty(&serde_json::json!({ "model": model, "pin": PIN, "runs": json })).unwrap(),
        );
        println!("\nsaved {}", out.join("results.json").display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// R-09: no variant offers a tool that changes the shared worktree.
    #[test]
    fn no_variant_offers_a_tool_that_changes_files() {
        for v in [Variant::Search, Variant::ReadOnly, Variant::Map] {
            let names: Vec<String> = v.tools().into_iter().map(|t| t.function.name).collect();
            assert!(names.iter().all(|n| FIND_TOOLS.contains(&n.as_str())), "{names:?}");
            assert!(names.contains(&"read_file".to_string()));
        }
    }

    #[test]
    fn a_fact_counts_only_as_a_whole_word() {
        assert!(has_word("MAX_IDENTICAL_FAILURES is 3.", "3"));
        assert!(!has_word("see line 304", "3"));
        assert!(has_word("the contain method", "contain"));
        assert!(!has_word("a container", "contain"));
        assert!(has_word("FNV-1a, 64-bit", "FNV"));
    }
}
