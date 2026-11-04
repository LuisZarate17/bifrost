//! Check with Claude (I6): asking the `claude` command to review a turn's changes.
//!
//! **The one place Bifrost reaches the cloud, and only on a click.** Mimir's button
//! calls [`turn_diff`] to show what would go, and [`run`] only after the user has
//! seen that and pressed Send. Nothing here runs by itself, and with the switch off
//! (`Config::claude_checks`) Mimir never calls it.
//!
//! It runs `claude -p`, so it uses whatever the user already signed `claude` in with —
//! no key is stored here. The prompt goes in on standard input. **Only the question and
//! the diff go out**, and that takes more than `--tools ""`, which only turns off the
//! built-in tools: `--safe-mode` keeps the user's hooks, `CLAUDE.md` files and settings
//! from loading, `--strict-mcp-config` keeps their MCP servers (claude.ai connectors
//! included) from starting, and the command runs in an empty folder of its own, so it
//! finds no project notes or settings either.
//!
//! The run is [`check::run_with_input`]: a job for the whole process tree, no console
//! window, a timeout, and Stop.
//!
//! Blocking, like the rest of this crate. Tauri callers go through `spawn_blocking`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::check::{self, Ended};
use crate::edit::{self, Content, MAX_EDIT_BYTES, Net};

/// How long a check may take before it is stopped.
pub const RUN_TIMEOUT: Duration = Duration::from_secs(180);

/// How long `claude auth status` may take. It makes no model call; it answers in a
/// fraction of a second when it works at all.
const STATUS_TIMEOUT: Duration = Duration::from_secs(15);

/// The most of the command's output that is kept — room for any answer, where
/// [`check::HEAD`] would cut the JSON the answer comes in.
const KEEP: usize = 512 * 1024;

/// The most diff text one check sends. A file whose diff would take it past this is
/// named but not sent.
pub const MAX_DIFF_BYTES: usize = 300 * 1024;

/// The most of the user's question one check sends. A longer one is cut, and the
/// prompt and the preview both say so.
pub const MAX_QUESTION_BYTES: usize = 20 * 1024;

/// Added to the command's own system prompt. Kept to plain words: it is one argument,
/// and a `claude.cmd` shim passes it through cmd.exe.
const REVIEW_PROMPT: &str = "You are checking a change made by a local coding model. \
    Review the diff against the question the user asked it. Point out bugs, missed requirements \
    and risky changes. Be concise. You cannot see any other files, so say so rather than guess \
    about code you were not shown.";

// ── finding the command ─────────────────────────────────────────────────────

/// Where the `claude` command is: the path set in the config if there is one,
/// else `claude` on `PATH`, else `%USERPROFILE%\.local\bin\claude.exe` — where its
/// installer puts it, and which an app started from Explorer may not have on its
/// `PATH`. On Windows `claude.cmd` is found as well, where an npm install leaves it.
/// `None` when none of these is a file.
pub fn resolve(configured: Option<&str>) -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    resolve_in(configured, std::env::var_os("PATH"), home)
}

/// [`resolve`] given its `PATH` and home folder, so a test need not change the process's.
fn resolve_in(
    configured: Option<&str>,
    path: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    if let Some(p) = configured.map(str::trim).filter(|p| !p.is_empty()) {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let names: &[&str] = if cfg!(windows) {
        &["claude.exe", "claude.cmd"]
    } else {
        &["claude"]
    };
    let in_dir = |dir: &Path| names.iter().map(|n| dir.join(n)).find(|p| p.is_file());
    let on_path = path
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .find_map(|dir| in_dir(&dir));
    on_path.or_else(|| in_dir(&PathBuf::from(home?).join(".local").join("bin")))
}

/// An empty folder to run `claude` in, so it finds no project notes.
fn neutral_dir() -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join("bifrost-claude-check");
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not make {}: {e}", dir.display()))?;
    Ok(dir)
}

/// What the settings line says about the `claude` command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub found: bool,
    pub path: Option<String>,
    pub signed_in: bool,
    pub email: Option<String>,
}

