//! J1 — can a local model do real coding work, unattended?
//!
//!   cargo run -p bench --bin realprobe -- --selfcheck
//!   cargo run -p bench --bin realprobe -- --model qwen3-coder:30b
//!   cargo run -p bench --bin realprobe -- --model gpt-oss:20b --think high --runs 3 --only 03
//!
//! Gives each of the ten tasks in `evals/realprobe/` to a local model through `hugin -p`, in a
//! fresh worktree at the pinned commit, and scores the patch Hugin hands back with tests the
//! model never saw: a second fresh worktree, the patch applied, the hidden tests added, the
//! task's check run with the hidden filter, then the suites that must still pass, then a look
//! that no test of the pinned tree has gone missing.
//!
//! Hugin runs in a sandbox: `APPDATA`, `LOCALAPPDATA`, `TEMP` and `TMP` point into a folder of
//! this invocation's own, with a `config.json` that names the model, so the user's real
//! settings are never read or written. Hugin's per-run build cache in it is deleted after each
//! run. The scoring runs have a sandbox of their own the same way, so that a test the model
//! wrote, calling `Config::save()`, cannot touch the real config.
//!
//! The scoring builds in the repo's own `target/` (as taskprobe does: a fresh target
//! directory rebuilds every proc-macro, and this machine has refused a freshly linked one
//! before), so no other cargo command may run beside it. A scoring build that times out, is
//! stopped, ends with no exit code or stays blocked by Smart App Control is an ERROR, never the
//! model's FAIL. A test run is built first (`--no-run`) and then run with a limit of its own
//! ([`TEST_TIMEOUT`]): tests that began and never ended are the model's code hanging, a FAIL.
//!
//! Every cargo command goes through `check::run_with_input`, which is `check::run` with room
//! for the whole output: `run`'s 6 KB head and 2 KB tail can cut out the `test result:` lines
//! of the test programs in between, and the score is the sum of those lines. The memory and
//! disk caps are the same ones. A test program that began and never reported is counted here
//! as `check::Outcome::passed` counts it.
//!
//! `--selfcheck` runs no model and no Hugin: it proves each task scores the way it claims to.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bifrost_core::config::Think;
use bifrost_core::{check, ollama};
use serde_json::Value;

// What this probe and askprobe share: the worktree, the sandbox, the installed-model check.
#[path = "../harness.rs"]
mod harness;

use harness::{Sandbox, Worktree, copy_hugin, first_line, git, require_installed, unix_now};

// Hugin's own reading of "a patch that changes existing tests", so that the selfcheck asks the
// question Hugin will ask of every patch a model hands back.
#[allow(dead_code)]
#[path = "../../../../apps/hugin/src/bent.rs"]
mod bent;

/// What Hugin is told it may take, and how long this program waits for it before it ends the
/// process tree and books an ERROR.
const HUGIN_TIME: u64 = 900;
const HUGIN_GUARD: Duration = Duration::from_secs(1000);
/// One scoring command, cold: a build of every test binary in the package.
const SCORE_TIMEOUT: Duration = Duration::from_secs(1800);
const BUILD_TIMEOUT: Duration = Duration::from_secs(1800);
/// The run of already built tests: far more than any test of these tasks needs, so a run that is
/// still going has hung.
const TEST_TIMEOUT: Duration = Duration::from_secs(300);
/// The output kept of a command: all of it, for any run this program makes.
const KEEP: usize = 16 * 1024 * 1024;
/// A command refused by Smart App Control is touched and run again, this many runs in all.
const BLOCK_TRIES: usize = 3;
/// A pass inside Hugin's own default time is a pass a person would have waited for.
const UNDER_SECS: f64 = 540.0;
/// A model tag no Ollama has: the selfcheck proves a run refuses it.
const NO_SUCH_MODEL: &str = "realprobe-no-such-model";

const USAGE: &str = "\
usage: realprobe --model <tag> [--runs N] [--only <id-prefix>] [--think <off|low|medium|high>] [--keep]
       realprobe --selfcheck [--only <id-prefix>] [--keep]
       realprobe --help

  --model <tag>     the Ollama model to give the tasks to (required; the config's is never used)
  --runs N          runs of each task (default 1)
  --only <prefix>   only the tasks whose id starts with this
  --think <level>   the model's `think` setting for the whole run: off, low, medium or high
  --keep            keep the worktrees and the sandbox (%TEMP%\\bifrost-realprobe-<pid>)
  --selfcheck       no model, no Hugin: prove every task scores as it claims to
  --help, -h        this text

Run it from the repository root. Results go to evals/out/J1-real-<unix>/results.json.
";

// ── the tasks ────────────────────────────────────────────────────────────────

/// How a task's hidden tests are put in.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Hidden {
    /// A `mod realprobe_hidden` added at the end of an existing source file.
    Append { file: String, src: String },
    /// A new file, such as an integration test that runs the built binary.
    File { path: String, src: String },
}

#[derive(Debug, Clone)]
struct Wrong {
    patch: String,
    why: String,
}

#[derive(Debug, Clone)]
struct Task {
    id: String,
    pin: String,
    check: Vec<String>,
    hidden: Vec<Hidden>,
    hidden_count: usize,
    hidden_filter: String,
    suites: Vec<Vec<String>>,
    wrong: Vec<Wrong>,
    /// The task's folder under `evals/realprobe`.
    dir: PathBuf,
}

fn text_of(v: &Value, key: &str) -> Result<String, String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("`{key}` is missing or not text"))
}

fn words_of(v: &Value) -> Result<Vec<String>, String> {
    v.as_array()
        .ok_or("a command is not a list of words")?
        .iter()
        .map(|w| {
            w.as_str()
                .map(str::to_string)
                .ok_or_else(|| "a command word is not text".to_string())
        })
        .collect()
}

