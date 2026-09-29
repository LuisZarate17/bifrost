//! Letting a model change files — and the rules that make that survivable.
//!
//! Reading a file wrong costs one bad answer. **Writing one wrong corrupts it**,
//! so everything here is built around four guarantees, each with a test:
//!
//! 1. **Nothing outside the folder you picked.** Every path is resolved and
//!    canonicalised before it is compared, so `..`, a symlink or a junction
//!    cannot walk out of it. `.git` is refused even inside it.
//! 2. **What you approved is what is written.** An edit is *planned* — resolved,
//!    matched, rendered to the exact bytes it will write, diffed — before anyone
//!    is asked. [`apply`] writes those bytes and nothing else.
//! 3. **Nothing is written over a file that changed since you saw the diff.** The
//!    plan carries a hash of the file as it was; [`apply`] refuses if the file on
//!    disk no longer matches it.
//! 4. **Every edit can be undone.** The original bytes are kept before anything
//!    is written, and [`undo`] restores them — but only if the file is still
//!    exactly what the edit left, so undo never throws away work done since.
//!
//! **Why search-and-replace rather than rewriting whole files or applying diffs.**
//! An exact match that must occur exactly once is its own safety check: if the
//! model misremembers the file, the match fails and nothing is written, instead
//! of a confident rewrite quietly dropping lines it forgot. The G7 probe measured
//! this against whole-file rewrites on the model that will actually do it.
//!
//! **Line endings are normalised to match, and preserved to write.** Every file in
//! this repo is checked out with CRLF (`core.autocrlf = true`) and a model writes
//! `\n` — so an exact byte match would fail on every file the user owns. Matching
//! happens on `\n` text; the result is written back in the file's own style.
//!
//! Blocking, like the rest of this crate. Tauri callers go through
//! `spawn_blocking`.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Files bigger than this are refused rather than edited. Generous — a model
/// has to read a file to edit it, and reads stop at 64 KB — but it keeps a
/// runaway request from rewriting something enormous.
pub const MAX_EDIT_BYTES: usize = 1024 * 1024;

/// Lines of unchanged context either side of a change, in the diff you approve.
const CONTEXT: usize = 3;

// ── the folder ──────────────────────────────────────────────────────────────

/// The one folder the model may change files in.
///
/// Held canonicalised — on Windows that is the `\\?\C:\…` verbatim form, with
/// the on-disk case — so every containment check compares like with like.
#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    /// Open a folder as the workspace. It must exist and be a directory.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self, String> {
        let dir = dir.as_ref();
        let root = std::fs::canonicalize(dir)
            .map_err(|e| format!("cannot use {} as the project folder: {e}", dir.display()))?;
        if !root.is_dir() {
            return Err(format!("{} is not a folder", dir.display()));
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The root as a person would write it — without the `\\?\` prefix.
    pub fn root_display(&self) -> String {
        plain(&self.root)
    }

    /// A path inside the workspace, shown relative to it with forward slashes.
    /// This is what the model and the approval card see.
    pub fn relative(&self, p: &Path) -> String {
        p.strip_prefix(&self.root)
            .map(|r| r.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| plain(p))
    }

    /// Resolve a path the model gave for a file that **must already exist**.
    pub fn resolve_existing(&self, requested: &str) -> Result<PathBuf, String> {
        let joined = self.join(requested)?;
        let real = std::fs::canonicalize(&joined)
            .map_err(|_| format!("{} does not exist in the project folder", clean(requested)))?;
        self.contain(&real, requested)?;
        if real.is_dir() {
            return Err(format!("{} is a folder, not a file", clean(requested)));
        }
        Ok(real)
    }

    /// Resolve a path for a file that **must not exist yet**.
    ///
    /// The folder it goes in must already exist: creating directories on the
    /// model's say-so is one more thing to approve and one more way to litter.
    pub fn resolve_new(&self, requested: &str) -> Result<PathBuf, String> {
        let joined = self.join(requested)?;
        if joined.exists() {
            return Err(format!(
                "{} already exists — change it with edit_file rather than creating it",
                clean(requested)
            ));
        }
        let name = joined
            .file_name()
            .ok_or_else(|| format!("{} is not a file name", clean(requested)))?
            .to_owned();
        let parent = joined
            .parent()
            .ok_or_else(|| format!("{} has no folder", clean(requested)))?;
        let parent = std::fs::canonicalize(parent).map_err(|_| {
            format!(
                "the folder for {} does not exist — only files in existing folders can be created",
                clean(requested)
            )
        })?;
        let real = parent.join(name);
        self.contain(&real, requested)?;
        Ok(real)
    }

    /// Relative paths are relative to the workspace; absolute ones stand as given
    /// and are checked by [`contain`] like any other.
    fn join(&self, requested: &str) -> Result<PathBuf, String> {
        let r = clean(requested);
        if r.is_empty() {
            return Err("no path was given".to_string());
        }
        if r.contains('\0') {
            return Err("that path contains a NUL byte".to_string());
        }
        let p = Path::new(&r);
        Ok(if p.is_absolute() { p.to_path_buf() } else { self.root.join(p) })
    }

    /// The containment check. `real` is already canonical, so `..`, symlinks and
    /// junctions have all been resolved to where they actually point.
    fn contain(&self, real: &Path, requested: &str) -> Result<(), String> {
        self.contain_with(real, requested, app_data().as_deref())
    }

    /// [`contain`](Self::contain), with Mímir's own folder passed in so a test
    /// can stand one up without touching the real `%APPDATA%`.
    fn contain_with(&self, real: &Path, requested: &str, app_data: Option<&Path>) -> Result<(), String> {
        if !real.starts_with(&self.root) {
            return Err(format!(
                "{} is outside the project folder ({}). Only files inside it can be changed.",
                clean(requested),
                self.root_display()
            ));
        }
        // Git's own files are not the user's files. Editing them can corrupt the
        // repository, and no request that means well needs to.
        let inside = real.strip_prefix(&self.root).unwrap_or(real);
        if inside
            .components()
            .any(|c| matches!(c, Component::Normal(n) if n.eq_ignore_ascii_case(".git")))
        {
            return Err(format!("{} is inside .git, which is never edited", clean(requested)));
        }
        // The check command is one a person chose, but cargo and npm take their
        // settings from the project they run in. A model that could edit
        // `.cargo/config.toml` (a `rustc-wrapper`), `rust-toolchain.toml` (a
        // toolchain `path`) or `.npmrc` (a `script-shell`) could decide what that
        // command really executes.
        // `name:stream` is the same file on NTFS, reached another way —
        // `.npmrc::$DATA` writes `.npmrc`. No file a project is made of needs a
        // colon in its name, so none gets one, and none slips past the rules
        // above or below by using one.
        if inside
            .components()
            .any(|c| matches!(c, Component::Normal(n) if n.to_string_lossy().contains(':')))
        {
            return Err(format!("{} names a file stream, which is never edited", clean(requested)));
        }
        if decides_what_the_check_runs(inside) {
            return Err(format!(
                "{} decides what the check command runs, so it is never edited",
                clean(requested)
            ));
        }
        // Mímir's settings and undo records. Only reachable when the folder you
        // chose contains %APPDATA% — your home folder, say — and then editing
        // `config.json` would let the model choose its own check command.
        if app_data.is_some_and(|d| real.starts_with(d)) {
            return Err(format!(
                "{} is inside Mímir's own settings folder, which is never edited",
                clean(requested)
            ));
        }
        Ok(())
    }
}

/// `.cargo/…`, `rust-toolchain(.toml)` and `.npmrc`, anywhere in the folder.
fn decides_what_the_check_runs(inside: &Path) -> bool {
    let named = |n: &std::ffi::OsStr, what: &str| n.eq_ignore_ascii_case(what);
    inside.components().any(|c| matches!(c, Component::Normal(n) if named(n, ".cargo")))
        || inside.file_name().is_some_and(|n| {
            named(n, "rust-toolchain") || named(n, "rust-toolchain.toml") || named(n, ".npmrc")
        })
}

/// `%APPDATA%\odin`, canonical, if it exists.
fn app_data() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("APPDATA")?).join("odin");
    std::fs::canonicalize(dir).ok()
}

/// Models wrap paths in quotes or backticks surprisingly often.
fn clean(requested: &str) -> String {
    requested
        .trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == '`')
        .trim()
        .to_string()
}