/// Whether the command is there and signed in, by running `claude auth status`.
/// That makes no model call, so asking costs nothing.
pub fn status(configured: Option<&str>) -> Status {
    let Some(path) = resolve(configured) else {
        return Status {
            found: false,
            path: None,
            signed_in: false,
            email: None,
        };
    };
    let shown = path.display().to_string();
    let ask = || -> Option<serde_json::Value> {
        let dir = neutral_dir().ok()?;
        let argv = vec![shown.clone(), "auth".to_string(), "status".to_string()];
        let out = check::run_with_input(&dir, &argv, b"", KEEP, STATUS_TIMEOUT, &[], || false).ok()?;
        json_object(&out.output)
    };
    let reply = ask();
    let field = |name: &str| reply.as_ref().and_then(|v| v.get(name));
    Status {
        found: true,
        path: Some(shown),
        signed_in: field("loggedIn").and_then(|v| v.as_bool()).unwrap_or(false),
        email: field("email").and_then(|v| v.as_str()).map(str::to_string),
    }
}

/// The JSON object in a command's output: the whole of it, else a line that is one,
/// else whatever lies between the first `{` and the last `}`. Output and errors share
/// one pipe, so a warning may come before or after it.
fn json_object(output: &str) -> Option<serde_json::Value> {
    let object = |s: &str| {
        serde_json::from_str::<serde_json::Value>(s.trim())
            .ok()
            .filter(|v| v.is_object())
    };
    object(output)
        .or_else(|| {
            output
                .lines()
                .rev()
                .find_map(|l| l.trim().starts_with('{').then(|| object(l)).flatten())
        })
        .or_else(|| {
            let (start, end) = (output.find('{')?, output.rfind('}')?);
            output.get(start..=end).and_then(object)
        })
}

// ── what would be sent ──────────────────────────────────────────────────────

/// A turn's changes as one unified diff, and how much there is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diff {
    /// How many files the turn changed.
    pub files: usize,
    /// The size of `text`.
    pub bytes: usize,
    pub text: String,
}

/// The diff of what the turn behind these undo ids did, built from the undo records
/// and the files as they are now: per file, the original (the oldest record's backup,
/// nothing for a create) against the file today (nothing for a delete). A move is
/// shown as `old -> new`. A binary file, a file over [`MAX_EDIT_BYTES`] and anything
/// past [`MAX_DIFF_BYTES`] is named in one line and not sent.
///
/// This is also the preview: it is the exact text [`run`] sends.
pub fn turn_diff(ids: &[String], backups: &Path) -> Result<Diff, String> {
    let nets = edit::turn_changes(ids, backups)?;
    let mut text = String::new();
    let mut files = 0;
    for net in &nets {
        let Some(part) = file_diff(net) else { continue };
        files += 1;
        // Counted whether or not it fits, so the preview says how many files changed.
        if text.len() + part.len() > MAX_DIFF_BYTES {
            text.push_str(&format!(
                "# {}: not shown, the diff is over the {} KB limit\n",
                name_of(net),
                MAX_DIFF_BYTES / 1024
            ));
        } else {
            text.push_str(&part);
        }
    }
    Ok(Diff {
        files,
        bytes: text.len(),
        text,
    })
}

fn name_of(net: &Net) -> String {
    match &net.moved_to {
        Some(to) => format!("{} -> {to}", net.display),
        None => net.display.clone(),
    }
}

/// One side of a file as text: `None` for no file, `Err` with why it cannot be shown.
fn side(content: &Content) -> Result<Option<String>, String> {
    match content {
        Content::Absent => Ok(None),
        Content::TooBig(n) => Err(format!("{n} bytes, over the {MAX_EDIT_BYTES} byte limit")),
        Content::Bytes(b) => {
            let rest = b.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(b);
            if rest.contains(&0) || std::str::from_utf8(rest).is_err() {
                return Err("not text".to_string());
            }
            Ok(Some(edit::for_model(b)))
        }
    }
}

/// One file's part of the diff, or `None` when the turn left it as it found it.
fn file_diff(net: &Net) -> Option<String> {
    let name = name_of(net);
    let (before, after) = match (side(&net.before), side(&net.after)) {
        (Ok(b), Ok(a)) => (b, a),
        (Err(why), _) | (_, Err(why)) => return Some(format!("# {name}: not shown ({why})\n")),
    };
    if before.is_none() && after.is_none() {
        return None;
    }
    let rename = if net.moved_to.is_some() {
        format!("# renamed: {name}\n")
    } else {
        String::new()
    };
    if before == after {
        return (!rename.is_empty()).then_some(rename);
    }
    let old = format!("a/{}", net.display);
    let new = format!("b/{}", net.moved_to.as_ref().unwrap_or(&net.display));
    let diff = similar::TextDiff::from_lines(before.as_deref().unwrap_or(""), after.as_deref().unwrap_or(""))
        .unified_diff()
        .context_radius(3)
        .header(
            if before.is_some() { &old } else { "/dev/null" },
            if after.is_some() { &new } else { "/dev/null" },
        )
        .to_string();
    Some(format!("{rename}{diff}"))
}

