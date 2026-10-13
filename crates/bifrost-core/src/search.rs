//! Finding code in the project folder: which files there are, and which lines
//! hold some text.
//!
//! **Why the model needs this.** Without it, `read_file` only helps when someone
//! names the file. H1's fix probe watched the model read a compiler error that
//! could only be in `main.rs` and go on editing `config.rs`, because it had no
//! way to ask "where is `address` called?". Grep answers that exactly, and —
//! unlike retrieval by meaning — finds *every* call site, which is what a rename
//! needs.
//!
//! Five rules, each with a test:
//!
//! 1. **Nothing outside the folder.** Patterns are relative; `..`, a drive or a
//!    verbatim path is refused, not resolved. The walk does not follow links or
//!    junctions, and skips `.git` and Bifrost's own settings folder — the same
//!    places [`crate::edit`] will not write.
//! 2. **Not the build's files.** `target/` and `node_modules/` are skipped even
//!    in a folder with no `.gitignore`, and git's own ignore rules are honoured:
//!    `.gitignore` files inside the folder and, when it sits in a repository, in the
//!    folders above it up to the repository's root, plus `.git/info/exclude`.
//!    Otherwise one search of a Rust project reads gigabytes of
//!    build output.
//! 3. **It ends.** Files visited, bytes read and time are capped, and Stop
//!    reaches a walk under way — choosing your whole home folder by mistake
//!    must not hang the app.
//! 4. **Its answer fits.** About 8 KB, sorted so a rerun reads the same, with the
//!    full count and a note when lines were left out. 200 matches at 200
//!    characters would be 12k tokens of a 32k context.
//! 5. **Secrets are not read unasked.** Since H5 a search runs without asking, and
//!    the walk includes hidden files — so without this an unasked search for a
//!    word would read `.env`. Files whose *names* look like they hold secrets
//!    ([`secret_name`]) are not searched, and the answer says how many were
//!    skipped. [`list`] still shows their names, and `read_file` asks.
//!
//! Blocking, like the rest of this crate. Tauri callers go through
//! `spawn_blocking`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use globset::{GlobBuilder, GlobMatcher};

use crate::edit::{self, Workspace};

/// What one answer may hold, in bytes.
pub const OUTPUT_CAP: usize = 8 * 1024;

/// A matching line is cut after this many characters. Minified code is one
/// line; a line is shown so the model can find it again, not read whole.
pub const LINE_CAP: usize = 200;

/// Files bigger than this are not searched. Nothing a person wrote by hand is
/// this big, and something generated is not where an answer lives.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// How far one walk may go before it stops and says so.
pub const MAX_FILES: usize = 20_000;
pub const MAX_BYTES_READ: u64 = 64 * 1024 * 1024;
pub const MAX_TIME: Duration = Duration::from_secs(15);

/// Folders skipped wherever they are: git's own, and build output that a
/// project without a `.gitignore` would otherwise have searched.
const SKIPPED: [&str; 3] = [".git", "target", "node_modules"];

// ── what the card and the model see ────────────────────────────────────────

/// A listing as a person reads it: the pattern, or what no pattern means.
pub fn describe_list(pattern: Option<&str>) -> String {
    pattern.map(str::to_string).unwrap_or_else(|| "the whole project".to_string())
}

/// A search as a person reads it.
pub fn describe_search(text: &str, pattern: Option<&str>) -> String {
    match pattern {
        Some(p) => format!("\"{text}\" in {p}"),
        None => format!("\"{text}\""),
    }
}

/// Whether a finding call could run at all — asked before the card is shown.
///
/// A pattern reaching outside the folder, or text over several lines, can
/// never succeed. Like an edit whose text is not in the file, it goes straight
/// back to the model with the reason, rather than asking a person to approve
/// something that will only fail.
pub fn validate(ws: &Workspace, text: Option<&str>, pattern: Option<&str>) -> Result<(), String> {
    if let Some(t) = text {
        search_text(t)?;
    }
    if let Some(p) = pattern {
        Pattern::parse(ws.root(), p)?;
    }
    Ok(())
}

/// The text a search looks for, or why it cannot be looked for.
fn search_text(text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Err("there is nothing to search for".to_string());
    }
    if text.contains('\n') {
        return Err("search looks at one line at a time — send text without line breaks".to_string());
    }
    Ok(())
}

// ── the pattern ─────────────────────────────────────────────────────────────

/// A glob, checked and compiled.
struct Pattern {
    matcher: GlobMatcher,
    /// No slash in it: matched against the file's name wherever it is, the way
    /// `.gitignore` and ripgrep read one. `*.rs` means every Rust file.
    by_name: bool,
}

impl Pattern {
    fn parse(root: &Path, raw: &str) -> Result<Self, String> {
        let p = raw.trim().trim_matches(|c| c == '"' || c == '\'' || c == '`').replace('\\', "/");
        // Anywhere in it, not only as a whole part: a glob can spell a parent
        // inside an alternation — `{..,x}/**` has no part that *is* `..`. The
        // walk never leaves the folder whatever the pattern says, but a pattern
        // that reaches for outside is refused as one, not quietly matched to
        // nothing. A real file name with `..` in it is rare enough to lose.
        // Brackets are read through too, so `[.][.]` counts as the `..` it is.
        let flat: String = p.chars().filter(|c| !matches!(c, '[' | ']')).collect();
        let outside = p.starts_with('/') || p.contains(':') || flat.contains("//") || flat.contains("..");
        if outside {
            return Err(format!(
                "{raw} is not inside the project folder — patterns are relative to it, like src/**/*.rs"
            ));
        }
        let p = p.trim_start_matches("./").trim_end_matches('/');
        let p = if p.is_empty() { "**" } else { p };
        // A folder's name means everything in it.
        let wild = p.contains(['*', '?', '[', '{']);
        let p = if !wild && root.join(p).is_dir() { format!("{p}/**") } else { p.to_string() };
        let matcher = GlobBuilder::new(&p)
            .case_insensitive(true)
            .literal_separator(true)
            .build()
            .map_err(|e| format!("{raw} is not a pattern that can be read: {e}"))?
            .compile_matcher();
        Ok(Self { by_name: !p.contains('/'), matcher })
    }

    fn matches(&self, rel: &str) -> bool {
        if self.by_name {
            self.matcher.is_match(rel.rsplit('/').next().unwrap_or(rel))
        } else {
            self.matcher.is_match(rel)
        }
    }
}

// ── the walk ────────────────────────────────────────────────────────────────

/// Why a walk ended before the end of the folder.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Cut {
    Stopped,
    TooManyFiles,
    TooMuchRead,
    TooLong,
}

impl Cut {
    fn say(self) -> &'static str {
        match self {
            Cut::Stopped => "the user stopped it",
            Cut::TooManyFiles => "it had looked at 20,000 files",
            Cut::TooMuchRead => "it had read 64 MB",
            Cut::TooLong => "it had taken 15 seconds",
        }
    }
}

