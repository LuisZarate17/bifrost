//! The settings all three binaries share, at `%APPDATA%\odin\config.json`.
//!
//! **A missing file is a normal state, not an error.** `load()` cannot fail: no
//! file, unreadable file, or malformed JSON all yield the built-in defaults, and
//! nothing here prints or panics. That is deliberate and load-bearing — PLAN.md
//! decision #5 requires Heimdall to keep working on a machine where Mímir was
//! never installed, so "no config" has to be the ordinary path rather than an
//! error anyone has to handle.
//!
//! JSON rather than TOML so a frontend can read it directly later without a Rust
//! round trip, and `%APPDATA%` rather than anywhere repo-local, because a repo
//! path is wrong the moment something is installed instead of run out of a build
//! directory.
//!
//! Blocking, like the rest of this crate. Tauri callers go through
//! `spawn_blocking`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Chosen at A2, where size decided accuracy and quantization decided nothing.
/// Reports `completion`, `vision` and `tools`, so one resident model can serve
/// the popup, the CLI and (from G6) tool calling.
pub const DEFAULT_MODEL: &str = "qwen3-vl:8b-instruct-q4_K_M";

/// With no question to read, this does all the intent-guessing (PLAN.md §2).
///
/// **This is the only copy in source.** It used to exist here, in the CLI's
/// `prompt.txt`, as an inline fallback in the CLI's `main.rs`, and as a `const`
/// in Heimdall's `App.tsx` — four places to keep in step, which nothing enforced.
/// `evals/prompts/current.txt` is byte-identical and stays, because that one is
/// measured input for the bench rather than a copy of this.
pub const DEFAULT_PROMPT: &str =
    "What am I looking at? If there's an error or something broken, explain it \
     and the fix. Otherwise explain what matters here.";

/// Heimdall's value, and now everyone's.
///
/// **Measured 2026-09-19.** Ollama keys a loaded runner by the model *and* by
/// `num_ctx`, so asking at a different context than you warmed at tears the
/// runner down and rebuilds it. The CLI used to warm at 8192 while Heimdall asked
/// at 32768, and every switch between them cost about three seconds:
///
/// | request | `load_duration` |
/// |---|---|
/// | warm at 8192, cold | 4.96s |
/// | ask again at 8192 | **0.00s** — the warm survived |
/// | ask at 32768 | **3.09s** — reloaded |
/// | back to 8192 | **2.86s** — reloaded again |
///
/// **Why 32768 and not less, carried over from Heimdall where this used to live.**
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
/// Heimdall's four-screenshot cap will ever use.
///
/// `crates/bench` deliberately keeps its own 8192 constant instead of reading
/// this: it is a measurement harness, and changing its context would make F1's
/// saved runs incomparable to new ones.
pub const DEFAULT_NUM_CTX: u32 = 32768;

/// Chosen at Gate 2 (2026-10-01) from the H4 benchmark: 25/48 against 6/48 for
/// `DEFAULT_MODEL`, at 32s/task. Text-only — reports no `vision` capability — so
/// it is never the model Heimdall can use, and the two cannot be resident
/// together on this card: 18 GB plus the 6 GB vision model does not fit in 16 GB.
/// See `docs/results/H4-results.md`.
pub const DEFAULT_CODING_MODEL: &str = "qwen3-coder:30b";

/// What the apps agree on. Extended rather than replaced as Stage G lands —
/// G2 writes `model` here when you pick one, and Heimdall reads it on every summon.
///
/// `#[serde(default)]` on the struct means a file missing any field fills that
/// field from `Default` rather than failing to parse. So a config written before
/// a field existed still loads, and a hand-edited file holding only `{"model":
/// "..."}` is valid. That is what makes adding a field later cheap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub model: String,
    pub prompt: String,
    pub num_ctx: u32,
    /// The one folder Mímir may change files in, or `None` for no editing at
    /// all. Set only through a native folder picker the user operates — there is
    /// no command that takes a path for it, so nothing reaching the app's IPC can
    /// widen it. Heimdall and the CLI ignore it.
    pub workspace: Option<String>,
    /// Folders chosen before, newest first, for Mímir's new-chat screen. Only
    /// the picker adds to it, and the webview can pick an entry only by its
    /// position — so choosing a recent folder never lets a path in from outside,
    /// and `workspace` stays something the user picked with their own hands.
    pub recent_workspaces: Vec<String>,
    /// The check command for each folder, keyed by the folder as `workspace`
    /// stores it — the command the model may run to find out whether its edits
    /// broke the build. Mímir writes an entry only from the candidates
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
    /// The model Mímir offers to switch to once a project folder is chosen.
    /// An empty string turns the offer off entirely — there is no separate
    /// on/off flag, because an empty tag can never be installed and so
    /// [`crate::ollama::find`]-style lookups simply never match it.
    pub coding_model: String,
    /// The model Mímir switched away from, on the user's say-so, to load
    /// `coding_model`. What "switch back" restores once the folder is
    /// cleared. Cleared by switching back, and by any manual model pick —
    /// a pick made by hand ends the switch-back story outright, rather than
    /// fighting it on the next folder change.
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
        }
    }
}

/// `%APPDATA%\odin\config.json`, or `None` if `%APPDATA%` is not set.
///
/// Returns the path whether or not anything is there — callers that want to know
/// if it exists should ask the filesystem.
pub fn path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join("odin").join("config.json"))
}

/// Read the config, falling back to defaults for anything missing or unreadable.
///
/// Deliberately returns `Config` rather than `Result<Config, _>`. There is no
/// failure a caller could usefully handle: if the file is absent the defaults
/// *are* the right answer, and if it is corrupt, refusing to summon the popup
/// would be a worse outcome than ignoring it. Callers that need to know whether
/// a file exists can use [`path`].
pub fn load() -> Config {
    let Some(p) = path() else {
        return Config::default();
    };
    let Ok(text) = std::fs::read_to_string(&p) else {
        return Config::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// Write the config, creating `%APPDATA%\odin\` if it is not there.
///
/// This one *does* return a `Result`: a save is something the user asked for, so
/// silently doing nothing would be wrong. G2 writes the model choice through it.
pub fn save(config: &Config) -> Result<(), String> {
    let p = path().ok_or_else(|| "APPDATA is not set".to_string())?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    let json = serde_json::to_string_pretty(config)
        .map_err(|e| format!("serializing config: {e}"))?;
    std::fs::write(&p, json).map_err(|e| format!("writing {}: {e}", p.display()))
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
    fn empty_object_is_entirely_defaults() {
        assert_eq!(
            serde_json::from_str::<Config>("{}").unwrap(),
            Config::default()
        );
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

    /// **A typo can only make Mímir stricter.** Without the lenient reader the
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
}
