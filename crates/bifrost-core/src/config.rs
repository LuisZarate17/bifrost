//! The settings all three binaries share, at `%APPDATA%\bifrost\config.json`
//! (see [`crate::paths`] for the folder and its move from `%APPDATA%\odin`).
//!
//! **A missing file is a normal state, not an error.** `load()` cannot fail: no
//! file yields the built-in defaults, and nothing here prints or panics. That is
//! deliberate and load-bearing — PLAN.md decision #5 requires Vidar to keep
//! working on a machine where Mimir was never installed, so "no config" has to be
//! the ordinary path rather than an error anyone has to handle.
//!
//! **A file that is there but cannot be read is not the same as no file.** One
//! that is not UTF-8 text, or is not a JSON object (a trailing comma, a
//! single-backslash path, a `//` comment, a half-finished write), still loads —
//! `load()` still cannot fail — but as the defaults *with `ask_every_call` on*,
//! because a typo must not silently mean "never ask". And before anything can
//! save over it, the file is copied to `config.json.bad` beside it, so a
//! hand-typed setting is never erased by the next unrelated save. A leading
//! byte-order mark (PowerShell 5.1's `Set-Content -Encoding UTF8` writes one) is
//! not a typo and is skipped. Saving goes through a temporary file beside it and a rename,
//! so a reader never sees a half-written file, and fields this build does not
//! know are kept.
//!
//! JSON rather than TOML so a frontend can read it directly later without a Rust
//! round trip, and `%APPDATA%` rather than anywhere repo-local, because a repo
//! path is wrong the moment something is installed instead of run out of a build
//! directory.
//!
//! Blocking, like the rest of this crate. Tauri callers go through
//! `spawn_blocking`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// Chosen at A2, where size decided accuracy and quantization decided nothing.
/// Reports `completion`, `vision` and `tools`, so one resident model can serve
/// the popup, the CLI and (from G6) tool calling.
pub const DEFAULT_MODEL: &str = "qwen3-vl:8b-instruct-q4_K_M";

/// With no question to read, this does all the intent-guessing (PLAN.md §2).
///
/// **This is the only copy in source.** It used to exist here, in the CLI's
/// `prompt.txt`, as an inline fallback in the CLI's `main.rs`, and as a `const`
/// in Vidar's `App.tsx` — four places to keep in step, which nothing enforced.
/// `evals/prompts/current.txt` is byte-identical and stays, because that one is
/// measured input for the bench rather than a copy of this.
pub const DEFAULT_PROMPT: &str =
    "What am I looking at? If there's an error or something broken, explain it \
     and the fix. Otherwise explain what matters here.";

/// Vidar's value, and now everyone's.
///
/// **Measured 2026-09-19.** Ollama keys a loaded runner by the model *and* by
/// `num_ctx`, so asking at a different context than you warmed at tears the
/// runner down and rebuilds it. The CLI used to warm at 8192 while Vidar asked
/// at 32768, and every switch between them cost about three seconds:
///
/// | request | `load_duration` |
/// |---|---|
/// | warm at 8192, cold | 4.96s |
/// | ask again at 8192 | **0.00s** — the warm survived |
/// | ask at 32768 | **3.09s** — reloaded |
/// | back to 8192 | **2.86s** — reloaded again |
///
/// **Why 32768 and not less, carried over from Vidar where this used to live.**
/// A 1080p screen costs ~2,070 image tokens — measured at A2 as 2,042, and
/// confirmed at 2,056/4,132/6,208/8,284 for one through four shots. Ollama
/// derives its own default from detected VRAM and lands at 4k on this card,
/// which would silently truncate even one screenshot, so it is always explicit.
/// 8192 was sized for a single picture; a conversation carrying four costs 8,284
/// tokens before a word of answer.
///
/// **And why not more.** Measured on the 5070 Ti, resident and fully on the GPU:
///
/// | num_ctx | VRAM     | on GPU |
/// |---------|----------|--------|
/// | 8192    | 6.09 GB  | yes    |
/// | 16384   | 7.23 GB  | yes    |
/// | 32768   | 9.51 GB  | yes    |
/// | 65536   | 12.74 GB | **no** — spills to system RAM |
///
/// 32768 is the last rung before the cliff, and room for far more context than
/// Vidar's four-screenshot cap will ever use.
///
/// `crates/bench` deliberately keeps its own 8192 constant instead of reading
/// this: it is a measurement harness, and changing its context would make F1's
/// saved runs incomparable to new ones.
pub const DEFAULT_NUM_CTX: u32 = 32768;
/// How long a shell command may run, by default (`command_timeout_secs`).
pub const DEFAULT_COMMAND_TIMEOUT_SECS: u64 = 120;
/// The shell Hugin's commands run in (`hugin_shell`). **Chosen at I3 (2026-10-15) by
/// cmdprobe**, eight tasks that need a command, on qwen3-coder. After the probe's two fixes
/// (a task's wording, and PowerShell's exit codes) Git Bash and Windows PowerShell tied,
/// 43/48 each over two runs, with cmd at 42 — and cmd's `find` was Git's, which walked
/// the whole drive. The tie goes to Git Bash: as fast, the shell Claude Code uses on
/// Windows, and the one whose one-liners got the line count right. Where Git Bash is not
/// installed Hugin uses PowerShell.
pub const DEFAULT_HUGIN_SHELL: &str = "bash";

/// Chosen at Gate 2 (2026-10-01) from the H4 benchmark: 25/48 against 6/48 for
/// `DEFAULT_MODEL`, at 32s/task. Text-only — reports no `vision` capability — so
/// it is never the model Vidar can use, and the two cannot be resident
/// together on this card: 18 GB plus the 6 GB vision model does not fit in 16 GB.
/// See `docs/archive/results/H4-results.md`.
pub const DEFAULT_CODING_MODEL: &str = "qwen3-coder:30b";

/// Ollama's `think` setting for a model (J0d): `off`, or how hard it reasons before it
/// answers. A model with no levels takes any level other than `off` as on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Think {
    Off,
    Low,
    Medium,
    High,
}

impl Think {
    /// The word for a level, in any case; anything else is none.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// What the config says about one model. Every field is optional: unset is the model's own.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ModelSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub think: Option<Think>,
}