/// `\\?\C:\x` → `C:\x`, and `\\?\UNC\server\share` → `\\server\share`, for
/// anything a person reads — and anything handed to a program as a folder,
/// which the verbatim form of a network share would reach as a relative path.
fn plain(p: &Path) -> String {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or_else(|| s.into_owned())
}

// ── text ────────────────────────────────────────────────────────────────────

/// FNV-1a, 64-bit. Stable across builds and runs — unlike std's `DefaultHasher`
/// — which matters because undo compares a hash stored on disk by one run of the
/// app with a file read by another.
pub fn fingerprint(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

const BOM: &[u8] = b"\xEF\xBB\xBF";

/// A text file taken apart into what a model sees and what has to be put back.
#[derive(Debug, Clone, PartialEq)]
struct Text {
    /// With `\n` line endings and no BOM.
    body: String,
    crlf: bool,
    bom: bool,
}

impl Text {
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_EDIT_BYTES {
            return Err(format!(
                "the file is {} bytes, over the {} byte limit for an edit",
                bytes.len(),
                MAX_EDIT_BYTES
            ));
        }
        let (bom, rest) = match bytes.strip_prefix(BOM) {
            Some(r) => (true, r),
            None => (false, bytes),
        };
        if rest.contains(&0) {
            return Err("that looks like a binary file, which is never edited".to_string());
        }
        let s = std::str::from_utf8(rest)
            .map_err(|_| "the file is not UTF-8 text, so it is not edited".to_string())?;

        let crlf_count = s.matches("\r\n").count();
        let lf_count = s.matches('\n').count();
        let crlf = crlf_count > 0;
        // Mixed endings cannot be put back faithfully: every untouched line would
        // come out changed. Refusing is better than a diff full of noise.
        if crlf && crlf_count != lf_count {
            return Err(
                "the file mixes CRLF and LF line endings, so an edit could not preserve them"
                    .to_string(),
            );
        }
        Ok(Self { body: s.replace("\r\n", "\n"), crlf, bom })
    }

    fn render(&self, body: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(body.len() + body.len() / 20 + 3);
        if self.bom {
            out.extend_from_slice(BOM);
        }
        if self.crlf {
            out.extend_from_slice(body.replace('\n', "\r\n").as_bytes());
        } else {
            out.extend_from_slice(body.as_bytes());
        }
        out
    }
}

/// Text the model wrote, reduced to `\n` endings to match against the file.
fn from_model(s: &str) -> String {
    s.replace("\r\n", "\n")
}

/// A file's contents as the model should read them: `\n` endings, no BOM.
///
/// `\r` is invisible to a person but not to a model, and it costs a token per
/// line. Reads and edits agree on this form, so text the model copies out of a
/// read matches what an edit looks for.
pub fn for_model(bytes: &[u8]) -> String {
    let rest = bytes.strip_prefix(BOM).unwrap_or(bytes);
    String::from_utf8_lossy(rest).replace("\r\n", "\n")
}

// ── diffs ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiffLine {
    /// `" "` unchanged, `"-"` removed, `"+"` added.
    pub tag: String,
    pub text: String,
    /// Line number in the old file (for `" "` and `"-"`) — 1-based.
    pub old: Option<usize>,
    /// Line number in the new file (for `" "` and `"+"`) — 1-based.
    pub new: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hunk {
    pub lines: Vec<DiffLine>,
}

fn diff(old: &str, new: &str) -> (Vec<Hunk>, usize, usize) {
    use similar::{ChangeTag, TextDiff};

    let d = TextDiff::from_lines(old, new);
    let mut hunks = Vec::new();
    let (mut added, mut removed) = (0, 0);

    for group in d.grouped_ops(CONTEXT) {
        let mut lines = Vec::new();
        for op in group {
            for change in d.iter_changes(&op) {
                let tag = match change.tag() {
                    ChangeTag::Equal => " ",
                    ChangeTag::Delete => {
                        removed += 1;
                        "-"
                    }
                    ChangeTag::Insert => {
                        added += 1;
                        "+"
                    }
                };
                lines.push(DiffLine {
                    tag: tag.to_string(),
                    text: change.value().trim_end_matches('\n').to_string(),
                    old: change.old_index().map(|i| i + 1),
                    new: change.new_index().map(|i| i + 1),
                });
            }
        }
        hunks.push(Hunk { lines });
    }
    (hunks, added, removed)
}

