//! H1 — does a check command help the model catch its own mistakes?
//!
//!   cargo run -p bench --bin fixprobe
//!   cargo run -p bench --bin fixprobe -- --runs 1 --only f03 --variant with
//!
//! **What G7 could not see.** An edit can apply byte-exact and still break the
//! build: rename a function and forget its call site, and every edit "worked".
//! Nothing told the model. H1 gives it `run_check` — this measures whether it
//! uses it, and whether that turns broken builds into working ones.
//!
//! Eight tasks, each written so that **the obvious edit breaks the build**: a
//! rename whose call site is in another file, a type changed under a function
//! that returns it, a constant removed while something still uses it. One
//! project starts broken and the task is only "fix it".
//!
//! Each task runs twice over — **with** `run_check` offered, and **without** —
//! at the app's own sampling and once at temperature 0, the edit probe's scheme.
//! A run passes when the final `cargo check` passes **and** the change asked for
//! is there. Doing nothing never passes; neither does "fixing" the build by
//! reverting the task.
//!
//! **Offline by construction.** The project has no dependencies and its own
//! `[workspace]`, and every cargo call runs with `CARGO_NET_OFFLINE=true` and one
//! shared target directory, warmed before the first task — a fresh one would
//! rebuild from nothing each run, and on this machine a fresh build is also what
//! Smart App Control likes to refuse (PLAN.md §3). A run whose check was refused
//! by it is an ERROR — it measured Windows, not the model.

#[path = "../probe.rs"]
mod probe;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use odin_core::chat::{Message, Options};
use odin_core::edit::Workspace;
use odin_core::{check, config, ollama, tools};

use probe::{Check, Setup, Transport};

type Files = BTreeMap<String, String>;

/// Past the app's own bound, so a task that needs eleven rounds is seen as
/// that rather than as a failure. Whether it would have fitted the app's ten is
/// reported separately.
const PROBE_ROUNDS: usize = 20;

// ── the project ─────────────────────────────────────────────────────────────

const CARGO_TOML: &str = "[package]
name = \"demo\"
version = \"0.1.0\"
edition = \"2021\"

[workspace]
";

const CONFIG_RS: &str = "pub const PORT: u16 = 8080;
pub const HOST: &str = \"127.0.0.1\";