/// What the apps agree on. Extended rather than replaced as Stage G lands —
/// G2 writes `model` here when you pick one, and Vidar reads it on every summon.
///
/// `#[serde(default)]` on the struct means a file missing any field fills that
/// field from `Default` rather than failing to parse. So a config written before
/// a field existed still loads, and a hand-edited file holding only `{"model":
/// "..."}` is valid. That is what makes adding a field later cheap.
///
/// Every field is also read on its own terms: a `null` or a value of the wrong
/// type gives *that field's* default, never the whole file's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    #[serde(deserialize_with = "lenient_model")]
    pub model: String,
    #[serde(deserialize_with = "lenient_prompt")]
    pub prompt: String,
    #[serde(deserialize_with = "lenient_num_ctx")]
    pub num_ctx: u32,
    /// The one folder Mimir may change files in, or `None` for no editing at
    /// all. Set only through a native folder picker the user operates — there is
    /// no command that takes a path for it, so nothing reaching the app's IPC can
    /// widen it. Vidar and the CLI ignore it.
    #[serde(deserialize_with = "lenient")]
    pub workspace: Option<String>,
    /// Folders chosen before, newest first, for Mimir's new-chat screen. Only
    /// the picker adds to it, and the webview can pick an entry only by its
    /// position — so choosing a recent folder never lets a path in from outside,
    /// and `workspace` stays something the user picked with their own hands.
    #[serde(deserialize_with = "lenient")]
    pub recent_workspaces: Vec<String>,
    /// The check command for each folder, keyed by the folder as `workspace`
    /// stores it — the command the model may run to find out whether its edits
    /// broke the build. Mimir writes an entry only from the candidates
    /// `check::candidates` detected, chosen by position, so no command typed in
    /// the window reaches this map. Anything else is added by hand. Look an entry
    /// up with [`Config::check_for`], which ignores case the way Windows paths do.
    ///
    /// **Read leniently**, because this is the one field people type by hand: an
    /// entry that is not a list of strings is dropped rather than failing the
    /// whole file — which would reset the model, the folder and everything else
    /// to defaults, and the next save would write that over the file.
    #[serde(deserialize_with = "lenient_checks")]
    pub checks: BTreeMap<String, Vec<String>>,
    /// The model Mimir offers to switch to once a project folder is chosen.
    /// An empty string turns the offer off entirely — there is no separate
    /// on/off flag, because an empty tag can never be installed and so
    /// [`crate::ollama::find`]-style lookups simply never match it.
    #[serde(deserialize_with = "lenient_coding_model")]
    pub coding_model: String,
    /// The model Mimir switched away from, on the user's say-so, to load
    /// `coding_model`. What "switch back" restores once the folder is
    /// cleared. Cleared by switching back, and by any manual model pick —
    /// a pick made by hand ends the switch-back story outright, rather than
    /// fighting it on the next folder change.
    #[serde(deserialize_with = "lenient")]
    pub model_before_coding: Option<String>,
    /// Ask before every tool call again, as G6 did, instead of letting reads,
    /// listings and searches inside the project folder run on their own (H5,
    /// see `tools::approval`). Off by default and set only by hand in this file.
    ///
    /// **Read leniently, and in the stricter direction.** `load()` answers a
    /// parse error with *every* default, so a strict bool would turn
    /// `"ask_every_call": "yes"` into "off" — the one outcome a person who typed
    /// it does not want. `true`, `"true"` (any case) and `1` mean on; `false`,
    /// `"false"` and `0` mean off; a missing field is off; anything else is on.
    #[serde(deserialize_with = "lenient_ask")]
    pub ask_every_call: bool,
    /// The folders where Mimir sends the project's own notes file (`AGENTS.md` and
    /// its kin, see `instructions`) with every question — **off for a folder until
    /// the user turns it on**, because the file is the project's text, and a cloned
    /// repository's author wrote it. Written only by the notes chip. Look a folder up
    /// with [`Config::instructions_for`], which ignores case like [`Config::check_for`].
    ///
    /// **Read leniently**, like `checks`: an entry that is not a string is dropped, and
    /// a field that is not a list means none — never a reset of the whole file.
    #[serde(deserialize_with = "lenient_instructions")]
    pub instructions: BTreeSet<String>,
    /// How long a shell command Hugin runs may take, in seconds, before it is killed.
    /// Set by hand; a value that is not a whole number is the default.
    #[serde(deserialize_with = "lenient_command_timeout")]
    pub command_timeout_secs: u64,
    /// The shell Hugin runs commands in: `powershell`, `cmd` or `bash`. Set by hand
    /// or with `--shell`; anything else is the default.
    #[serde(deserialize_with = "lenient_hugin_shell")]
    pub hugin_shell: String,
    /// The folder Hugin last opened its interactive interface in, which Ctrl+Alt+H and
    /// `hugin --launch` open it in again. Written by Hugin, never by line mode; anything
    /// that is not a string reads as none.
    #[serde(deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub hugin_last_folder: Option<String>,
    /// Whether the tray has ever offered "Start at login" and recorded the answer. The
    /// first run of the tray turns it on and sets this; after that only the tray's switch
    /// changes it. Anything that is not a bool reads as never recorded.
    #[serde(deserialize_with = "lenient")]
    pub bifrost_login_chosen: bool,
    /// Whether Mimir offers **Check with Claude** on a finished turn (I6). Off until
    /// the user turns it on in Mimir's settings; even on, nothing is sent until the
    /// button is pressed. **Read strictly in the safe direction**: anything that is
    /// not a bool is off.
    #[serde(deserialize_with = "lenient")]
    pub claude_checks: bool,
    /// Where the `claude` command is, when it is not found on its own — `PATH`, then
    /// `%USERPROFILE%\.local\bin\claude.exe`. Set by hand; anything that is not a
    /// string reads as none.
    #[serde(deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub claude_command: Option<String>,
    /// Settings for one model, keyed by its tag: for now only `think` (J0d). Look an entry
    /// up with [`Config::settings_for`], which ignores case. There is no per-model sampling:
    /// the app sends none, so each model's own settings apply.
    ///
    /// **Read leniently**, like `checks`: an entry that is not an object is dropped, and a
    /// `think` that is not `off`, `low`, `medium` or `high` is unset — never a reset of the
    /// whole file.
    #[serde(deserialize_with = "lenient_model_settings", skip_serializing_if = "BTreeMap::is_empty")]
    pub model_settings: BTreeMap<String, ModelSettings>,
    /// Your own "best at" labels for a model, keyed by its tag, shown in Mimir's model picker
    /// after the built-in ones. Look a model up with [`Config::tags_for`], which ignores case
    /// and treats `name` and `name:latest` as one model.
    ///
    /// **Read leniently**: an entry that is not a list is dropped, a label that is not text or
    /// is blank is dropped, and an entry left with no labels is dropped.
    #[serde(deserialize_with = "lenient_model_tags", skip_serializing_if = "BTreeMap::is_empty")]
    pub model_tags: BTreeMap<String, Vec<String>>,
    /// Fields this build does not know — a newer build's, or a person's own notes —
    /// kept as they were so the next save does not drop them.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
    /// **Set by [`load_from`] when the file is there but nothing of it was kept**:
    /// it could not be read at all (locked by another program, a sharing violation),
    /// or it did not parse and the copy to `config.json.bad` failed too. This value
    /// is then only the fail-closed defaults, and every caller loads, changes one
    /// thing and saves, so [`save_to`] refuses to write it: the real file, which is
    /// still on disk, is not replaced by defaults. Never read from or written to the
    /// file.
    #[serde(skip)]
    pub unsaved: bool,
}