// ── the run ─────────────────────────────────────────────────────────────────

/// Why a check gave no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    /// The user pressed Stop.
    Stopped,
    /// Anything else, in words for the card.
    Failed(String),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::Stopped => f.write_str("stopped"),
            RunError::Failed(m) => f.write_str(m),
        }
    }
}

fn failed<T>(message: impl Into<String>) -> Result<T, RunError> {
    Err(RunError::Failed(message.into()))
}

const NOT_SIGNED_IN: &str = "Claude is not signed in. Run `claude` in a terminal to sign in, then try again.";

/// The text that goes to Claude on standard input, and whether the question had to be
/// cut to [`MAX_QUESTION_BYTES`] to get there.
pub fn prompt(question: &str, diff: &str) -> (String, bool) {
    let question = question.trim();
    let mut end = question.len().min(MAX_QUESTION_BYTES);
    while !question.is_char_boundary(end) {
        end -= 1;
    }
    let cut = end < question.len();
    let note = if cut {
        format!(
            "\n[The question was {} bytes; only the first {} are shown.]",
            question.len(),
            end
        )
    } else {
        String::new()
    };
    let text = format!(
        "The user asked a local coding model:\n\n<question>\n{}{note}\n</question>\n\n\
         The model's changes in response, as a unified diff:\n\n<diff>\n{}\n</diff>\n",
        &question[..end],
        diff.trim_end()
    );
    (text, cut)
}

/// Exactly what a check would send for a turn: the prompt built from the question and
/// the turn's diff as it is now, with the sizes the preview shows and a fingerprint of
/// the text. The caller builds this again at send time and refuses if the fingerprint
/// differs from the one the user was shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    /// How many files the turn changed.
    pub files: usize,
    /// The size of `text` — what is actually sent, the question included.
    pub bytes: usize,
    /// Whether the question was cut to [`MAX_QUESTION_BYTES`].
    pub question_cut: bool,
    pub text: String,
    /// [`edit::fingerprint`] of `text`, in hex (a u64 does not survive JSON as a number).
    pub hash: String,
}

/// Build what a check would send. `Err` when the turn has no changes left.
pub fn prepare(question: &str, ids: &[String], backups: &Path) -> Result<Prepared, String> {
    let diff = turn_diff(ids, backups)?;
    if diff.files == 0 {
        return Err("This turn has no changes left to check — they were undone.".to_string());
    }
    let (text, question_cut) = prompt(question, &diff.text);
    let hash = format!("{:016x}", edit::fingerprint(text.as_bytes()));
    Ok(Prepared {
        files: diff.files,
        bytes: text.len(),
        question_cut,
        text,
        hash,
    })
}