/// `task.json` as a task. `dir` is where the task's other files are.
fn parse_task(text: &str, dir: &Path) -> Result<Task, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    let hidden = v
        .get("hidden")
        .and_then(Value::as_array)
        .ok_or("`hidden` is missing")?
        .iter()
        .map(|h| match text_of(h, "kind")?.as_str() {
            "append" => Ok(Hidden::Append {
                file: text_of(h, "file")?,
                src: text_of(h, "src")?,
            }),
            "file" => Ok(Hidden::File {
                path: text_of(h, "path")?,
                src: text_of(h, "src")?,
            }),
            other => Err(format!("a hidden entry has kind `{other}`: append or file")),
        })
        .collect::<Result<Vec<_>, String>>()?;
    let wrong = v
        .get("wrong")
        .and_then(Value::as_array)
        .ok_or("`wrong` is missing")?
        .iter()
        .map(|w| {
            Ok(Wrong {
                patch: text_of(w, "patch")?,
                why: text_of(w, "why")?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let suites = v
        .get("suites")
        .and_then(Value::as_array)
        .ok_or("`suites` is missing")?
        .iter()
        .map(words_of)
        .collect::<Result<Vec<_>, String>>()?;
    let check = words_of(v.get("check").ok_or("`check` is missing")?)?;
    if check.is_empty() || hidden.is_empty() {
        return Err("`check` and `hidden` may not be empty".to_string());
    }
    Ok(Task {
        id: text_of(&v, "id")?,
        pin: text_of(&v, "pin")?,
        check,
        hidden,
        hidden_count: v
            .get("hidden_count")
            .and_then(Value::as_u64)
            .ok_or("`hidden_count` is missing")? as usize,
        hidden_filter: text_of(&v, "hidden_filter")?,
        suites,
        wrong,
        dir: dir.to_path_buf(),
    })
}

/// Every task under `<repo>/evals/realprobe`, by folder name.
fn load_tasks(repo: &Path) -> Result<Vec<Task>, String> {
    let root = repo.join("evals").join("realprobe");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&root)
        .map_err(|e| format!("could not read {}: {e} — run from the repository root", root.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("task.json").is_file())
        .collect();
    dirs.sort();
    dirs.iter()
        .map(|dir| {
            let text = std::fs::read_to_string(dir.join("task.json")).map_err(|e| format!("{}: {e}", dir.display()))?;
            parse_task(&text, dir).map_err(|e| format!("{}: {e}", dir.join("task.json").display()))
        })
        .collect()
}

// ── git ──────────────────────────────────────────────────────────────────────

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// What a worktree needs besides the commit: the Mimir tasks build a Tauri app, which wants the
/// (git-ignored) `apps/mimir/dist`. The main checkout's, or an empty folder where it has none.
fn prepare(repo: &Path, tree: &Path, task: &Task) -> Result<(), String> {
    if !task.check.iter().any(|w| w == "mimir") {
        return Ok(());
    }
    let (from, to) = (repo.join("apps/mimir/dist"), tree.join("apps/mimir/dist"));
    if to.exists() {
        return Ok(());
    }
    if from.is_dir() {
        copy_dir(&from, &to).map_err(|e| format!("could not copy apps/mimir/dist: {e}"))
    } else {
        std::fs::create_dir_all(&to).map_err(|e| format!("could not make apps/mimir/dist: {e}"))
    }
}

/// Touch a source file so the next build sees it as changed — the fix for a Smart App Control
/// block on a freshly linked binary.
fn touch(tree: &Path, task: &Task) {
    for h in &task.hidden {
        let rel = match h {
            Hidden::Append { file, .. } => file,
            Hidden::File { path, .. } => path,
        };
        let p = tree.join(rel);
        if p.extension().is_some_and(|e| e == "rs")
            && let Ok(bytes) = std::fs::read(&p)
        {
            let _ = std::fs::write(&p, bytes);
            return;
        }
    }
}

// ── running cargo ────────────────────────────────────────────────────────────

/// Why a run says nothing about the model's code, if it does not: it hit its timeout, was
/// stopped, ran out of disk or ended with no exit code. `None`: it finished by itself.
fn unfinished(o: &check::Outcome) -> Option<String> {
    match &o.ended {
        check::Ended::TimedOut => Some(format!(
            "a scoring command timed out after {:.0}s — environment",
            o.elapsed.as_secs_f64()
        )),
        check::Ended::Stopped => Some("a scoring command was stopped — environment".to_string()),
        check::Ended::DiskFull => {
            Some("a scoring command was stopped for a nearly full disk — environment".to_string())
        }
        check::Ended::Exited(None) => Some("a scoring command ended with no exit code — environment".to_string()),
        check::Ended::Exited(Some(_)) => None,
    }
}

/// `argv` in `dir`, whole output kept, however it ended. A Smart App Control block is touched
/// past and run again, up to `BLOCK_TRIES` runs in all. `Err`: it could not start, or stayed
/// blocked.
fn cargo_raw(
    dir: &Path,
    task: Option<&Task>,
    argv: &[String],
    env: &[(String, String)],
    timeout: Duration,
) -> Result<check::Outcome, String> {
    for attempt in 1..=BLOCK_TRIES {
        let o = check::run_with_input(dir, argv, &[], KEEP, timeout, env, || false)?;
        if o.blocked.is_some() {
            if attempt < BLOCK_TRIES {
                if let Some(task) = task {
                    touch(dir, task);
                }
                continue;
            }
            return Err("blocked by Smart App Control".to_string());
        }
        return Ok(o);
    }
    Err("blocked by Smart App Control".to_string())
}

/// [`cargo_raw`] for a command that has to finish by itself. `Err` is an ERROR: it could not
/// start, did not finish, or stayed blocked.
fn cargo(
    dir: &Path,
    task: Option<&Task>,
    argv: &[String],
    env: &[(String, String)],
    timeout: Duration,
) -> Result<check::Outcome, String> {
    let o = cargo_raw(dir, task, argv, env, timeout)?;
    match unfinished(&o) {
        Some(why) => Err(why),
        None => Ok(o),
    }
}

/// How a test run ended.
enum Ran {
    /// The build or the run finished by itself, whatever it found.
    Done(check::Outcome),
    /// The tests had begun and the run hit [`TEST_TIMEOUT`]: the model's code hangs.
    Hung,
}

/// Whether a run was cut off by its timeout while test programs were running: a `running N tests`
/// line is out and the run did not finish. A timeout before any such line is the environment's.
fn hung(o: &check::Outcome) -> bool {
    o.ended == check::Ended::TimedOut && tally(&o.output).began > 0
}

/// `argv` is a `cargo test` command, `filter` the test filter after `--`. Built first, with the
/// usual limit (`--no-run`: a compile error ends it there, as `Done`), then run with
/// [`TEST_TIMEOUT`]. A hang is `Hung`; any other end that is not a finish is an ERROR.
fn cargo_tests(
    dir: &Path,
    task: &Task,
    argv: &[String],
    filter: Option<&str>,
    env: &[(String, String)],
) -> Result<Ran, String> {
    let mut build = argv.to_vec();
    build.insert(
        argv.iter().position(|w| w == "--").unwrap_or(argv.len()),
        "--no-run".to_string(),
    );
    let built = cargo(dir, Some(task), &build, env, SCORE_TIMEOUT)?;
    if !built.passed() {
        return Ok(Ran::Done(built));
    }
    let mut run = argv.to_vec();
    if let Some(filter) = filter {
        run.extend(["--".to_string(), filter.to_string()]);
    }
    let o = cargo_raw(dir, Some(task), &run, env, TEST_TIMEOUT)?;
    if hung(&o) {
        return Ok(Ran::Hung);
    }
    match unfinished(&o) {
        Some(why) => Err(why),
        None => Ok(Ran::Done(o)),
    }
}

/// What `cargo test` printed, summed over every test program.
#[derive(Debug, Default, PartialEq, Eq)]
struct Tally {
    passed: usize,
    failed: usize,
    began: usize,
    reported: usize,
}

impl Tally {
    /// A test program began and never reported: something exited or aborted the run.
    fn cut_short(&self) -> bool {
        self.began > self.reported
    }
}

/// `running 12 tests` or `running 1 test`.
fn is_libtest_header(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("running ") else {
        return false;
    };
    let count = rest.strip_suffix(" tests").or_else(|| rest.strip_suffix(" test"));
    count.is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Sum the passed and failed counts of every `test result:` line.
fn tally(output: &str) -> Tally {
    let mut t = Tally::default();
    for line in output.lines() {
        let line = line.trim_end_matches('\r');
        if is_libtest_header(line) {
            t.began += 1;
        } else if let Some(rest) = line.strip_prefix("test result: ") {
            t.reported += 1;
            for part in rest.split(';') {
                let part = part.trim();
                let number = |suffix: &str| {
                    part.strip_suffix(suffix)
                        .and_then(|n| n.rsplit(' ').next())
                        .and_then(|n| n.parse::<usize>().ok())
                };
                t.passed += number(" passed").unwrap_or(0);
                t.failed += number(" failed").unwrap_or(0);
            }
        }
    }
    t
}

/// The first thing worth saying about a command that did not pass: the tests that failed, else
/// the first compiler error, else its exit code.
fn failure_note(output: &str, code: Option<i32>) -> String {
    let failed: Vec<&str> = output
        .lines()
        .filter(|l| l.trim_start().starts_with("test ") && l.trim_end().ends_with("FAILED"))
        .map(str::trim)
        .collect();
    if !failed.is_empty() {
        let shown = failed.iter().take(3).copied().collect::<Vec<_>>().join(", ");
        return if failed.len() > 3 {
            format!("{shown} (+{} more)", failed.len() - 3)
        } else {
            shown
        };
    }
    if let Some(line) = output.lines().find(|l| l.starts_with("error")) {
        return format!("does not compile: {}", line.trim());
    }
    format!("exit code {}", code.map_or("none".to_string(), |c| c.to_string()))
}

/// Whether the hidden tests' run is a pass: exit 0, no test program cut short, nothing failed, and
/// exactly `want` tests passed over all the programs. `Err` says why not.
fn judge_hidden(output: &str, code: Option<i32>, want: usize) -> Result<(), String> {
    let t = tally(output);
    if code != Some(0) {
        return Err(format!(
            "hidden tests: {} ({} passed, {} failed, wanted {want})",
            failure_note(output, code),
            t.passed,
            t.failed
        ));
    }
    if t.cut_short() {
        return Err("hidden tests: a test program ended without reporting its result".to_string());
    }
    if t.failed > 0 || t.passed != want {
        return Err(format!(
            "hidden tests: {} passed and {} failed, wanted {want} passed",
            t.passed, t.failed
        ));
    }
    Ok(())
}

/// The tests a `cargo test -- --list` printed, each with the test program it is in. A program
/// is named by its `Running` line without the path of its executable, which carries a hash.
/// Doc-tests are left out: their names carry line numbers.
fn parse_list(output: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let (mut program, mut doc) = (String::new(), false);
    for line in output.lines() {
        let line = line.trim_end_matches('\r');
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("Running ") {
            program = rest.rfind(" (").map_or(rest, |at| &rest[..at]).to_string();
            doc = false;
        } else if trimmed.starts_with("Doc-tests ") {
            doc = true;
        } else if !doc && let Some(name) = line.strip_suffix(": test") {
            found.insert(format!("{program} :: {name}"));
        }
    }
    found
}

// ── scoring ──────────────────────────────────────────────────────────────────

/// What scoring says about a patch.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Judged {
    Correct,
    /// Not correct, and why. The model's failure.
    Wrong(String),
    /// Scoring itself failed: nothing is known about the patch.
    Error(String),
}

struct Scorer {
    /// The repo's own `target/`.
    target: PathBuf,
    /// Where `APPDATA`, `LOCALAPPDATA`, `TEMP` and `TMP` point during scoring: a folder of this
    /// invocation's own, so a test that saves the config writes there.
    home: PathBuf,
    /// The test names listed at the pin, by task id: worked out once.
    pins: HashMap<String, BTreeSet<String>>,
}

impl Scorer {
    fn new(repo: &Path, scratch: &Path) -> Self {
        Self {
            target: repo.join("target"),
            home: scratch.join("scoring-home"),
            pins: HashMap::new(),
        }
    }

    fn env(&self) -> Vec<(String, String)> {
        let show = |p: &Path| p.to_string_lossy().into_owned();
        let (appdata, local, temp) = (
            self.home.join("appdata"),
            self.home.join("local"),
            self.home.join("temp"),
        );
        for dir in [&appdata, &local, &temp] {
            let _ = std::fs::create_dir_all(dir);
        }
        vec![
            ("CARGO_TARGET_DIR".to_string(), show(&self.target)),
            ("CARGO_TERM_COLOR".to_string(), "never".to_string()),
            // A patch that adds a dependency must not make cargo download it and run its build
            // script: model-written code is scored, never code from the network.
            ("CARGO_NET_OFFLINE".to_string(), "true".to_string()),
            ("APPDATA".to_string(), show(&appdata)),
            ("LOCALAPPDATA".to_string(), show(&local)),
            ("TEMP".to_string(), show(&temp)),
            ("TMP".to_string(), show(&temp)),
        ]
    }

    /// `suite -- --list`, as a set of tests.
    fn list(&self, tree: &Path, task: &Task, suite: &[String]) -> Result<BTreeSet<String>, String> {
        let mut argv = suite.to_vec();
        argv.extend(["--".to_string(), "--list".to_string()]);
        let o = cargo(tree, Some(task), &argv, &self.env(), SCORE_TIMEOUT)?;
        if !o.passed() {
            return Err(format!(
                "could not list the tests: {}",
                failure_note(&o.output, Some(-1))
            ));
        }
        Ok(parse_list(&o.output))
    }

    /// Score `patch` (none: the pin as it is) in `tree`, which must be clean at the pin. Hidden
    /// tests are added, the check run with the hidden filter, then every suite, then the list of
    /// the pin's tests is looked for in the result.
    fn score(&mut self, tree: &Path, task: &Task, patch: Option<&Path>) -> Judged {
        match self.try_score(tree, task, patch) {
            Ok(judged) => judged,
            Err(why) => Judged::Error(why),
        }
    }

    fn try_score(&mut self, tree: &Path, task: &Task, patch: Option<&Path>) -> Result<Judged, String> {
        let env = self.env();
        if !self.pins.contains_key(&task.id) {
            let mut all = BTreeSet::new();
            for suite in &task.suites {
                all.extend(self.list(tree, task, suite).map_err(|e| format!("the pin: {e}"))?);
            }
            self.pins.insert(task.id.clone(), all);
        }

        if let Some(patch) = patch {
            let out = git(Some(tree), &["apply", "--whitespace=nowarn", &patch.to_string_lossy()])?;
            if !out.status.success() {
                return Ok(Judged::Wrong(format!(
                    "patch did not apply: {}",
                    first_line(&out.stderr)
                )));
            }
        }

        for h in &task.hidden {
            let (rel, src, append) = match h {
                Hidden::Append { file, src } => (file, src, true),
                Hidden::File { path, src } => (path, src, false),
            };
            let src = std::fs::read_to_string(task.dir.join(src))
                .map_err(|e| format!("{}: {e}", src))?
                .replace("\r\n", "\n");
            let at = tree.join(rel);
            let body = if append {
                match std::fs::read_to_string(&at) {
                    Ok(cur) => format!("{}\n\n{src}", cur.trim_end_matches('\n')),
                    Err(e) => return Ok(Judged::Wrong(format!("{rel} cannot be read after the patch: {e}"))),
                }
            } else {
                if let Some(parent) = at.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
                }
                src
            };
            std::fs::write(&at, body).map_err(|e| format!("{}: {e}", at.display()))?;
        }

        let o = match cargo_tests(tree, task, &task.check, Some(&task.hidden_filter), &env)? {
            Ran::Hung => {
                return Ok(Judged::Wrong(format!(
                    "hidden tests: tests hung (still running after {}s)",
                    TEST_TIMEOUT.as_secs()
                )));
            }
            Ran::Done(o) => o,
        };
        let code = match o.ended {
            check::Ended::Exited(code) => code,
            _ => None,
        };
        if let Err(why) = judge_hidden(&o.output, code, task.hidden_count) {
            return Ok(Judged::Wrong(why));
        }

        for suite in &task.suites {
            let o = match cargo_tests(tree, task, suite, None, &env)? {
                Ran::Hung => {
                    return Ok(Judged::Wrong(format!(
                        "`{}` fails: tests hung (still running after {}s)",
                        suite.join(" "),
                        TEST_TIMEOUT.as_secs()
                    )));
                }
                Ran::Done(o) => o,
            };
            let code = match o.ended {
                check::Ended::Exited(code) => code,
                _ => None,
            };
            if code != Some(0) || tally(&o.output).cut_short() {
                let why = if code == Some(0) {
                    "a test program ended without reporting its result".to_string()
                } else {
                    failure_note(&o.output, code)
                };
                return Ok(Judged::Wrong(format!("`{}` fails: {why}", suite.join(" "))));
            }
        }

        let mut now = BTreeSet::new();
        for suite in &task.suites {
            match self.list(tree, task, suite) {
                Ok(tests) => now.extend(tests),
                Err(e) => return Ok(Judged::Wrong(format!("after the patch: {e}"))),
            }
        }
        let missing: Vec<&String> = self.pins[&task.id].difference(&now).collect();
        if !missing.is_empty() {
            let shown = missing
                .iter()
                .take(3)
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let more = if missing.len() > 3 {
                format!(" (+{} more)", missing.len() - 3)
            } else {
                String::new()
            };
            return Ok(Judged::Wrong(format!(
                "{} test(s) of the pinned tree are gone: {shown}{more}",
                missing.len()
            )));
        }
        Ok(Judged::Correct)
    }
}

// ── the verdict ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Pass,
    CorrectNotDone,
    FalseDone,
    Fail,
    Error,
}

impl Verdict {
    const ALL: [Verdict; 5] = [
        Verdict::Pass,
        Verdict::CorrectNotDone,
        Verdict::FalseDone,
        Verdict::Fail,
        Verdict::Error,
    ];

    fn label(self) -> &'static str {
        match self {
            Verdict::Pass => "PASS",
            Verdict::CorrectNotDone => "CORRECT-NOT-DONE",
            Verdict::FalseDone => "FALSE-DONE",
            Verdict::Fail => "FAIL",
            Verdict::Error => "ERROR",
        }
    }
}