/// A field read on its own: whatever is there if it fits, else the type's default
/// (`null`, a number where a list belongs, ...), never an error that would take the
/// rest of the file with it.
fn lenient<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    lenient_or(d, T::default)
}

/// [`lenient`] for a field whose default is not the type's own — `Config::default()`'s.
fn lenient_or<'de, D, T>(d: D, default: impl FnOnce() -> T) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let value = serde_json::Value::deserialize(d)?;
    Ok(T::deserialize(value).unwrap_or_else(|_| default()))
}

fn lenient_model<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    lenient_or(d, || DEFAULT_MODEL.to_string())
}

fn lenient_prompt<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    lenient_or(d, || DEFAULT_PROMPT.to_string())
}

fn lenient_num_ctx<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    lenient_or(d, || DEFAULT_NUM_CTX)
}

fn lenient_command_timeout<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    // 0 would stop every command on its first look at the clock (review I3, finding 4).
    let secs: u64 = lenient_or(d, || DEFAULT_COMMAND_TIMEOUT_SECS)?;
    Ok(if secs == 0 { DEFAULT_COMMAND_TIMEOUT_SECS } else { secs })
}

/// `hugin_shell`: one of the three names, else the default.
fn lenient_hugin_shell<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let text: String = lenient_or(d, || DEFAULT_HUGIN_SHELL.to_string())?;
    Ok(match crate::check::Shell::parse(&text) {
        Some(shell) => shell.name().to_string(),
        None => DEFAULT_HUGIN_SHELL.to_string(),
    })
}

fn lenient_coding_model<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    lenient_or(d, || DEFAULT_CODING_MODEL.to_string())
}

/// `ask_every_call`: only an explicit no is a no.
fn lenient_ask<'de, D>(d: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde_json::Value;
    Ok(match Value::deserialize(d)? {
        Value::Bool(b) => b,
        Value::String(s) => !s.eq_ignore_ascii_case("false"),
        Value::Number(n) => n.as_i64() != Some(0) && n.as_u64() != Some(0),
        _ => true,
    })
}

/// `checks`, keeping only entries that are a folder mapped to a list of strings.
fn lenient_checks<'de, D>(d: D) -> Result<BTreeMap<String, Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(d)?;
    let Some(map) = value.as_object() else {
        return Ok(BTreeMap::new());
    };
    Ok(map
        .iter()
        .filter_map(|(folder, argv)| {
            let argv: Vec<String> = argv
                .as_array()?
                .iter()
                .map(|a| a.as_str().map(str::to_string))
                .collect::<Option<_>>()?;
            Some((folder.clone(), argv))
        })
        .collect())
}

/// `model_settings`, keeping only the entries that are a tag mapped to an object, each with
/// the `think` it names if that is one of the four words, else none.
fn lenient_model_settings<'de, D>(d: D) -> Result<BTreeMap<String, ModelSettings>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(d)?;
    let Some(map) = value.as_object() else {
        return Ok(BTreeMap::new());
    };
    Ok(map
        .iter()
        .filter_map(|(tag, entry)| {
            let think = entry.as_object()?.get("think").and_then(|v| v.as_str()).and_then(Think::parse);
            Some((tag.clone(), ModelSettings { think }))
        })
        .collect())
}

/// `model_tags`, keeping only the entries that are a tag mapped to a list with at least one
/// non-blank string in it.
fn lenient_model_tags<'de, D>(d: D) -> Result<BTreeMap<String, Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(d)?;
    let Some(map) = value.as_object() else {
        return Ok(BTreeMap::new());
    };
    Ok(map
        .iter()
        .filter_map(|(tag, labels)| {
            let labels: Vec<String> = labels
                .as_array()?
                .iter()
                .filter_map(|l| l.as_str())
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect();
            (!labels.is_empty()).then(|| (tag.clone(), labels))
        })
        .collect())
}

/// `llama3.1` and `llama3.1:latest` name the same model to Ollama, in any case.
fn same_model(a: &str, b: &str) -> bool {
    let bare = |s: &str| {
        let s = s.to_lowercase();
        s.strip_suffix(":latest").map(str::to_string).unwrap_or(s)
    };
    bare(a) == bare(b)
}

/// `instructions`, keeping only the strings.
fn lenient_instructions<'de, D>(d: D) -> Result<BTreeSet<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(d)?;
    Ok(value
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default())
}

impl Config {
    /// The check command set for this folder, if any.
    pub fn check_for(&self, root: &str) -> Option<&Vec<String>> {
        self.checks
            .iter()
            .find(|(k, v)| k.eq_ignore_ascii_case(root) && !v.is_empty())
            .map(|(_, v)| v)
    }

    /// Set or clear this folder's check, replacing any entry that differs only
    /// in case.
    pub fn set_check(&mut self, root: &str, argv: Option<Vec<String>>) {
        self.checks.retain(|k, _| !k.eq_ignore_ascii_case(root));
        if let Some(argv) = argv {
            self.checks.insert(root.to_string(), argv);
        }
    }

    /// What the config says about this model, if it has an entry. Ollama's tags are
    /// lower case, but a tag typed by hand may not be.
    pub fn settings_for(&self, model: &str) -> Option<&ModelSettings> {
        self.model_settings.iter().find(|(k, _)| k.eq_ignore_ascii_case(model)).map(|(_, v)| v)
    }

    /// The labels the config gives this model, if any: the entries whose tag is this model,
    /// ignoring case and a trailing `:latest`, in the config's order, without repeats.
    pub fn tags_for(&self, model: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for label in self.model_tags.iter().filter(|(k, _)| same_model(k, model)).flat_map(|(_, v)| v) {
            if !out.contains(label) {
                out.push(label.clone());
            }
        }
        out
    }

    /// Whether the project's notes are sent for this folder.
    pub fn instructions_for(&self, root: &str) -> bool {
        self.instructions.iter().any(|k| k.eq_ignore_ascii_case(root))
    }

    /// Turn the notes on or off for this folder, replacing any entry that differs
    /// only in case.
    pub fn set_instructions(&mut self, root: &str, on: bool) {
        self.instructions.retain(|k| !k.eq_ignore_ascii_case(root));
        if on {
            self.instructions.insert(root.to_string());
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: DEFAULT_MODEL.to_string(),
            prompt: DEFAULT_PROMPT.to_string(),
            num_ctx: DEFAULT_NUM_CTX,
            workspace: None,
            recent_workspaces: Vec::new(),
            checks: BTreeMap::new(),
            coding_model: DEFAULT_CODING_MODEL.to_string(),
            model_before_coding: None,
            ask_every_call: false,
            instructions: BTreeSet::new(),
            command_timeout_secs: DEFAULT_COMMAND_TIMEOUT_SECS,
            hugin_shell: DEFAULT_HUGIN_SHELL.to_string(),
            hugin_last_folder: None,
            bifrost_login_chosen: false,
            claude_checks: false,
            claude_command: None,
            model_settings: BTreeMap::new(),
            model_tags: BTreeMap::new(),
            extra: serde_json::Map::new(),
            unsaved: false,
        }
    }
}