/// Every file in the folder that the rules allow, in path order, as
/// `(forward-slash relative path, full path)`. `each` returns how many bytes it
/// read, so the walk can stop at [`MAX_BYTES_READ`].
fn walk(
    ws: &Workspace,
    app_data: Option<&Path>,
    cancelled: &dyn Fn() -> bool,
    mut each: impl FnMut(&str, &Path) -> u64,
) -> Option<Cut> {
    // The plain form: a walk from `\\?\C:\…` has been seen to read nested
    // `.gitignore` files differently, and a plain root is what every other
    // program is given.
    let root = PathBuf::from(ws.root_display());
    let walker = walker(&root, app_data.map(Path::to_path_buf), None);

    let started = Instant::now();
    let (mut files, mut read) = (0usize, 0u64);
    for entry in walker.build() {
        if cancelled() {
            return Some(Cut::Stopped);
        }
        if started.elapsed() > MAX_TIME {
            return Some(Cut::TooLong);
        }
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        files += 1;
        if files > MAX_FILES {
            return Some(Cut::TooManyFiles);
        }
        let Ok(rel) = entry.path().strip_prefix(&root) else { continue };
        let rel = rel.to_string_lossy().replace('\\', "/");
        read += each(&rel, entry.path());
        if read > MAX_BYTES_READ {
            return Some(Cut::TooMuchRead);
        }
    }
    None
}

/// The folder that holds the `.git` this folder sits in: the folder itself, or
/// the nearest one above it that has a `.git` (a folder, or the file a git
/// worktree has). `None` when it is not inside a repository at all.
fn repo_root(root: &Path) -> Option<PathBuf> {
    root.ancestors().find(|dir| dir.join(".git").symlink_metadata().is_ok()).map(Path::to_path_buf)
}

/// The one walker both [`walk`] and [`visible`] use, so that what a read may
/// run unasked is decided by exactly the rules `list_files` shows.
///
/// **The rules are git's.** A file git hides is hidden here: what a `.gitignore`
/// names, at any level, and what `.git/info/exclude` names. When the chosen folder
/// sits inside a repository (a package in a monorepo), the `.gitignore` files in the
/// folders above it count too, **up to the repository's root and no further**. The
/// `ignore` crate's own `parents` setting goes up to the top of the drive, so it
/// is not used there; instead the walk starts at the repository's root and
/// `filter_entry` keeps only the folders on the way down to the chosen one and
/// everything inside it. Each folder's rules are then read as git reads them, with a
/// deeper file overriding a higher one. Patterns match without regard to case on
/// Windows, as they do in git there (`core.ignorecase` is true). The global
/// excludes file is still not read: it is the user's own taste, not the project's.
///
/// **Outside any repository** there is no root to stop at, so the `ignore` crate's
/// `parents` setting is used and every folder above counts. That can only hide more
/// (a hidden file asks before it is read), never less.
///
/// **A chosen folder that the rules above it hide** (`build/` in the repository's
/// `.gitignore`, chosen folder `repo/build/out`) would, walked from the repository's
/// root, list nothing at all: the walk never goes into `build`. The user chose that
/// folder on purpose, so it is then treated as a project of its own, as it was before
/// the repository's rules were read: the walk starts at it, no folder above counts
/// (`parents(false)`), and the `.gitignore` files inside it still apply. Whether this
/// is the case is found by [`hidden_from_above`].
///
/// `only`, when given, narrows the walk to the folders on the way to that path
/// and the path itself, which is how [`visible`] asks the walker about one file
/// without reading the whole project. The skips are the same either way.
fn walker(root: &Path, app_data: Option<PathBuf>, only: Option<PathBuf>) -> ignore::WalkBuilder {
    match repo_root(root) {
        Some(repo) if repo == root || !hidden_from_above(&repo, root, &app_data) => {
            configured(&repo, root, false, app_data, only)
        }
        Some(_) => configured(root, root, false, app_data, only),
        None => configured(root, root, true, app_data, only),
    }
}

/// Whether the rules from the repository's root down to the chosen folder hide
/// the chosen folder itself. It asks the very walk [`walker`] would use, narrowed
/// to the chosen folder, whether that folder comes out of it: the `ignore` crate
/// does not descend into a folder a rule hides, nor into one below a hidden one, so
/// if the folder is not yielded some rule on the way hid it. Using the walk itself
/// means there is no second copy of git's rules to drift from the first.
fn hidden_from_above(repo: &Path, root: &Path, app_data: &Option<PathBuf>) -> bool {
    !configured(repo, root, false, app_data.clone(), Some(root.to_path_buf()))
        .build()
        .filter_map(Result::ok)
        .any(|e| e.path() == root)
}

/// The walker proper: starts at `top`, keeps only the folders on the way down to
/// `root` and everything inside it, and reads ignore files as [`walker`] describes.
/// `parents` is the `ignore` crate's own climb above `top`, wanted only outside a
/// repository.
fn configured(top: &Path, root: &Path, parents: bool, app_data: Option<PathBuf>, only: Option<PathBuf>) -> ignore::WalkBuilder {
    let root = root.to_path_buf();
    let mut walker = ignore::WalkBuilder::new(top);
    walker
        .hidden(false)
        .parents(parents)
        .require_git(false)
        .git_global(false)
        .git_exclude(true)
        .ignore_case_insensitive(cfg!(windows))
        .follow_links(false)
        .sort_by_file_path(|a, b| a.cmp(b))
        .filter_entry(move |e| {
            if e.path_is_symlink() {
                return false;
            }
            // Above the chosen folder only the way down to it is walked: the
            // folders between the repository's root and it, kept for their ignore
            // files. Nothing beside it is listed, read or searched.
            if !e.path().starts_with(&root) {
                return e.file_type().is_some_and(|t| t.is_dir()) && root.starts_with(e.path());
            }
            if only.as_ref().is_some_and(|target| !target.starts_with(e.path())) {
                return false;
            }
            let name = e.file_name().to_string_lossy();
            // In a git worktree, `.git` is a file pointing at the real one.
            // Still git's, never the project's.
            if name.eq_ignore_ascii_case(".git") {
                return false;
            }
            let is_dir = e.file_type().is_some_and(|t| t.is_dir());
            if is_dir && e.path() != root {
                if SKIPPED.iter().any(|s| name.eq_ignore_ascii_case(s)) {
                    return false;
                }
                if let Some(app) = &app_data
                    && std::fs::canonicalize(e.path()).is_ok_and(|real| real.starts_with(app))
                {
                    return false;
                }
            }
            true
        });
    walker
}

/// Whether `list_files` would show `real` — a canonical path to a file.
///
/// The rule behind automatic reads: *if the listing would show it, reading it
/// does not ask.* So this walks the same way the listing does — hidden files in,
/// `.gitignore` files honoured wherever they sit, `.git`, `target`,
/// `node_modules`, Bifrost's own settings folder and every link skipped — but only
/// down the chain of folders from the root to `real`, and asks whether `real`
/// comes out. A file a `.gitignore` hides, one in a skipped folder, and one
/// reached through a link are therefore all `false`, with no second copy of
/// those rules to drift from the first.
pub fn visible(ws: &Workspace, real: &Path) -> bool {
    let root = PathBuf::from(ws.root_display());
    let target = PathBuf::from(edit::plain(real));
    if !target.starts_with(&root) {
        return false;
    }
    walker(&root, edit::app_data(), Some(target.clone()))
        .build()
        .filter_map(Result::ok)
        .any(|e| e.path() == target && e.file_type().is_some_and(|t| t.is_file()))
}

