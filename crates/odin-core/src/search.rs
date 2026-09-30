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
//! Four rules, each with a test:
//!
//! 1. **Nothing outside the folder.** Patterns are relative; `..`, a drive or a
//!    verbatim path is refused, not resolved. The walk does not follow links or
//!    junctions, and skips `.git` and Mímir's own settings folder — the same
//!    places [`crate::edit`] will not write.
//! 2. **Not the build's files.** `target/` and `node_modules/` are skipped even
//!    in a folder with no `.gitignore`, and `.gitignore` files inside the folder
//!    are honoured. Otherwise one search of a Rust project reads gigabytes of
//!    build output.
//! 3. **It ends.** Files visited, bytes read and time are capped, and Stop
//!    reaches a walk under way — choosing your whole home folder by mistake
//!    must not hang the app.
//! 4. **Its answer fits.** About 8 KB, sorted so a rerun reads the same, with the
//!    full count and a note when lines were left out. 200 matches at 200
//!    characters would be 12k tokens of a 32k context.
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
    let skip_app_data = app_data.map(Path::to_path_buf);
    let mut walker = ignore::WalkBuilder::new(&root);
    walker
        .hidden(false)
        .parents(false)
        .require_git(false)
        .git_global(false)
        .git_exclude(false)
        .follow_links(false)
        .sort_by_file_path(|a, b| a.cmp(b))
        .filter_entry(move |e| {
            if e.path_is_symlink() {
                return false;
            }
            let name = e.file_name().to_string_lossy();
            // In a git worktree, `.git` is a file pointing at the real one.
            // Still git's, never the project's.
            if name.eq_ignore_ascii_case(".git") {
                return false;
            }
            let is_dir = e.file_type().is_some_and(|t| t.is_dir());
            if is_dir && e.depth() > 0 {
                if SKIPPED.iter().any(|s| name.eq_ignore_ascii_case(s)) {
                    return false;
                }
                if let Some(app) = &skip_app_data
                    && std::fs::canonicalize(e.path()).is_ok_and(|real| real.starts_with(app))
                {
                    return false;
                }
            }
            true
        });

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

    let (mut matches, mut files) = (0usize, 0usize);
    let mut lines: Vec<String> = Vec::new();
    let mut size = 0usize;
    let cut = walk(ws, app_data, cancelled, |rel, full| {
        if glob.as_ref().is_some_and(|g| !g.matches(rel)) {
            return 0;
        }
        let Ok(meta) = std::fs::metadata(full) else { return 0 };
        if meta.len() > MAX_FILE_BYTES {
            return 0;
        }
        let Ok(bytes) = std::fs::read(full) else { return 0 };
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
    if matches == 0 {
        return Ok(format!("No matches for {what}.{}", cut_note(cut)));
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
    out.push_str(&cut_note(cut));
    Ok(out)
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
            let dir = std::env::temp_dir().join(format!("odin-search-{tag}-{}-{n}", std::process::id()));
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
}
