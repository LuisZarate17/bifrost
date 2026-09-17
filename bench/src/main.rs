//! Measure a model against real screenshots — the Rust port of `evals/bench.mjs`.
//!
//! F1 exists because a model choice goes stale. New VLMs ship constantly, and the
//! public benchmarks that informed A2 measure scanned documents, not
//! syntax-highlighted code on a dark theme. Nobody benchmarks this use case
//! except us, so swapping a model has to be a ten-minute job rather than a vibes
//! decision.
//!
//! **The prompt is a dimension, not a constant.** It does all the intent-guessing
//! in this app, so it is tested exactly like a model is. A prompt tweak that
//! fixes today's screenshot routinely breaks one fixed last week, and the only
//! way to notice is a fixed set, re-run.
//!
//! ## The correctness rule this harness exists to respect
//!
//! **Ollama caches prefill across calls to the same loaded model whenever the
//! token prefix matches, and it does so per image.** Alternating between two eval
//! images does *not* defeat it: once both have been seen, prefill for a repeat
//! collapses from ~450 ms to ~5 ms and the measurement is worthless.
//!
//! The only thing that reliably clears it is an explicit unload. So every
//! measured run here unloads first, and the timings are honest cold numbers.
//! This was established empirically in `bench.mjs`; it is not a guess.

use std::io::Write;
use std::time::Duration;

use base64::prelude::*;
use warden_core::{ollama, vram};

mod report;

const NUM_CTX: u32 = 8192;
const GENERATE_TIMEOUT: Duration = Duration::from_secs(300);

const USAGE: &str = "\
bench — measure a model against real screenshots

  bench --models TAG[,TAG...] --images PATH[,PATH...] [--prompts FILE[,FILE...]]
        [--reps N] [--out DIR]

Every measured run unloads the model first. Ollama caches prefill per image, and
an unload is the only thing that reliably clears it — without this, a second run
against the same image is far faster and means nothing.
";

struct Args {
    models: Vec<String>,
    images: Vec<String>,
    prompt_files: Vec<String>,
    reps: usize,
    out: String,
}

fn split_list(v: &str) -> Vec<String> {
    v.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut a = Args {
        models: vec![],
        images: vec![],
        prompt_files: vec![],
        reps: 1,
        // Already gitignored: these hold answers about real desktop
        // screenshots and stay on this machine.
        out: "evals/out".to_string(),
    };

    let mut i = 0;
    while i < args.len() {
        let value = |i: usize, what: &str| -> Result<String, String> {
            args.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("{what} needs a value"))
        };
        match args[i].as_str() {
            "--models" => {
                a.models = split_list(&value(i, "--models")?);
                i += 1;
            }
            "--images" => {
                a.images = split_list(&value(i, "--images")?);
                i += 1;
            }
            "--prompts" => {
                a.prompt_files = split_list(&value(i, "--prompts")?);
                i += 1;
            }
            "--out" => {
                a.out = value(i, "--out")?;
                i += 1;
            }
            "--reps" => {
                a.reps = value(i, "--reps")?
                    .parse()
                    .map_err(|_| "--reps must be a number".to_string())?;
                i += 1;
            }
            "--selftest-shuffle" => return Err("selftest".into()),
            "-h" | "--help" | "help" => return Err("help".into()),
            // Unknown tokens error rather than being ignored. C4a learned this
            // the expensive way: a silently-swallowed argument means the run you
            // thought you configured is not the run that happened.
            other => return Err(format!("unknown argument: {other}")),
        }
        i += 1;
    }

    if a.models.is_empty() {
        return Err("--models is required".into());
    }
    if a.images.is_empty() {
        return Err("--images is required".into());
    }
    if a.reps == 0 {
        return Err("--reps must be at least 1".into());
    }
    Ok(a)
}

struct Row {
    model: String,
    image: String,
    prompt: String,
    ttft_ms: f64,
    prefill_ms: f64,
    total_s: f64,
    decode_tps: f64,
    chars: usize,
    resident_mib: Option<u64>,
    load_ns: u64,
}

fn median(mut v: Vec<f64>) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v[v.len() / 2]
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n - 1).chain("…".chars()).collect()
    }
}

