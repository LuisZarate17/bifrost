//! J1c — how fast is a local model on this machine, and what does it cost in memory?
//!
//!   cargo run -p bench --bin speedprobe -- --model qwen3-coder:30b
//!   cargo run -p bench --bin speedprobe -- --model gpt-oss:20b --skip-build
//!
//! Measures one model, the same way every time, so that models can be compared:
//!
//! 1. unloads everything Ollama holds, and waits until `ollama ps` is empty;
//! 2. reads the card (NVML, median of 3) before the model is loaded;
//! 3. loads the model at the config's `num_ctx`, timing the load;
//! 4. reads the card again, and `ollama ps` for the model's size, how much of it sits on the card
//!    (so, what share runs on the CPU) and its digest;
//! 5. reads Windows' "shared GPU memory" for Ollama's runner: the GPU memory that spilled into
//!    system RAM. Read through PowerShell, from the `\GPU Process Memory(*)\Shared Usage`
//!    counter, summed over the instances of the runner's process ids. `N/A` if that cannot be read;
//! 6. prefill and decode speed at about 2k and 16k prompt tokens: three runs each of a
//!    deterministic filler of code text, `num_predict` 128, temperature 0, Ollama's own
//!    prompt-eval and eval clocks; the median of each. The first line of the prompt differs per
//!    run, so the prompt cache cannot make a prefill look free. The prompt text is the same for
//!    every model; the number of tokens it comes to is not, and is reported;
//! 7. free RAM under a cold `cargo build -p bifrost-core` (a fresh target folder, deleted
//!    afterwards) with the model still loaded: the lowest available physical memory seen, sampled
//!    every 250 ms, and the build's seconds. A build Windows blocks is recorded as blocked;
//! 8. unloads the model.
//!
//! Run it from the repository root. Results go to evals/out/J1-speed-<unix>/results.json.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use bifrost_core::{check, config, ollama, vram};
use serde_json::{Value, json};

// What the probes share. This one needs only the installed-model check and the clock.
#[allow(dead_code)]
#[path = "../harness.rs"]
mod harness;

use harness::{require_installed, unix_now};

const USAGE: &str = "\
usage: speedprobe --model <tag> [--skip-build]
       speedprobe --help

  --model <tag>    the Ollama model to measure (required; must be installed)
  --skip-build     skip step 7, the free-RAM reading under a cold `cargo build -p bifrost-core`
  --help, -h       this text

Run it from the repository root. Loads and unloads the model; it leaves nothing loaded.
Results go to evals/out/J1-speed-<unix>/results.json.
";

/// Prompt sizes measured, in tokens (approximate: the model's own tokenizer has the last word,
/// and the count it reports is what is recorded).
const PROMPT_SIZES: [usize; 2] = [2_000, 16_000];
const RUNS: usize = 3;
const NUM_PREDICT: u32 = 128;
/// Code text tokenizes at about this many characters to a token; a rough figure on purpose.
const CHARS_PER_TOKEN: f64 = 3.0;
const GENERATE_TIMEOUT: Duration = Duration::from_secs(900);
const UNLOAD_WAIT: Duration = Duration::from_secs(60);
const BUILD_TIMEOUT: Duration = Duration::from_secs(1800);
const RAM_INTERVAL: Duration = Duration::from_millis(250);
/// What the card is given to let go of a runner's memory before it is read.
const SETTLE: Duration = Duration::from_secs(2);
/// Output kept of the build: only its tail is ever shown.
const KEEP_TAIL: usize = 400;

// ── flags ────────────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
struct Opts {
    model: String,
    skip_build: bool,
}

#[derive(Debug, PartialEq)]
enum Cmd {
    Help,
    Run(Opts),
}

fn parse_args(args: &[String]) -> Result<Cmd, String> {
    let mut model: Option<String> = None;
    let mut skip_build = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--help" | "-h" => return Ok(Cmd::Help),
            "--skip-build" => skip_build = true,
            "--model" => {
                let tag = it.next().map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
                model = Some(tag.ok_or("--model needs a model tag")?);
            }
            other if other.starts_with('-') => return Err(format!("unknown flag: {other}")),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    let model = model.ok_or("--model <tag> is required")?;
    Ok(Cmd::Run(Opts { model, skip_build }))
}