/// File names that look like they hold a secret: credentials, keys, tokens.
///
/// By name only, case-insensitively — a guess, and a conservative one: a false
/// positive costs one question, a false negative reads a key to the model.
/// Exact names, then `.env.*` and the ssh key prefixes, then key-file extensions.
/// Not `credentials*` as a prefix, which would catch `credentials.rs`. `id_rsa.pub`
/// is a public key and harmless, but the prefix rule catches it too; asking about
/// it is an acceptable price for a rule that fits on one screen.
pub fn secret_name(path: &Path) -> bool {
    let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_lowercase()) else {
        return false;
    };
    const EXACT: [&str; 20] = [
        ".env",
        ".envrc",
        ".htpasswd",
        ".dockercfg",
        ".bash_history",
        ".my.cnf",
        ".boto",
        ".s3cfg",
        ".netrc",
        "_netrc",
        ".npmrc",
        ".pypirc",
        ".pgpass",
        ".git-credentials",
        "credentials",
        "credentials.json",
        "secrets.json",
        "secrets.yaml",
        "secrets.yml",
        "secrets.toml",
    ];
    const PREFIXES: [&str; 6] = [".env.", "id_rsa", "id_dsa", "id_ecdsa", "id_ed25519", "service-account"];
    // `env` catches `prod.env`; `tfstate` and `tfvars` hold Terraform's secrets in the clear.
    const EXTENSIONS: [&str; 15] = [
        "pem", "key", "pfx", "p12", "p8", "ppk", "kdbx", "jks", "keystore", "env", "tfstate", "tfvars",
        "gpg", "pgp", "asc",
    ];
    // Names that are only secret in their own folder: `config` is everywhere,
    // `.kube/config` holds cluster credentials.
    const IN_FOLDER: [(&str, &str); 4] =
        [(".kube", "config"), (".aws", "config"), (".docker", "config.json"), (".azure", "accesstokens.json")];
    let parent = path
        .parent()
        .and_then(Path::file_name)
        .map(|p| p.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    EXACT.contains(&name.as_str())
        || PREFIXES.iter().any(|p| name.starts_with(p))
        // A backup keeps the secret: `terraform.tfstate.backup`.
        || name.ends_with(".tfstate.backup")
        || Path::new(&name).extension().is_some_and(|e| EXTENSIONS.iter().any(|x| e == *x))
        || IN_FOLDER.iter().any(|(dir, file)| parent == *dir && name == *file)
}

/// The note that goes at the end of an answer the walk cut short.
fn cut_note(cut: Option<Cut>) -> String {
    match cut {
        Some(c) => format!("\n(The search stopped early because {} — narrow the pattern.)", c.say()),
        None => String::new(),
    }
}

// ── the two tools ───────────────────────────────────────────────────────────

/// The files matching `pattern`, or every file without one.
pub fn list(ws: &Workspace, pattern: Option<&str>, cancelled: impl Fn() -> bool) -> Result<String, String> {
    list_in(ws, pattern, edit::app_data().as_deref(), &cancelled)
}

fn list_in(
    ws: &Workspace,
    pattern: Option<&str>,
    app_data: Option<&Path>,
    cancelled: &dyn Fn() -> bool,
) -> Result<String, String> {
    let glob = pattern.map(|p| Pattern::parse(ws.root(), p)).transpose()?;
    let mut found: Vec<String> = Vec::new();
    let cut = walk(ws, app_data, cancelled, |rel, _| {
        if glob.as_ref().is_none_or(|g| g.matches(rel)) {
            found.push(rel.to_string());
        }
        0
    });

    let what = describe_list(pattern);
    if found.is_empty() {
        return Ok(format!("No files match {what}.{}", cut_note(cut)));
    }
    let n = found.len();
    let mut out = format!("{n} file{} match{} {what}:", plural(n), if n == 1 { "es" } else { "" });
    let mut shown = 0;
    for f in &found {
        if out.len() + f.len() + 1 > OUTPUT_CAP {
            break;
        }
        out.push('\n');
        out.push_str(f);
        shown += 1;
    }
    if shown < n {
        out.push_str(&format!("\n… and {} more — narrow the pattern.", n - shown));
    }
    out.push_str(&cut_note(cut));
    Ok(out)
}

/// Every line containing `text`, in files matching `pattern` or in all of them.
///
/// **Literal and smart-case**: case matters only when `text` has a capital in
/// it, as in ripgrep's `--smart-case`. `greet` finds `Greet`; `Greet` does not
/// find `greet`.
pub fn search(
    ws: &Workspace,
    text: &str,
    pattern: Option<&str>,
    cancelled: impl Fn() -> bool,
) -> Result<String, String> {
    search_in(ws, text, pattern, edit::app_data().as_deref(), &cancelled)
}

fn search_in(
    ws: &Workspace,
    text: &str,
    pattern: Option<&str>,
    app_data: Option<&Path>,
    cancelled: &dyn Fn() -> bool,
) -> Result<String, String> {
    search_text(text)?;
    let glob = pattern.map(|p| Pattern::parse(ws.root(), p)).transpose()?;
    let exact = text.chars().any(char::is_uppercase);
    let needle = if exact { text.to_string() } else { text.to_lowercase() };

    let (mut matches, mut files, mut secrets, mut unsafe_files) = (0usize, 0usize, 0usize, 0usize);
    let mut lines: Vec<String> = Vec::new();
    let mut size = 0usize;
    let cut = walk(ws, app_data, cancelled, |rel, full| {
        if glob.as_ref().is_some_and(|g| !g.matches(rel)) {
            return 0;
        }
        if secret_name(full) {
            secrets += 1;
            return 0;
        }
        // The walk saw a path, and a path is not a file: a link may have replaced it
        // since, and a hard link makes a file here be one outside the folder. So the
        // file is opened through the checks an automatic read gets, and read from
        // that handle.
        let checked = std::fs::canonicalize(full)
            .ok()
            .filter(|real| real.starts_with(ws.root()))
            .and_then(|real| edit::open_checked(&real));
        let Some(file) = checked else {
            unsafe_files += 1;
            return 0;
        };
        let Ok(meta) = file.metadata() else { return 0 };
        if meta.len() > MAX_FILE_BYTES {
            return 0;
        }
        let mut bytes = Vec::new();
        if std::io::Read::read_to_end(&mut std::io::Read::take(file, MAX_FILE_BYTES + 1), &mut bytes).is_err()
            || bytes.len() as u64 > MAX_FILE_BYTES
        {
            return 0;
        }
        // A NUL early on means binary: an image, an archive, a compiled file.
        if bytes[..bytes.len().min(8192)].contains(&0) {
            return bytes.len() as u64;
        }
        let body = String::from_utf8_lossy(&bytes);
        let mut here = false;
        for (i, line) in body.lines().enumerate() {
            let hit = if exact { line.contains(&needle) } else { line.to_lowercase().contains(&needle) };
            if !hit {
                continue;
            }
            matches += 1;
            here = true;
            let shown = format!("{rel}:{}: {}", i + 1, cut_line(line.trim_end()));
            if size + shown.len() < OUTPUT_CAP {
                size += shown.len() + 1;
                lines.push(shown);
            }
        }
        files += here as usize;
        bytes.len() as u64
    });

    let what = describe_search(text, pattern);
    let mut skipped = String::new();
    if secrets > 0 {
        skipped.push_str(&format!(
            "\n({secrets} file(s) whose names look like they hold secrets were not searched; \
             read_file one to ask the user.)"
        ));
    }
    if unsafe_files > 0 {
        skipped.push_str(&format!(
            "\n({unsafe_files} file(s) with a second name or reached through a link were not searched.)"
        ));
    }
    if matches == 0 {
        return Ok(format!("No matches for {what}.{skipped}{}", cut_note(cut)));
    }
    let mut out = format!(
        "{matches} match{} for {what} in {files} file{}:",
        if matches == 1 { "" } else { "es" },
        plural(files)
    );
    for l in &lines {
        out.push('\n');
        out.push_str(l);
    }
    if lines.len() < matches {
        out.push_str(&format!(
            "\n… and {} more — narrow the text or the pattern.",
            matches - lines.len()
        ));
    }
    out.push_str(&skipped);
    out.push_str(&cut_note(cut));
    Ok(out)
}