/// Send `prompt` (see [`prepare`]) to Claude for review. `configured` is the config's
/// `claude_command`. `cancelled` is polled every 100 ms: when it says so the whole
/// process tree is killed and the result is [`RunError::Stopped`].
pub fn run(configured: Option<&str>, prompt: &str, cancelled: impl Fn() -> bool) -> Result<String, RunError> {
    let Some(command) = resolve(configured) else {
        return failed(
            "The `claude` command was not found. Install Claude Code, or set `claude_command` in config.json to its path.",
        );
    };
    let dir = neutral_dir().map_err(RunError::Failed)?;
    let argv: Vec<String> = [
        &command.display().to_string(),
        "-p",
        "--output-format",
        "json",
        "--tools",
        "",
        "--no-session-persistence",
        "--safe-mode",
        "--strict-mcp-config",
        "--append-system-prompt",
        REVIEW_PROMPT,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    let outcome = check::run_with_input(&dir, &argv, prompt.as_bytes(), KEEP, RUN_TIMEOUT, &[], cancelled)
        .map_err(|e| RunError::Failed(format!("Could not start `claude`: {e}")))?;
    if outcome.blocked.is_some() {
        let name = command
            .file_name()
            .map_or("claude".into(), |n| n.to_string_lossy().into_owned());
        return failed(format!(
            "Windows blocked {name} from running (Smart App Control), so the check could not start."
        ));
    }
    match outcome.ended {
        Ended::Stopped => Err(RunError::Stopped),
        Ended::TimedOut => failed(format!(
            "Claude did not answer within {} seconds, so the check was stopped.",
            RUN_TIMEOUT.as_secs()
        )),
        Ended::DiskFull => failed("The disk was nearly full, so the check was stopped.".to_string()),
        Ended::Exited(code) => answer(&outcome.output, code),
    }
}

/// The answer in `claude -p --output-format json`'s output, or why there is none.
fn answer(output: &str, code: Option<i32>) -> Result<String, RunError> {
    let reply = json_object(output);
    let text = reply
        .as_ref()
        .and_then(|v| v.get("result"))
        .and_then(|v| v.as_str())
        .map(str::trim);
    let is_error = reply
        .as_ref()
        .and_then(|v| v.get("is_error"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if is_error || (reply.is_some() && code != Some(0)) {
        let said = text.filter(|t| !t.is_empty());
        let lower = said.unwrap_or_default().to_lowercase();
        if lower.contains("not logged in") || lower.contains("/login") || lower.contains("invalid api key") {
            return failed(NOT_SIGNED_IN);
        }
        return failed(match said {
            Some(t) => format!("Claude reported an error: {t}"),
            None => format!(
                "Claude reported an error (exit {}).",
                code.map_or("none".into(), |c| c.to_string())
            ),
        });
    }
    if let Some(text) = text {
        return if text.is_empty() {
            failed("Claude sent back an empty answer.")
        } else {
            Ok(text.to_string())
        };
    }
    let lower = output.to_lowercase();
    if lower.contains("not logged in") || lower.contains("/login") {
        return failed(NOT_SIGNED_IN);
    }
    let seen: String = output.trim().chars().take(300).collect();
    failed(match code {
        Some(0) | None => format!("Claude's reply could not be read: {seen}"),
        Some(c) => format!("`claude` exited with code {c}: {seen}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh folder per test, as in `edit`'s tests.
    struct Dir(PathBuf);

    impl Dir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("bifrost-claude-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Dir(dir)
        }
        fn write(&self, name: &str, text: &str) -> PathBuf {
            let p = self.0.join(name);
            std::fs::write(&p, text).unwrap();
            p
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn net(display: &str, before: Content, after: Content) -> Net {
        Net {
            display: display.into(),
            moved_to: None,
            before,
            after,
        }
    }

    fn text(s: &str) -> Content {
        Content::Bytes(s.as_bytes().to_vec())
    }

    #[test]
    fn an_edit_is_a_unified_diff() {
        let d = file_diff(&net("src/a.rs", text("one\ntwo\nthree\n"), text("one\n2\nthree\n"))).unwrap();
        assert!(d.starts_with("--- a/src/a.rs\n+++ b/src/a.rs\n@@"), "{d}");
        assert!(d.contains("-two\n+2\n"), "{d}");
    }

    #[test]
    fn a_create_comes_from_nothing_and_a_delete_goes_to_nothing() {
        let c = file_diff(&net("new.txt", Content::Absent, text("hi\n"))).unwrap();
        assert!(c.starts_with("--- /dev/null\n+++ b/new.txt\n"), "{c}");
        assert!(c.contains("+hi\n"));
        let d = file_diff(&net("old.txt", text("bye\n"), Content::Absent)).unwrap();
        assert!(d.starts_with("--- a/old.txt\n+++ /dev/null\n"), "{d}");
        assert!(d.contains("-bye\n"));
    }

    #[test]
    fn a_file_left_as_it_was_is_not_in_the_diff() {
        assert_eq!(file_diff(&net("same.txt", text("x\n"), text("x\n"))), None);
        assert_eq!(file_diff(&net("gone.txt", Content::Absent, Content::Absent)), None);
    }

    #[test]
    fn a_move_shows_both_names() {
        let mut moved = net("a.txt", text("x\n"), text("x\n"));
        moved.moved_to = Some("sub/b.txt".into());
        assert_eq!(file_diff(&moved).unwrap(), "# renamed: a.txt -> sub/b.txt\n");
        moved.after = text("x\ny\n");
        let d = file_diff(&moved).unwrap();
        assert!(
            d.contains("# renamed: a.txt -> sub/b.txt\n--- a/a.txt\n+++ b/sub/b.txt\n"),
            "{d}"
        );
        assert!(d.contains("+y\n"));
    }

    #[test]
    fn crlf_and_a_bom_do_not_show_up_as_changes() {
        let before = Content::Bytes(b"\xEF\xBB\xBFone\r\ntwo\r\n".to_vec());
        let after = Content::Bytes(b"\xEF\xBB\xBFone\r\n2\r\n".to_vec());
        let d = file_diff(&net("w.txt", before, after)).unwrap();
        assert!(!d.contains('\r') && !d.contains('\u{feff}'), "{d:?}");
        assert!(d.contains("-two\n+2\n"), "{d}");
    }

    #[test]
    fn binary_and_oversize_files_are_named_not_sent() {
        let b = file_diff(&net(
            "img.png",
            Content::Bytes(vec![0, 1, 2]),
            Content::Bytes(vec![0, 9]),
        ))
        .unwrap();
        assert_eq!(b, "# img.png: not shown (not text)\n");
        let big = file_diff(&net("big.txt", text("a\n"), Content::TooBig(5_000_000))).unwrap();
        assert!(
            big.starts_with("# big.txt: not shown (5000000 bytes, over the"),
            "{big}"
        );
    }

    /// Make an edit, a create, a delete and a move with the real edit code, then ask
    /// for the turn.
    #[test]
    fn a_turns_diff_comes_from_its_undo_records() {
        let dir = Dir::new("turn");
        let project = dir.0.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let ws = edit::Workspace::open(&project).unwrap();
        let backups = dir.0.join("backups");
        std::fs::write(project.join("a.txt"), "one\r\ntwo\r\n").unwrap();
        std::fs::write(project.join("gone.txt"), "bye\n").unwrap();
        std::fs::write(project.join("old.txt"), "same\n").unwrap();

        let go = |plan: Result<edit::Plan, String>| edit::apply(&plan.unwrap(), &backups).unwrap().id;
        let ids = vec![
            go(edit::plan_replace(&ws, "a.txt", "two", "2")),
            go(edit::plan_create(&ws, "n.txt", "new\n")),
            go(edit::plan_delete(&ws, "gone.txt")),
            go(edit::plan_move(&ws, "old.txt", "sub/new.txt")),
        ];

        let diff = turn_diff(&ids, &backups).unwrap();
        assert_eq!(diff.files, 4, "{}", diff.text);
        assert_eq!(diff.bytes, diff.text.len());
        let t = &diff.text;
        assert!(
            t.contains("--- a/a.txt\n+++ b/a.txt\n") && t.contains("-two\n+2\n"),
            "{t}"
        );
        assert!(!t.contains('\r'), "{t:?}");
        assert!(t.contains("--- /dev/null\n+++ b/n.txt\n"), "{t}");
        assert!(
            t.contains("--- a/gone.txt\n+++ /dev/null\n") && t.contains("-bye\n"),
            "{t}"
        );
        assert!(t.contains("# renamed: old.txt -> sub/new.txt\n"), "{t}");
        // The file as it is now: a hand edit since the turn is in the diff too.
        std::fs::write(project.join("n.txt"), "new\nmore\n").unwrap();
        assert!(turn_diff(&ids, &backups).unwrap().text.contains("+more\n"));
        // An undone change has no record, so it is no longer part of the turn.
        edit::undo(&ids[2], &backups).unwrap();
        assert_eq!(turn_diff(&ids, &backups).unwrap().files, 3);
    }

    #[test]
    fn json_is_found_among_other_output() {
        let v = json_object("warning: x\n{\"result\":\"ok\"}\n").unwrap();
        assert_eq!(v["result"], "ok");
        assert!(json_object("no json here").is_none());
        assert!(json_object("[1,2]").is_none());
    }

    #[test]
    fn an_answer_is_the_result_field() {
        let ok = answer(
            r#"{"type":"result","is_error":false,"result":"  Looks fine.\n"}"#,
            Some(0),
        );
        assert_eq!(ok, Ok("Looks fine.".to_string()));
    }

    #[test]
    fn failures_are_said_in_words() {
        let msg = |r: Result<String, RunError>| match r {
            Err(RunError::Failed(m)) => m,
            other => panic!("{other:?}"),
        };
        assert!(
            msg(answer(
                r#"{"is_error":true,"result":"Not logged in - Please run /login"}"#,
                Some(1)
            ))
            .contains("not signed in")
        );
        assert!(
            msg(answer(r#"{"is_error":true,"result":"Overloaded"}"#, Some(0)))
                .contains("Claude reported an error: Overloaded")
        );
        assert!(msg(answer(r#"{"result":"x"}"#, Some(2))).contains("reported an error"));
        assert!(msg(answer(r#"{"result":""}"#, Some(0))).contains("empty"));
        assert!(msg(answer("garbage", Some(0))).contains("could not be read"));
        assert!(msg(answer("boom", Some(3))).contains("exited with code 3: boom"));
    }

    #[test]
    fn the_prompt_carries_the_question_and_the_diff() {
        let (p, cut) = prompt("  fix the bug \n", "--- a/x\n+++ b/x\n");
        assert!(!cut);
        assert!(p.contains("<question>\nfix the bug\n</question>"));
        assert!(p.contains("<diff>\n--- a/x\n+++ b/x\n</diff>"));
    }

    #[test]
    fn a_long_question_is_cut_and_says_so() {
        // Multi-byte characters, so the cut must land on a boundary.
        let long = "\u{e9}".repeat(MAX_QUESTION_BYTES);
        let (p, cut) = prompt(&long, "d");
        assert!(cut);
        assert!(p.contains("only the first 20480 are shown"), "{}", &p[..200]);
        assert!(p.len() < MAX_QUESTION_BYTES + 400);
    }

    #[test]
    fn the_preview_is_the_prompt_and_its_hash_follows_the_files() {
        let dir = Dir::new("prepare");
        let project = dir.0.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let ws = edit::Workspace::open(&project).unwrap();
        let backups = dir.0.join("backups");
        std::fs::write(project.join("a.txt"), "one\n").unwrap();
        let id = edit::apply(&edit::plan_replace(&ws, "a.txt", "one", "two").unwrap(), &backups)
            .unwrap()
            .id;
        let ids = vec![id];

        let a = prepare("q", &ids, &backups).unwrap();
        assert_eq!(
            a,
            prepare("q", &ids, &backups).unwrap(),
            "the same turn gives the same preview"
        );
        assert_eq!(a.bytes, a.text.len());
        assert!(
            a.text.contains("<question>\nq\n"),
            "the size shown includes the question"
        );
        assert!(prepare("a much longer question", &ids, &backups).unwrap().bytes > a.bytes);
        std::fs::write(project.join("a.txt"), "three\n").unwrap();
        assert_ne!(
            prepare("q", &ids, &backups).unwrap().hash,
            a.hash,
            "a changed file is a different send"
        );
        assert!(prepare("q", &[], &backups).is_err(), "nothing to check");
    }

    #[test]
    fn a_configured_path_must_be_a_file() {
        let dir = Dir::new("resolve");
        let f = dir.write("claude.cmd", "@echo off\n");
        assert_eq!(resolve(Some(f.to_str().unwrap())), Some(f));
        assert_eq!(resolve(Some(dir.0.join("none.cmd").to_str().unwrap())), None);
    }

    #[cfg(windows)]
    #[test]
    fn an_npm_claude_cmd_is_found_on_path_and_in_the_fallback() {
        let dir = Dir::new("npm");
        let bin = dir.0.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("claude.cmd"), "@echo off\n").unwrap();
        let path = std::env::join_paths([dir.0.join("empty"), bin.clone()]).unwrap();
        assert_eq!(resolve_in(None, Some(path), None), Some(bin.join("claude.cmd")));

        let home = dir.0.join("home");
        let local = home.join(".local").join("bin");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::write(local.join("claude.cmd"), "@echo off\n").unwrap();
        assert_eq!(
            resolve_in(None, None, Some(home.clone().into())),
            Some(local.join("claude.cmd"))
        );
        // An exe beside it wins.
        std::fs::write(local.join("claude.exe"), "").unwrap();
        assert_eq!(
            resolve_in(None, None, Some(home.into())),
            Some(local.join("claude.exe"))
        );
    }

    // ── against a stand-in `claude` ─────────────────────────────────────────

    /// A `claude.cmd` that records what it was given beside itself and prints `reply`.
    #[cfg(windows)]
    fn stand_in(dir: &Dir, reply: &str, extra: &str) -> String {
        let script = format!(
            "@echo off\r\n\
             if \"%1\"==\"auth\" (\r\n  echo {{\"loggedIn\":true,\"authMethod\":\"claude.ai\",\"email\":\"me@example.com\"}}\r\n  exit /b 0\r\n)\r\n\
             findstr \"^\" > \"%~dp0stdin.txt\"\r\n\
             echo %* > \"%~dp0args.txt\"\r\n\
             cd > \"%~dp0cwd.txt\"\r\n\
             {extra}\r\n\
             echo {reply}\r\n"
        );
        dir.write("claude.cmd", &script).to_string_lossy().into_owned()
    }

    #[cfg(windows)]
    #[test]
    fn status_reads_claude_auth_status() {
        let dir = Dir::new("status");
        let cmd = stand_in(&dir, "", "");
        let s = status(Some(&cmd));
        assert!(s.found && s.signed_in, "{s:?}");
        assert_eq!(s.email.as_deref(), Some("me@example.com"));
        let missing = status(Some(dir.0.join("nope.cmd").to_str().unwrap()));
        assert_eq!((missing.found, missing.signed_in), (false, false));
    }

    #[cfg(windows)]
    #[test]
    fn run_sends_the_prompt_on_stdin_with_no_tools_in_a_folder_of_its_own() {
        let dir = Dir::new("run");
        let cmd = stand_in(&dir, r#"{"type":"result","is_error":false,"result":"Looks fine."}"#, "");
        let (sent, _) = prompt("fix it", "--- a/x\n+++ b/x\n+y\n");
        let got = run(Some(&cmd), &sent, || false).unwrap();
        assert_eq!(got, "Looks fine.");

        let stdin = std::fs::read_to_string(dir.0.join("stdin.txt")).unwrap();
        assert!(
            stdin.contains("fix it") && stdin.contains("+++ b/x") && stdin.contains("+y"),
            "{stdin}"
        );
        let args = std::fs::read_to_string(dir.0.join("args.txt")).unwrap();
        for want in [
            "-p",
            "--output-format json",
            "--tools \"\"",
            "--no-session-persistence",
            "--safe-mode",
            "--strict-mcp-config",
            "--append-system-prompt",
        ] {
            assert!(args.contains(want), "{want} in {args}");
        }
        let cwd = std::fs::read_to_string(dir.0.join("cwd.txt")).unwrap();
        assert!(cwd.trim_end().ends_with("bifrost-claude-check"), "{cwd}");
    }

    #[cfg(windows)]
    #[test]
    fn run_reports_an_error_reply_in_words() {
        let dir = Dir::new("err");
        let cmd = stand_in(
            &dir,
            r#"{"is_error":true,"result":"Not logged in - Please run /login"}"#,
            "",
        );
        match run(Some(&cmd), "q", || false) {
            Err(RunError::Failed(m)) => assert!(m.contains("not signed in"), "{m}"),
            other => panic!("{other:?}"),
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_smart_app_control_block_is_its_own_message() {
        let dir = Dir::new("sac");
        let cmd = stand_in(
            &dir,
            "{}",
            "echo An Application Control policy has blocked this file& exit /b 1",
        );
        match run(Some(&cmd), "q", || false) {
            Err(RunError::Failed(m)) => assert!(m.contains("Windows blocked claude.cmd"), "{m}"),
            other => panic!("{other:?}"),
        }
    }

    #[cfg(windows)]
    #[test]
    fn stop_ends_a_slow_claude() {
        let dir = Dir::new("stop");
        let cmd = stand_in(&dir, "{}", "ping -n 30 127.0.0.1 >nul");
        let started = std::time::Instant::now();
        let got = run(Some(&cmd), "q", || started.elapsed() > Duration::from_millis(700));
        assert_eq!(got, Err(RunError::Stopped));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_missing_command_is_said_in_words() {
        match run(Some(r"C:\no\such\claude.exe"), "q", || false) {
            Err(RunError::Failed(m)) => assert!(m.contains("not found"), "{m}"),
            other => panic!("{other:?}"),
        }
    }
}