/// `config.json` in [`crate::paths::data_dir`], or `None` if `%APPDATA%` is not set.
///
/// Returns the path whether or not anything is there — callers that want to know
/// if it exists should ask the filesystem.
pub fn path() -> Option<PathBuf> {
    Some(crate::paths::data_dir()?.join("config.json"))
}

/// Read the config, falling back to defaults for anything missing or unreadable.
///
/// Deliberately returns `Config` rather than `Result<Config, _>`. There is no
/// failure a caller could usefully handle: if the file is absent the defaults
/// *are* the right answer, and if it is corrupt, refusing to summon the popup
/// would be a worse outcome than ignoring it. A file that is there but unusable is
/// kept as `config.json.bad` and answered with `ask_every_call` on — see the
/// module docs. Callers that need to know whether a file exists can use [`path`].
pub fn load() -> Config {
    match path() {
        Some(p) => load_from(&p),
        None => Config::default(),
    }
}

/// [`load`] for a given file, so tests need not touch `%APPDATA%`.
pub fn load_from(p: &Path) -> Config {
    let bytes = match read_with_retries(p) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Config::default(),
        // A file that is there but cannot be read (another program has it open
        // without sharing) is not "no file": its settings still exist. Fail
        // closed, and refuse to let a save replace it with these defaults.
        Err(_) => {
            return Config {
                ask_every_call: true,
                unsaved: true,
                ..Config::default()
            };
        }
    };
    let parsed = String::from_utf8(bytes).ok().and_then(|text| {
        // A byte-order mark is how some Windows editors say "this is UTF-8".
        serde_json::from_str::<Config>(text.strip_prefix('\u{feff}').unwrap_or(&text)).ok()
    });
    parsed.unwrap_or_else(|| unreadable(p))
}

/// `std::fs::read`, tried a few times when it fails with anything but
/// `NotFound`: a sharing violation is usually another program's write finishing,
/// and a short wait turns it into a plain read. `NotFound` is final at once.
fn read_with_retries(p: &Path) -> std::io::Result<Vec<u8>> {
    let mut tries = 0;
    loop {
        match std::fs::read(p) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound && tries < 5 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            other => return other,
        }
    }
}

/// The answer for a file that is there but cannot be used: keep a copy of it
/// beside itself, and fail towards asking. The copy is best effort — if even that
/// fails, `load` must still not fail, but then nothing keeps what the person
/// typed, so the result is marked `unsaved` and a save will not write over it.
fn unreadable(p: &Path) -> Config {
    let mut bad = p.as_os_str().to_owned();
    bad.push(".bad");
    let kept = std::fs::copy(p, PathBuf::from(bad)).is_ok();
    Config {
        ask_every_call: true,
        unsaved: !kept,
        ..Config::default()
    }
}

/// Write the config, creating `%APPDATA%\bifrost\` if it is not there.
///
/// This one *does* return a `Result`: a save is something the user asked for, so
/// silently doing nothing would be wrong. G2 writes the model choice through it.
pub fn save(config: &Config) -> Result<(), String> {
    let p = path().ok_or_else(|| "APPDATA is not set".to_string())?;
    save_to(&p, config)
}

/// Load the config, change it, save it — **as one step no other [`update`] can
/// interleave with.** Every command that changes one setting used to `load`, edit
/// and `save` on its own, so two that read before either wrote lost one of the two
/// changes, and `accept_model_offer` held its copy across a network call. Callers
/// give the change as `f`; whatever it returns is handed back once the save is done.
///
/// The lock is per process (Vidar and the CLI only read), and `f` runs under it,
/// so it must not wait on the network or on another `update`. A config loaded from a
/// file that could not be read is still refused by [`save_to`], so `f`'s change is
/// then reported as an error and the real file is left alone.
pub fn update<T>(f: impl FnOnce(&mut Config) -> T) -> Result<T, String> {
    let p = path().ok_or_else(|| "APPDATA is not set".to_string())?;
    update_at(&p, f)
}

/// [`update`] for a given file, so tests need not touch `%APPDATA%`.
pub fn update_at<T>(p: &Path, f: impl FnOnce(&mut Config) -> T) -> Result<T, String> {
    static LOCK: Mutex<()> = Mutex::new(());
    // A panic inside another `f` leaves nothing half-written (the save is a rename),
    // so a poisoned lock is still safe to take.
    let _held = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut config = load_from(p);
    let before = config.clone();
    let out = f(&mut config);
    // Nothing changed (an offer that no longer applies, a check already chosen):
    // nothing is written, so the file is not rewritten for no reason — and a file
    // that would not parse is left as the person wrote it.
    if config != before {
        save_to(p, &config)?;
    }
    Ok(out)
}

/// [`save`] for a given file. **Written to a temporary file beside it and renamed**, as
/// `store::save` does, so a reader (Vidar reads this on every summon) sees the
/// old file or the new one, never a half-written one.
///
/// The temporary file has a name of its own for every save (process id and a
/// process-wide counter), so two saves at once, from two threads or two programs,
/// never write or rename the same file; it is removed if the rename fails. A
/// config marked [`Config::unsaved`] is refused, see there.
pub fn save_to(p: &Path, config: &Config) -> Result<(), String> {
    if config.unsaved {
        return Err(format!(
            "{} could not be read when it was loaded, so it is not saved over",
            p.display()
        ));
    }
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    let json = serde_json::to_string_pretty(config)
        .map_err(|e| format!("serializing config: {e}"))?;
    let mut tmp = p.as_os_str().to_owned();
    static SAVES: AtomicU64 = AtomicU64::new(0);
    tmp.push(format!(".{}.{}.tmp", std::process::id(), SAVES.fetch_add(1, Ordering::Relaxed)));
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, json).map_err(|e| format!("writing {}: {e}", tmp.display()))?;
    rename_patiently(&tmp, p).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("replacing {}: {e}", p.display())
    })
}