// ── the arithmetic ───────────────────────────────────────────────────────────

/// The middle value of `v`; the mean of the middle two for an even count. `None` for none.
fn median(v: &[f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    let mut v = v.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = v.len() / 2;
    Some(if v.len() % 2 == 1 { v[mid] } else { (v[mid - 1] + v[mid]) / 2.0 })
}

/// A prompt of about `tokens` tokens: a header line that differs with `run` (so no two runs share
/// a prefix), then lines of code, each one different from the last, until it is long enough.
/// Deterministic: the same arguments give the same text.
fn filler_prompt(run: usize, tokens: usize) -> String {
    let want = (tokens as f64 * CHARS_PER_TOKEN) as usize;
    let mut text = format!("Run {run} of {tokens}. Read the code below, then say in one sentence what it does.\n\n");
    let mut i = 0usize;
    while text.len() < want {
        text.push_str(&format!(
            "fn step_{i}(value: u64) -> u64 {{ value.wrapping_mul({}).rotate_left({}) ^ {i} }}\n",
            2 * i + 3,
            i % 61 + 1
        ));
        i += 1;
    }
    text
}

fn mib(bytes: u64) -> u64 {
    bytes / (1024 * 1024)
}

// ── shared GPU memory ────────────────────────────────────────────────────────

/// Finds Ollama's runner (`llama-server.exe`, or `ollama.exe runner ...`), then reads the counter
/// for every GPU process, one tab-separated line each: `PID<TAB>n`, `CTR<TAB>instance<TAB>bytes`.
const SHARED_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
Get-CimInstance Win32_Process |
  Where-Object { $_.Name -eq 'llama-server.exe' -or ($_.Name -eq 'ollama.exe' -and $_.CommandLine -match '(^|\s)runner(\s|$)') } |
  ForEach-Object { "PID`t$($_.ProcessId)" }
(Get-Counter '\GPU Process Memory(*)\Shared Usage').CounterSamples |
  ForEach-Object { "CTR`t$($_.InstanceName)`t$([int64]$_.CookedValue)" }
"#;

#[derive(Debug, PartialEq)]
struct SharedGpu {
    pids: Vec<u32>,
    bytes: u64,
}

/// The process id in a counter instance name such as `pid_1234_luid_0x00000000_0x0000F5A1_phys_0`.
fn instance_pid(instance: &str) -> Option<u32> {
    instance.strip_prefix("pid_")?.split('_').next()?.parse().ok()
}

/// The `Shared Usage` of the runner's processes, from [`SHARED_SCRIPT`]'s output. An instance is
/// one process on one adapter, so a runner on two adapters adds up.
fn parse_shared(output: &str) -> Result<SharedGpu, String> {
    let mut pids: Vec<u32> = Vec::new();
    let mut counters: Vec<(u32, u64)> = Vec::new();
    for line in output.lines() {
        let parts: Vec<&str> = line.trim().split('\t').collect();
        match parts.as_slice() {
            ["PID", pid] => pids.extend(pid.trim().parse::<u32>().ok()),
            ["CTR", instance, bytes] => {
                if let (Some(pid), Ok(bytes)) = (instance_pid(instance), bytes.trim().parse::<u64>()) {
                    counters.push((pid, bytes));
                }
            }
            _ => {}
        }
    }
    if pids.is_empty() {
        return Err("no Ollama runner process found".to_string());
    }
    let bytes = counters.iter().filter(|(pid, _)| pids.contains(pid)).map(|(_, b)| *b).sum();
    Ok(SharedGpu { pids, bytes })
}

fn read_shared_gpu() -> Result<SharedGpu, String> {
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", SHARED_SCRIPT])
        .output()
        .map_err(|e| format!("could not run powershell: {e}"))?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        return Err(format!("powershell failed: {}", why.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim()));
    }
    parse_shared(&String::from_utf8_lossy(&out.stdout))
}