// ── the map ─────────────────────────────────────────────────────────────────

/// Extensions of the files a map lists: what a person writes by hand, in the
/// languages this project's folders hold. Prose, data and images say nothing
/// about where code lives.
const MAP_EXTENSIONS: [&str; 9] = ["rs", "ts", "tsx", "js", "mjs", "toml", "css", "sh", "py"];

/// How much of a file the map reads. A header comment is on line 1 to 3; 2 KB
/// is far more than that, and the bound is what keeps a map of 2,000 files
/// from reading 2,000 whole files.
const MAP_READ_BYTES: u64 = 2048;

/// A header comment is cut after this many characters, like [`LINE_CAP`] for a
/// match: it is there to say what the file is for, not to be read whole.
const MAP_COMMENT_CAP: usize = 120;

/// One line per code file — its path, and the first comment in its first three
/// lines — as a map the model is given before it asks anything.
///
/// **Why.** `list_files` answers "which files" and `search` answers "which
/// lines", but both need the model to guess a name or a word first. A map
/// answers "where would this live?" in one read. It is bounded to about
/// `budget_chars` (the find probe uses 7,000, about 2k tokens of a 32k context)
/// and, when the files do not fit, ends with `… N more files`.
///
/// The walk is [`walk`]'s, so the same rules hold: `.gitignore`, `target/`,
/// `node_modules/`, links, Bifrost's folder and Stop. Files whose names look like
/// secrets are left out ([`secret_name`]), reads go through
/// [`edit::open_checked`] and stop at 2 KB, and a file with a NUL in it is
/// binary and left out. A file with no header comment gets its path alone.
///
/// Empty when no code file was found, so a caller can append it only if it
/// says something.
pub fn repo_map(ws: &Workspace, budget_chars: usize, cancelled: impl Fn() -> bool) -> String {
    map_in(ws, budget_chars, edit::app_data().as_deref(), &cancelled)
}

fn map_in(ws: &Workspace, budget_chars: usize, app_data: Option<&Path>, cancelled: &dyn Fn() -> bool) -> String {
    let mut lines: Vec<String> = Vec::new();
    walk(ws, app_data, cancelled, |rel, full| {
        let code = full
            .extension()
            .is_some_and(|e| MAP_EXTENSIONS.iter().any(|x| e.eq_ignore_ascii_case(x)));
        if !code || secret_name(full) {
            return 0;
        }
        // Opened the way `search_in` opens: a path is not a file, and the
        // handle is what shows a link or a second name.
        let checked = std::fs::canonicalize(full)
            .ok()
            .filter(|real| real.starts_with(ws.root()))
            .and_then(|real| edit::open_checked(&real));
        let Some(file) = checked else { return 0 };
        let mut bytes = Vec::new();
        if std::io::Read::read_to_end(&mut std::io::Read::take(file, MAP_READ_BYTES), &mut bytes).is_err() {
            return 0;
        }
        if bytes.contains(&0) {
            return bytes.len() as u64;
        }
        let head = String::from_utf8_lossy(&bytes);
        lines.push(match header_comment(&head) {
            Some(c) => format!("{rel} — {c}"),
            None => rel.to_string(),
        });
        bytes.len() as u64
    });

    let mut out = String::new();
    let mut shown = 0;
    for l in &lines {
        if out.len() + l.len() + 1 > budget_chars {
            break;
        }
        out.push_str(l);
        out.push('\n');
        shown += 1;
    }
    if shown < lines.len() {
        out.push_str(&format!("… {} more files", lines.len() - shown));
    } else {
        out.pop();
    }
    out
}

/// The first header comment within the first three lines, marker stripped and
/// cut. `#!` is a shebang and `#[` / `#![` a Rust attribute, neither a comment.
fn header_comment(head: &str) -> Option<String> {
    head.lines().take(3).find_map(|line| {
        let t = line.trim();
        let text = if let Some(r) = t.strip_prefix("//!").or_else(|| t.strip_prefix("///")).or_else(|| t.strip_prefix("//"))
        {
            r
        } else if let Some(r) = t.strip_prefix("/*") {
            let r = r.strip_prefix('*').unwrap_or(r);
            r.strip_suffix("*/").unwrap_or(r)
        } else if t.starts_with('#') && !t.starts_with("#!") && !t.starts_with("#[") {
            &t[1..]
        } else {
            return None;
        };
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        Some(if text.chars().count() > MAP_COMMENT_CAP {
            format!("{}…", text.chars().take(MAP_COMMENT_CAP).collect::<String>())
        } else {
            text.to_string()
        })
    })
}