// ── planning ────────────────────────────────────────────────────────────────

/// An edit worked out in full, before anyone is asked about it.
///
/// Holds the exact bytes that will be written. Nothing downstream re-derives
/// them — [`apply`] writes `after` or nothing — so the diff you approve and the
/// file you get cannot disagree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub path: PathBuf,
    /// Relative to the workspace, with forward slashes. What the card shows.
    pub display: String,
    /// True when this creates a file rather than changing one.
    pub create: bool,
    /// Hash of the file as it was when planned; `None` for a new file.
    pub before: Option<u64>,
    pub after: Vec<u8>,
    pub hunks: Vec<Hunk>,
    pub added: usize,
    pub removed: usize,
}

/// Replace one exact piece of text with another.
///
/// `old` must occur **exactly once**. None means the model misremembered the
/// file; more than one means it did not say which. Either way nothing is
/// written, and the message says how to fix the request — it goes back to the
/// model, which can read the file again and retry.
pub fn plan_replace(ws: &Workspace, path: &str, old: &str, new: &str) -> Result<Plan, String> {
    let real = ws.resolve_existing(path)?;
    let bytes = std::fs::read(&real).map_err(|e| format!("cannot read {}: {e}", clean(path)))?;
    let text = Text::parse(&bytes)?;

    let mut old = from_model(old);
    let mut new = from_model(new);
    if old.is_empty() {
        return Err(
            "old_text is empty — say exactly which text to replace, copied from the file".to_string(),
        );
    }
    if old == new {
        return Err(
            "old_text and new_text are identical, so nothing would change. old_text is the text \
             as it is in the file now; new_text is that text with your change made."
                .to_string(),
        );
    }

    let mut count = text.body.matches(old.as_str()).count();

    // One narrow second chance: blank lines the file does not have, after the
    // text. Measured at G7 — asked to add a function after the last one, the
    // model copied `}` plus two newlines where the file ends with one, and was
    // refused every time. Trimmed from both sides, so what it meant to append
    // still follows the text it meant to append it to. Only when the trimmed
    // text matches exactly once; anything looser could pick the wrong place.
    if count == 0 && old.ends_with('\n') {
        let trimmed = old.trim_end_matches('\n');
        let extra = old.len() - trimmed.len();
        if !trimmed.is_empty() && text.body.matches(trimmed).count() == 1 {
            let keep_new = new.trim_end_matches('\n').len().max(new.len().saturating_sub(extra));
            old = trimmed.to_string();
            new.truncate(keep_new);
            count = 1;
        }
    }

    if count == 0 {
        return Err(format!(
            "old_text was not found in {}. It must match the file exactly, including \
             indentation.{}",
            ws.relative(&real),
            closest(&text.body, &old)
                .map(|(from, to, s)| format!(
                    " The closest text in the file is lines {from}–{to}:\n{s}\nCopy it exactly \
                     as old_text."
                ))
                .unwrap_or_else(|| " Read the file again and copy the text from it.".to_string())
        ));
    }
    if count > 1 {
        return Err(format!(
            "old_text appears {count} times in {}. Include more of the surrounding lines so \
             it matches exactly one place.",
            ws.relative(&real)
        ));
    }

    let body = if new.is_empty() {
        let at = text.body.find(old.as_str()).expect("counted exactly once above");
        tidy_deletion(&text.body[..at], &text.body[at + old.len()..], text.body.ends_with('\n'))
    } else {
        text.body.replacen(old.as_str(), new.as_str(), 1)
    };
    let (hunks, added, removed) = diff(&text.body, &body);
    Ok(Plan {
        display: ws.relative(&real),
        path: real,
        create: false,
        before: Some(fingerprint(&bytes)),
        after: text.render(&body),
        hunks,
        added,
        removed,
    })
}

/// The lines of `body` that look most like `wanted`, for telling the model what
/// the file actually says when its text did not match.
///
/// **Measured at G7:** told only "not found, read the file again", the model
/// re-read the file and sent the same wrong text again — a semicolon the file
/// did not have, five times running. Seeing the real text beside its attempt is
/// what lets it see the difference.
///
/// Compares every window of as many lines as `wanted` has, by character-level
/// similarity. Skipped for large files or long snippets, where it would be
/// slow and a hint matters less than a prompt answer.
fn closest(body: &str, wanted: &str) -> Option<(usize, usize, String)> {
    const MAX_BODY: usize = 64 * 1024;
    const MAX_LINES: usize = 60;
    const MIN_RATIO: f32 = 0.6;

    let want_lines = wanted.trim_end_matches('\n').lines().count().max(1);
    let lines: Vec<&str> = body.lines().collect();
    if body.len() > MAX_BODY || want_lines > MAX_LINES || lines.len() < want_lines {
        return None;
    }
    let wanted = wanted.trim_end_matches('\n');
    let mut best: Option<(f32, usize)> = None;
    for start in 0..=lines.len() - want_lines {
        let window = lines[start..start + want_lines].join("\n");
        let ratio = similar::TextDiff::from_chars(window.as_str(), wanted).ratio();
        if best.is_none_or(|(r, _)| ratio > r) {
            best = Some((ratio, start));
        }
    }
    let (ratio, start) = best?;
    (ratio >= MIN_RATIO).then(|| {
        let end = start + want_lines;
        (start + 1, end, lines[start..end].join("\n"))
    })
}

/// Join what is left around a deletion without leaving a gap.
///
/// **Measured at G7:** asked to delete a function, the model removes the
/// function but not the blank line that separated it — 6 runs out of 6 — which
/// leaves a run of blank lines where it was, or trailing ones at the end of the
/// file. So, only at the join and only for a deletion: a run of blank lines is
/// collapsed to one, and at the end of the file to none — keeping whether the
/// file ended with a newline. Nothing else in the file is touched, and the diff
/// shown for approval is of this result, so it is never a surprise.
fn tidy_deletion(before: &str, after: &str, ended_with_newline: bool) -> String {
    let nb = before.len() - before.trim_end_matches('\n').len();
    let na = after.len() - after.trim_start_matches('\n').len();

    if after.trim_start_matches('\n').is_empty() {
        let kept = before.trim_end_matches('\n');
        return if kept.is_empty() || !ended_with_newline {
            kept.to_string()
        } else {
            format!("{kept}\n")
        };
    }
    if nb + na > 2 {
        return format!(
            "{}\n\n{}",
            before.trim_end_matches('\n'),
            after.trim_start_matches('\n')
        );
    }
    format!("{before}{after}")
}

