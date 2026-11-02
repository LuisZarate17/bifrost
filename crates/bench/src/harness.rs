//! What the probes that run `hugin` on a pinned commit share: a detached worktree, a sandbox for
//! Hugin's settings, the "is the model installed" check and the copy of the built `hugin.exe`.
//!
//! Included by `realprobe` and `askprobe` as `#[path = "../harness.rs"] mod harness;`, as
//! `probe.rs` is (a lib in this crate would be named `bench`, which a bin cannot share on Windows).

use std::path::{Path, PathBuf};
use std::process::Command;

use bifrost_core::config::Think;

// ── git ──────────────────────────────────────────────────────────────────────

/// `git -c core.autocrlf=false <args>` in `dir` (the current directory if none): the files a
/// worktree holds are the committed bytes, LF, whatever the machine's own setting is.
pub fn git(dir: Option<&Path>, args: &[&str]) -> Result<std::process::Output, String> {
    let mut cmd = Command::new("git");
    cmd.args(["-c", "core.autocrlf=false"]).args(args);
    if let Some(dir) = dir {
        cmd.current_dir(dir);
    }
    cmd.output().map_err(|e| format!("could not run git: {e}"))
}

pub fn first_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string()
}

/// A detached worktree at a commit, removed and pruned on drop unless `keep` is set.
pub struct Worktree {
    pub path: PathBuf,
    keep: bool,
}

impl Worktree {
    pub fn add(at: &Path, treeish: &str, keep: bool) -> Result<Self, String> {
        let _ = git(None, &["worktree", "prune"]);
        let _ = std::fs::remove_dir_all(at);
        if let Some(parent) = at.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let out = git(None, &["worktree", "add", "--detach", &at.to_string_lossy(), treeish])?;
        if !out.status.success() {
            return Err(format!("could not check out {treeish}: {}", first_line(&out.stderr)));
        }
        Ok(Self { path: at.to_path_buf(), keep })
    }

    /// Back to the commit: tracked files restored, files that are not tracked removed. Git-ignored
    /// files (the Mimir `dist`) stay.
    pub fn reset(&self) -> Result<(), String> {
        for args in [&["checkout", "--", "."][..], &["clean", "-fdq"]] {
            let out = git(Some(&self.path), args)?;
            if !out.status.success() {
                return Err(format!("git {} failed: {}", args[0], first_line(&out.stderr)));
            }
        }
        Ok(())
    }
}

impl Drop for Worktree {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        let _ = git(None, &["worktree", "remove", "--force", &self.path.to_string_lossy()]);
        let _ = std::fs::remove_dir_all(&self.path);
        let _ = git(None, &["worktree", "prune"]);
    }
}

// ── Hugin ────────────────────────────────────────────────────────────────────

/// The config the sandbox holds: the model for both the chat and the coding model, and its
/// `think` setting if one was asked for.
pub fn sandbox_config(model: &str, think: Option<Think>) -> String {
    let mut config = serde_json::json!({ "model": model, "coding_model": model });
    if let Some(think) = think {
        config["model_settings"] = serde_json::json!({ model: { "think": think.name() } });
    }
    config.to_string()
}

pub struct Sandbox {
    pub env: Vec<(String, String)>,
    /// The sandbox's `LOCALAPPDATA` (only `realprobe` clears what Hugin builds in it).
    #[allow(dead_code)]
    local: PathBuf,
}

impl Sandbox {
    /// A folder of this invocation's own for everything Hugin would put under the user's profile.
    pub fn make(root: &Path, model: &str, think: Option<Think>) -> Result<Self, String> {
        let (appdata, local, temp) = (root.join("appdata"), root.join("local"), root.join("temp"));
        for dir in [&appdata.join("bifrost"), &local, &temp] {
            std::fs::create_dir_all(dir).map_err(|e| format!("could not make {}: {e}", dir.display()))?;
        }
        let config = appdata.join("bifrost").join("config.json");
        std::fs::write(&config, sandbox_config(model, think)).map_err(|e| format!("could not write {}: {e}", config.display()))?;
        let show = |p: &Path| p.to_string_lossy().into_owned();
        Ok(Self {
            local: local.clone(),
            env: vec![
                // Hugin's own check builds the model's patch; it must not fetch a new dependency.
                ("CARGO_NET_OFFLINE".to_string(), "true".to_string()),
                ("APPDATA".to_string(), show(&appdata)),
                ("LOCALAPPDATA".to_string(), show(&local)),
                ("TEMP".to_string(), show(&temp)),
                ("TMP".to_string(), show(&temp)),
            ],
        })
    }

    /// Delete what `hugin -p` built for its check, `<LOCALAPPDATA>ifrost\hugin-p`: a folder of
    /// build output for each repository it ran in, which a long invocation would fill the disk
    /// with. Best effort; the next run makes it again.
    #[allow(dead_code)]
    pub fn clear_build_cache(&self) {
        let _ = std::fs::remove_dir_all(self.local.join("bifrost").join("hugin-p"));
    }
}

/// `hugin.exe` as built in `target`, copied into `scratch` so a later build cannot replace the
/// binary mid-run. The path of the copy.
pub fn copy_hugin(target: &Path, scratch: &Path) -> Result<PathBuf, String> {
    let built = target.join("debug").join("hugin.exe");
    let hugin = scratch.join("hugin.exe");
    std::fs::create_dir_all(scratch)
        .and_then(|()| std::fs::copy(&built, &hugin).map(|_| ()))
        .map_err(|e| format!("could not copy {}: {e}", built.display()))?;
    Ok(hugin)
}

// ── Ollama ───────────────────────────────────────────────────────────────────

/// Whether Ollama has `model`, as `ollama list` names it: the tag, case aside; a name with no tag
/// is its `:latest`.
pub fn model_installed(installed: &[String], model: &str) -> bool {
    let want = model.trim().to_ascii_lowercase();
    let latest = if want.contains(':') { want.clone() } else { format!("{want}:latest") };
    installed.iter().any(|n| {
        let n = n.to_ascii_lowercase();
        n == want || n == latest
    })
}

/// A real run does nothing for a model Ollama does not have.
pub fn require_installed(installed: &[String], model: &str) -> Result<(), String> {
    if model_installed(installed, model) {
        Ok(())
    } else {
        Err(format!("{model} is not installed in Ollama (ollama pull {model}); installed: {}", installed.join(", ")))
    }
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default()
}