pub fn address() -> String {
    format!(\"{HOST}:{PORT}\")
}

pub fn port() -> u16 {
    PORT
}
";

const MAIN_RS: &str = "mod config;

fn greet(name: &str) -> String {
    format!(\"Hello, {name}!\")
}

fn main() {
    println!(\"{}\", greet(\"world\"));
    println!(\"listening on {}\", config::address());
    println!(\"port {}\", config::port());
}
";

/// The same `main.rs`, broken the way a hurried edit breaks it.
const MAIN_RS_BROKEN: &str = "mod config;

fn greet(name: &str) -> String {
    format!(\"Hello, {name}!\")
}

fn main() {
    let name = \"world\"
    println!(\"{}\", greet(name));
    println!(\"listening on {}\", config::address());
    println!(\"port {}\", config::port());
}
";

fn project(main_rs: &str) -> Files {
    [("Cargo.toml", CARGO_TOML), ("src/config.rs", CONFIG_RS), ("src/main.rs", main_rs)]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn crlf(s: &str) -> Vec<u8> {
    s.replace('\n', "\r\n").into_bytes()
}

// ── the tasks ───────────────────────────────────────────────────────────────

struct Task {
    id: &'static str,
    prompt: &'static str,
    start: &'static str,
    /// The change asked for is there. Checked after the build passes.
    done: fn(&Files) -> Result<(), String>,
}

fn file<'a>(f: &'a Files, name: &str) -> Result<&'a str, String> {
    f.get(name).map(String::as_str).ok_or_else(|| format!("{name} is gone"))
}

fn tasks() -> Vec<Task> {
    vec![
        Task {
            id: "f01-rename",
            prompt: "In src/main.rs, rename the function greet to welcome.",
            start: MAIN_RS,
            done: |f| {
                let m = file(f, "src/main.rs")?;
                if !m.contains("fn welcome(") {
                    return Err("there is no fn welcome".into());
                }
                if m.contains("greet(") {
                    return Err("greet is still there".into());
                }
                Ok(())
            },
        },
        Task {
            id: "f02-parameter",
            prompt: "In src/main.rs, give greet a second parameter, excited: bool. When it is true \
                     the greeting ends with \"!\", otherwise with \".\".",
            start: MAIN_RS,
            done: |f| {
                let m = file(f, "src/main.rs")?;
                if !m.contains("excited: bool") {
                    return Err("greet has no excited: bool parameter".into());
                }
                Ok(())
            },
        },
        Task {
            id: "f03-type",
            prompt: "In src/config.rs, change the type of PORT from u16 to u32.",
            start: MAIN_RS,
            done: |f| {
                let c = file(f, "src/config.rs")?;
                if !c.contains("PORT: u32") {
                    return Err("PORT is not a u32".into());
                }
                Ok(())
            },
        },
        Task {
            id: "f04-across-files",
            prompt: "In src/config.rs, rename the function address to bind_address.",
            start: MAIN_RS,
            done: |f| {
                let c = file(f, "src/config.rs")?;
                let m = file(f, "src/main.rs")?;
                if !c.contains("fn bind_address(") {
                    return Err("there is no fn bind_address".into());
                }
                if c.contains("fn address(") || m.contains("::address(") {
                    return Err("address is still defined or called".into());
                }
                Ok(())
            },
        },
        Task {
            id: "f05-broken",
            prompt: "The project does not build. Find out why and fix it.",
            start: MAIN_RS_BROKEN,
            done: |f| {
                let m = file(f, "src/main.rs")?;
                if !m.contains("greet(name)") {
                    return Err("the fix changed what main does".into());
                }
                Ok(())
            },
        },
        Task {
            id: "f06-remove-const",
            prompt: "Remove the HOST constant from src/config.rs. address() should use \"0.0.0.0\" \
                     directly instead.",
            start: MAIN_RS,
            done: |f| {
                let c = file(f, "src/config.rs")?;
                if c.contains("HOST") {
                    return Err("HOST is still there".into());
                }
                if !c.contains("0.0.0.0") {
                    return Err("address does not use 0.0.0.0".into());
                }
                Ok(())
            },
        },
        Task {
            id: "f07-return-type",
            prompt: "In src/config.rs, make port() return a String instead of a u16.",
            start: MAIN_RS,
            done: |f| {
                let c = file(f, "src/config.rs")?;
                if !c.contains("fn port() -> String") {
                    return Err("port() does not return a String".into());
                }
                Ok(())
            },
        },
        Task {
            id: "f08-new-fn",
            prompt: "In src/main.rs, add a function shout(name: &str) -> String that returns \
                     greet(name) in upper case, and call shout instead of greet in main.",
            start: MAIN_RS,
            done: |f| {
                let m = file(f, "src/main.rs")?;
                if !m.contains("fn shout(name: &str) -> String") {
                    return Err("there is no fn shout(name: &str) -> String".into());
                }
                let main = m.split("fn main()").nth(1).ok_or("fn main is gone")?;
                if !main.contains("shout(") {
                    return Err("main does not call shout".into());
                }
                Ok(())
            },
        },
    ]
}

// ── running one task ────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum Variant {
    With,
    Without,
}

impl Variant {
    fn name(self) -> &'static str {
        match self {
            Variant::With => "with check",
            Variant::Without => "without",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Pass,
    Fail,
    Error,
}

impl Verdict {
    fn label(self) -> &'static str {
        match self {
            Verdict::Pass => "PASS",
            Verdict::Fail => "FAIL",
            Verdict::Error => "ERROR",
        }
    }
}

struct Outcome {
    verdict: Verdict,
    why: String,
    rounds: usize,
    /// Would the app's own bound have stopped it?
    over_app_bound: bool,
    checks: usize,
    secs: f64,
    log: Vec<String>,
}

/// Every text file under `dir`, `\n` endings, keyed by forward-slash path.
/// `target/` is the build's, not the model's.
fn tree(dir: &Path) -> Files {
    let mut out = Files::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                if p.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                stack.push(p);
            } else if let Ok(t) = std::fs::read_to_string(&p) {
                let rel = p.strip_prefix(dir).unwrap().to_string_lossy().replace('\\', "/");
                out.insert(rel, t.replace("\r\n", "\n"));
            }
        }
    }
    out
}

fn write_project(dir: &Path, files: &Files) {
    for (rel, text) in files {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, crlf(text)).unwrap();
    }
}

fn run(model: &str, task: &Task, variant: Variant, opts: Options, scratch: &Path, cargo: &Check) -> Outcome {
    let started = Instant::now();
    let dir = scratch.join("project");
    let backups = scratch.join("backups");
    let _ = std::fs::remove_dir_all(scratch);
    write_project(&dir, &project(task.start));
    let ws = Workspace::open(&dir).expect("fixture folder");

    let with = variant == Variant::With;
    let offered = tools::available(true, with.then(|| check::label(&cargo.argv)).as_deref());
    let mut messages = vec![Message::system(tools::system_prompt(&ws, with)), Message::user(task.prompt)];
    let setup = Setup { ws: &ws, backups: &backups, check: with.then_some(cargo), confine_reads: false, compact: true };
    let r = probe::run_loop(model, &mut messages, &offered, opts, Transport::Recover, PROBE_ROUNDS, &setup);

    let mut out = Outcome {
        verdict: Verdict::Fail,
        why: String::new(),
        rounds: r.rounds,
        // The app asks at most MAX_ROUNDS times; an answer on a later ask is one
        // it would never have seen.
        over_app_bound: r.rounds > tools::MAX_ROUNDS,
        checks: r.checks.len(),
        secs: 0.0,
        log: r.log,
    };
    if let Some(e) = r.error {
        out.verdict = Verdict::Error;
        out.why = e;
    } else if r.blocked {
        out.verdict = Verdict::Error;
        out.why = "Smart App Control blocked the model's check".into();
    } else {
        // The judge's own check, whatever the model did or did not run.
        match check::run(ws.root(), &cargo.argv, check::CHECK_TIMEOUT, &cargo.env, || false) {
            Err(e) => {
                out.verdict = Verdict::Error;
                out.why = e;
            }
            Ok(o) if o.blocked.is_some() => {
                out.verdict = Verdict::Error;
                out.why = "Smart App Control blocked the final check".into();
            }
            Ok(o) if probe::never_finished(&o.ended) => {
                // R-10: a check that timed out or was stopped says nothing about the code.
                out.verdict = Verdict::Error;
                out.why = format!("the final check did not finish ({:?}) — environment", o.ended);
            }
            Ok(o) if !o.passed() => {
                let first = o.output.lines().find(|l| l.starts_with("error")).unwrap_or("").to_string();
                out.why = format!("does not build: {first}");
            }
            Ok(_) => match (task.done)(&tree(&dir)) {
                Ok(()) if r.hit_bound => out.why = format!("still calling tools after {PROBE_ROUNDS} rounds"),
                Ok(()) => out.verdict = Verdict::Pass,
                Err(e) => out.why = format!("builds, but {e}"),
            },
        }
    }
    // R-10: a run that would fail after its own `run_check` timed out is the environment's.
    if out.verdict == Verdict::Fail && r.check_timeouts > 0 {
        out.verdict = Verdict::Error;
        out.why = format!("the model's check timed out — environment ({})", out.why);
    }
    if r.stopped && out.verdict != Verdict::Pass {
        out.why = format!("stopped after repeating a failing call — {}", out.why);
    }
    out.secs = started.elapsed().as_secs_f64();
    out
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
        Some("with") => vec![Variant::With],
        Some("without") => vec![Variant::Without],
        _ => vec![Variant::With, Variant::Without],
    };
    let tasks: Vec<Task> =
        tasks().into_iter().filter(|t| only.as_deref().is_none_or(|o| t.id.starts_with(o))).collect();

    let scratch_root = std::env::temp_dir().join(format!("odin-fixprobe-{}", std::process::id()));
    let cargo = Check {
        argv: vec!["cargo".into(), "check".into()],
        env: vec![
            ("CARGO_TARGET_DIR".into(), scratch_root.join("target").to_string_lossy().into_owned()),
            ("CARGO_NET_OFFLINE".into(), "true".into()),
        ],
    };

    println!("H1 — fix probe");
    println!("model:  {model}");
    println!("ctx:    {}", cfg.num_ctx);
    println!("tasks:  {} · each one's obvious edit breaks the build", tasks.len());
    println!("runs:   {runs} at the app's own settings, plus 1 at temperature 0");
    println!("bound:  {PROBE_ROUNDS} rounds (the app stops at {})\n", tools::MAX_ROUNDS);

    // The shared target directory, built once — and the place to find out now,
    // not forty runs in, that this machine will not build the fixture at all.
    let warm = scratch_root.join("warm");
    write_project(&warm, &project(MAIN_RS));
    match check::run(&warm, &cargo.argv, check::CHECK_TIMEOUT, &cargo.env, || false) {
        Ok(o) if o.passed() => println!("fixture builds ({:.1}s)", o.elapsed.as_secs_f64()),
        Ok(o) => {
            eprintln!("the untouched fixture does not build:\n{}", o.output);
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("could not run cargo: {e}");
            std::process::exit(1);
        }
    }

    match ollama::load(&model, cfg.num_ctx) {
        Ok(d) => println!("warmed in {:.1}s\n", d.as_secs_f64()),
        Err(e) => {
            eprintln!("could not load {model}: {e}");
            std::process::exit(1);
        }
    }

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let settings: Vec<(&str, Options, usize)> =
        vec![("app", Options::new(cfg.num_ctx), runs), ("t=0", Options::measured(cfg.num_ctx), 1)];

    let mut rows = Vec::new();
    let mut json = Vec::new();

    for &variant in &variants {
        for (label, opts, n) in &settings {
            // `pass` is what the app would have shown: a pass that needed more
            // rounds than the app allows is counted apart, as `late`, because
            // the check itself costs rounds and would otherwise flatter it.
            let (mut pass, mut late, mut fail, mut error) = (0, 0, 0, 0);
            let (mut secs, mut checks, mut over) = (0.0, 0, 0);
            println!("── {} · {} ──", variant.name(), label);
            for task in &tasks {
                let mut line = String::new();
                for i in 0..*n {
                    let scratch = scratch_root.join(format!("{}-{}-{i}", label.replace('=', ""), task.id));
                    let o = run(&model, task, variant, *opts, &scratch, &cargo);
                    let _ = std::fs::remove_dir_all(&scratch);
                    match o.verdict {
                        Verdict::Pass if o.over_app_bound => late += 1,
                        Verdict::Pass => pass += 1,
                        Verdict::Fail => fail += 1,
                        Verdict::Error => error += 1,
                    }
                    secs += o.secs;
                    checks += o.checks;
                    over += o.over_app_bound as usize;
                    let shown = if o.verdict == Verdict::Pass && o.over_app_bound { "LATE" } else { o.verdict.label() };
                    line.push_str(&format!("{shown:<5} "));
                    if o.verdict != Verdict::Pass {
                        println!("    {} run {}: {} — {}", task.id, i + 1, o.verdict.label(), o.why);
                        for l in &o.log {
                            println!("      {l}");
                        }
                    }
                    json.push(serde_json::json!({
                        "variant": variant.name(), "setting": label, "task": task.id, "run": i + 1,
                        "verdict": o.verdict.label(), "why": o.why, "rounds": o.rounds,
                        "overAppBound": o.over_app_bound, "checks": o.checks, "log": o.log,
                        "secs": (o.secs * 10.0).round() / 10.0,
                    }));
                }
                println!("  {:<18} {line}", task.id);
            }
            let total = pass + late + fail + error;
            let pct = 100.0 * pass as f64 / total.max(1) as f64;
            println!(
                "  → {pass}/{total} pass within the app's {} rounds ({pct:.0}%) · {late} only later · {fail} fail · \
                 {error} error · {checks} checks run · {over} over the app's bound · {:.1}s avg\n",
                tools::MAX_ROUNDS,
                secs / total.max(1) as f64
            );
            rows.push(format!(
                "| {} | {} | {pass}/{total} ({pct:.0}%) | {late} | {fail} | {error} | {:.1} | {over} | {:.1}s |",
                variant.name(),
                label,
                checks as f64 / total.max(1) as f64,
                secs / total.max(1) as f64,
            ));
        }
    }

    println!("─── {model} ───");
    println!(
        "| variant | setting | pass (≤{} rounds) | pass later | fail | error | checks/run | over app bound | time/run |",
        tools::MAX_ROUNDS
    );
    println!("|---|---|---|---|---|---|---|---|---|");
    for r in &rows {
        println!("{r}");
    }

    let out = PathBuf::from("evals/out").join(format!("H1-fix-{stamp}"));
    if std::fs::create_dir_all(&out).is_ok() {
        let _ = std::fs::write(
            out.join("results.json"),
            serde_json::to_string_pretty(&serde_json::json!({ "model": model, "runs": json })).unwrap(),
        );
        println!("\nsaved {}", out.join("results.json").display());
    }
    let _ = std::fs::remove_dir_all(&scratch_root);
}