// ── free RAM ─────────────────────────────────────────────────────────────────

/// Available physical memory, in MiB. This is `Win32_OperatingSystem.FreePhysicalMemory`: what
/// can be handed out without paging, standby cache included.
#[cfg(windows)]
fn free_ram_mib() -> Option<u64> {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    let mut status = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    // SAFETY: `status` is a live MEMORYSTATUSEX with its length set, as the call requires.
    unsafe { GlobalMemoryStatusEx(&mut status) }.ok()?;
    Some(mib(status.ullAvailPhys))
}

#[cfg(not(windows))]
fn free_ram_mib() -> Option<u64> {
    None
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct RamStats {
    first_mib: Option<u64>,
    min_mib: Option<u64>,
    samples: usize,
}

impl RamStats {
    fn note(&mut self, mib: u64) {
        self.first_mib.get_or_insert(mib);
        self.min_mib = Some(self.min_mib.map_or(mib, |m| m.min(mib)));
        self.samples += 1;
    }
}

/// Samples free RAM every [`RAM_INTERVAL`] on a thread of its own until [`RamWatch::finish`].
struct RamWatch {
    stop: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<RamStats>,
}

impl RamWatch {
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            let mut stats = RamStats::default();
            loop {
                if let Some(free) = free_ram_mib() {
                    stats.note(free);
                }
                if flag.load(Ordering::Relaxed) {
                    return stats;
                }
                std::thread::sleep(RAM_INTERVAL);
            }
        });
        Self { stop, handle }
    }

    fn finish(self) -> RamStats {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.join().unwrap_or_default()
    }
}

// ── the steps ────────────────────────────────────────────────────────────────