fn main() -> std::process::ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse(&argv) {
        Ok(a) => a,
        Err(e) if e == "help" => {
            print!("{USAGE}");
            return std::process::ExitCode::SUCCESS;
        }
        Err(e) if e == "selftest" => return selftest_shuffle(),
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            return std::process::ExitCode::from(2);
        }
    };

    match run(&args) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(args: &Args) -> Result<(), String> {
    // Prompts are a dimension. With none given the run still has exactly one —
    // whatever the app ships — so the shape of the matrix never changes.
    // name, text, sha256
    let prompts: Vec<(String, String, String)> = if args.prompt_files.is_empty() {
        vec![(
            "(built-in)".to_string(),
            ollama::DEFAULT_PROMPT.to_string(),
            report::sha256_hex(ollama::DEFAULT_PROMPT.as_bytes()),
        )]
    } else {
        args.prompt_files
            .iter()
            .map(|p| {
                let text = std::fs::read_to_string(p).map_err(|e| format!("{p}: {e}"))?;
                // Trimmed, and every prompt equally. One trailing newline flipped
                // a case-02 verdict at C1b — the fix is a rule applied uniformly,
                // not a special case.
                let name = std::path::Path::new(p)
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| p.clone());
                let trimmed = text.trim().to_string();
                let sha = report::sha256_hex(trimmed.as_bytes());
                Ok((name, trimmed, sha))
            })
            .collect::<Result<Vec<_>, String>>()?
    };

    // name, base64, path, bytes, sha256
    let images: Vec<(String, String, String, usize, String)> = args
        .images
        .iter()
        .map(|p| {
            let bytes = std::fs::read(p).map_err(|e| format!("{p}: {e}"))?;
            let name = std::path::Path::new(p)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| p.clone());
            let sha = report::sha256_hex(&bytes);
            Ok((name, BASE64_STANDARD.encode(&bytes), p.clone(), bytes.len(), sha))
        })
        .collect::<Result<Vec<_>, String>>()?;

    let card = vram::Card::open();
    match &card {
        Ok(c) => println!("card: {}", c.name),
        Err(e) => println!("card: unavailable ({e}) — the VRAM column will be blank"),
    }
    println!(
        "matrix: {} model(s) x {} image(s) x {} prompt(s) x {} rep(s)\n",
        args.models.len(),
        images.len(),
        prompts.len(),
        args.reps
    );

    let dir = report::stamp_dir(&args.out);
    let mut rows: Vec<Row> = vec![];
    // Every rep, kept whole. The terminal only ever sees the medians.
    let mut runs: Vec<report::Run> = vec![];
    let mut failures: Vec<String> = vec![];

    for model in &args.models {
        for (image_name, image_b64, _, _, _) in &images {
            for (prompt_name, prompt_text, _) in &prompts {
                let mut ttfts = vec![];
                let mut prefills = vec![];
                let mut totals = vec![];
                let mut tps = vec![];
                let mut chars = 0usize;
                let mut resident = None;
                let mut load_ns = 0u64;

                for rep in 1..=args.reps {
                    // Unload first, every time. This is the whole reason the
                    // harness is shaped this way — see the module comment.
                    ollama::unload(model)?;
                    let baseline = card.as_ref().ok().and_then(|c| c.sample().ok());

                    print!(
                        "  {model} · {image_name} · {prompt_name} · rep {rep}/{} … ",
                        args.reps
                    );
                    let _ = std::io::stdout().flush();

                    let attempt = ollama::generate_measured(
                        model,
                        prompt_text,
                        Some(image_b64),
                        NUM_CTX,
                        GENERATE_TIMEOUT,
                    );

                    // One bad cell must not destroy a matrix that has been
                    // running for an hour, and must not be written down as if
                    // it were a measurement either.
                    let m = match attempt {
                        Ok(m) => m,
                        Err(e) => {
                            println!("FAILED — {e}");
                            failures.push(format!("{model} · {image_name} · {prompt_name} · rep {rep}: {e}"));
                            continue;
                        }
                    };

                    let loaded = card.as_ref().ok().and_then(|c| c.sample().ok());
                    if let (Some(b), Some(l)) = (baseline, loaded) {
                        resident = Some(l.used_mib.saturating_sub(b.used_mib));
                    }

                    runs.push(report::Run {
                        model: model.clone(),
                        image: image_name.clone(),
                        prompt: prompt_name.clone(),
                        rep,
                        measured: m.clone(),
                        baseline_mib: baseline.map(|s| s.used_mib),
                        loaded_mib: loaded.map(|s| s.used_mib),
                    });

                    println!(
                        "{:.0}ms to first, {:.1}s total, {} chars",
                        m.ttft.as_secs_f64() * 1000.0,
                        m.total.as_secs_f64(),
                        m.answer.len()
                    );

                    ttfts.push(m.ttft.as_secs_f64() * 1000.0);
                    // TTFT with the cold load taken back out. Both numbers are
                    // real and answer different questions: TTFT is what the press
                    // feels like, prefill is what the model costs once resident —
                    // and prefill is the one comparable with A2, whose harness
                    // paid the load outside the measurement by warming up on a
                    // synthetic image first.
                    prefills
                        .push((m.ttft.as_secs_f64() - m.load_ns as f64 / 1e9).max(0.0) * 1000.0);
                    totals.push(m.total.as_secs_f64());
                    tps.push(m.decode_tps());
                    chars = m.answer.len();
                    load_ns = m.load_ns;
                }

                rows.push(Row {
                    model: model.clone(),
                    image: image_name.clone(),
                    prompt: prompt_name.clone(),
                    ttft_ms: median(ttfts),
                    prefill_ms: median(prefills),
                    total_s: median(totals),
                    decode_tps: median(tps),
                    chars,
                    resident_mib: resident,
                    load_ns,
                });
            }
        }
        // Leave the card as we found it.
        ollama::unload(model)?;
    }

    let rule = "-".repeat(114);
    let mut table = String::new();
    use std::fmt::Write as _;
    let _ = writeln!(table, "{rule}");
    let _ = writeln!(
        table,
        "{:<30} {:<22} {:<12} {:>9} {:>9} {:>8} {:>9} {:>9} {:>8}",
        "MODEL", "IMAGE", "PROMPT", "TTFT", "PREFILL", "TOTAL", "DECODE", "VRAM", "CHARS"
    );
    let _ = writeln!(table, "{rule}");
    for r in &rows {
        let _ = writeln!(
            table,
            "{:<30} {:<22} {:<12} {:>7.0}ms {:>7.0}ms {:>7.1}s {:>6.0}t/s {:>6}MiB {:>8}",
            truncate(&r.model, 30),
            truncate(&r.image, 22),
            truncate(&r.prompt, 12),
            r.ttft_ms,
            r.prefill_ms,
            r.total_s,
            r.decode_tps,
            r.resident_mib
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-".into()),
            r.chars,
        );
    }
    let _ = writeln!(table, "{rule}");
    print!("\n{table}");

    // A cold load inside a measured run makes its timings incomparable with a
    // warm one. Say so, rather than letting the number quietly mislead.
    for r in &rows {
        if r.load_ns > 0 {
            println!(
                "note: {} · {} paid a {:.1}s cold load inside the measured run — that is the gap between TTFT and PREFILL, and why only PREFILL compares with A2",
                r.model,
                r.image,
                r.load_ns as f64 / 1e9
            );
        }
    }

    // Everything the terminal cannot hold: the answers themselves, every rep's
    // numbers, and enough of the inputs to reproduce the run.
    let manifest = report::Manifest {
        started: format!("{:?}", std::time::SystemTime::now()),
        card: card
            .as_ref()
            .map(|c| c.name.clone())
            .unwrap_or_else(|_| "unavailable".into()),
        num_ctx: NUM_CTX,
        reps: args.reps,
        models: args.models.clone(),
        images: images
            .iter()
            .map(|(n, _, path, bytes, sha)| (n.clone(), path.clone(), *bytes, sha.clone()))
            .collect(),
        prompts: prompts
            .iter()
            .map(|(n, text, sha)| (n.clone(), sha.clone(), text.clone()))
            .collect(),
    };
    report::write_manifest(&dir, &manifest)?;
    for r in &runs {
        report::write_run(&dir, r)?;
    }
    let image_names: Vec<String> = images.iter().map(|(n, _, _, _, _)| n.clone()).collect();
    let key = report::write_answers(&dir, &runs, &image_names)?;
    report::write_summary(&dir, &table)?;

    println!(
        "\n{} row(s), {} run(s) saved to {}",
        rows.len(),
        runs.len(),
        dir.display()
    );
    println!("  answers.md      blinded, {} image(s) to grade", key.len());
    println!("  blind-key.json  open this after grading, not before");

    // Said last and said plainly. A benchmark that hides its failures is a
    // benchmark you will quote without knowing what is missing from it.
    if !failures.is_empty() {
        println!("\n{} run(s) produced no usable numbers and were NOT recorded:", failures.len());
        for f in &failures {
            println!("  {f}");
        }
    }
    Ok(())
}