/// `std::fs::rename`, tried again for up to about half a second when Windows says
/// "access denied". Replacing a file fails that way while another program holds
/// it without delete sharing — a virus scanner looking at the temporary file just
/// written, the indexer, a reader that opened it the old way — and that hold lasts
/// milliseconds. Without this, picking a model could fail with "Access is denied"
/// for no reason the person can see. (Seen twice as a test that hung, before the
/// test learned to report a failed save; not reproduced on demand.) Any other error
/// is returned at once.
fn rename_patiently(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut tries = 0;
    loop {
        match std::fs::rename(from, to) {
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && tries < 25 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            done => return done,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        // The forward-compatibility property: a file written before a field
        // existed must still load. This is what makes G2 and G4 able to add
        // fields without migrating a config holding real settings.
        let partial: Config = serde_json::from_str(r#"{"model":"llama3.1:8b"}"#).unwrap();
        assert_eq!(partial.model, "llama3.1:8b");
        assert_eq!(partial.prompt, DEFAULT_PROMPT);
        assert_eq!(partial.num_ctx, DEFAULT_NUM_CTX);
    }

    #[test]
    fn the_command_settings_default_and_read_leniently() {
        let c = Config::default();
        assert_eq!((c.command_timeout_secs, c.hugin_shell.as_str()), (120, "bash"));
        let read = |json: &str| serde_json::from_str::<Config>(json).unwrap();
        let c = read(r#"{"command_timeout_secs":30,"hugin_shell":"Bash"}"#);
        assert_eq!((c.command_timeout_secs, c.hugin_shell.as_str()), (30, "bash"));
        for bad in [r#""soon""#, "null", "-5", "1.5", "[]", "0"] {
            assert_eq!(read(&format!(r#"{{"command_timeout_secs":{bad},"model":"m"}}"#)).command_timeout_secs, 120, "{bad}");
        }
        for bad in [r#""zsh""#, "null", "3", "[]"] {
            let c = read(&format!(r#"{{"hugin_shell":{bad},"model":"m"}}"#));
            assert_eq!((c.hugin_shell.as_str(), c.model.as_str()), ("bash", "m"), "{bad}");
        }
    }

    #[test]
    fn the_last_hugin_folder_reads_leniently_and_is_skipped_when_none() {
        let read = |json: &str| serde_json::from_str::<Config>(json).unwrap();
        assert_eq!(read(r#"{"hugin_last_folder":"C:\\w"}"#).hugin_last_folder.as_deref(), Some(r"C:\w"));
        for bad in ["null", "3", "[]", "{}", "true"] {
            let c = read(&format!(r#"{{"hugin_last_folder":{bad},"model":"m"}}"#));
            assert_eq!((c.hugin_last_folder, c.model.as_str()), (None, "m"), "{bad}");
        }
        assert!(!serde_json::to_string(&Config::default()).unwrap().contains("hugin_last_folder"));
    }

    #[test]
    fn empty_object_is_entirely_defaults() {
        assert_eq!(
            serde_json::from_str::<Config>("{}").unwrap(),
            Config::default()
        );
    }

    #[test]
    fn the_login_choice_reads_leniently_and_defaults_to_never_recorded() {
        let read = |json: &str| serde_json::from_str::<Config>(json).unwrap();
        assert!(!read("{}").bifrost_login_chosen);
        assert!(read(r#"{"bifrost_login_chosen":true}"#).bifrost_login_chosen);
        for bad in ["null", "3", "\"yes\"", "[]", "{}"] {
            let c = read(&format!(r#"{{"bifrost_login_chosen":{bad},"model":"m"}}"#));
            assert_eq!((c.bifrost_login_chosen, c.model.as_str()), (false, "m"), "{bad}");
        }
    }

    #[test]
    fn claude_checks_are_off_unless_a_bool_says_on() {
        let read = |json: &str| serde_json::from_str::<Config>(json).unwrap();
        assert!(!read("{}").claude_checks);
        assert!(read(r#"{"claude_checks":true}"#).claude_checks);
        for bad in ["null", "3", "\"true\"", "[]", "{}"] {
            let c = read(&format!(r#"{{"claude_checks":{bad},"claude_command":7,"model":"m"}}"#));
            assert_eq!((c.claude_checks, c.claude_command, c.model.as_str()), (false, None, "m"), "{bad}");
        }
        assert_eq!(read(r#"{"claude_command":"C:\\c.exe"}"#).claude_command.as_deref(), Some(r"C:\c.exe"));
        assert!(!serde_json::to_string(&Config::default()).unwrap().contains("claude_command"));
    }

    #[test]
    fn round_trips() {
        let c = Config {
            model: "qwen3-vl:4b-instruct-q8_0".into(),
            prompt: "what is this".into(),
            num_ctx: 4096,
            workspace: Some(r"C:\work\demo".into()),
            recent_workspaces: vec![r"C:\work\demo".into(), r"C:\work\older".into()],
            checks: BTreeMap::from([(r"C:\work\demo".into(), vec!["cargo".into(), "check".into()])]),
            coding_model: "qwen3-coder:30b".into(),
            model_before_coding: Some("qwen3-vl:8b-instruct-q4_K_M".into()),
            ask_every_call: true,
            instructions: BTreeSet::from([r"C:\work\demo".into()]),
            command_timeout_secs: 30,
            hugin_shell: "bash".into(),
            hugin_last_folder: Some(r"C:\work\demo".into()),
            bifrost_login_chosen: true,
            claude_checks: true,
            claude_command: Some(r"C:\tools\claude.exe".into()),
            model_settings: BTreeMap::from([("gpt-oss:20b".into(), ModelSettings { think: Some(Think::High) })]),
            model_tags: BTreeMap::from([("qwen3.6:35b-a3b".into(), vec!["Favourite".into()])]),
            extra: serde_json::Map::new(),
            unsaved: false,
        };
        let back: Config = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(c, back);
    }

    /// A config saved before H5 added these fields has neither — and must still
    /// load, with the offer on by default and nothing to switch back to.
    #[test]
    fn a_config_from_before_the_coding_model_existed_still_loads() {
        let c: Config = serde_json::from_str(r#"{"model":"llama3.1:8b"}"#).unwrap();
        assert_eq!(c.coding_model, DEFAULT_CODING_MODEL);
        assert_eq!(c.model_before_coding, None);
    }

    /// Windows paths ignore case, and the folder can reach the config spelled
    /// either way — from the picker, or typed by hand into `checks`.
    #[test]
    fn a_check_is_found_whatever_the_case_of_its_folder() {
        let mut c = Config::default();
        c.set_check(r"C:\Work\Demo", Some(vec!["cargo".into(), "check".into()]));
        assert_eq!(c.check_for(r"c:\work\demo").map(|v| v.join(" ")), Some("cargo check".into()));
        assert_eq!(c.check_for(r"C:\work\other"), None);
    }

    #[test]
    fn setting_a_check_replaces_one_that_differs_only_in_case() {
        let mut c = Config::default();
        c.set_check(r"C:\Work\Demo", Some(vec!["cargo".into(), "check".into()]));
        c.set_check(r"c:\work\demo", Some(vec!["cargo".into(), "test".into()]));
        assert_eq!(c.checks.len(), 1);
        c.set_check(r"C:\WORK\DEMO", None);
        assert!(c.checks.is_empty());
    }

    /// **A typo in a hand-typed check costs that check, not the whole file.**
    /// `"cargo clippy"` as a string instead of a list used to fail the parse,
    /// and every setting fell back to its default.
    #[test]
    fn a_malformed_check_is_dropped_and_everything_else_survives() {
        let c: Config = serde_json::from_str(
            r#"{"model":"llama3.1:8b","workspace":"C:\\w","checks":{
                "C:\\a":"cargo clippy",
                "C:\\b":["cargo",1],
                "C:\\c":["cargo","check"]}}"#,
        )
        .unwrap();
        assert_eq!(c.model, "llama3.1:8b");
        assert_eq!(c.workspace.as_deref(), Some(r"C:\w"));
        assert_eq!(c.checks.len(), 1);
        assert!(c.check_for(r"C:\c").is_some());
        // Not even a map: no checks, and still no reset.
        let c: Config = serde_json::from_str(r#"{"model":"x","checks":"oops"}"#).unwrap();
        assert_eq!((c.model.as_str(), c.checks.len()), ("x", 0));
    }

    /// An empty command is no command: a hand-edited `"C:\\x": []` must not
    /// offer the model a check that cannot run.
    #[test]
    fn an_empty_command_is_not_a_check() {
        let mut c = Config::default();
        c.checks.insert(r"C:\x".into(), vec![]);
        assert_eq!(c.check_for(r"C:\x"), None);
    }

    #[test]
    fn a_missing_ask_every_call_is_off() {
        let c: Config = serde_json::from_str(r#"{"model":"x"}"#).unwrap();
        assert!(!c.ask_every_call);
        assert!(!Config::default().ask_every_call);
    }

    #[test]
    fn ask_every_call_reads_true_false_strings_and_numbers() {
        let read = |v: &str| {
            serde_json::from_str::<Config>(&format!(r#"{{"ask_every_call":{v}}}"#))
                .unwrap()
                .ask_every_call
        };
        for on in ["true", r#""true""#, r#""TRUE""#, "1"] {
            assert!(read(on), "{on}");
        }
        for off in ["false", r#""false""#, r#""False""#, "0"] {
            assert!(!read(off), "{off}");
        }
    }

    /// **A typo can only make Mimir stricter.** Without the lenient reader the
    /// whole file would fail to parse and every setting would reset.
    #[test]
    fn a_malformed_ask_every_call_means_ask_and_everything_else_survives() {
        for bad in [r#""yes""#, "[1]", "null", "{}", "2.5"] {
            let c: Config = serde_json::from_str(&format!(
                r#"{{"model":"llama3.1:8b","workspace":"C:\\w","ask_every_call":{bad}}}"#
            ))
            .unwrap();
            assert!(c.ask_every_call, "{bad}");
            assert_eq!(c.model, "llama3.1:8b");
            assert_eq!(c.workspace.as_deref(), Some(r"C:\w"));
        }
    }

    /// Windows paths ignore case, and the folder can reach the config spelled
    /// either way.
    #[test]
    fn notes_are_found_whatever_the_case_of_their_folder_and_set_replaces_one() {
        let mut c = Config::default();
        assert!(!c.instructions_for(r"C:\Work\Demo"), "off by default");
        c.set_instructions(r"C:\Work\Demo", true);
        assert!(c.instructions_for(r"c:\work\demo"));
        assert!(!c.instructions_for(r"C:\work\other"));
        c.set_instructions(r"c:\work\demo", true);
        assert_eq!(c.instructions.len(), 1);
        c.set_instructions(r"C:\WORK\DEMO", false);
        assert!(c.instructions.is_empty());
    }

    /// A config from before H10 has no `instructions`; a typo in one costs that entry.
    #[test]
    fn a_malformed_instructions_field_is_dropped_and_everything_else_survives() {
        let c: Config = serde_json::from_str(r#"{"model":"x"}"#).unwrap();
        assert!(c.instructions.is_empty());
        let c: Config =
            serde_json::from_str(r#"{"model":"x","instructions":["C:\\a",1,null,{"b":2},"C:\\c"]}"#).unwrap();
        assert_eq!(c.model, "x");
        assert_eq!(c.instructions.len(), 2);
        assert!(c.instructions_for(r"C:\a") && c.instructions_for(r"C:\c"));
        for bad in [r#""C:\\a""#, "true", "{}", "null", "7"] {
            let c: Config =
                serde_json::from_str(&format!(r#"{{"model":"x","instructions":{bad}}}"#)).unwrap();
            assert_eq!((c.model.as_str(), c.instructions.len()), ("x", 0), "{bad}");
        }
    }

    #[test]
    fn model_settings_parse_and_a_model_without_an_entry_has_none() {
        let c: Config = serde_json::from_str(
            r#"{"model":"m","model_settings":{"gpt-oss:20b":{"think":"high"},"qwen3:8b":{"think":"off"},"x":{"think":"Low"},"y":{}}}"#,
        )
        .unwrap();
        assert_eq!(c.model, "m");
        assert_eq!(c.model_settings.len(), 4);
        let think = |tag: &str| c.settings_for(tag).map(|s| s.think);
        assert_eq!(think("gpt-oss:20b"), Some(Some(Think::High)));
        assert_eq!(think("qwen3:8b"), Some(Some(Think::Off)));
        assert_eq!(think("x"), Some(Some(Think::Low)), "a level is read in any case");
        assert_eq!(think("y"), Some(None), "an entry with nothing in it sets nothing");
        assert_eq!(think("qwen3-coder:30b"), None);
        // Nothing set is nothing written, so a config that never used it is unchanged on disk.
        assert!(Config::default().model_settings.is_empty());
        assert!(!serde_json::to_string(&Config::default()).unwrap().contains("model_settings"));
        let saved = serde_json::to_string(&c).unwrap();
        assert!(saved.contains(r#""gpt-oss:20b":{"think":"high"}"#), "{saved}");
    }

    /// A typo in a hand-typed entry costs that entry, or that value, and not the file.
    #[test]
    fn a_bad_model_settings_entry_is_dropped_and_a_bad_think_is_unset() {
        let c: Config = serde_json::from_str(
            r#"{"model":"mine","workspace":"C:\\w","model_settings":{
                "a":"high",
                "b":["think","high"],
                "c":null,
                "d":{"think":"extreme"},
                "e":{"think":3},
                "f":{"think":true},
                "g":{"think":"medium"}}}"#,
        )
        .unwrap();
        assert_eq!((c.model.as_str(), c.workspace.as_deref()), ("mine", Some(r"C:\w")));
        let tags: Vec<&str> = c.model_settings.keys().map(String::as_str).collect();
        assert_eq!(tags, ["d", "e", "f", "g"], "the entries that are not objects are dropped");
        for unset in ["d", "e", "f"] {
            assert_eq!(c.settings_for(unset).and_then(|s| s.think), None, "{unset}");
        }
        assert_eq!(c.settings_for("g").and_then(|s| s.think), Some(Think::Medium));
        // Not even a map: no settings, and still no reset.
        for bad in [r#""oops""#, "null", "[]", "7", "true"] {
            let c: Config = serde_json::from_str(&format!(r#"{{"model":"x","model_settings":{bad}}}"#)).unwrap();
            assert_eq!((c.model.as_str(), c.model_settings.len()), ("x", 0), "{bad}");
        }
    }

    #[test]
    fn a_models_settings_are_found_whatever_the_case_of_its_tag() {
        let c: Config =
            serde_json::from_str(r#"{"model_settings":{"GPT-OSS:20b":{"think":"low"}}}"#).unwrap();
        assert_eq!(c.settings_for("gpt-oss:20b").and_then(|s| s.think), Some(Think::Low));
        assert_eq!(c.settings_for("GPT-OSS:20B").and_then(|s| s.think), Some(Think::Low));
        assert!(c.settings_for("gpt-oss:120b").is_none());
        // The words themselves, in any case; a number or a stray word is none.
        assert_eq!(Think::parse(" High "), Some(Think::High));
        assert_eq!(Think::parse("OFF").map(Think::name), Some("off"));
        assert_eq!((Think::parse("true"), Think::parse(""), Think::parse("max")), (None, None, None));
    }

    #[test]
    fn model_tags_parse_and_a_model_without_an_entry_has_none() {
        let c: Config = serde_json::from_str(
            r#"{"model":"m","model_tags":{"qwen3.6:35b-a3b":["Favourite","Fast"],"llama3.1:8b":["Chat"]}}"#,
        )
        .unwrap();
        assert_eq!(c.model, "m");
        assert_eq!(c.model_tags.len(), 2);
        assert_eq!(c.tags_for("qwen3.6:35b-a3b"), ["Favourite", "Fast"]);
        assert_eq!(c.tags_for("llama3.1:8b"), ["Chat"]);
        assert!(c.tags_for("gpt-oss:20b").is_empty());
        assert!(Config::default().tags_for("anything").is_empty());
    }

    /// A typo in a hand-typed entry costs that entry, or that label, and not the file.
    #[test]
    fn a_bad_model_tags_entry_or_label_is_dropped() {
        let c: Config = serde_json::from_str(
            r#"{"model":"mine","model_tags":{
                "a":"Favourite",
                "b":{"x":"y"},
                "c":null,
                "d":[],
                "e":["", "  ", 3, null, true],
                "f":["Good", "", 7, " Padded "]}}"#,
        )
        .unwrap();
        assert_eq!(c.model, "mine");
        let tags: Vec<&str> = c.model_tags.keys().map(String::as_str).collect();
        assert_eq!(tags, ["f"], "the entries with no usable label are dropped");
        assert_eq!(c.tags_for("f"), ["Good", "Padded"]);
        // Not even a map: no tags, and still no reset.
        for bad in [r#""oops""#, "null", "[]", "7", "true"] {
            let c: Config = serde_json::from_str(&format!(r#"{{"model":"x","model_tags":{bad}}}"#)).unwrap();
            assert_eq!((c.model.as_str(), c.model_tags.len()), ("x", 0), "{bad}");
        }
    }

    #[test]
    fn a_models_tags_are_found_whatever_the_case_or_a_latest_suffix() {
        let c: Config = serde_json::from_str(
            r#"{"model_tags":{"Huge/Model:Latest":["One"],"plain":["Two"],"tagged:8b":["Three"]}}"#,
        )
        .unwrap();
        assert_eq!(c.tags_for("huge/model:latest"), ["One"]);
        assert_eq!(c.tags_for("HUGE/MODEL"), ["One"]);
        assert_eq!(c.tags_for("plain:latest"), ["Two"]);
        assert_eq!(c.tags_for("PLAIN"), ["Two"]);
        assert_eq!(c.tags_for("tagged:8B"), ["Three"]);
        assert!(c.tags_for("tagged").is_empty(), "a different tag is a different model");
        assert!(c.tags_for("plain:8b").is_empty());
    }

    #[test]
    fn model_tags_survive_a_save_and_an_empty_map_is_not_written() {
        let p = temp_file("model-tags");
        std::fs::write(&p, r#"{"model":"mine","model_tags":{"qwen3.6:35b-a3b":["Favourite"]}}"#).unwrap();
        let c = load_from(&p);
        save_to(&p, &c).unwrap();
        let after = std::fs::read_to_string(&p).unwrap();
        assert!(after.contains(r#""model_tags""#), "{after}");
        assert_eq!(load_from(&p).tags_for("qwen3.6:35b-a3b"), ["Favourite"]);
        assert_eq!(load_from(&p), c);
        // Nothing set is nothing written, so a config that never used it is unchanged on disk.
        assert!(Config::default().model_tags.is_empty());
        assert!(!serde_json::to_string(&Config::default()).unwrap().contains("model_tags"));
    }

    /// A `config.json` path in a folder of its own, **removed when the test ends** —
    /// passing or failing — so test runs do not leave folders in `%TEMP%` (they did:
    /// 176 of them before this guard).
    struct TempConfig {
        dir: PathBuf,
        path: PathBuf,
    }

    impl std::ops::Deref for TempConfig {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.path
        }
    }

    impl AsRef<Path> for TempConfig {
        fn as_ref(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempConfig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn temp_file(name: &str) -> TempConfig {
        let dir = std::env::temp_dir().join(format!("bifrost-config-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempConfig { path: dir.join("config.json"), dir }
    }

    /// **A file that does not parse fails towards asking, and is kept.** A typo in
    /// the file must not silently mean "never ask", and the next save must not be
    /// the end of what the person typed.
    #[test]
    fn a_file_that_does_not_parse_asks_every_call_and_is_copied_to_bad() {
        let cases: Vec<Vec<u8>> = vec![
            br#"{"ask_every_call": true, "model": "x",}"#.to_vec(),
            br#"{"workspace": "C:\Users\me\proj"}"#.to_vec(),
            b"{\"model\": \"x\"}\n// note\n".to_vec(),
            br#"{"model": "x", "mo"#.to_vec(),
            b"[]".to_vec(),
            vec![0xFF, 0xFE, b'{'],
        ];
        for bytes in cases {
            let p = temp_file("bad");
            std::fs::write(&p, &bytes).unwrap();
            let c = load_from(&p);
            assert!(c.ask_every_call, "{:?}", String::from_utf8_lossy(&bytes));
            assert_eq!(c.model, DEFAULT_MODEL);
            let bad = p.with_extension("json.bad");
            assert_eq!(std::fs::read(&bad).unwrap(), bytes);
            // A save afterwards replaces the file but leaves the copy alone.
            save_to(&p, &c).unwrap();
            assert_eq!(std::fs::read(&bad).unwrap(), bytes);
        }
    }

    #[test]
    fn a_missing_file_is_plain_defaults_and_leaves_no_copy() {
        let p = temp_file("missing");
        assert_eq!(load_from(&p), Config::default());
        assert!(!p.with_extension("json.bad").exists());
    }

    #[test]
    fn a_byte_order_mark_is_skipped() {
        let p = temp_file("bom");
        let mut text = vec![0xEF, 0xBB, 0xBF];
        text.extend_from_slice(br#"{"model":"mine","ask_every_call":false}"#);
        std::fs::write(&p, text).unwrap();
        let c = load_from(&p);
        assert_eq!((c.model.as_str(), c.ask_every_call), ("mine", false));
        assert!(!p.with_extension("json.bad").exists());
    }

    #[test]
    fn a_null_or_wrong_type_costs_that_field_only() {
        let c: Config = serde_json::from_str(
            r#"{"model":"mine","coding_model":null,"prompt":7,"num_ctx":"big","workspace":3,
                "recent_workspaces":"x","model_before_coding":[],"checks":{"C:\\a":["cargo"]}}"#,
        )
        .unwrap();
        assert_eq!(c.model, "mine");
        assert_eq!(c.coding_model, DEFAULT_CODING_MODEL);
        assert_eq!(c.prompt, DEFAULT_PROMPT);
        assert_eq!(c.num_ctx, DEFAULT_NUM_CTX);
        assert_eq!((c.workspace, c.model_before_coding), (None, None));
        assert!(c.recent_workspaces.is_empty());
        assert_eq!(c.checks.len(), 1);
    }

    #[test]
    fn unknown_fields_survive_a_load_and_save() {
        let p = temp_file("extra");
        std::fs::write(&p, r#"{"model":"mine","future_setting":{"a":1}}"#).unwrap();
        let c = load_from(&p);
        save_to(&p, &c).unwrap();
        let after = std::fs::read_to_string(&p).unwrap();
        assert!(after.contains("future_setting"), "{after}");
        assert_eq!(load_from(&p), c);
        let leftovers = std::fs::read_dir(p.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0, "the temp file is renamed away");
    }

    #[test]
    fn a_normal_load_is_not_marked_unsaved() {
        let p = temp_file("normal");
        assert!(!load_from(&p).unsaved);
        std::fs::write(&p, r#"{"model":"mine"}"#).unwrap();
        assert!(!load_from(&p).unsaved);
        // A parse failure whose `.bad` copy succeeded may still be saved over.
        std::fs::write(&p, "{ not json").unwrap();
        assert!(!load_from(&p).unsaved);
    }

    /// A file another program holds open without sharing is not "no file": the load
    /// asks every call, is marked, and a save leaves the real file alone.
    #[cfg(windows)]
    #[test]
    fn a_locked_file_loads_closed_and_is_not_saved_over() {
        use std::os::windows::fs::OpenOptionsExt;
        let p = temp_file("locked");
        let real = r#"{"model":"mine","ask_every_call":false}"#;
        std::fs::write(&p, real).unwrap();
        let guard = std::fs::OpenOptions::new().read(true).share_mode(0).open(&p).unwrap();
        let c = load_from(&p);
        assert!(c.ask_every_call && c.unsaved);
        assert!(save_to(&p, &c).unwrap_err().contains("could not be read"));
        drop(guard);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), real);
        let again = load_from(&p);
        assert!(!again.unsaved);
        assert_eq!(again.model, "mine");
    }

    /// **Two changes made at once are both kept.** Each thread changes a different
    /// field many times through `update_at`; without the lock one thread's save
    /// would put back the other's old value.
    #[test]
    fn updates_from_two_threads_both_keep_their_changes() {
        let p = temp_file("update");
        std::thread::scope(|scope| {
            let (p1, p2) = (&p, &p);
            let a = scope.spawn(move || {
                for i in 0..50 {
                    update_at(p1, |c| c.num_ctx = 1000 + i).unwrap();
                }
            });
            let b = scope.spawn(move || {
                for i in 0..50 {
                    update_at(p2, |c| c.model = format!("m{i}")).unwrap();
                }
            });
            a.join().unwrap();
            b.join().unwrap();
        });
        let c = load_from(&p);
        assert_eq!((c.num_ctx, c.model.as_str()), (1049, "m49"));
    }

    #[cfg(windows)]
    #[test]
    fn an_update_of_an_unreadable_file_is_refused_and_returns_the_error() {
        use std::os::windows::fs::OpenOptionsExt;
        let p = temp_file("update-locked");
        std::fs::write(&p, r#"{"model":"mine"}"#).unwrap();
        let guard = std::fs::OpenOptions::new().read(true).share_mode(0).open(&p).unwrap();
        let err = update_at(&p, |c| c.num_ctx = 1).unwrap_err();
        assert!(err.contains("could not be read"), "{err}");
        drop(guard);
        assert_eq!(load_from(&p).model, "mine");
    }

    /// **A save waits out a brief hold on the file.** Another program holding
    /// config.json without delete sharing (a scanner, the indexer) makes replacing it
    /// fail with "access denied" for as long as it holds it; a save that lands then
    /// must succeed once the hold ends, not report an error.
    #[cfg(windows)]
    #[test]
    fn a_save_waits_out_a_brief_hold_on_the_file() {
        use std::os::windows::fs::OpenOptionsExt;
        let p = temp_file("held");
        save_to(&p, &Config::default()).unwrap();
        // FILE_SHARE_READ | FILE_SHARE_WRITE, but not FILE_SHARE_DELETE.
        let held = std::fs::OpenOptions::new().read(true).share_mode(1 | 2).open(&*p).unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(150));
            drop(held);
        });
        let c = Config { model: "after the hold".into(), ..Config::default() };
        let saved = save_to(&p, &c);
        release.join().unwrap();
        assert!(saved.is_ok(), "{saved:?}");
        assert_eq!(load_from(&p).model, "after the hold");
    }

    #[test]
    fn concurrent_saves_never_collide_and_a_reader_never_sees_a_broken_file() {
        let p = temp_file("concurrent");
        save_to(&p, &Config::default()).unwrap();
        let stop = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|scope| {
            // Every save's error is collected, never unwrapped in the thread: a writer
            // that panicked used to leave `stop` unset, and the reader then spun for
            // ever — the test hung instead of failing and saying why.
            let writers: Vec<_> = (0..6)
                .map(|w| {
                    let p = &p;
                    scope.spawn(move || {
                        (0..100)
                            .filter_map(|i| {
                                let c = Config { model: format!("m{w}-{i}"), ..Config::default() };
                                save_to(p, &c).err()
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            let reader = scope.spawn(|| {
                let mut broken = 0;
                while !stop.load(Ordering::Relaxed) {
                    // A read can be refused for an instant while a rename lands
                    // on Windows; what it does return must always parse.
                    if let Ok(text) = std::fs::read_to_string(&p)
                        && serde_json::from_str::<Config>(&text).is_err()
                    {
                        broken += 1;
                    }
                }
                broken
            });
            let errors: Vec<String> = writers.into_iter().flat_map(|w| w.join().unwrap_or_default()).collect();
            stop.store(true, Ordering::Relaxed);
            let broken = reader.join().unwrap_or(usize::MAX);
            assert!(errors.is_empty(), "{} of 600 saves failed, e.g. {:?}", errors.len(), errors.first());
            assert_eq!(broken, 0, "a reader saw a broken file {broken} times");
        });
        assert!(!p.with_extension("json.bad").exists());
    }
}