/// The verdict for what scoring said and what Hugin's status was. Correct and done is a PASS;
/// correct and anything else is CORRECT-NOT-DONE; not correct and done is FALSE-DONE, the
/// model's claim that did not hold; not correct otherwise is a FAIL; scoring that failed is an
/// ERROR whatever Hugin said.
fn book(status: &str, judged: &Judged) -> (Verdict, String) {
    let done = status == "done";
    match judged {
        Judged::Error(why) => (Verdict::Error, why.clone()),
        Judged::Correct if done => (
            Verdict::Pass,
            "the hidden tests, the suites and the pinned tests all hold".to_string(),
        ),
        Judged::Correct => (Verdict::CorrectNotDone, format!("correct, but Hugin said {status}")),
        Judged::Wrong(why) if done => (Verdict::FalseDone, why.clone()),
        Judged::Wrong(why) => (Verdict::Fail, why.clone()),
    }
}

// ── Hugin ────────────────────────────────────────────────────────────────────

/// What a `hugin -p --json` run said.
#[derive(Debug, Default, PartialEq)]
struct HuginReport {
    status: String,
    warnings: usize,
    seconds: f64,
    error: Option<String>,
    patch_file: Option<PathBuf>,
    diff: String,
    diff_omitted: bool,
}

/// The report in Hugin's output: the last line that is a JSON object with a `status`. Hugin's
/// stdout and stderr arrive interleaved, so it is looked for, not assumed to be the whole.
fn parse_report(output: &str) -> Result<HuginReport, String> {
    for line in output.lines().rev() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(status) = v.get("status").and_then(Value::as_str) else {
            continue;
        };
        return Ok(HuginReport {
            status: status.to_string(),
            warnings: v.get("warnings").and_then(Value::as_array).map_or(0, Vec::len),
            seconds: v.get("seconds").and_then(Value::as_f64).unwrap_or(0.0),
            error: v.get("error").and_then(Value::as_str).map(str::to_string),
            patch_file: v.get("patch_file").and_then(Value::as_str).map(PathBuf::from),
            diff: v.get("diff").and_then(Value::as_str).unwrap_or("").to_string(),
            diff_omitted: v.get("diff_omitted").and_then(Value::as_bool).unwrap_or(false),
        });
    }
    Err("Hugin printed no JSON report".to_string())
}

