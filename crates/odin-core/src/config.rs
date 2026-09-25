//! The settings all three binaries share, at `%APPDATA%\odin\config.json`.
//!
//! **A missing file is a normal state, not an error.** `load()` cannot fail: no
//! file, unreadable file, or malformed JSON all yield the built-in defaults, and
//! nothing here prints or panics. That is deliberate and load-bearing — PLAN.md
//! decision #5 requires Warden to keep working on a machine where Citadel was
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
/// in Warden's `App.tsx` — four places to keep in step, which nothing enforced.
/// `evals/prompts/current.txt` is byte-identical and stays, because that one is
/// measured input for the bench rather than a copy of this.
pub const DEFAULT_PROMPT: &str =
    "What am I looking at? If there's an error or something broken, explain it \
     and the fix. Otherwise explain what matters here.";

/// Warden's value, and now everyone's.
///
/// **Measured 2026-09-19.** Ollama keys a loaded runner by the model *and* by
/// `num_ctx`, so asking at a different context than you warmed at tears the
/// runner down and rebuilds it. The CLI used to warm at 8192 while Warden asked
/// at 32768, and every switch between them cost about three seconds:
///
/// | request | `load_duration` |
/// |---|---|
/// | warm at 8192, cold | 4.96s |
/// | ask again at 8192 | **0.00s** — the warm survived |
/// | ask at 32768 | **3.09s** — reloaded |
/// | back to 8192 | **2.86s** — reloaded again |
///
/// **Why 32768 and not less, carried over from Warden where this used to live.**
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
/// Warden's four-screenshot cap will ever use.
///
/// `crates/bench` deliberately keeps its own 8192 constant instead of reading
/// this: it is a measurement harness, and changing its context would make F1's
/// saved runs incomparable to new ones.
pub const DEFAULT_NUM_CTX: u32 = 32768;

/// What the apps agree on. Extended rather than replaced as Stage G lands —
/// G2 writes `model` here when you pick one, and Warden reads it on every summon.
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
    /// The one folder Citadel may change files in, or `None` for no editing at
    /// all. Set only through a native folder picker the user operates — there is
    /// no command that takes a path for it, so nothing reaching the app's IPC can
    /// widen it. Warden and the CLI ignore it.
    pub workspace: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: DEFAULT_MODEL.to_string(),
            prompt: DEFAULT_PROMPT.to_string(),
            num_ctx: DEFAULT_NUM_CTX,
            workspace: None,
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
        };
        let back: Config = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(c, back);
    }
}