/// Step 1: every model Ollama holds, unloaded, and `ps` empty. The models unloaded.
fn unload_everything() -> Result<Vec<String>, String> {
    let mut gone = Vec::new();
    for m in ollama::ps()? {
        if let Some(tag) = m.model.clone().or_else(|| m.name.clone()) {
            ollama::unload(&tag)?;
            gone.push(tag);
        }
    }
    let deadline = Instant::now() + UNLOAD_WAIT;
    loop {
        if ollama::ps()?.is_empty() {
            return Ok(gone);
        }
        if Instant::now() >= deadline {
            return Err(format!("Ollama still lists a model {}s after the unload", UNLOAD_WAIT.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn card_json(c: &Option<vram::VramSample>) -> Value {
    c.map_or(Value::Null, |s| json!({ "used_mib": s.used_mib, "total_mib": s.total_mib }))
}

/// One prompt size: three runs, and the medians.
fn measure_speed(model: &str, tokens: usize, num_ctx: u32) -> Value {
    if tokens as u64 + NUM_PREDICT as u64 + 64 > num_ctx as u64 {
        let why = format!("skipped: num_ctx {num_ctx} is too small for a prompt of about {tokens} tokens");
        println!("  ~{tokens} tokens: {why}");
        return json!({ "target_tokens": tokens, "skipped": why });
    }
    let (mut prefill, mut decode) = (Vec::new(), Vec::new());
    let (mut runs, mut errors) = (Vec::new(), Vec::new());
    let mut prompt_tokens = None;
    for run in 1..=RUNS {
        let prompt = filler_prompt(run, tokens);
        match ollama::generate_timed(model, &prompt, num_ctx, NUM_PREDICT, GENERATE_TIMEOUT) {
            Ok(t) if t.prompt_eval_duration > 0 && t.eval_duration > 0 => {
                println!(
                    "  ~{tokens} tokens, run {run}: {} prompt tokens, prefill {:.0} tok/s, decode {:.1} tok/s ({} tokens)",
                    t.prompt_eval_count,
                    t.prefill_tps(),
                    t.decode_tps(),
                    t.eval_count
                );
                prefill.push(t.prefill_tps());
                decode.push(t.decode_tps());
                prompt_tokens = Some(t.prompt_eval_count);
                runs.push(json!({
                    "run": run,
                    "prompt_eval_count": t.prompt_eval_count,
                    "prompt_eval_ms": t.prompt_eval_duration as f64 / 1e6,
                    "eval_count": t.eval_count,
                    "eval_ms": t.eval_duration as f64 / 1e6,
                    "load_ms": t.load_duration as f64 / 1e6,
                    "prefill_tps": t.prefill_tps(),
                    "decode_tps": t.decode_tps(),
                }));
            }
            // A zero clock is a prompt Ollama served from its cache, or a reply cut to nothing:
            // not a speed.
            Ok(t) => {
                let why = format!("run {run}: no usable clock (prompt_eval {} ns, eval {} ns)", t.prompt_eval_duration, t.eval_duration);
                println!("  ~{tokens} tokens, {why}");
                errors.push(why);
            }
            Err(e) => {
                println!("  ~{tokens} tokens, run {run}: FAILED - {e}");
                errors.push(format!("run {run}: {e}"));
            }
        }
    }
    json!({
        "target_tokens": tokens,
        "prompt_eval_count": prompt_tokens,
        "prefill_tps_median": median(&prefill),
        "decode_tps_median": median(&decode),
        "runs": runs,
        "errors": errors,
    })
}

/// Step 7: a cold build of bifrost-core in a fresh target folder, free RAM watched throughout.
fn measure_build(repo: &Path) -> Value {
    let target: PathBuf = std::env::temp_dir().join(format!("bifrost-speedprobe-{}-target", std::process::id()));
    let _ = std::fs::remove_dir_all(&target);
    let env = vec![("CARGO_TARGET_DIR".to_string(), target.to_string_lossy().into_owned())];
    let argv = ["cargo", "build", "-p", "bifrost-core"].map(String::from);

    let watch = RamWatch::start();
    let started = Instant::now();
    let result = check::run(repo, &argv, BUILD_TIMEOUT, &env, || false);
    let seconds = started.elapsed().as_secs_f64();
    let ram = watch.finish();

    let removed = std::fs::remove_dir_all(&target).is_ok() || !target.exists();
    let (status, note) = match &result {
        Err(e) => ("could not start", e.clone()),
        Ok(o) if o.blocked.is_some() => ("blocked", format!("Windows blocked {}", o.blocked.as_deref().unwrap_or(""))),
        Ok(o) if o.passed() => ("built", String::new()),
        Ok(o) => {
            let tail: String = o.output.chars().rev().take(KEEP_TAIL).collect::<Vec<_>>().into_iter().rev().collect();
            ("failed", format!("{:?}: {}", o.ended, tail.trim()))
        }
    };
    let mib = |m: Option<u64>| m.map_or("?".to_string(), |m| m.to_string());
    println!(
        "  build: {status} in {seconds:.0}s; free RAM {} MiB at the start, lowest {} MiB ({} samples)",
        mib(ram.first_mib),
        mib(ram.min_mib),
        ram.samples
    );
    if !note.is_empty() {
        println!("  build note: {note}");
    }
    if !removed {
        println!("  could not delete {}", target.display());
    }
    json!({
        "status": status,
        "note": note,
        "build_seconds": seconds,
        "free_ram_start_mib": ram.first_mib,
        "free_ram_min_mib": ram.min_mib,
        "free_ram_drop_mib": ram.first_mib.zip(ram.min_mib).map(|(a, b)| a.saturating_sub(b)),
        "samples": ram.samples,
        "target_deleted": removed,
    })
}

fn fmt_opt<T: std::fmt::Display>(v: Option<T>, unit: &str) -> String {
    v.map_or("N/A".to_string(), |v| format!("{v}{unit}"))
}

fn run(opts: &Opts) -> Result<(), String> {
    let model = opts.model.as_str();
    let repo = std::env::current_dir().map_err(|e| format!("no current directory: {e}"))?;
    let num_ctx = config::load().num_ctx;
    let started_unix = unix_now();

    ollama::ensure_running().map_err(|e| format!("could not start Ollama: {e}"))?;
    let tags = ollama::tags().map_err(|e| format!("could not ask Ollama what is installed: {e}"))?;
    let installed: Vec<String> = tags.iter().map(|m| m.name.clone()).collect();
    require_installed(&installed, model)?;
    let disk_bytes = tags.iter().find(|m| m.name.eq_ignore_ascii_case(model)).map(|m| m.size);
    let detail = ollama::show(model).ok();

    let card = vram::Card::open();
    let card_name = card.as_ref().map(|c| c.name.clone()).ok();
    println!("speedprobe - J1c");
    println!("model:   {model}");
    println!("num_ctx: {num_ctx}");
    println!("card:    {}\n", card.as_ref().map(|c| c.name.clone()).unwrap_or_else(|e| format!("unavailable ({e})")));
    let sample = || card.as_ref().ok().and_then(|c| c.sample().ok());

    println!("1. unloading everything");
    let unloaded = unload_everything()?;
    println!("  unloaded: {}", if unloaded.is_empty() { "nothing was loaded".to_string() } else { unloaded.join(", ") });
    std::thread::sleep(SETTLE);

    println!("2. card before");
    let before = sample();
    println!("  {}", fmt_opt(before.map(|s| s.used_mib), " MiB used"));

    println!("3. loading at num_ctx {num_ctx}");
    let load = ollama::load(model, num_ctx);
    // From here the model may be resident: whatever happens, it is unloaded at the end.
    let outcome = measure_loaded(opts, &repo, num_ctx, &card, before, load);
    println!("8. unloading {model}");
    let unloaded_ok = match ollama::unload(model) {
        Ok(true) => true,
        Ok(false) => {
            println!("  Ollama still lists it");
            false
        }
        Err(e) => {
            println!("  could not unload: {e}");
            false
        }
    };
    let mut sections = outcome?;

    let finished_unix = unix_now();
    sections["model"] = json!(model);
    sections["num_ctx"] = json!(num_ctx);
    sections["card_name"] = json!(card_name);
    sections["started_unix"] = json!(started_unix);
    sections["finished_unix"] = json!(finished_unix);
    sections["unloaded_at_start"] = json!(unloaded);
    sections["unloaded_at_end"] = json!(unloaded_ok);
    sections["disk_bytes"] = json!(disk_bytes);
    sections["model_details"] = json!(detail.as_ref().map(|d| json!({
        "family": d.family,
        "parameter_size": d.parameter_size,
        "quantization_level": d.quantization_level,
        "max_context": d.max_context,
    })));
    sections["card_before"] = card_json(&before);

    let out = repo.join("evals").join("out").join(format!("J1-speed-{started_unix}"));
    std::fs::create_dir_all(&out).map_err(|e| format!("could not make {}: {e}", out.display()))?;
    let file = out.join("results.json");
    std::fs::write(&file, serde_json::to_string_pretty(&sections).unwrap_or_default()).map_err(|e| format!("could not write {}: {e}", file.display()))?;
    println!("\nsaved {}", file.display());
    Ok(())
}

/// Steps 4 to 7, with the model loaded (or not: `load` says). The measurements as JSON.
fn measure_loaded(
    opts: &Opts,
    repo: &Path,
    num_ctx: u32,
    card: &Result<vram::Card, String>,
    before: Option<vram::VramSample>,
    load: Result<Duration, String>,
) -> Result<Value, String> {
    let model = opts.model.as_str();
    let load = load?;
    println!("  loaded in {:.1}s", load.as_secs_f64());
    let sample = || card.as_ref().ok().and_then(|c| c.sample().ok());

    println!("4. card after, and the split");
    let after = sample();
    let change = after.zip(before).map(|(a, b)| a.used_mib as i64 - b.used_mib as i64);
    let ps = ollama::ps()?;
    let me = ollama::find(&ps, model).or(ps.first());
    let size = me.and_then(|m| m.size);
    let size_vram = me.and_then(|m| m.size_vram);
    let cpu_share = me.and_then(|m| m.cpu_share());
    let digest = me.and_then(|m| m.digest.clone());
    println!("  card {} (change {})", fmt_opt(after.map(|s| s.used_mib), " MiB used"), fmt_opt(change, " MiB"));
    println!(
        "  size {}, on the card {}, CPU share {}",
        fmt_opt(size.map(mib), " MiB"),
        fmt_opt(size_vram.map(mib), " MiB"),
        fmt_opt(cpu_share.map(|s| format!("{:.1}", s * 100.0)), "%")
    );
    println!("  digest {}", fmt_opt(digest.as_deref(), ""));

    println!("5. shared GPU memory");
    let shared_load = read_shared_gpu();
    match &shared_load {
        Ok(s) => println!("  {} MiB (runner pid {:?})", mib(s.bytes), s.pids),
        Err(e) => println!("  N/A: {e}"),
    }

    println!("6. prefill and decode speed ({RUNS} runs each, num_predict {NUM_PREDICT})");
    let speeds: Vec<Value> = PROMPT_SIZES.iter().map(|&t| measure_speed(model, t, num_ctx)).collect();
    // Context is allocated as it is used on some builds, so the spill is read again, after the work.
    let shared_after = read_shared_gpu();

    let build = if opts.skip_build {
        println!("7. free RAM under a build: skipped (--skip-build)");
        json!({ "status": "skipped" })
    } else {
        println!("7. free RAM under a cold cargo build -p bifrost-core, model loaded");
        measure_build(repo)
    };

    let shared = |r: &Result<SharedGpu, String>| match r {
        Ok(s) => json!({ "mib": mib(s.bytes), "bytes": s.bytes, "runner_pids": s.pids }),
        Err(e) => json!({ "mib": null, "unavailable": e }),
    };
    println!("\n--- {model} at num_ctx {num_ctx} ---");
    for s in &speeds {
        if s.get("skipped").is_some() {
            continue;
        }
        println!(
            "~{:>5} tokens ({} actual): prefill {} tok/s, decode {} tok/s",
            s["target_tokens"],
            fmt_opt(s["prompt_eval_count"].as_u64(), ""),
            fmt_opt(s["prefill_tps_median"].as_f64().map(|v| format!("{v:.0}")), ""),
            fmt_opt(s["decode_tps_median"].as_f64().map(|v| format!("{v:.1}")), "")
        );
    }

    Ok(json!({
        "load_seconds": load.as_secs_f64(),
        "card_after": card_json(&after),
        "card_change_mib": change,
        "size_bytes": size,
        "size_vram_bytes": size_vram,
        "size_mib": size.map(mib),
        "size_vram_mib": size_vram.map(mib),
        "cpu_share": cpu_share,
        "digest": digest,
        "shared_gpu_after_load": shared(&shared_load),
        "shared_gpu_after_speed": shared(&shared_after),
        "speed": speeds,
        "build": build,
    }))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opts = match parse_args(&args) {
        Ok(Cmd::Help) => {
            print!("{USAGE}");
            return;
        }
        Ok(Cmd::Run(o)) => o,
        Err(e) => {
            eprintln!("{e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    if let Err(e) = run(&opts) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn flags_are_parsed_and_unknown_ones_refused() {
        assert_eq!(parse_args(&strings(&["--help"])), Ok(Cmd::Help));
        assert_eq!(parse_args(&strings(&["--model", "a:b", "-h"])), Ok(Cmd::Help));
        assert_eq!(parse_args(&strings(&["--model", " a:b "])), Ok(Cmd::Run(Opts { model: "a:b".into(), skip_build: false })));
        assert_eq!(parse_args(&strings(&["--skip-build", "--model", "a"])), Ok(Cmd::Run(Opts { model: "a".into(), skip_build: true })));
        assert!(parse_args(&[]).unwrap_err().contains("--model"));
        assert!(parse_args(&strings(&["--model"])).is_err());
        assert!(parse_args(&strings(&["--model", " "])).is_err());
        assert!(parse_args(&strings(&["--model", "a", "--nope"])).unwrap_err().contains("unknown flag"));
        assert!(parse_args(&strings(&["--model", "a", "extra"])).unwrap_err().contains("unexpected"));
    }

    #[test]
    fn the_median_is_the_middle() {
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[5.0]), Some(5.0));
        assert_eq!(median(&[3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(&[4.0, 1.0, 3.0, 2.0]), Some(2.5));
        // One slow run (a cold cache) does not move it.
        assert_eq!(median(&[100.0, 101.0, 5.0]), Some(100.0));
    }

    #[test]
    fn a_filler_is_the_size_asked_for_and_never_the_same_twice() {
        for tokens in PROMPT_SIZES {
            let p = filler_prompt(1, tokens);
            let want = (tokens as f64 * CHARS_PER_TOKEN) as usize;
            assert!(p.len() >= want && p.len() < want + 200, "{} chars for {tokens} tokens", p.len());
            assert_eq!(p, filler_prompt(1, tokens), "deterministic");
            // The difference is in the first line, so no prefix is shared past it.
            let other = filler_prompt(2, tokens);
            assert_ne!(p.lines().next(), other.lines().next());
            assert_eq!(p.lines().nth(2), other.lines().nth(2));
        }
        assert!(filler_prompt(1, 16_000).len() > 7 * filler_prompt(1, 2_000).len());
        // Every code line is different from the last.
        let p = filler_prompt(1, 2_000);
        let lines: Vec<&str> = p.lines().skip(2).collect();
        assert!(lines.windows(2).all(|w| w[0] != w[1]));
    }

    #[test]
    fn instances_name_their_process() {
        assert_eq!(instance_pid("pid_1234_luid_0x00000000_0x0000F5A1_phys_0"), Some(1234));
        assert_eq!(instance_pid("pid_7"), Some(7));
        assert_eq!(instance_pid("luid_0x0_phys_0"), None);
        assert_eq!(instance_pid("pid_x_luid"), None);
    }

    #[test]
    fn the_runners_shared_usage_is_summed_over_its_instances() {
        let out = "PID\t4242\r\n\
                   CTR\tpid_4242_luid_0x00000000_0x0000F5A1_phys_0\t1073741824\r\n\
                   CTR\tpid_4242_luid_0x00000000_0x0000F5A2_phys_0\t1048576\r\n\
                   CTR\tpid_9999_luid_0x00000000_0x0000F5A1_phys_0\t5000000000\r\n\
                   CTR\tpid_42420_luid_0x00000000_0x0000F5A1_phys_0\t7\r\n\
                   noise line\r\n";
        assert_eq!(parse_shared(out), Ok(SharedGpu { pids: vec![4242], bytes: 1_073_741_824 + 1_048_576 }));
        // Two runner processes add up; one with no instance counts for nothing.
        let two = "PID\t1\nPID\t2\nCTR\tpid_1_luid_0x0_phys_0\t10\nCTR\tpid_2_luid_0x0_phys_0\t5\nCTR\tpid_3_luid_0x0_phys_0\t99\n";
        assert_eq!(parse_shared(two).unwrap().bytes, 15);
        assert_eq!(parse_shared("PID\t5\nCTR\tpid_6_luid_0x0_phys_0\t99\n").unwrap().bytes, 0);
    }

    #[test]
    fn no_runner_is_not_zero_bytes() {
        assert!(parse_shared("").unwrap_err().contains("no Ollama runner"));
        assert!(parse_shared("CTR\tpid_1_luid_0x0_phys_0\t10\n").is_err());
    }

    #[test]
    fn the_ram_floor_is_the_lowest_sample() {
        let mut s = RamStats::default();
        for v in [9000, 8000, 8500, 7000, 9500] {
            s.note(v);
        }
        assert_eq!(s, RamStats { first_mib: Some(9000), min_mib: Some(7000), samples: 5 });
    }

    #[test]
    fn mebibytes_round_down() {
        assert_eq!(mib(1024 * 1024 * 3 + 5), 3);
        assert_eq!(mib(0), 0);
    }
}