/// The patch to score. Hugin keeps the patch byte for byte in `patch_file` whenever there is one,
/// and prints the diff itself only up to 16 KB: the file is read first, the printed diff is the
/// fallback, and a diff that was left out with no file to read is an ERROR. `None`: the model
/// changed nothing.
fn patch_of(report: &HuginReport, spare: &Path) -> Result<Option<PathBuf>, String> {
    if let Some(file) = &report.patch_file
        && file.is_file()
    {
        return Ok(Some(file.clone()));
    }
    if !report.diff.is_empty() {
        std::fs::write(spare, &report.diff).map_err(|e| format!("could not write {}: {e}", spare.display()))?;
        return Ok(Some(spare.to_path_buf()));
    }
    if report.diff_omitted || report.patch_file.is_some() {
        return Err("Hugin's patch is not where it said".to_string());
    }
    Ok(None)
}

/// A command's words as `--check` reads them: quoted where a word has a space.
fn join_check(argv: &[String]) -> String {
    argv.iter()
        .map(|w| {
            if w.is_empty() || w.chars().any(char::is_whitespace) {
                format!("\"{w}\"")
            } else {
                w.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The check Hugin is given for the task.
fn model_check(task: &Task) -> String {
    join_check(&visible_check(task))
}

/// The task's check as the model may run it: without any `--test <name>` that names a hidden
/// test file. That target does not exist in the model's tree, so cargo would refuse every check,
/// and its name would give away a hidden test.
fn visible_check(task: &Task) -> Vec<String> {
    let hidden: Vec<&str> = task
        .hidden
        .iter()
        .filter_map(|h| match h {
            Hidden::File { path, .. } => Path::new(path).file_stem().and_then(|s| s.to_str()),
            Hidden::Append { .. } => None,
        })
        .collect();
    let mut out = Vec::new();
    let mut words = task.check.iter().peekable();
    while let Some(w) = words.next() {
        if w == "--test" && words.peek().is_some_and(|n| hidden.contains(&n.as_str())) {
            words.next();
            continue;
        }
        out.push(w.clone());
    }
    out
}

// ── one run ──────────────────────────────────────────────────────────────────

struct Ctx {
    repo: PathBuf,
    scratch: PathBuf,
    keep: bool,
}

#[derive(Debug, Clone)]
struct RunResult {
    task: String,
    run: usize,
    verdict: Verdict,
    why: String,
    status: String,
    seconds: f64,
    warnings: usize,
}

impl RunResult {
    fn under_540s(&self) -> Option<bool> {
        (self.verdict == Verdict::Pass).then_some(self.seconds <= UNDER_SECS)
    }
}

/// Hugin on the task in a fresh worktree at the pin.
fn run_hugin(
    ctx: &Ctx,
    hugin: &Path,
    sandbox: &Sandbox,
    task: &Task,
    run: usize,
    model: &str,
) -> Result<HuginReport, String> {
    let work = Worktree::add(
        &ctx.scratch.join(format!("work-{}-{run}", task.id)),
        &task.pin,
        ctx.keep,
    )?;
    prepare(&ctx.repo, &work.path, task)?;
    let text = std::fs::read(task.dir.join("task.txt")).map_err(|e| format!("task.txt: {e}"))?;
    let argv: Vec<String> = [
        &hugin.to_string_lossy(),
        "-p",
        "-",
        "--check",
        &model_check(task),
        "--json",
        "--time",
        &HUGIN_TIME.to_string(),
        "--model",
        model,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let ran = check::run_with_input(&work.path, &argv, &text, KEEP, HUGIN_GUARD, &sandbox.env, || false);
    // Its check built in a cache of its own: the patch is all that is read from here on.
    sandbox.clear_build_cache();
    let o = ran?;
    match &o.ended {
        check::Ended::TimedOut => {
            return Err(format!(
                "Hugin ran past {}s and its process tree was killed",
                HUGIN_GUARD.as_secs()
            ));
        }
        check::Ended::Stopped => return Err("Hugin was stopped".to_string()),
        check::Ended::DiskFull => return Err("Hugin was stopped for a nearly full disk".to_string()),
        check::Ended::Exited(_) => {}
    }
    // Hugin's own report wins: its exit code 3 with a check that Windows blocked still carries a
    // patch worth scoring. Without a report, a block is the reason.
    parse_report(&o.output).map_err(|e| {
        if let Some(what) = &o.blocked {
            return format!("Windows blocked {what}");
        }
        let tail: String = o
            .output
            .chars()
            .rev()
            .take(300)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("{e} (exit {:?}): {}", o.ended, tail.trim())
    })
}

fn run_one(
    ctx: &Ctx,
    scorer: &mut Scorer,
    hugin: &Path,
    sandbox: &Sandbox,
    task: &Task,
    run: usize,
    model: &str,
) -> RunResult {
    let result = |verdict, why: String, status: &str, seconds, warnings| RunResult {
        task: task.id.clone(),
        run,
        verdict,
        why,
        status: status.to_string(),
        seconds,
        warnings,
    };
    let started = Instant::now();
    let report = match run_hugin(ctx, hugin, sandbox, task, run, model) {
        Ok(r) => r,
        Err(why) => return result(Verdict::Error, why, "-", started.elapsed().as_secs_f64(), 0),
    };
    let booked = |verdict, why| result(verdict, why, &report.status, report.seconds, report.warnings);
    if report.status == "error" {
        return booked(
            Verdict::Error,
            format!(
                "Hugin made no proposal: {}",
                report.error.as_deref().unwrap_or("no reason given")
            ),
        );
    }
    let spare = ctx.scratch.join(format!("diff-{}-{run}.patch", task.id));
    let patch = match patch_of(&report, &spare) {
        Ok(p) => p,
        Err(why) => return booked(Verdict::Error, why),
    };
    let judged = match patch {
        None => Judged::Wrong("no patch: the model changed nothing".to_string()),
        Some(patch) => match Worktree::add(&ctx.scratch.join("score"), &task.pin, ctx.keep) {
            Err(why) => Judged::Error(why),
            Ok(tree) => match prepare(&ctx.repo, &tree.path, task) {
                Err(why) => Judged::Error(why),
                Ok(()) => scorer.score(&tree.path, task, Some(&patch)),
            },
        },
    };
    let (verdict, why) = book(&report.status, &judged);
    booked(verdict, why)
}

// ── reports ──────────────────────────────────────────────────────────────────

fn count(results: &[RunResult], verdict: Verdict) -> usize {
    results.iter().filter(|r| r.verdict == verdict).count()
}

/// The table at the end: runs by verdict, then the false-done count on a line of its own.
fn summary(results: &[RunResult]) -> Vec<String> {
    let mut lines = vec!["| verdict | runs |".to_string(), "|---|---|".to_string()];
    for v in Verdict::ALL {
        lines.push(format!("| {} | {} |", v.label(), count(results, v)));
    }
    lines.push(format!("| total | {} |", results.len()));
    let passes = count(results, Verdict::Pass);
    let quick = results.iter().filter(|r| r.under_540s() == Some(true)).count();
    lines.push(String::new());
    lines.push(format!(
        "passes: {passes} of {} runs, {quick} of them within {UNDER_SECS:.0}s",
        results.len()
    ));
    lines.push(format!(
        "false-done: {} of {} runs (Hugin said done, the hidden tests say it is not)",
        count(results, Verdict::FalseDone),
        results.len()
    ));
    lines
}

fn results_json(model: &str, results: &[RunResult]) -> Value {
    let runs: Vec<Value> = results
        .iter()
        .map(|r| {
            serde_json::json!({
                "task": r.task, "run": r.run, "verdict": r.verdict.label(), "why": r.why, "status": r.status,
                "seconds": (r.seconds * 10.0).round() / 10.0, "warnings": r.warnings, "under_540s": r.under_540s(),
            })
        })
        .collect();
    serde_json::json!({ "model": model, "runs": runs })
}

// ── selfcheck ────────────────────────────────────────────────────────────────

/// What at the pin gives a task's hidden tests away, if anything: a hidden file already in the
/// tree, the hidden module already in an appended file, or the task folder itself.
fn hidden_in_pin(task: &Task) -> Result<Vec<String>, String> {
    let rev = format!("{}^{{commit}}", task.pin);
    let ok = git(None, &["rev-parse", "--verify", &rev])?;
    if !ok.status.success() {
        return Err(format!("the pin {} is not a commit here", task.pin));
    }
    let listed = git(None, &["ls-tree", "-r", "--name-only", &task.pin])?;
    if !listed.status.success() {
        return Err(format!("git ls-tree failed: {}", first_line(&listed.stderr)));
    }
    let files = String::from_utf8_lossy(&listed.stdout).into_owned();
    let mut found = Vec::new();
    if let Some(line) = files.lines().find(|l| l.starts_with("evals/realprobe/")) {
        found.push(format!("{line} is in the pinned tree"));
    }
    for h in &task.hidden {
        match h {
            Hidden::File { path, .. } if files.lines().any(|l| l == path) => {
                found.push(format!("{path} is in the pinned tree"))
            }
            Hidden::Append { file, .. } => {
                let shown = git(None, &["show", &format!("{}:{file}", task.pin)])?;
                if String::from_utf8_lossy(&shown.stdout).contains(&task.hidden_filter) {
                    found.push(format!("{file} already holds {}", task.hidden_filter));
                }
            }
            Hidden::File { .. } => {}
        }
    }
    Ok(found)
}

/// The existing tests a patch removes lines from, as Hugin reads them (`apps/hugin/src/bent.rs`),
/// one note each. Hugin sets any such patch's status to `review`, so a reference that does it
/// could never be a PASS.
fn tests_edited_by(patch: &[u8]) -> Vec<String> {
    bent::changed_tests(patch)
        .iter()
        .map(|b| format!("the reference patch removes lines of existing tests — {}", b.warning()))
        .collect()
}

/// [`tests_edited_by`] for the task's `reference.patch`.
fn reference_edits_tests(task: &Task) -> Result<Vec<String>, String> {
    let path = task.dir.join("reference.patch");
    let patch = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(tests_edited_by(&patch))
}

fn selfcheck(ctx: &Ctx, tasks: &[Task]) -> bool {
    println!("J1 — realprobe selfcheck (no model, no Hugin)\n");
    println!(
        "| task | reference correct? | pin is FAIL? | wrong patches FAIL | hidden unreachable? | reference edits no test? |"
    );
    println!("|---|---|---|---|---|---|");
    let mut scorer = Scorer::new(&ctx.repo, &ctx.scratch);
    let mut all_ok = true;
    for task in tasks {
        let mut notes: Vec<String> = Vec::new();
        fn cell(ok: bool, what: &str, judged: &Judged, notes: &mut Vec<String>) -> String {
            if !ok {
                notes.push(format!("{what}: {judged:?}"));
            }
            if ok { "yes".to_string() } else { "NO".to_string() }
        }

        let mut reference = "NO".to_string();
        let mut pin = "NO".to_string();
        let mut wrongs = format!("0/{}", task.wrong.len());
        match Worktree::add(&ctx.scratch.join("score"), &task.pin, ctx.keep)
            .and_then(|t| prepare(&ctx.repo, &t.path, task).map(|()| t))
        {
            Err(e) => notes.push(format!("could not check out the pin: {e}")),
            Ok(tree) => {
                let j = scorer.score(&tree.path, task, Some(&task.dir.join("reference.patch")));
                reference = cell(j == Judged::Correct, "reference", &j, &mut notes);

                let reset =
                    |notes: &mut Vec<String>| tree.reset().map_err(|e| notes.push(format!("reset: {e}"))).is_ok();
                if reset(&mut notes) {
                    let j = scorer.score(&tree.path, task, None);
                    pin = cell(matches!(j, Judged::Wrong(_)), "pin", &j, &mut notes);
                }
                let mut good = 0;
                for w in &task.wrong {
                    if !reset(&mut notes) {
                        break;
                    }
                    let j = scorer.score(&tree.path, task, Some(&task.dir.join(&w.patch)));
                    if matches!(j, Judged::Wrong(_)) {
                        good += 1;
                    } else {
                        notes.push(format!("{} ({}): {j:?}", w.patch, w.why));
                    }
                }
                wrongs = format!("{good}/{}", task.wrong.len());
                if good != task.wrong.len() {
                    wrongs.push_str(" NO");
                }
            }
        }
        let hidden = match hidden_in_pin(task) {
            Ok(found) if found.is_empty() => "yes".to_string(),
            Ok(found) => {
                notes.extend(found);
                "NO".to_string()
            }
            Err(e) => {
                notes.push(e);
                "NO".to_string()
            }
        };
        let untouched = match reference_edits_tests(task) {
            Ok(edited) if edited.is_empty() => "yes".to_string(),
            Ok(edited) => {
                notes.extend(edited);
                "NO".to_string()
            }
            Err(e) => {
                notes.push(e);
                "NO".to_string()
            }
        };
        let row_ok = reference == "yes"
            && pin == "yes"
            && wrongs == format!("{0}/{0}", task.wrong.len())
            && hidden == "yes"
            && untouched == "yes";
        all_ok &= row_ok;
        println!(
            "| {} | {reference} | {pin} | {wrongs} | {hidden} | {untouched} |",
            task.id
        );
        for n in notes {
            println!("    {}: {n}", task.id);
        }
    }

    let refused = require_installed(&["qwen3:8b".to_string()], NO_SUCH_MODEL).is_err()
        && require_installed(&[], NO_SUCH_MODEL).is_err()
        && require_installed(&["qwen3:8b".to_string()], "qwen3:8b").is_ok();
    all_ok &= refused;
    println!(
        "\n| a model Ollama does not list is refused before any work | {} |",
        if refused { "yes" } else { "NO" }
    );
    println!("\nselfcheck: {}", if all_ok { "all hold" } else { "FAILED" });
    all_ok
}

// ── main ─────────────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
struct Opts {
    model: Option<String>,
    runs: usize,
    only: Option<String>,
    think: Option<Think>,
    keep: bool,
    selfcheck: bool,
}

#[derive(Debug, PartialEq)]
enum Cmd {
    Help,
    Run(Opts),
}

fn parse_args(args: &[String]) -> Result<Cmd, String> {
    let mut o = Opts {
        model: None,
        runs: 1,
        only: None,
        think: None,
        keep: false,
        selfcheck: false,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = |flag: &str, what: &str| it.next().cloned().ok_or_else(|| format!("{flag} needs {what}"));
        match arg.as_str() {
            "--help" | "-h" => return Ok(Cmd::Help),
            "--keep" => o.keep = true,
            "--selfcheck" => o.selfcheck = true,
            "--model" => {
                let tag = value("--model", "a model tag")?;
                if tag.trim().is_empty() {
                    return Err("--model needs a model tag".to_string());
                }
                o.model = Some(tag.trim().to_string());
            }
            "--runs" => {
                let n = value("--runs", "a number")?;
                o.runs = n
                    .parse()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| format!("--runs takes a number above 0, not {n}"))?;
            }
            "--only" => o.only = Some(value("--only", "an id prefix")?),
            "--think" => {
                let word = value("--think", "off, low, medium or high")?;
                o.think = Some(
                    Think::parse(&word).ok_or_else(|| format!("--think takes off, low, medium or high, not {word}"))?,
                );
            }
            other if other.starts_with('-') => return Err(format!("unknown flag: {other}")),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    if o.model.is_none() && !o.selfcheck {
        return Err("--model <tag> is required".to_string());
    }
    Ok(Cmd::Run(o))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(real_main(&args));
}

/// Everything, so that every worktree and the scratch folder are dropped before the process exits.
fn real_main(args: &[String]) -> i32 {
    let opts = match parse_args(args) {
        Ok(Cmd::Help) => {
            print!("{USAGE}");
            return 0;
        }
        Ok(Cmd::Run(o)) => o,
        Err(e) => {
            eprintln!("{e}\n\n{USAGE}");
            return 2;
        }
    };
    let repo = std::env::current_dir().unwrap_or_default();
    let tasks: Vec<Task> = match load_tasks(&repo) {
        Ok(t) => t
            .into_iter()
            .filter(|t| opts.only.as_deref().is_none_or(|o| t.id.starts_with(o)))
            .collect(),
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    if tasks.is_empty() {
        eprintln!("no task matches --only {}", opts.only.as_deref().unwrap_or(""));
        return 2;
    }

    let scratch = std::env::temp_dir().join(format!("bifrost-realprobe-{}", std::process::id()));
    let ctx = Ctx {
        repo,
        scratch,
        keep: opts.keep,
    };
    let code = if opts.selfcheck {
        i32::from(!selfcheck(&ctx, &tasks))
    } else {
        real_run(&ctx, &opts, &tasks)
    };
    if opts.keep {
        println!("kept {}", ctx.scratch.display());
    } else {
        let _ = std::fs::remove_dir_all(&ctx.scratch);
        let _ = git(None, &["worktree", "prune"]);
    }
    code
}

fn real_run(ctx: &Ctx, opts: &Opts, tasks: &[Task]) -> i32 {
    let model = opts.model.clone().unwrap_or_default();

    // Started if it is not running, as the apps and the CLI do. Then the model must be one Ollama
    // has: a run against a tag it does not list would spend its hours failing at the first request.
    if let Err(e) = ollama::ensure_running() {
        eprintln!("could not start Ollama: {e}");
        return 1;
    }
    let installed: Vec<String> = match ollama::tags() {
        Ok(models) => models.into_iter().map(|m| m.name).collect(),
        Err(e) => {
            eprintln!("could not ask Ollama what is installed: {e}");
            return 1;
        }
    };
    if let Err(e) = require_installed(&installed, &model) {
        eprintln!("{e}");
        return 1;
    }

    // Hugin: cargo itself says whether the copy in target/ is stale (it is a no-op when it is
    // not), so there is no second opinion here about which sources count. Built in the repo's own
    // target/, then copied away so a later build cannot replace the binary mid-run.
    let target = ctx.repo.join("target");
    let build_env = vec![("CARGO_TARGET_DIR".to_string(), target.to_string_lossy().into_owned())];
    let build = ["cargo", "build", "-p", "hugin"].map(String::from);
    match cargo(&ctx.repo, None, &build, &build_env, BUILD_TIMEOUT) {
        Ok(o) if o.passed() => {}
        Ok(o) => {
            eprintln!("cargo build -p hugin failed: {}", failure_note(&o.output, Some(-1)));
            return 1;
        }
        Err(e) => {
            eprintln!("cargo build -p hugin: {e}");
            return 1;
        }
    }
    let hugin = match copy_hugin(&target, &ctx.scratch) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let sandbox = match Sandbox::make(&ctx.scratch.join("sandbox"), &model, opts.think) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };

    println!("J1 — realprobe");
    println!("model:  {model}");
    println!("think:  {}", opts.think.map_or("unset (the model's own)", Think::name));
    println!("tasks:  {} × {} runs", tasks.len(), opts.runs);
    println!(
        "hugin:  {} (--time {HUGIN_TIME}, killed past {}s)\n",
        hugin.display(),
        HUGIN_GUARD.as_secs()
    );

    let mut scorer = Scorer::new(&ctx.repo, &ctx.scratch);
    let mut results: Vec<RunResult> = Vec::new();
    for task in tasks {
        for run in 1..=opts.runs {
            let r = run_one(ctx, &mut scorer, &hugin, &sandbox, task, run, &model);
            println!(
                "{:<17} {} run {}: status {}, {:.0}s, {} warning(s) — {}",
                r.verdict.label(),
                r.task,
                r.run,
                r.status,
                r.seconds,
                r.warnings,
                r.why
            );
            results.push(r);
        }
    }

    println!("\n─── {model} ───");
    for line in summary(&results) {
        println!("{line}");
    }
    let out = ctx
        .repo
        .join("evals")
        .join("out")
        .join(format!("J1-real-{}", unix_now()));
    if std::fs::create_dir_all(&out).is_ok() {
        let file = out.join("results.json");
        let _ = std::fs::write(
            &file,
            serde_json::to_string_pretty(&results_json(&model, &results)).unwrap_or_default(),
        );
        println!("\nsaved {}", file.display());
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::sandbox_config;

    fn strings(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    const TASK_JSON: &str = r#"{
      "id": "01-x", "pin": "0940923",
      "check": ["cargo", "test", "-p", "bench", "--no-fail-fast"],
      "hidden": [
        {"kind": "append", "file": "crates/bench/src/bin/taskprobe.rs", "src": "hidden.rs"},
        {"kind": "file", "path": "crates/bench/tests/realprobe_x.rs", "src": "hidden_cli.rs"}
      ],
      "hidden_count": 9, "hidden_filter": "realprobe_hidden",
      "suites": [["cargo", "test", "-p", "bench"]],
      "wrong": [{"patch": "wrong-1.patch", "why": "exits 1"}]
    }"#;

    #[test]
    fn a_task_json_parses_with_both_kinds_of_hidden_entry() {
        let t = parse_task(TASK_JSON, Path::new("evals/realprobe/01-x")).unwrap();
        assert_eq!((t.id.as_str(), t.pin.as_str(), t.hidden_count), ("01-x", "0940923", 9));
        assert_eq!(t.check, strings(&["cargo", "test", "-p", "bench", "--no-fail-fast"]));
        assert_eq!(
            t.hidden,
            vec![
                Hidden::Append {
                    file: "crates/bench/src/bin/taskprobe.rs".into(),
                    src: "hidden.rs".into()
                },
                Hidden::File {
                    path: "crates/bench/tests/realprobe_x.rs".into(),
                    src: "hidden_cli.rs".into()
                },
            ]
        );
        assert_eq!(t.suites, vec![strings(&["cargo", "test", "-p", "bench"])]);
        assert_eq!(
            (t.wrong[0].patch.as_str(), t.wrong[0].why.as_str()),
            ("wrong-1.patch", "exits 1")
        );
        assert_eq!(t.hidden_filter, "realprobe_hidden");
    }

    #[test]
    fn a_task_json_that_is_wrong_is_refused() {
        let dir = Path::new("x");
        assert!(parse_task("nope", dir).is_err());
        assert!(
            parse_task(&TASK_JSON.replace("append", "prepend"), dir)
                .unwrap_err()
                .contains("prepend")
        );
        assert!(
            parse_task(&TASK_JSON.replace("\"hidden_count\": 9,", ""), dir)
                .unwrap_err()
                .contains("hidden_count")
        );
        assert!(
            parse_task(&TASK_JSON.replace("\"pin\": \"0940923\",", ""), dir)
                .unwrap_err()
                .contains("pin")
        );
    }

    #[test]
    fn every_task_in_the_repository_parses() {
        // Cargo runs a package's tests with the package folder as the current directory.
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let tasks = load_tasks(&repo).unwrap();
        assert_eq!(tasks.len(), 10);
        assert!(
            tasks
                .iter()
                .all(|t| t.pin == "0940923" && t.hidden_count > 0 && !t.wrong.is_empty())
        );
        assert_eq!(
            tasks.iter().filter(|t| t.hidden.len() == 2).count(),
            2,
            "tasks 01 and 05 have a file as well"
        );
    }

    #[test]
    fn counts_are_summed_over_every_test_result_line() {
        let out = "running 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 12 filtered out; finished in 0.00s\n\n\
                   running 5 tests\ntest a ... ok\n\ntest result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 3 filtered out; finished in 0.10s\n\n\
                   running 4 tests\n\ntest result: FAILED. 3 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.10s\n";
        let t = tally(out);
        assert_eq!(
            t,
            Tally {
                passed: 8,
                failed: 1,
                began: 3,
                reported: 3
            }
        );
        assert!(!t.cut_short());
        assert!(
            judge_hidden(out, Some(101), 8)
                .unwrap_err()
                .contains("8 passed, 1 failed")
        );
    }

    #[test]
    fn a_test_program_cut_short_is_not_a_pass() {
        let out = "running 5 tests\ntest result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0s\n\nrunning 4 tests\ntest a ... ok\n";
        let t = tally(out);
        assert!(t.cut_short());
        assert!(judge_hidden(out, Some(0), 5).unwrap_err().contains("without reporting"));
        // Exactly the wanted count, a clean exit, nothing cut short: correct.
        let whole = "running 2 tests\ntest result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0s\n\
                     running 7 tests\ntest result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0s\n";
        assert!(judge_hidden(whole, Some(0), 9).is_ok());
        assert!(judge_hidden(whole, Some(0), 10).unwrap_err().contains("9 passed"));
        assert!(judge_hidden(whole, None, 9).is_err());
        let compile = "error[E0425]: cannot find value `x` in this scope\nerror: could not compile `bench`\n";
        assert!(
            judge_hidden(compile, Some(101), 9)
                .unwrap_err()
                .contains("does not compile: error[E0425]")
        );
    }

    #[test]
    fn the_list_names_each_test_with_its_program_and_leaves_out_doc_tests() {
        let out = "   Compiling bench v0.1.0\n     Running unittests src\\main.rs (target\\debug\\deps\\bench-aaaa.exe)\ntests::one: test\ntests::two: test\n\n\
                   2 tests, 0 benchmarks\n     Running tests\\cli.rs (target\\debug\\deps\\cli-bbbb.exe)\ntests::one: test\n\
                   \x20  Doc-tests bench\nsrc/lib.rs - f (line 3): test\n";
        let set = parse_list(out);
        let got: Vec<&str> = set.iter().map(String::as_str).collect();
        assert_eq!(
            got,
            [
                "tests\\cli.rs :: tests::one",
                "unittests src\\main.rs :: tests::one",
                "unittests src\\main.rs :: tests::two"
            ]
        );
    }

    fn done(v: Verdict, status: &str, j: Judged) -> Verdict {
        let (got, _) = book(status, &j);
        assert_eq!(got, v, "{status} {j:?}");
        got
    }

    #[test]
    fn the_verdict_table_has_all_five() {
        let wrong = || Judged::Wrong("x".into());
        assert_eq!(done(Verdict::Pass, "done", Judged::Correct), Verdict::Pass);
        for status in ["failed", "review", "partial"] {
            done(Verdict::CorrectNotDone, status, Judged::Correct);
            done(Verdict::Fail, status, wrong());
        }
        done(Verdict::FalseDone, "done", wrong());
        for status in ["done", "failed"] {
            done(Verdict::Error, status, Judged::Error("blocked".into()));
        }
        let labels: Vec<&str> = Verdict::ALL.iter().map(|v| v.label()).collect();
        assert_eq!(labels, ["PASS", "CORRECT-NOT-DONE", "FALSE-DONE", "FAIL", "ERROR"]);
    }

    #[test]
    fn the_summary_counts_each_verdict_and_states_false_done_on_its_own() {
        let run = |verdict, seconds| RunResult {
            task: "t".into(),
            run: 1,
            verdict,
            why: String::new(),
            status: "done".into(),
            seconds,
            warnings: 0,
        };
        let results = [
            run(Verdict::Pass, 100.0),
            run(Verdict::Pass, 600.0),
            run(Verdict::FalseDone, 50.0),
            run(Verdict::Fail, 1.0),
            run(Verdict::Error, 1.0),
        ];
        let text = summary(&results).join("\n");
        assert!(
            text.contains("| PASS | 2 |")
                && text.contains("| FALSE-DONE | 1 |")
                && text.contains("| CORRECT-NOT-DONE | 0 |")
        );
        assert!(text.contains("| total | 5 |"));
        assert!(text.contains("2 of 5 runs, 1 of them within 540s"));
        assert!(text.contains("false-done: 1 of 5 runs"));
        let json = results_json("m", &results);
        assert_eq!(json["model"], "m");
        assert_eq!(json["runs"][1]["under_540s"], false);
        assert!(json["runs"][2]["under_540s"].is_null());
        assert_eq!(json["runs"][0]["verdict"], "PASS");
    }

    fn args(words: &[&str]) -> Result<Cmd, String> {
        parse_args(&strings(words))
    }

    #[test]
    fn flags_parse() {
        let Ok(Cmd::Run(o)) = args(&[
            "--model", "m:1", "--runs", "3", "--only", "03", "--think", "HIGH", "--keep",
        ]) else {
            panic!()
        };
        assert_eq!(
            (
                o.model.as_deref(),
                o.runs,
                o.only.as_deref(),
                o.think,
                o.keep,
                o.selfcheck
            ),
            (Some("m:1"), 3, Some("03"), Some(Think::High), true, false)
        );
        let Ok(Cmd::Run(o)) = args(&["--model", "m"]) else {
            panic!()
        };
        assert_eq!((o.runs, o.think, o.keep), (1, None, false));
        let Ok(Cmd::Run(o)) = args(&["--selfcheck"]) else {
            panic!()
        };
        assert!(o.selfcheck && o.model.is_none());
        assert_eq!(args(&["--help"]), Ok(Cmd::Help));
        assert_eq!(args(&["-h"]), Ok(Cmd::Help));
        // A value is the next word, even one that starts with a dash.
        let Ok(Cmd::Run(o)) = args(&["--model", "m", "--only", "-x"]) else {
            panic!()
        };
        assert_eq!(o.only.as_deref(), Some("-x"));
    }

    #[test]
    fn a_missing_model_and_a_bad_flag_are_refused() {
        assert!(args(&[]).unwrap_err().contains("--model"));
        assert!(args(&["--runs", "2"]).unwrap_err().contains("--model"));
        assert!(args(&["--model"]).unwrap_err().contains("needs a model tag"));
        assert_eq!(args(&["--model", "m", "--bogus"]).unwrap_err(), "unknown flag: --bogus");
        assert_eq!(args(&["-x"]).unwrap_err(), "unknown flag: -x");
        assert!(
            args(&["--model", "m", "stray"])
                .unwrap_err()
                .contains("unexpected argument: stray")
        );
        assert!(args(&["--model", "m", "--runs", "0"]).is_err());
        assert!(args(&["--model", "m", "--runs", "many"]).is_err());
        assert!(
            args(&["--model", "m", "--think", "max"])
                .unwrap_err()
                .contains("--think takes")
        );
        assert!(args(&["--model", " "]).is_err());
    }

    #[test]
    fn a_model_ollama_does_not_list_is_refused() {
        let installed = strings(&["qwen3-coder:30b", "Gemma3:latest"]);
        assert!(
            require_installed(&installed, NO_SUCH_MODEL)
                .unwrap_err()
                .contains("not installed")
        );
        assert!(require_installed(&[], "qwen3-coder:30b").is_err());
        assert!(require_installed(&installed, "qwen3-coder:30b").is_ok());
        assert!(require_installed(&installed, "QWEN3-coder:30B").is_ok());
        assert!(
            require_installed(&installed, "gemma3").is_ok(),
            "a name with no tag is its :latest"
        );
        assert!(
            require_installed(&installed, "qwen3-coder").is_err(),
            "…which is not another tag"
        );
    }

    #[test]
    fn hugin_is_given_the_check_and_the_sandbox_config() {
        assert_eq!(
            join_check(&strings(&["cargo", "test", "-p", "bench"])),
            "cargo test -p bench"
        );
        assert_eq!(join_check(&strings(&["cargo", "test", "a b"])), "cargo test \"a b\"");
        let plain: Value = serde_json::from_str(&sandbox_config("m:1", None)).unwrap();
        assert_eq!(plain, serde_json::json!({ "model": "m:1", "coding_model": "m:1" }));
        let high: Value = serde_json::from_str(&sandbox_config("m:1", Some(Think::High))).unwrap();
        assert_eq!(high["model_settings"]["m:1"]["think"], "high");
        assert_eq!(high["coding_model"], "m:1");
    }

    /// **The model's check never names a hidden test:** for tasks 01 and 05 the scoring check
    /// runs the hidden `--test` target, which the model's tree does not have.
    #[test]
    fn the_models_check_leaves_out_a_hidden_test_target() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for task in load_tasks(&repo).unwrap() {
            let check = model_check(&task);
            assert!(!check.contains("realprobe"), "{}: {check}", task.id);
        }
        let task = load_tasks(&repo)
            .unwrap()
            .into_iter()
            .find(|t| t.id == "01-taskprobe-help")
            .unwrap();
        assert_eq!(model_check(&task), "cargo test -p bench --bin taskprobe --no-fail-fast");
    }

    fn outcome(ended: check::Ended, output: &str) -> check::Outcome {
        check::Outcome {
            ended,
            elapsed: Duration::from_secs(1),
            output: output.to_string(),
            bytes: output.len() as u64,
            complete: true,
            blocked: None,
            unfinished_tests: 0,
        }
    }

    /// **A hung test is the model's FAIL:** a timeout after `running N tests` is a hang; one
    /// before it, or any other kind of end, is not.
    #[test]
    fn a_timeout_after_the_tests_began_is_a_hang() {
        let began =
            "   Finished `test` profile\n     Running unittests src\\lib.rs\n\nrunning 3 tests\ntest a ... ok\n";
        assert!(hung(&outcome(check::Ended::TimedOut, began)));
        assert!(
            !hung(&outcome(check::Ended::TimedOut, "   Compiling x v0.1.0\n")),
            "no test had begun: the environment's"
        );
        assert!(!hung(&outcome(check::Ended::Stopped, began)));
        assert!(!hung(&outcome(check::Ended::Exited(Some(0)), began)));
        // Still a timeout to `unfinished`, which is what `cargo` makes an ERROR of.
        assert!(unfinished(&outcome(check::Ended::TimedOut, began)).is_some());
        assert!(unfinished(&outcome(check::Ended::Exited(Some(101)), began)).is_none());
    }

    #[test]
    fn hugins_build_cache_is_deleted_and_the_config_is_kept() {
        let root = std::env::temp_dir().join(format!("realprobe-sandbox-test-{}", std::process::id()));
        let sandbox = Sandbox::make(&root, "m:1", None).unwrap();
        let cache = root
            .join("local")
            .join("bifrost")
            .join("hugin-p")
            .join("abc")
            .join("debug");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("big.rlib"), "x").unwrap();
        sandbox.clear_build_cache();
        assert!(!root.join("local").join("bifrost").join("hugin-p").exists());
        assert!(root.join("appdata").join("bifrost").join("config.json").is_file());
        sandbox.clear_build_cache(); // nothing there: no harm
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_scoring_runs_have_a_home_of_their_own() {
        let scratch = std::env::temp_dir().join(format!("realprobe-home-test-{}", std::process::id()));
        let env = Scorer::new(Path::new("repo"), &scratch).env();
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| PathBuf::from(v)).unwrap();
        for key in ["APPDATA", "LOCALAPPDATA", "TEMP", "TMP"] {
            assert!(get(key).starts_with(&scratch) && get(key).is_dir(), "{key}");
        }
        assert_eq!(get("TEMP"), get("TMP"));
        assert_ne!(get("APPDATA"), get("LOCALAPPDATA"));
        assert!(
            env.iter().any(|(k, v)| k == "CARGO_NET_OFFLINE" && v == "true"),
            "scoring must be offline"
        );
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// Hugin's own check, in the model's sandbox, is offline too.
    #[test]
    fn the_models_sandbox_is_offline() {
        let root = std::env::temp_dir().join(format!("realprobe-offline-test-{}", std::process::id()));
        let sandbox = harness::Sandbox::make(&root, "m:1", None).unwrap();
        assert!(sandbox.env.iter().any(|(k, v)| k == "CARGO_NET_OFFLINE" && v == "true"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_patch_that_removes_a_test_line_is_told_from_one_that_only_adds() {
        let adds = "diff --git a/crates/x/src/a.rs b/crates/x/src/a.rs\n--- a/crates/x/src/a.rs\n+++ b/crates/x/src/a.rs\n@@ -1,2 +1,3 @@\n fn f() {}\n+fn g() {}\n #[cfg(test)]\n";
        assert!(tests_edited_by(adds.as_bytes()).is_empty());
        let code = "diff --git a/crates/x/src/a.rs b/crates/x/src/a.rs\n--- a/crates/x/src/a.rs\n+++ b/crates/x/src/a.rs\n@@ -1,2 +1,2 @@\n-fn f() {}\n+fn f() -> u8 { 0 }\n";
        assert!(
            tests_edited_by(code.as_bytes()).is_empty(),
            "a changed line of the code is not a test edited"
        );
        let asserts = "diff --git a/crates/x/src/a.rs b/crates/x/src/a.rs\n--- a/crates/x/src/a.rs\n+++ b/crates/x/src/a.rs\n@@ -5,2 +5,2 @@\n-        assert_eq!(f(), 1);\n+        assert_eq!(f(), 2);\n";
        assert_eq!(tests_edited_by(asserts.as_bytes()).len(), 1);
        let file = "diff --git a/apps/x/tests/a.rs b/apps/x/tests/a.rs\n--- a/apps/x/tests/a.rs\n+++ b/apps/x/tests/a.rs\n@@ -1,1 +1,1 @@\n-let x = 1;\n+let x = 2;\n";
        assert!(tests_edited_by(file.as_bytes())[0].contains("apps/x/tests/a.rs (-1)"));
    }

    /// **A reference that edits an existing test can never be a PASS:** Hugin turns that patch
    /// into status `review`.
    #[test]
    fn no_reference_patch_edits_an_existing_test() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for task in load_tasks(&repo).unwrap() {
            let edited = reference_edits_tests(&task).unwrap();
            assert!(edited.is_empty(), "{}: {edited:?}", task.id);
        }
    }

    #[test]
    fn hugins_report_is_found_among_other_output_and_its_patch_is_read() {
        let json = r#"{"status":"review","warnings":["a","b"],"error":null,"patch_file":null,"seconds":12.5,"diff":"--- a\n","diff_omitted":false}"#;
        let r = parse_report(&format!("noise\n{json}\n")).unwrap();
        assert_eq!((r.status.as_str(), r.warnings, r.seconds), ("review", 2, 12.5));
        assert!(parse_report("no json here\n{not json}\n").is_err());

        let dir = std::env::temp_dir().join(format!("realprobe-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let spare = dir.join("spare.patch");
        // The printed diff is the fallback; the kept file wins when there is one.
        assert_eq!(patch_of(&r, &spare).unwrap().as_deref(), Some(spare.as_path()));
        assert_eq!(std::fs::read_to_string(&spare).unwrap(), "--- a\n");
        let kept = dir.join("kept.patch");
        std::fs::write(&kept, "kept").unwrap();
        let with_file = HuginReport {
            patch_file: Some(kept.clone()),
            diff_omitted: true,
            diff: String::new(),
            ..Default::default()
        };
        assert_eq!(patch_of(&with_file, &spare).unwrap().as_deref(), Some(kept.as_path()));
        let lost = HuginReport {
            patch_file: Some(dir.join("gone.patch")),
            diff_omitted: true,
            ..Default::default()
        };
        assert!(patch_of(&lost, &spare).is_err());
        assert_eq!(patch_of(&HuginReport::default(), &spare).unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