/// Check the blind shuffle actually shuffles, without spending a minute of GPU
/// per sample.
///
/// This exists because the first implementation **did not shuffle** — three
/// consecutive real runs produced the identical permutation, and nothing about
/// the output said so. A shuffle that quietly returns input order is worse than
/// none, because `answers.md` still claims to be blind while the grader reads
/// the models in the order they listed them.
fn selftest_shuffle() -> std::process::ExitCode {
    const N: usize = 2;
    const SAMPLES: usize = 2000;

    let mut first_is_zero = 0usize;
    for i in 0..SAMPLES {
        // Distinct seeds standing in for distinct clock readings.
        let order = report::shuffled_seeded(N, &format!("seed-{i}"));
        if order[0] == 0 {
            first_is_zero += 1;
        }
    }

    let share = first_is_zero as f64 / SAMPLES as f64;
    println!("shuffle self-test: {SAMPLES} samples of n={N}");
    println!("  first slot held index 0 in {first_is_zero} ({:.1}%)", share * 100.0);

    // A fair shuffle of two lands near 50%. The failure this catches is a stuck
    // permutation, which shows as 0% or 100% — so the band is wide on purpose.
    // It is a smoke test for "is it moving at all", not a randomness test.
    if !(0.35..=0.65).contains(&share) {
        eprintln!("  FAIL: expected roughly half, so the shuffle looks stuck");
        return std::process::ExitCode::FAILURE;
    }

    // And the clock-driven path must vary too, which is the bit that broke.
    let live: std::collections::BTreeSet<Vec<usize>> =
        (0..200).map(|_| report::shuffled(N)).collect();
    println!("  clock-seeded distinct permutations over 200 calls: {}", live.len());
    if live.len() < 2 {
        eprintln!("  FAIL: every clock-seeded shuffle returned the same order");
        return std::process::ExitCode::FAILURE;
    }

    println!("  PASS");
    std::process::ExitCode::SUCCESS
}
