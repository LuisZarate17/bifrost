//! What Hugin learns when it starts: the folder, where git stands in it, the model,
//! and the model warming up in the background.
//!
//! **Only the folder can stop Hugin.** Everything else here is a convenience, so a
//! missing git, an unreadable config or an Ollama that will not start is a blank in the
//! header or a line of error, never a reason not to start.

use std::path::Path;
use std::process::Command;
use std::sync::mpsc::{self, Receiver};

use bifrost_core::edit::Workspace;
use bifrost_core::{config, ollama};

pub struct Start {
    /// The folder, as the model and the tools see it.
    pub folder: String,
    pub branch: Option<String>,
    pub changed: Option<usize>,
    pub model: String,
    pub num_ctx: u32,
}

/// Open the folder and learn the rest. `Err` says why the folder cannot be used.
pub fn learn(folder: &Path) -> Result<Start, String> {
    let ws = Workspace::open(folder)?;
    let folder = ws.root_display();
    let (branch, changed) = git_state(Path::new(&folder));
    let cfg = config::load();
    Ok(Start {
        folder,
        branch,
        changed,
        model: cfg.coding_model,
        num_ctx: cfg.num_ctx,
    })
}

/// The branch and the number of changed files, or `None` for both when `folder` is not
/// in a repository or git cannot say. The count is `git status --porcelain` lines, which
/// is one per changed or untracked file.
pub fn git_state(folder: &Path) -> (Option<String>, Option<usize>) {
    let Some(branch) = git(folder, &["rev-parse", "--abbrev-ref", "HEAD"]) else {
        return (None, None);
    };
    let branch = branch.trim().to_string();
    if branch.is_empty() {
        return (None, None);
    }
    let changed = git(folder, &["status", "--porcelain"]).map(|s| s.lines().filter(|l| !l.trim().is_empty()).count());
    (Some(branch), changed)
}

/// What git printed, if it ran and succeeded.
fn git(folder: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).current_dir(folder).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Start Ollama if it is not running and load `model` into memory, on a thread of its
/// own so the prompt is not held up by a cold start. **A failure is sent once** through
/// the receiver, for the line mode to show; the first turn would meet the same problem
/// and say so in its own words.
pub fn warm(model: String, num_ctx: u32) -> Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let warmed = ollama::ensure_running().and_then(|_| ollama::load(&model, num_ctx));
        if let Err(e) = warmed {
            let _ = tx.send(format!("could not warm up {model}: {e}"));
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("hugin-start-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_folder_that_is_not_a_repository_has_no_branch() {
        let dir = scratch("plain");
        assert_eq!(git_state(&dir), (None, None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_repository_has_a_branch_and_counts_changed_files() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let dir = scratch("repo");
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-b", "work"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "--allow-empty",
            "-m",
            "x",
        ]);
        std::fs::write(dir.join("a.txt"), "a").unwrap();
        std::fs::write(dir.join("b.txt"), "b").unwrap();
        assert_eq!(git_state(&dir), (Some("work".to_string()), Some(2)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_that_does_not_exist_is_the_one_thing_that_fails() {
        assert!(learn(&std::env::temp_dir().join("hugin-no-such-folder-here")).is_err());
    }
}