/// Line endings for a file that does not exist yet: the platform's own, which
/// is also what a `core.autocrlf` checkout puts on every other file.
fn new_file_style() -> Text {
    Text { body: String::new(), crlf: cfg!(windows), bom: false }
}

/// Create a file that does not exist yet.
pub fn plan_create(ws: &Workspace, path: &str, content: &str) -> Result<Plan, String> {
    let real = ws.resolve_new(path)?;
    let body = from_model(content);
    if body.len() > MAX_EDIT_BYTES {
        return Err(format!("that content is over the {MAX_EDIT_BYTES} byte limit"));
    }
    let (hunks, added, removed) = diff("", &body);
    Ok(Plan {
        display: ws.relative(&real),
        path: real,
        create: true,
        before: None,
        after: new_file_style().render(&body),
        hunks,
        added,
        removed,
    })
}

/// Replace a file's whole contents. **Not offered to the model** — it exists so
/// the G7 probe can measure it against search-and-replace on the same tasks.
pub fn plan_rewrite(ws: &Workspace, path: &str, content: &str) -> Result<Plan, String> {
    let real = ws.resolve_existing(path)?;
    let bytes = std::fs::read(&real).map_err(|e| format!("cannot read {}: {e}", clean(path)))?;
    let text = Text::parse(&bytes)?;
    let body = from_model(content);
    let (hunks, added, removed) = diff(&text.body, &body);
    Ok(Plan {
        display: ws.relative(&real),
        path: real,
        create: false,
        before: Some(fingerprint(&bytes)),
        after: text.render(&body),
        hunks,
        added,
        removed,
    })
}

// ── applying, and undoing ───────────────────────────────────────────────────

/// What was done, and the handle to undo it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Applied {
    pub id: String,
    pub display: String,
    pub added: usize,
    pub removed: usize,
    pub create: bool,
}

/// Kept beside each backup so [`undo`] knows what it is restoring and can tell
/// whether the file has moved on since.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    path: PathBuf,
    display: String,
    create: bool,
    /// Hash of what the edit wrote. Undo only proceeds if the file still is this.
    after: u64,
}

/// `%APPDATA%\odin\backups\`, beside the conversations.
pub fn backups_dir() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join("odin").join("backups"))
}

/// Write `bytes` to `path` via a temporary file in the same folder and a
/// rename, so a crash leaves either the old file or the new one — never half.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.odin-tmp"));
    std::fs::write(&tmp, bytes).map_err(|e| format!("writing {}: {e}", plain(path)))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("replacing {}: {e}", plain(path))
    })
}

fn new_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("{nanos:x}")
}

/// Apply a plan — **only if the file is still exactly what the plan was made
/// from.** Backs the original up first.
pub fn apply(plan: &Plan, backups: &Path) -> Result<Applied, String> {
    let current = std::fs::read(&plan.path).ok();
    match (plan.before, &current) {
        (None, Some(_)) => {
            return Err(format!(
                "{} was created by something else after this edit was shown — nothing was written",
                plan.display
            ));
        }
        (Some(_), None) => {
            return Err(format!(
                "{} was deleted after this edit was shown — nothing was written",
                plan.display
            ));
        }
        (Some(want), Some(bytes)) if fingerprint(bytes) != want => {
            return Err(format!(
                "{} changed after this edit was shown, so the diff you approved no longer \
                 describes it — nothing was written",
                plan.display
            ));
        }
        _ => {}
    }

    std::fs::create_dir_all(backups).map_err(|e| format!("creating {}: {e}", plain(backups)))?;
    let id = new_id();
    if let Some(original) = &current {
        std::fs::write(backups.join(format!("{id}.bak")), original)
            .map_err(|e| format!("backing up {}: {e}", plan.display))?;
    }
    let record = Record {
        path: plan.path.clone(),
        display: plan.display.clone(),
        create: plan.create,
        after: fingerprint(&plan.after),
    };
    std::fs::write(
        backups.join(format!("{id}.json")),
        serde_json::to_vec_pretty(&record).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("recording the backup: {e}"))?;

    write_atomic(&plan.path, &plan.after)?;
    Ok(Applied {
        id,
        display: plan.display.clone(),
        added: plan.added,
        removed: plan.removed,
        create: plan.create,
    })
}