fn cut_line(line: &str) -> String {
    if line.chars().count() <= LINE_CAP {
        line.to_string()
    } else {
        format!("{}…", line.chars().take(LINE_CAP).collect::<String>())
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway project, removed on drop — `edit`'s Sandbox, again.
    struct Sandbox {
        dir: PathBuf,
    }

    impl Sandbox {
        fn new(tag: &str) -> Self {
            let n = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            let dir = std::env::temp_dir().join(format!("bifrost-search-{tag}-{}-{n}", std::process::id()));
            std::fs::create_dir_all(dir.join("project")).unwrap();
            Self { dir }
        }
        fn write(&self, rel: &str, text: &str) {
            let p = self.dir.join("project").join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        fn ws(&self) -> Workspace {
            Workspace::open(self.dir.join("project")).unwrap()
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn never() -> bool {
        false
    }

    fn listed(ws: &Workspace, pattern: Option<&str>) -> Vec<String> {
        list_in(ws, pattern, None, &never).unwrap().lines().skip(1).map(str::to_string).collect()
    }

    // ── what is walked ──

    #[test]
    fn build_output_and_git_are_skipped_even_without_a_gitignore() {
        let s = Sandbox::new("skip");
        s.write("src/main.rs", "fn main() {}\n");
        s.write("target/debug/huge.rs", "x\n");
        s.write("web/node_modules/lib/index.js", "x\n");
        s.write(".git/config", "x\n");
        s.write(".github/workflows/ci.yml", "x\n");
        // `.github` is not `.git`: hidden files are the project's own.
        assert_eq!(listed(&s.ws(), None), [".github/workflows/ci.yml", "src/main.rs"]);
    }

    /// A git worktree's `.git` is a file, not a folder — found by the find
    /// probe, whose codebase is one.
    #[test]
    fn a_worktrees_git_file_is_skipped_too() {
        let s = Sandbox::new("worktree");
        s.write(".git", "gitdir: C:/repo/.git/worktrees/x\n");
        s.write("src/main.rs", "x\n");
        assert_eq!(listed(&s.ws(), None), ["src/main.rs"]);
        assert!(search_in(&s.ws(), "gitdir", None, None, &never).unwrap().starts_with("No matches"));
    }

    /// Through the `\\?\` root `Workspace::open` gives, a nested `.gitignore`
    /// still applies.
    #[test]
    fn a_nested_gitignore_is_honoured() {
        let s = Sandbox::new("gitignore");
        s.write("app/.gitignore", "generated/\n*.log\n");
        s.write("app/src/lib.rs", "x\n");
        s.write("app/generated/out.rs", "x\n");
        s.write("app/debug.log", "x\n");
        let ws = s.ws();
        assert!(ws.root().to_string_lossy().starts_with(r"\\?\"));
        assert_eq!(listed(&ws, Some("app")), ["app/.gitignore", "app/src/lib.rs"]);
    }

    #[test]
    fn a_junction_out_of_the_folder_is_not_followed() {
        let s = Sandbox::new("junction");
        let outside = s.dir.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.rs"), "let secret = 1;\n").unwrap();
        s.write("src/main.rs", "fn main() {}\n");
        let link = s.dir.join("project").join("link");
        let made = std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(&link).arg(&outside).output();
        if !matches!(made, Ok(ref o) if o.status.success()) {
            eprintln!("could not create a junction here; skipping");
            return;
        }
        let ws = s.ws();
        assert_eq!(listed(&ws, None), ["src/main.rs"]);
        assert!(search_in(&ws, "secret", None, None, &never).unwrap().starts_with("No matches"));
    }

    #[test]
    fn mimirs_own_folder_is_skipped() {
        let s = Sandbox::new("appdata");
        s.write("AppData/Roaming/odin/config.json", "{\"checks\":{}}\n");
        s.write("notes.txt", "checks\n");
        let app = std::fs::canonicalize(s.dir.join("project/AppData/Roaming/odin")).unwrap();
        let out = search_in(&s.ws(), "checks", None, Some(&app), &never).unwrap();
        assert!(out.contains("notes.txt") && !out.contains("config.json"), "{out}");
    }

    // ── patterns ──

    #[test]
    fn a_pattern_outside_the_folder_is_refused() {
        let s = Sandbox::new("outside");
        let ws = s.ws();
        for bad in [
            "../**",
            "src/../../x",
            "C:/**",
            r"C:\Users\**",
            r"\\?\C:\**",
            "/etc/*",
            r"\\server\share\*",
            // Found by the review: an alternation hides a parent from a check
            // that looks at whole parts.
            "{..,x}/**",
            "src/{a,..}/*",
            "[.][.]/x",
            "{C:/,x}/**",
            "x/{//server,y}/*",
        ] {
            let err = list_in(&ws, Some(bad), None, &never).unwrap_err();
            assert!(err.contains("not inside the project folder"), "{bad}: {err}");
        }
    }

    #[test]
    fn a_bare_name_matches_anywhere_and_a_path_only_there() {
        let s = Sandbox::new("names");
        s.write("src/main.rs", "x\n");
        s.write("src/net/client.rs", "x\n");
        s.write("README.md", "x\n");
        let ws = s.ws();
        assert_eq!(listed(&ws, Some("*.rs")), ["src/main.rs", "src/net/client.rs"]);
        assert_eq!(listed(&ws, Some("src/*.rs")), ["src/main.rs"]);
        assert_eq!(listed(&ws, Some("src/**/*.rs")), ["src/main.rs", "src/net/client.rs"]);
        assert_eq!(listed(&ws, Some("readme.MD")), ["README.md"], "case does not matter on Windows");
    }

    #[test]
    fn a_folder_name_lists_what_is_in_it() {
        let s = Sandbox::new("folder");
        s.write("src/main.rs", "x\n");
        s.write("src/net/client.rs", "x\n");
        s.write("docs/a.md", "x\n");
        let ws = s.ws();
        assert_eq!(listed(&ws, Some("src")), ["src/main.rs", "src/net/client.rs"]);
        assert_eq!(listed(&ws, Some("src/")), ["src/main.rs", "src/net/client.rs"]);
    }

    #[test]
    fn nothing_found_says_so() {
        let s = Sandbox::new("none");
        s.write("a.txt", "x\n");
        assert_eq!(list_in(&s.ws(), Some("*.rs"), None, &never).unwrap(), "No files match *.rs.");
    }

    // ── search ──

    #[test]
    fn search_is_smart_case() {
        let s = Sandbox::new("case");
        s.write("a.rs", "fn Greet() {}\nfn greet() {}\n");
        let ws = s.ws();
        let lower = search_in(&ws, "greet", None, None, &never).unwrap();
        assert!(lower.starts_with("2 matches"), "{lower}");
        let upper = search_in(&ws, "Greet", None, None, &never).unwrap();
        assert!(upper.starts_with("1 match "), "{upper}");
        assert!(upper.contains("a.rs:1: fn Greet() {}"), "{upper}");
    }

    #[test]
    fn search_is_literal_not_a_regex() {
        let s = Sandbox::new("literal");
        s.write("a.rs", "let v = a.b(c);\nlet w = axb(c);\n");
        let out = search_in(&s.ws(), "a.b(", None, None, &never).unwrap();
        assert!(out.starts_with("1 match "), "{out}");
    }

    #[test]
    fn search_keeps_to_its_pattern_and_gives_line_numbers() {
        let s = Sandbox::new("scoped");
        s.write("src/config.rs", "pub const PORT: u16 = 8080;\n\npub fn port() -> u16 {\n    PORT\n}\n");
        s.write("README.md", "Listens on PORT 8080.\n");
        let out = search_in(&s.ws(), "PORT", Some("*.rs"), None, &never).unwrap();
        assert_eq!(
            out,
            "2 matches for \"PORT\" in *.rs in 1 file:\nsrc/config.rs:1: pub const PORT: u16 = 8080;\nsrc/config.rs:4:     PORT"
        );
    }

    /// CRLF files, like every file in this repo: the `\r` is not part of the
    /// line the model is shown, or the text it copies into an edit.
    #[test]
    fn a_crlf_line_is_shown_without_its_carriage_return() {
        let s = Sandbox::new("crlf");
        s.write("a.rs", "fn one() {}\r\nfn two() {}\r\n");
        let out = search_in(&s.ws(), "two", None, None, &never).unwrap();
        assert!(out.ends_with("a.rs:2: fn two() {}"), "{out:?}");
    }

    #[test]
    fn binary_and_huge_files_are_not_searched() {
        let s = Sandbox::new("binary");
        let p = s.dir.join("project");
        std::fs::write(p.join("image.png"), [0x89, b'P', b'N', b'G', 0, 0, b'n', b'e', b'e', b'd', b'l', b'e']).unwrap();
        std::fs::write(p.join("big.txt"), format!("needle\n{}", "x".repeat(MAX_FILE_BYTES as usize))).unwrap();
        s.write("small.txt", "needle\n");
        let out = search_in(&s.ws(), "needle", None, None, &never).unwrap();
        assert!(out.starts_with("1 match "), "{out}");
    }

    #[test]
    fn a_big_answer_is_cut_and_says_how_much_is_left() {
        let s = Sandbox::new("cap");
        let body: String = (0..2000).map(|i| format!("let needle_{i} = {i};\n")).collect();
        s.write("many.rs", &body);
        let out = search_in(&s.ws(), "needle", None, None, &never).unwrap();
        assert!(out.len() <= OUTPUT_CAP + 200, "{} bytes", out.len());
        assert!(out.starts_with("2000 matches"), "{}", &out[..60]);
        assert!(out.contains("more — narrow the text or the pattern"));
    }

    #[test]
    fn a_long_line_is_cut() {
        let s = Sandbox::new("long");
        s.write("min.js", &format!("var needle={};\n", "x".repeat(5000)));
        let out = search_in(&s.ws(), "needle", None, None, &never).unwrap();
        let line = out.lines().nth(1).unwrap();
        assert!(line.chars().count() < LINE_CAP + 30 && line.ends_with('…'), "{}", line.len());
    }

    #[test]
    fn stop_ends_a_walk_under_way() {
        let s = Sandbox::new("stop");
        for i in 0..50 {
            s.write(&format!("f{i}.txt"), "needle\n");
        }
        let calls = std::cell::Cell::new(0);
        let out = search_in(&s.ws(), "needle", None, None, &|| {
            calls.set(calls.get() + 1);
            calls.get() > 5
        })
        .unwrap();
        assert!(out.contains("stopped early because the user stopped it"), "{out}");
    }

    #[test]
    fn a_multi_line_search_is_refused() {
        let s = Sandbox::new("multiline");
        assert!(search_in(&s.ws(), "a\nb", None, None, &never).is_err());
    }

    /// What `ask_with_tools` asks before showing a card: a call that could never
    /// run is refused then, not after someone approved it.
    #[test]
    fn a_call_that_could_never_run_is_refused_before_it_is_shown() {
        let s = Sandbox::new("validate");
        let ws = s.ws();
        assert!(validate(&ws, Some("greet"), Some("src/**/*.rs")).is_ok());
        assert!(validate(&ws, None, None).is_ok());
        assert!(validate(&ws, None, Some("../**")).unwrap_err().contains("not inside"));
        assert!(validate(&ws, Some("a\nb"), None).unwrap_err().contains("one line"));
    }

    // ── what automatic reads may open ──

    /// `visible` takes the canonical path a read resolves to.
    fn seen(ws: &Workspace, rel: &str) -> bool {
        match std::fs::canonicalize(ws.root().join(rel)) {
            Ok(real) => visible(ws, &real),
            Err(_) => false,
        }
    }

    #[test]
    fn a_plain_file_is_visible() {
        let s = Sandbox::new("visible");
        s.write("src/main.rs", "fn main() {}\n");
        s.write(".github/ci.yml", "x\n");
        let ws = s.ws();
        assert!(seen(&ws, "src/main.rs"));
        // Hidden files are the project's own, as in the listing.
        assert!(seen(&ws, ".github/ci.yml"));
    }

    #[test]
    fn what_the_listing_hides_is_not_visible() {
        let s = Sandbox::new("invisible");
        s.write(".gitignore", "local.txt\n");
        s.write("local.txt", "x\n");
        s.write("app/.gitignore", "generated/\n*.log\n");
        s.write("app/generated/out.rs", "x\n");
        s.write("app/run.log", "x\n");
        s.write("app/src/lib.rs", "x\n");
        s.write(".git/config", "x\n");
        s.write("target/debug/x.rs", "x\n");
        s.write("web/node_modules/lib/index.js", "x\n");
        let ws = s.ws();
        assert!(seen(&ws, "app/src/lib.rs"));
        for hidden in [
            "local.txt",
            "app/generated/out.rs",
            "app/run.log",
            ".git/config",
            "target/debug/x.rs",
            "web/node_modules/lib/index.js",
        ] {
            assert!(!seen(&ws, hidden), "{hidden}");
        }
        // And it agrees with the listing on every one of them.
        assert_eq!(listed(&ws, None), [".gitignore", "app/.gitignore", "app/src/lib.rs"]);
    }

    #[test]
    fn a_file_reached_through_a_junction_is_not_visible() {
        let s = Sandbox::new("visible-junction");
        let outside = s.dir.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.rs"), "x\n").unwrap();
        s.write("src/main.rs", "x\n");
        let link = s.dir.join("project").join("link");
        let made = std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(&link).arg(&outside).output();
        if !matches!(made, Ok(ref o) if o.status.success()) {
            eprintln!("could not create a junction here; skipping");
            return;
        }
        let ws = s.ws();
        // Through the link as written, and as it canonicalises.
        assert!(!visible(&ws, &ws.root().join("link").join("secret.rs")));
        assert!(!visible(&ws, &std::fs::canonicalize(outside.join("secret.rs")).unwrap()));
    }

    #[test]
    fn secret_names_are_recognised_by_the_name_alone() {
        for yes in [
            ".env", ".ENV", ".env.local", ".env.production", ".netrc", "_netrc", ".npmrc", ".pypirc",
            ".pgpass", ".git-credentials", "credentials", "credentials.json", "secrets.json",
            "secrets.yaml", "secrets.yml", "secrets.toml", "id_rsa", "id_ed25519", "id_ecdsa", "id_dsa",
            // A public key is harmless, but the prefix rule catches it; one question is cheap.
            "id_rsa.pub", "ID_RSA", "server.pem", "TLS.KEY", "cert.pfx", "a.p12", "putty.ppk",
            "vault.kdbx", "app.jks", "debug.keystore", "prod.env", "Local.ENV", "terraform.tfstate",
            "prod.tfvars", "key.gpg", "key.asc", ".envrc", ".htpasswd", ".dockercfg", ".bash_history",
            ".my.cnf", ".boto", ".s3cfg", "terraform.tfstate.backup", "msg.pgp", "AuthKey_X.p8",
            "service-account.json", "service-account-prod.json",
        ] {
            assert!(secret_name(Path::new(yes)), "{yes}");
        }
        for no in [
            "credentials.rs", "credentials_test.go", "secrets.rs", "environment.rs", "env.rs", "envrc",
            "keyboard.rs", "monkey.txt", "key", "README.md", "config.toml", "pem", "config",
            "config.json", "backup.tfstate.rs",
        ] {
            assert!(!secret_name(Path::new(no)), "{no}");
        }
        // Mostly the name alone, not the folder it is in…
        assert!(secret_name(Path::new(r"C:\proj\config\.env")));
        assert!(!secret_name(Path::new(r"C:\proj\.env\notes.txt")));
        // …except for a few files that are only secret in their own folder.
        for yes in [r"C:\proj\.kube\config", r"C:\proj\.AWS\config", r"C:\proj\.docker\config.json"] {
            assert!(secret_name(Path::new(yes)), "{yes}");
        }
        for no in [r"C:\proj\src\config", r"C:\proj\.kube\notes.txt", r"C:\proj\app\config.json"] {
            assert!(!secret_name(Path::new(no)), "{no}");
        }
    }

    #[test]
    fn a_search_does_not_read_a_secret_named_file_and_says_so() {
        let s = Sandbox::new("secret");
        s.write(".env", "TOKEN=needle-in-env\n");
        s.write("keys/server.pem", "needle-in-pem\n");
        s.write("src/main.rs", "let needle = 1;\n");
        let out = search_in(&s.ws(), "needle", None, None, &never).unwrap();
        assert!(out.starts_with("1 match for \"needle\" in 1 file:"), "{out}");
        assert!(out.contains("src/main.rs:1:") && !out.contains("in-env") && !out.contains("in-pem"), "{out}");
        assert!(out.contains("2 file(s) whose names look like they hold secrets were not searched"), "{out}");
        assert!(out.contains("read_file one to ask the user"), "{out}");

        // With nothing else to find, the note is still there.
        let only = Sandbox::new("secret-only");
        only.write(".env", "TOKEN=needle\n");
        let out = search_in(&only.ws(), "needle", None, None, &never).unwrap();
        assert!(out.starts_with("No matches") && out.contains("1 file(s) whose names"), "{out}");

        // A pattern that leaves the secret out of the walk leaves the note out too.
        let out = search_in(&s.ws(), "needle", Some("*.rs"), None, &never).unwrap();
        assert!(!out.contains("secrets"), "{out}");
    }

    /// A hard link is a second name for one file, so a file in the folder can be
    /// one outside it; the handle shows that, the name does not.
    #[cfg(windows)]
    #[test]
    fn a_search_skips_a_file_with_a_second_name_and_says_so() {
        let s = Sandbox::new("search-hardlink");
        s.write("a.txt", "needle-in-a\n");
        s.write("src/main.rs", "let needle = 1;\n");
        if std::fs::hard_link(s.dir.join("project/a.txt"), s.dir.join("project/b.txt")).is_err() {
            eprintln!("could not create a hard link here; skipping");
            return;
        }
        let out = search_in(&s.ws(), "needle", None, None, &never).unwrap();
        assert!(out.starts_with("1 match for \"needle\" in 1 file:"), "{out}");
        assert!(out.contains("src/main.rs:1:") && !out.contains("in-a"), "{out}");
        assert!(out.contains("2 file(s) with a second name"), "{out}");
    }

    #[test]
    fn a_listing_still_shows_a_secret_named_file() {
        let s = Sandbox::new("secret-list");
        s.write(".env", "TOKEN=x\n");
        s.write("src/main.rs", "x\n");
        assert_eq!(listed(&s.ws(), None), [".env", "src/main.rs"]);
    }

    // ── the map ──

    fn mapped(s: &Sandbox, budget: usize) -> Vec<String> {
        let out = map_in(&s.ws(), budget, None, &never);
        if out.is_empty() { Vec::new() } else { out.lines().map(str::to_string).collect() }
    }

    #[test]
    fn each_kind_of_header_comment_is_found() {
        let s = Sandbox::new("map-kinds");
        s.write("a.rs", "//! inner doc
fn a() {}
");
        s.write("b.rs", "/// outer doc
fn b() {}
");
        s.write("c.ts", "// plain
export {};
");
        s.write("d.css", "/* block */
body {}
");
        s.write("e.js", "/** doc block */
let e;
");
        s.write("f.py", "# hash
x = 1
");
        s.write("g.toml", "# a manifest
[package]
");
        s.write("h.sh", "#!/bin/sh
# runs it
");
        assert_eq!(
            mapped(&s, 10_000),
            [
                "a.rs — inner doc",
                "b.rs — outer doc",
                "c.ts — plain",
                "d.css — block",
                "e.js — doc block",
                "f.py — hash",
                "g.toml — a manifest",
                "h.sh — runs it",
            ]
        );
    }

    #[test]
    fn shebangs_and_rust_attributes_are_not_headers() {
        let s = Sandbox::new("map-attrs");
        s.write("a.rs", "#![allow(dead_code)]
#[derive(Debug)]
// the real one
struct A;
");
        s.write("b.rs", "#![allow(dead_code)]
#[derive(Debug)]
struct B;
");
        assert_eq!(mapped(&s, 10_000), ["a.rs — the real one", "b.rs"]);
    }

    #[test]
    fn a_header_after_the_third_line_is_not_taken() {
        let s = Sandbox::new("map-late");
        s.write("late.rs", "use a;
use b;
use c;
use d;
// too late
");
        s.write("third.rs", "use a;
use b;
// just in time
");
        assert_eq!(mapped(&s, 10_000), ["late.rs", "third.rs — just in time"]);
    }

    #[test]
    fn a_long_header_is_cut() {
        let s = Sandbox::new("map-long");
        s.write("a.rs", &format!("//! {}
", "x".repeat(500)));
        let lines = mapped(&s, 10_000);
        assert_eq!(lines[0].chars().count(), "a.rs — ".chars().count() + MAP_COMMENT_CAP + 1, "{}", lines[0]);
        assert!(lines[0].ends_with('…'));
    }

    #[test]
    fn the_map_leaves_out_what_is_not_code_or_not_allowed() {
        let s = Sandbox::new("map-skip");
        s.write("src/main.rs", "// kept
");
        s.write("README.md", "# prose
");
        s.write("logo.png", "x");
        s.write(".env", "TOKEN=x
");
        s.write("secrets.toml", "# key
");
        s.write("keys/server.pem", "x");
        s.write(".gitignore", "local.rs
");
        s.write("local.rs", "// ignored
");
        s.write("target/debug/build.rs", "// build
");
        s.write("bin.rs", "// a\0b\n");
        assert_eq!(mapped(&s, 10_000), ["src/main.rs — kept"]);
    }

    #[test]
    fn a_map_with_nothing_to_list_is_empty() {
        let s = Sandbox::new("map-empty");
        s.write("notes.txt", "x
");
        assert_eq!(map_in(&s.ws(), 1000, None, &never), "");
    }

    #[test]
    fn a_map_is_sorted_and_says_how_many_files_the_budget_left_out() {
        let s = Sandbox::new("map-budget");
        for name in ["c", "a", "d", "b"] {
            s.write(&format!("src/{name}.rs"), &format!("//! file {name}
"));
        }
        // Each line is 21 characters with its newline: room for two.
        let line = "src/a.rs — file a
".len();
        assert_eq!(mapped(&s, line * 2), ["src/a.rs — file a", "src/b.rs — file b", "… 2 more files"]);
        assert_eq!(mapped(&s, line * 4).len(), 4, "everything fits, so no note");
        assert!(!map_in(&s.ws(), line * 4, None, &never).contains("more files"));
    }

    // ── git's rules, checked against git ──

    /// Whether `git` is on the PATH; the tests that ask it say so and skip if not.
    fn have_git() -> bool {
        std::process::Command::new("git").arg("--version").output().is_ok_and(|o| o.status.success())
    }

    /// A repository with every kind of ignore rule at different levels, the chosen
    /// folder being `repo/pkg/app` (a package in a monorepo), and files that each
    /// rule hides or, through a negation, does not. Every file is then put to
    /// `git check-ignore` and to this module: `visible` and `list` must say what
    /// git says. `visible` is what decides whether a read runs unasked, so a
    /// disagreement in either direction is a defect.
    #[test]
    fn the_ignore_rules_answer_the_way_git_check_ignore_does() {
        if !have_git() {
            eprintln!("skipped: git is not on the PATH");
            return;
        }
        let s = Sandbox::new("git-agrees");
        let repo = s.dir.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let init = std::process::Command::new("git").arg("init").arg("-q").current_dir(&repo).output().unwrap();
        assert!(init.status.success(), "git init failed");

        let put = |rel: &str, text: &str| {
            let p = repo.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        // Rules at the repository's root, above the chosen folder.
        put(".gitignore", "*.log\n!keep.log\nCache/\n/rooted.txt\n");
        put(".git/info/exclude", "local-only.txt\n");
        // A folder between the root and the chosen one.
        put("pkg/.gitignore", "mid.dat\n");
        // The chosen folder's own, with a negation, and a nested one below it.
        put("pkg/app/.gitignore", "*.tmp\n!important.tmp\n");
        put("pkg/app/sub/.gitignore", "zz.txt\n");

        let mut files = vec![
            "pkg/app/a.rs",
            "pkg/app/x.log",
            "pkg/app/keep.log",
            "pkg/app/sub/deep.log",
            "pkg/app/local-only.txt",
            "pkg/app/mid.dat",
            "pkg/app/y.tmp",
            "pkg/app/important.tmp",
            "pkg/app/rooted.txt",
            "pkg/app/sub/zz.txt",
            "pkg/app/sub/ok.txt",
            "pkg/app/.gitignore",
            "pkg/app/sub/.gitignore",
        ];
        // Windows git runs with core.ignorecase=true; `Cache/` hides `cache/`.
        if cfg!(windows) {
            files.push("pkg/app/cache/c.txt");
        }
        // Not under the chosen folder, so neither listed nor visible - and not
        // something this test compares.
        put("outside.txt", "x\n");
        put("pkg/sibling.txt", "x\n");
        for f in &files {
            put(f, "x\n");
        }

        let ws = Workspace::open(repo.join("pkg/app")).unwrap();
        let listing = listed(&ws, None);
        let mut wrong = Vec::new();
        for f in &files {
            let out = std::process::Command::new("git")
                .args(["-c", "core.ignorecase=true", "check-ignore", "-q", f])
                .current_dir(&repo)
                .output()
                .unwrap();
            let ignored = match out.status.code() {
                Some(0) => true,
                Some(1) => false,
                other => panic!("git check-ignore {f}: {other:?}"),
            };
            let rel = f.strip_prefix("pkg/app/").unwrap();
            let real = std::fs::canonicalize(repo.join(f)).unwrap();
            let sees = visible(&ws, &real);
            let lists = listing.iter().any(|l| l == rel);
            if sees == ignored || lists == ignored {
                wrong.push(format!("{rel}: git ignores it = {ignored}, visible = {sees}, listed = {lists}"));
            }
        }
        // The folder above is not walked beside the chosen one.
        let beside = std::fs::canonicalize(repo.join("pkg/sibling.txt")).unwrap();
        assert!(!visible(&ws, &beside), "a file beside the chosen folder was visible");
        assert!(wrong.is_empty(), "disagreements with git:\n{}", wrong.join("\n"));
    }

    /// A folder the user chose inside a folder the repository's `.gitignore` hides
    /// (`build/` ignored, `repo/build/out` chosen) is still listed and readable:
    /// it is its own project then, and its own `.gitignore` still applies.
    #[test]
    fn a_chosen_folder_inside_an_ignored_folder_is_its_own_project() {
        let s = Sandbox::new("ignored-above");
        let repo = s.dir.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join(".gitignore"), "build/
").unwrap();
        let out = repo.join("build").join("out");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("a.txt"), "hello").unwrap();
        std::fs::write(out.join("b.log"), "hello").unwrap();
        std::fs::write(out.join(".gitignore"), "*.log
").unwrap();
        let ws = Workspace::open(&out).unwrap();
        let files = list_in(&ws, None, None, &never).unwrap();
        assert!(files.lines().any(|l| l == "a.txt"), "{files}");
        assert!(!files.contains("b.log"), "{files}");
        assert!(seen(&ws, "a.txt"));
        assert!(!seen(&ws, "b.log"));
    }

    /// A `.gitignore` above the repository's root is not git's business and is not
    /// ours either: the walk stops at the root.
    #[test]
    fn a_gitignore_above_the_repository_root_is_not_read() {
        let s = Sandbox::new("above-root");
        std::fs::write(s.dir.join(".gitignore"), "a.rs\n").unwrap();
        s.write(".git/config", "x\n");
        s.write("a.rs", "x\n");
        s.write("b.rs", "x\n");
        // The project folder is the repository (it holds `.git`), the folder
        // above it holds a stray `.gitignore`.
        assert_eq!(listed(&s.ws(), None), ["a.rs", "b.rs"]);
    }

    #[test]
    fn info_exclude_and_case_different_patterns_hide_files_in_a_plain_repository() {
        let s = Sandbox::new("exclude-case");
        s.write(".git/info/exclude", "local-only.txt\n");
        s.write(".gitignore", "Secrets/\n");
        s.write("local-only.txt", "x\n");
        s.write("secrets/a.txt", "x\n");
        s.write("src/lib.rs", "x\n");
        let expect: &[&str] = if cfg!(windows) { &[".gitignore", "src/lib.rs"] } else { &[".gitignore", "secrets/a.txt", "src/lib.rs"] };
        assert_eq!(listed(&s.ws(), None), expect);
    }
}