/// Put a file back the way it was before an edit.
///
/// **Refuses if the file has changed since the edit** — undo restores the
/// original by overwriting, and doing that over later work would lose it.
pub fn undo(id: &str, backups: &Path) -> Result<String, String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("not an edit id: {id}"));
    }
    let meta = backups.join(format!("{id}.json"));
    let record: Record = serde_json::from_slice(
        &std::fs::read(&meta).map_err(|_| "that edit has already been undone, or was never made".to_string())?,
    )
    .map_err(|e| format!("reading the backup record: {e}"))?;

    let current = std::fs::read(&record.path).ok();
    match &current {
        Some(bytes) if fingerprint(bytes) == record.after => {}
        Some(_) => {
            return Err(format!(
                "{} has changed since the edit, so undoing it would lose those changes — left as is",
                record.display
            ));
        }
        None => {
            return Err(format!("{} no longer exists, so there is nothing to undo", record.display));
        }
    }

    if record.create {
        std::fs::remove_file(&record.path).map_err(|e| format!("removing {}: {e}", record.display))?;
    } else {
        let original = std::fs::read(backups.join(format!("{id}.bak")))
            .map_err(|e| format!("reading the backup: {e}"))?;
        write_atomic(&record.path, &original)?;
    }
    let _ = std::fs::remove_file(&meta);
    let _ = std::fs::remove_file(backups.join(format!("{id}.bak")));
    Ok(record.display)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh folder per test — tests run in parallel, and a shared one would
    /// make them interfere with each other rather than with the code under test.
    struct Sandbox {
        dir: PathBuf,
    }

    impl Sandbox {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("odin-edit-{tag}-{}", new_id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }
        fn ws(&self) -> Workspace {
            let root = self.dir.join("project");
            std::fs::create_dir_all(&root).unwrap();
            Workspace::open(&root).unwrap()
        }
        fn backups(&self) -> PathBuf {
            self.dir.join("backups")
        }
        fn write(&self, rel: &str, bytes: &[u8]) -> PathBuf {
            let p = self.dir.join("project").join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, bytes).unwrap();
            p
        }
        fn read(&self, rel: &str) -> Vec<u8> {
            std::fs::read(self.dir.join("project").join(rel)).unwrap()
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    // ── containment ─────────────────────────────────────────────────────────

    #[test]
    fn relative_paths_resolve_inside_the_folder() {
        let s = Sandbox::new("rel");
        let ws = s.ws();
        s.write("src/main.rs", b"fn main() {}\n");
        let p = ws.resolve_existing("src/main.rs").unwrap();
        assert!(p.starts_with(ws.root()));
        assert_eq!(ws.relative(&p), "src/main.rs");
    }

    #[test]
    fn dot_dot_cannot_climb_out() {
        let s = Sandbox::new("dotdot");
        let ws = s.ws();
        std::fs::write(s.dir.join("outside.txt"), b"secret").unwrap();
        let err = ws.resolve_existing("../outside.txt").unwrap_err();
        assert!(err.contains("outside the project folder"), "{err}");
        let err = ws.resolve_existing("src/../../outside.txt").unwrap_err();
        assert!(err.contains("outside"), "{err}");
    }

    #[test]
    fn an_absolute_path_elsewhere_is_refused() {
        let s = Sandbox::new("abs");
        let ws = s.ws();
        let outside = s.dir.join("elsewhere.txt");
        std::fs::write(&outside, b"x").unwrap();
        let err = ws.resolve_existing(outside.to_str().unwrap()).unwrap_err();
        assert!(err.contains("outside"), "{err}");
    }

    #[test]
    fn an_absolute_path_inside_is_accepted() {
        let s = Sandbox::new("absin");
        let ws = s.ws();
        let inside = s.write("a.txt", b"x");
        assert!(ws.resolve_existing(inside.to_str().unwrap()).is_ok());
    }

    #[test]
    fn a_sibling_folder_sharing_the_prefix_is_not_inside() {
        // `C:\x\project-old` starts with the *string* `C:\x\project`. A string
        // prefix check would let it through; a component check must not.
        let s = Sandbox::new("prefix");
        let ws = s.ws();
        let sib = s.dir.join("project-old");
        std::fs::create_dir_all(&sib).unwrap();
        std::fs::write(sib.join("f.txt"), b"x").unwrap();
        let err = ws.resolve_existing(sib.join("f.txt").to_str().unwrap()).unwrap_err();
        assert!(err.contains("outside"), "{err}");
    }

    #[test]
    fn a_junction_pointing_out_is_followed_and_refused() {
        // Canonicalising resolves the link to where it really points, which is
        // what makes this safe. Junctions need no admin rights on Windows, so
        // this runs everywhere the app does.
        let s = Sandbox::new("junction");
        let ws = s.ws();
        let target = s.dir.join("target");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("secret.txt"), b"x").unwrap();
        let link = ws.root().join("link");
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(plain(&link))
            .arg(plain(&target))
            .output();
        if !matches!(made, Ok(ref o) if o.status.success()) {
            eprintln!("could not create a junction here; skipping");
            return;
        }
        let err = ws.resolve_existing("link/secret.txt").unwrap_err();
        assert!(err.contains("outside"), "{err}");
        let err = ws.resolve_new("link/new.txt").unwrap_err();
        assert!(err.contains("outside"), "{err}");
    }

    #[test]
    fn files_that_decide_what_the_check_runs_are_never_edited() {
        let s = Sandbox::new("checkcfg");
        let ws = s.ws();
        s.write(".cargo/config.toml", b"[build]\n");
        s.write("rust-toolchain.toml", b"[toolchain]\n");
        s.write("web/.npmrc", b"x=1\n");
        for p in [".cargo/config.toml", "rust-toolchain.toml", "web/.npmrc"] {
            let err = ws.resolve_existing(p).unwrap_err();
            assert!(err.contains("decides what the check command runs"), "{p}: {err}");
        }
        // Creating one is refused too — including `.cargo/config` without the
        // extension, which cargo also reads.
        assert!(ws.resolve_new(".cargo/config").is_err());
        assert!(ws.resolve_new("RUST-TOOLCHAIN").is_err());
        // The files a project is made of are not caught by it.
        s.write("Cargo.toml", b"[package]\n");
        s.write("src/cargo_helpers.rs", b"\n");
        assert!(ws.resolve_existing("Cargo.toml").is_ok());
        assert!(ws.resolve_existing("src/cargo_helpers.rs").is_ok());
    }

    #[test]
    fn a_file_stream_is_never_edited() {
        let s = Sandbox::new("stream");
        let ws = s.ws();
        s.write("notes.txt", b"x");
        let err = ws.resolve_new(".npmrc::$DATA").unwrap_err();
        assert!(err.contains("file stream"), "{err}");
        let err = ws.resolve_new("notes.txt:hidden").unwrap_err();
        assert!(err.contains("file stream"), "{err}");
    }

    #[test]
    fn a_network_share_reads_as_one() {
        assert_eq!(plain(Path::new(r"\\?\UNC\server\share\proj")), r"\\server\share\proj");
        assert_eq!(plain(Path::new(r"\\?\C:\proj")), r"C:\proj");
    }

    #[test]
    fn mimirs_own_folder_is_never_edited_even_inside_the_project() {
        // As when the chosen folder is the home folder, which holds %APPDATA%.
        let s = Sandbox::new("appdata");
        let ws = s.ws();
        let cfg = s.write("AppData/Roaming/odin/config.json", b"{}");
        let app = std::fs::canonicalize(s.dir.join("project/AppData/Roaming/odin")).unwrap();
        let real = std::fs::canonicalize(&cfg).unwrap();
        let err = ws.contain_with(&real, "AppData/Roaming/odin/config.json", Some(&app)).unwrap_err();
        assert!(err.contains("Mímir's own settings"), "{err}");
        // Its neighbours are ordinary files.
        let other = std::fs::canonicalize(s.write("AppData/Roaming/other/x.txt", b"x")).unwrap();
        assert!(ws.contain_with(&other, "AppData/Roaming/other/x.txt", Some(&app)).is_ok());
    }

    #[test]
    fn dot_git_is_never_edited() {
        let s = Sandbox::new("git");
        let ws = s.ws();
        s.write(".git/config", b"[core]\n");
        s.write("sub/.GIT/HEAD", b"ref\n");
        assert!(ws.resolve_existing(".git/config").unwrap_err().contains(".git"));
        assert!(ws.resolve_existing("sub/.GIT/HEAD").unwrap_err().contains(".git"));
        assert!(ws.resolve_new(".git/new").unwrap_err().contains(".git"));
    }

    #[test]
    fn quoted_paths_are_unwrapped() {
        let s = Sandbox::new("quotes");
        let ws = s.ws();
        s.write("a.txt", b"x");
        assert!(ws.resolve_existing("`a.txt`").is_ok());
        assert!(ws.resolve_existing("\"a.txt\"").is_ok());
        assert!(ws.resolve_existing("  a.txt  ").is_ok());
    }

    #[test]
    fn empty_and_missing_paths_are_refused() {
        let s = Sandbox::new("empty");
        let ws = s.ws();
        assert!(ws.resolve_existing("").is_err());
        assert!(ws.resolve_existing("nope.txt").unwrap_err().contains("does not exist"));
    }

    #[test]
    fn a_folder_is_not_a_file() {
        let s = Sandbox::new("dir");
        let ws = s.ws();
        std::fs::create_dir_all(ws.root().join("src")).unwrap();
        assert!(ws.resolve_existing("src").unwrap_err().contains("folder"));
    }

    #[test]
    fn new_files_need_an_existing_folder_inside() {
        let s = Sandbox::new("new");
        let ws = s.ws();
        assert!(ws.resolve_new("fresh.txt").is_ok());
        assert!(ws.resolve_new("no/such/dir/f.txt").unwrap_err().contains("does not exist"));
        s.write("taken.txt", b"x");
        assert!(ws.resolve_new("taken.txt").unwrap_err().contains("already exists"));
        assert!(ws.resolve_new("../escape.txt").unwrap_err().contains("outside"));
    }

    // ── replace ─────────────────────────────────────────────────────────────

    #[test]
    fn replaces_exactly_one_occurrence() {
        let s = Sandbox::new("replace");
        let ws = s.ws();
        s.write("a.rs", b"let x = 1;\nlet y = 2;\n");
        let plan = plan_replace(&ws, "a.rs", "let y = 2;", "let y = 3;").unwrap();
        assert_eq!(plan.after, b"let x = 1;\nlet y = 3;\n");
        assert_eq!((plan.added, plan.removed), (1, 1));
        assert!(!plan.create);
    }

    #[test]
    fn crlf_files_match_lf_text_and_stay_crlf() {
        // The case that decides whether this works at all on this machine: every
        // file in the repo is CRLF on disk, and the model writes \n.
        let s = Sandbox::new("crlf");
        let ws = s.ws();
        s.write("a.rs", b"fn a() {\r\n    1\r\n}\r\nfn b() {}\r\n");
        let plan = plan_replace(&ws, "a.rs", "fn a() {\n    1\n}", "fn a() {\n    2\n}").unwrap();
        assert_eq!(plan.after, b"fn a() {\r\n    2\r\n}\r\nfn b() {}\r\n");
    }

    #[test]
    fn model_text_with_crlf_still_matches() {
        let s = Sandbox::new("crlf2");
        let ws = s.ws();
        s.write("a.txt", b"one\ntwo\n");
        let plan = plan_replace(&ws, "a.txt", "one\r\ntwo", "one\r\n2").unwrap();
        assert_eq!(plan.after, b"one\n2\n");
    }

    #[test]
    fn a_bom_is_preserved() {
        let s = Sandbox::new("bom");
        let ws = s.ws();
        s.write("a.txt", b"\xEF\xBB\xBFhello\r\n");
        let plan = plan_replace(&ws, "a.txt", "hello", "goodbye").unwrap();
        assert_eq!(plan.after, b"\xEF\xBB\xBFgoodbye\r\n");
    }

    #[test]
    fn untouched_lines_are_byte_identical() {
        let s = Sandbox::new("untouched");
        let ws = s.ws();
        let original = b"  a  \n\tb\n\n\nc   \n";
        s.write("a.txt", original);
        let plan = plan_replace(&ws, "a.txt", "\tb", "\tB").unwrap();
        assert_eq!(plan.after, b"  a  \n\tB\n\n\nc   \n");
    }

    #[test]
    fn no_trailing_newline_stays_without_one() {
        let s = Sandbox::new("nonl");
        let ws = s.ws();
        s.write("a.txt", b"x = 1");
        assert_eq!(plan_replace(&ws, "a.txt", "1", "2").unwrap().after, b"x = 2");
    }

    #[test]
    fn a_missing_match_writes_nothing_and_says_why() {
        let s = Sandbox::new("nomatch");
        let ws = s.ws();
        s.write("a.txt", b"fn main() {}\n");
        let err = plan_replace(&ws, "a.txt", "struct Unrelated;", "x").unwrap_err();
        assert!(err.contains("not found") && err.contains("Read the file again"), "{err}");
        assert_eq!(s.read("a.txt"), b"fn main() {}\n");
    }

    #[test]
    fn a_near_miss_is_shown_the_text_that_is_really_there() {
        // The G7 t02 case: the model added a semicolon the file does not have,
        // and without seeing the real line it sent the same mistake five times.
        let s = Sandbox::new("closest");
        let ws = s.ws();
        s.write(
            "main.rs",
            b"mod config;\r\n\r\nfn greet(name: &str) -> String {\r\n    format!(\"Hello, {name}!\")\r\n}\r\n",
        );
        let err = plan_replace(
            &ws,
            "main.rs",
            "fn greet(name: &str) -> String {\n    format!(\"Hello, {name}!\");\n}",
            "x",
        )
        .unwrap_err();
        assert!(err.contains("closest text in the file is lines 3–5"), "{err}");
        assert!(err.contains("    format!(\"Hello, {name}!\")\n}"), "the real lines, with \\n endings: {err}");
        assert!(!err.contains('\r'), "{err}");
    }

    #[test]
    fn blank_lines_the_file_does_not_have_are_forgiven_after_the_text() {
        // The G7 t07 case: `}` plus two newlines, where the file ends with one.
        let s = Sandbox::new("trailing");
        let ws = s.ws();
        s.write("app.ts", b"export function total(): number {\n  return sum;\n}\n");
        let plan = plan_replace(
            &ws,
            "app.ts",
            "  return sum;\n}\n\n",
            "  return sum;\n}\n\nexport function average(): number {\n  return 1;\n}\n\n",
        )
        .unwrap();
        assert_eq!(
            plan.after,
            b"export function total(): number {\n  return sum;\n}\n\nexport function average(): number {\n  return 1;\n}\n"
        );
    }

    #[test]
    fn the_trailing_newline_allowance_still_needs_a_unique_match() {
        let s = Sandbox::new("trailing-ambig");
        let ws = s.ws();
        s.write("a.rs", b"fn a() {\n}\nfn b() {\n}\n");
        let err = plan_replace(&ws, "a.rs", "}\n\n", "}\n\n// x\n").unwrap_err();
        assert!(err.contains("not found"), "{err}");
    }

    #[test]
    fn indentation_must_match_exactly() {
        // Deliberately strict. A whitespace-tolerant match could pick the wrong
        // one of two similar blocks, and a wrong edit is worse than a refused one.
        //
        // It is a *substring* match, though, so leaving indentation out entirely
        // still matches — and the file keeps its own, because only the matched
        // text is replaced. What fails is claiming more than the file has.
        let s = Sandbox::new("indent");
        let ws = s.ws();
        s.write("a.rs", b"    let x = 1;\n");
        let plan = plan_replace(&ws, "a.rs", "let x = 1;", "let x = 2;").unwrap();
        assert_eq!(plan.after, b"    let x = 2;\n");
        assert!(plan_replace(&ws, "a.rs", "      let x = 1;", "x").is_err());
        assert!(plan_replace(&ws, "a.rs", "\tlet x = 1;", "x").is_err());
    }

    #[test]
    fn an_ambiguous_match_is_refused() {
        let s = Sandbox::new("ambig");
        let ws = s.ws();
        s.write("a.rs", b"x += 1;\ny += 1;\nx += 1;\n");
        let err = plan_replace(&ws, "a.rs", "x += 1;", "x += 2;").unwrap_err();
        assert!(err.contains("2 times"), "{err}");
    }

    #[test]
    fn empty_or_no_op_edits_are_refused() {
        let s = Sandbox::new("noop");
        let ws = s.ws();
        s.write("a.txt", b"abc\n");
        assert!(plan_replace(&ws, "a.txt", "", "x").unwrap_err().contains("empty"));
        assert!(plan_replace(&ws, "a.txt", "abc", "abc").unwrap_err().contains("identical"));
    }

    #[test]
    fn binary_non_utf8_and_mixed_endings_are_refused() {
        let s = Sandbox::new("refuse");
        let ws = s.ws();
        s.write("bin.dat", b"ab\0cd");
        s.write("latin1.txt", b"caf\xe9\n");
        s.write("mixed.txt", b"a\r\nb\nc\r\n");
        assert!(plan_replace(&ws, "bin.dat", "ab", "x").unwrap_err().contains("binary"));
        assert!(plan_replace(&ws, "latin1.txt", "caf", "x").unwrap_err().contains("UTF-8"));
        assert!(plan_replace(&ws, "mixed.txt", "a", "x").unwrap_err().contains("mixes"));
    }

    #[test]
    fn a_huge_file_is_refused() {
        let s = Sandbox::new("huge");
        let ws = s.ws();
        s.write("big.txt", &vec![b'a'; MAX_EDIT_BYTES + 1]);
        assert!(plan_replace(&ws, "big.txt", "a", "b").unwrap_err().contains("limit"));
    }

    #[test]
    fn the_diff_describes_the_change() {
        let s = Sandbox::new("diff");
        let ws = s.ws();
        s.write("a.txt", b"1\n2\n3\n4\n5\n6\n7\n8\n9\n");
        let plan = plan_replace(&ws, "a.txt", "5\n", "five\nFIVE\n").unwrap();
        assert_eq!(plan.hunks.len(), 1);
        let tags: String = plan.hunks[0].lines.iter().map(|l| l.tag.as_str()).collect();
        // Three lines of context either side of one removal and two additions.
        assert_eq!(tags, "   -++   ");
        let minus = plan.hunks[0].lines.iter().find(|l| l.tag == "-").unwrap();
        assert_eq!((minus.text.as_str(), minus.old), ("5", Some(5)));
        assert_eq!((plan.added, plan.removed), (2, 1));
    }

    // ── deletion ────────────────────────────────────────────────────────────

    /// The G7 case, exactly: the model deletes the function but leaves the
    /// blank line above it, which left two stray blank lines at the end.
    const CONFIG: &[u8] = b"pub const PORT: u16 = 8080;\r\n\r\npub fn address() -> String {\r\n    x\r\n}\r\n\r\npub fn debug_mode() -> bool {\r\n    false\r\n}\r\n";

    #[test]
    fn deleting_the_last_function_leaves_no_trailing_blank_lines() {
        let s = Sandbox::new("del-eof");
        let ws = s.ws();
        s.write("c.rs", CONFIG);
        let plan = plan_replace(&ws, "c.rs", "pub fn debug_mode() -> bool {\n    false\n}", "").unwrap();
        assert_eq!(
            plan.after,
            b"pub const PORT: u16 = 8080;\r\n\r\npub fn address() -> String {\r\n    x\r\n}\r\n"
        );
    }

    #[test]
    fn deleting_between_two_blocks_leaves_one_blank_line() {
        let s = Sandbox::new("del-mid");
        let ws = s.ws();
        s.write("m.rs", b"fn a() {}\n\nfn b() {}\n\nfn c() {}\n");
        let plan = plan_replace(&ws, "m.rs", "fn b() {}", "").unwrap();
        assert_eq!(plan.after, b"fn a() {}\n\nfn c() {}\n");
    }

    #[test]
    fn a_deletion_that_took_its_blank_line_is_left_alone() {
        let s = Sandbox::new("del-clean");
        let ws = s.ws();
        s.write("m.rs", b"fn a() {}\n\nfn b() {}\n\nfn c() {}\n");
        let plan = plan_replace(&ws, "m.rs", "\nfn b() {}\n", "").unwrap();
        assert_eq!(plan.after, b"fn a() {}\n\nfn c() {}\n");
    }

    #[test]
    fn deleting_inside_a_line_changes_only_that() {
        let s = Sandbox::new("del-inline");
        let ws = s.ws();
        s.write("a.txt", b"a\n\n\nkeep these gaps\n\n\nlet x = mut 1;\n");
        let plan = plan_replace(&ws, "a.txt", "mut ", "").unwrap();
        assert_eq!(plan.after, b"a\n\n\nkeep these gaps\n\n\nlet x = 1;\n");
    }

    #[test]
    fn deleting_to_the_end_keeps_a_missing_final_newline_missing() {
        let s = Sandbox::new("del-nonl");
        let ws = s.ws();
        s.write("a.txt", b"one\n\ntwo");
        assert_eq!(plan_replace(&ws, "a.txt", "two", "").unwrap().after, b"one");
    }

    #[test]
    fn a_replacement_is_never_tidied() {
        // Only deletions: a replacement's whitespace is the model's to decide.
        let s = Sandbox::new("no-tidy");
        let ws = s.ws();
        s.write("a.txt", b"a\n\nb\n\nc\n");
        assert_eq!(plan_replace(&ws, "a.txt", "b", "\n\nB\n\n").unwrap().after, b"a\n\n\n\nB\n\n\n\nc\n");
    }

    // ── create ──────────────────────────────────────────────────────────────

    #[test]
    fn creates_with_the_platform_line_ending() {
        let s = Sandbox::new("create");
        let ws = s.ws();
        let plan = plan_create(&ws, "notes.md", "# Notes\n\nhi\n").unwrap();
        assert!(plan.create);
        let want: &[u8] = if cfg!(windows) { b"# Notes\r\n\r\nhi\r\n" } else { b"# Notes\n\nhi\n" };
        assert_eq!(plan.after, want);
        assert_eq!(plan.added, 3);
    }

    // ── apply and undo ──────────────────────────────────────────────────────

    #[test]
    fn apply_then_undo_restores_the_exact_bytes() {
        let s = Sandbox::new("roundtrip");
        let ws = s.ws();
        let original = b"\xEF\xBB\xBFa\r\nb\r\n".to_vec();
        s.write("a.txt", &original);
        let plan = plan_replace(&ws, "a.txt", "b", "B").unwrap();
        let done = apply(&plan, &s.backups()).unwrap();
        assert_eq!(s.read("a.txt"), b"\xEF\xBB\xBFa\r\nB\r\n");
        assert_eq!(undo(&done.id, &s.backups()).unwrap(), "a.txt");
        assert_eq!(s.read("a.txt"), original);
    }

    #[test]
    fn a_file_changed_after_planning_is_not_written() {
        let s = Sandbox::new("stale");
        let ws = s.ws();
        s.write("a.txt", b"one\n");
        let plan = plan_replace(&ws, "a.txt", "one", "two").unwrap();
        s.write("a.txt", b"one\nsomeone else's line\n");
        let err = apply(&plan, &s.backups()).unwrap_err();
        assert!(err.contains("changed after"), "{err}");
        assert_eq!(s.read("a.txt"), b"one\nsomeone else's line\n");
    }

    #[test]
    fn a_file_deleted_after_planning_is_not_recreated() {
        let s = Sandbox::new("deleted");
        let ws = s.ws();
        let p = s.write("a.txt", b"one\n");
        let plan = plan_replace(&ws, "a.txt", "one", "two").unwrap();
        std::fs::remove_file(&p).unwrap();
        assert!(apply(&plan, &s.backups()).unwrap_err().contains("deleted"));
        assert!(!p.exists());
    }

    #[test]
    fn create_refuses_if_the_file_appeared_meanwhile() {
        let s = Sandbox::new("race");
        let ws = s.ws();
        let plan = plan_create(&ws, "new.txt", "mine\n").unwrap();
        s.write("new.txt", b"theirs\n");
        assert!(apply(&plan, &s.backups()).unwrap_err().contains("created by something else"));
        assert_eq!(s.read("new.txt"), b"theirs\n");
    }

    #[test]
    fn undoing_a_create_removes_the_file() {
        let s = Sandbox::new("uncreate");
        let ws = s.ws();
        let plan = plan_create(&ws, "new.txt", "hi\n").unwrap();
        let done = apply(&plan, &s.backups()).unwrap();
        assert!(ws.root().join("new.txt").exists());
        undo(&done.id, &s.backups()).unwrap();
        assert!(!ws.root().join("new.txt").exists());
    }

    #[test]
    fn undo_will_not_overwrite_later_changes() {
        let s = Sandbox::new("undolater");
        let ws = s.ws();
        s.write("a.txt", b"one\n");
        let done = apply(&plan_replace(&ws, "a.txt", "one", "two").unwrap(), &s.backups()).unwrap();
        s.write("a.txt", b"two\nand my own edit\n");
        let err = undo(&done.id, &s.backups()).unwrap_err();
        assert!(err.contains("changed since the edit"), "{err}");
        assert_eq!(s.read("a.txt"), b"two\nand my own edit\n");
    }

    #[test]
    fn undo_works_once() {
        let s = Sandbox::new("undoonce");
        let ws = s.ws();
        s.write("a.txt", b"one\n");
        let done = apply(&plan_replace(&ws, "a.txt", "one", "two").unwrap(), &s.backups()).unwrap();
        undo(&done.id, &s.backups()).unwrap();
        assert!(undo(&done.id, &s.backups()).unwrap_err().contains("already been undone"));
    }

    #[test]
    fn undo_ids_cannot_reach_outside_the_backup_folder() {
        let s = Sandbox::new("undoid");
        assert!(undo("../../config", &s.backups()).unwrap_err().contains("not an edit id"));
        assert!(undo("", &s.backups()).is_err());
    }

    #[test]
    fn apply_leaves_no_temporary_file_behind() {
        let s = Sandbox::new("tmp");
        let ws = s.ws();
        s.write("a.txt", b"one\n");
        apply(&plan_replace(&ws, "a.txt", "one", "two").unwrap(), &s.backups()).unwrap();
        let names: Vec<String> = std::fs::read_dir(ws.root())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["a.txt"]);
    }

    #[test]
    fn fingerprints_are_stable() {
        // Pinned, because undo compares a hash written by one run of the app with
        // a file read by another. If this ever changes, every stored undo breaks.
        assert_eq!(fingerprint(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fingerprint(b"a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn reads_are_normalised_the_way_edits_match() {
        assert_eq!(for_model(b"\xEF\xBB\xBFa\r\nb\r\n"), "a\nb\n");
    }
}
