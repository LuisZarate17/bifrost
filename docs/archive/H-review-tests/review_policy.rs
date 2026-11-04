//! Independent review of Stage H's approval policy and path containment (H1, H2, H5, H9, H10).
//!
//! Everything here runs against odin-core's public API inside throwaway folders. `APPDATA` and
//! `USERPROFILE` are redirected to a scratch "home" before anything else runs, so nothing in this
//! file can touch the real `%APPDATA%/odin`.
//!
//! Tests named `defect_*` assert what the spec promises and FAIL while the defect exists. Tests
//! named `holds_*` assert a guarantee and are expected to pass. `note_*` tests only record
//! behaviour: they print what they saw and assert nothing that could be wrong.
#![cfg(windows)]
#![allow(clippy::too_many_lines)]

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::{Duration, Instant};

use odin_core::edit::{self, Located, Workspace};
use odin_core::tools::{self, Approval, Auto, Request};
use odin_core::{config, instructions, search};

const MARK: &str = "ODIN-REVIEW-MARKER-7731";

/// Forward slashes to backslashes. Paths in this file are written with `/` and converted here.
fn w(s: &str) -> String {
    s.replace('/', "\u{5c}")
}

/// Where the fixtures go: under the system temp folder, never under `%APPDATA%\odin`.
fn base() -> PathBuf {
    std::env::temp_dir().join("odin-review-policy")
}

fn home() -> PathBuf {
    base().join("home")
}

static INIT: Once = Once::new();

/// Tests that read or write the shared config file take this, so they cannot disturb each other.
static CONFIG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Point `APPDATA` and `USERPROFILE` at a scratch home, once, before anything reads them.
fn init() {
    INIT.call_once(|| {
        let appdata = home().join("AppData").join("Roaming");
        std::fs::create_dir_all(appdata.join("odin")).unwrap();
        // SAFETY: runs once, behind `Once`, before any test in this binary reads either variable.
        unsafe {
            std::env::set_var("APPDATA", &appdata);
            std::env::set_var("USERPROFILE", home());
        }
    });
}

/// A throwaway `project` folder with an `outside` folder beside it, removed on drop.
struct Sandbox {
    dir: PathBuf,
    links: RefCell<Vec<PathBuf>>,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        init();
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let dir = base().join("run").join(format!("{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(dir.join("project")).unwrap();
        std::fs::create_dir_all(dir.join("outside")).unwrap();
        Self { dir, links: RefCell::new(Vec::new()) }
    }
    fn project(&self) -> PathBuf {
        self.dir.join("project")
    }
    fn outside(&self) -> PathBuf {
        self.dir.join("outside")
    }
    fn ws(&self) -> Workspace {
        Workspace::open(self.project()).unwrap()
    }
    fn write(&self, rel: &str, text: &str) -> PathBuf {
        let p = self.project().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
        p
    }
    fn write_outside(&self, rel: &str, text: &str) -> PathBuf {
        let p = self.outside().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
        p
    }
    /// A junction `project/<link>` -> `target`. False where junctions cannot be made.
    fn junction(&self, link: &str, target: &Path) -> bool {
        let at = self.project().join(link);
        if let Some(parent) = at.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(edit::plain(&at))
            .arg(edit::plain(target))
            .output();
        let ok = matches!(made, Ok(ref o) if o.status.success());
        if ok {
            self.links.borrow_mut().push(at);
        } else {
            eprintln!("could not create a junction here; skipping");
        }
        ok
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        // Junctions first, so nothing can ever follow one into a real folder.
        for l in self.links.borrow().iter() {
            let _ = std::fs::remove_dir(l);
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The 8.3 alias of an existing path, if the volume gives it one.
fn short(p: &Path) -> Option<String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::GetShortPathNameW;
    use windows::core::PCWSTR;
    let wide: Vec<u16> = p.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut buf = vec![0u16; 2048];
    // SAFETY: `wide` is NUL-terminated and alive for the call; `buf` is a live buffer passed with its length.
    let n = unsafe { GetShortPathNameW(PCWSTR(wide.as_ptr()), Some(&mut buf)) } as usize;
    (n != 0 && n <= buf.len()).then(|| String::from_utf16_lossy(&buf[..n]))
}

// ── deciding a read the way the app does ───────────────────────────────────

#[derive(Debug)]
enum Out {
    /// Ran with no card; this is what the model got back.
    Ran(String),
    /// `approval` said automatic, but the checked open refused it, so the app would ask.
    AutoThenAsk,
    Ask,
    Reject(String),
    Other(String),
}

fn brief(o: &Out) -> String {
    match o {
        Out::Ran(t) => {
            format!("RAN UNASKED, {} bytes{}", t.len(), if t.contains(MARK) { " ** CONTAINS THE MARKER **" } else { "" })
        }
        Out::AutoThenAsk => "auto, then the checked open refused -> ask".into(),
        Out::Ask => "ask".into(),
        Out::Reject(w) => format!("reject ({w})"),
        Out::Other(s) => s.clone(),
    }
}

fn read_req(path: &str) -> Request {
    Request::Read { path: path.to_string(), range: None }
}

/// What the app does with a `read_file` call: `approval`, then `read_unasked` for an automatic one.
fn decide(ws: &Workspace, path: &str) -> Out {
    match tools::approval(Some(ws), &read_req(path), false) {
        Approval::Auto(Auto::Read { real, range }) => match tools::read_unasked(&real, range) {
            Some(t) => Out::Ran(t),
            None => Out::AutoThenAsk,
        },
        Approval::Ask => Out::Ask,
        Approval::Reject(w) => Out::Reject(w),
        other => Out::Other(format!("{other:?}")),
    }
}

/// Run every spelling; none may run unasked and return `marker`. Prints and returns the table.
fn matrix(ws: &Workspace, what: &str, spellings: &[String], marker: &str) -> String {
    let mut table = String::new();
    let mut bad = Vec::new();
    for s in spellings {
        let out = decide(ws, s);
        table.push_str(&format!("  {s:?}  ->  {}\n", brief(&out)));
        if matches!(&out, Out::Ran(t) if t.contains(marker)) {
            bad.push(s.clone());
        }
    }
    eprintln!("== {what} ==\n{table}");
    assert!(bad.is_empty(), "{what}: ran with no card and returned protected content for {bad:?}\n{table}");
    table
}

/// Relative spellings of `rel` (a path inside the project, written with `/`). `sub` must exist.
fn rel_variants(rel: &str) -> Vec<String> {
    let back = w(rel);
    vec![
        rel.to_string(),
        rel.to_uppercase(),
        rel.to_lowercase(),
        format!("./{rel}"),
        w(&format!("./{rel}")),
        format!("sub/../{rel}"),
        format!("sub//../{rel}"),
        w(&format!("sub/../{rel}")),
        format!("{rel}."),
        format!("{rel}.."),
        format!("{rel}. "),
        format!("{rel}::$DATA"),
        format!("{rel}::$data"),
        format!("{rel}:"),
        format!("{rel}:x"),
        format!("{rel}:x:$DATA"),
        format!("{rel}/"),
        format!("{back}{}", '\u{5c}'),
    ]
}

/// Spellings that wrap the path the way models do.
fn quoted_variants(rel: &str) -> Vec<String> {
    let (tick, dq, sq) = (char::from(96u8), char::from(34u8), char::from(39u8));
    vec![format!("{tick}{rel}{tick}"), format!("{dq}{rel}{dq}"), format!("{sq}{rel}{sq}")]
}

/// Absolute spellings of an existing absolute path given in plain form (a drive, then backslashes).
fn abs_variants(long: &str) -> Vec<String> {
    let bs = '\u{5c}';
    let drive = &long[..1];
    let rooted = &long[2..];
    let fwd = long.replace(bs, "/");
    let dollar = '$';
    let admin = format!("{drive}{dollar}{rooted}");
    let mut v = vec![
        long.to_string(),
        long.to_uppercase(),
        long.to_lowercase(),
        fwd.clone(),
        w(&format!("//?/{long}")),
        w(&format!("//?/{}", long.to_uppercase())),
        format!("//?/{fwd}"),
        w(&format!("//./{long}")),
        format!("//./{fwd}"),
        w(&format!("//localhost/{admin}")),
        w(&format!("//127.0.0.1/{admin}")),
        w(&format!("//?/UNC/localhost/{admin}")),
        format!("{long}."),
        format!("{long}::$DATA"),
        format!("{long}:x"),
        rooted.to_string(),
        rooted.replace(bs, "/"),
        format!("{}{}", &long[..2], rooted.trim_start_matches(bs)),
    ];
    if let Some(s) = short(Path::new(long)) {
        v.push(w(&format!("//?/{s}")));
        v.push(s.to_lowercase());
        v.push(s);
    }
    v
}

/// Every spelling of a file inside the project that these tests know.
fn all_variants(ws: &Workspace, rel: &str) -> Vec<String> {
    let bs = '\u{5c}';
    let mut v = rel_variants(rel);
    v.extend(quoted_variants(rel));
    let long = format!("{}{bs}{}", ws.root_display(), w(rel));
    v.extend(abs_variants(&long));
    // The alias a person would type from the project folder: the tail of the short form.
    if let (Some(sl), Some(sr)) = (short(Path::new(&long)), short(ws.root())) {
        if let Some(tail) = sl.strip_prefix(&format!("{sr}{bs}")) {
            v.push(tail.to_string());
            v.push(tail.replace(bs, "/"));
        }
    }
    v
}

// ═══ 1. Reads that must never run unasked, whatever the spelling ═══════════

#[test]
fn holds_a_secret_named_file_never_runs_unasked_in_any_spelling() {
    let s = Sandbox::new("spell-env");
    s.write(".env", &format!("TOKEN={MARK}\n"));
    s.write(".env.production", &format!("TOKEN={MARK}\n"));
    s.write("keys/server.pem", &format!("{MARK}\n"));
    s.write("sub/placeholder.txt", "x\n");
    s.write("src/main.rs", "fn main() {}\n");
    let ws = s.ws();
    for rel in [".env", ".env.production", "keys/server.pem"] {
        let t = matrix(&ws, &format!("secret-named {rel}"), &all_variants(&ws, rel), MARK);
        // The plain spelling must be a question, not a rejection: the control that makes the table meaningful.
        assert!(t.contains(&format!("{rel:?}  ->  ask")), "{rel} should ask:\n{t}");
    }
    // Control: an ordinary file does run, so the matrix is not passing because nothing ever runs.
    assert!(matches!(decide(&ws, "src/main.rs"), Out::Ran(t) if t.contains("fn main")));
}

#[test]
fn holds_a_gitignored_file_never_runs_unasked_in_any_spelling() {
    let s = Sandbox::new("spell-ignored");
    s.write(".gitignore", "ignored.txt\nbuild_out/\n");
    s.write("ignored.txt", &format!("{MARK}\n"));
    s.write("build_out/inner.txt", &format!("{MARK}\n"));
    s.write("nested/.gitignore", "*.cache\n!keep.cache\n");
    s.write("nested/a.cache", &format!("{MARK}\n"));
    s.write("nested/keep.cache", "kept\n");
    s.write("sub/placeholder.txt", "x\n");
    let ws = s.ws();
    for rel in ["ignored.txt", "build_out/inner.txt", "nested/a.cache"] {
        let t = matrix(&ws, &format!("gitignored {rel}"), &all_variants(&ws, rel), MARK);
        assert!(t.contains(&format!("{rel:?}  ->  ask")), "{rel} should ask:\n{t}");
    }
    // A negation re-includes exactly that file, so it runs.
    assert!(matches!(decide(&ws, "nested/keep.cache"), Out::Ran(t) if t.contains("kept")));
}

#[test]
fn holds_a_file_outside_the_folder_never_runs_unasked_in_any_spelling() {
    let s = Sandbox::new("spell-outside");
    s.write("sub/placeholder.txt", "x\n");
    let o = s.write_outside("o.txt", &format!("{MARK}\n"));
    let ws = s.ws();
    let mut sp: Vec<String> = Vec::new();
    for r in [
        "../outside/o.txt",
        "sub/../../outside/o.txt",
        "./../outside/o.txt",
        "..//outside//o.txt",
        "../outside/./o.txt",
        "../OUTSIDE/O.TXT",
        "../outside/o.txt.",
        "../outside/o.txt::$DATA",
        "../outside/o.txt:x",
        "../outside/o.txt ",
    ] {
        sp.push(r.to_string());
        sp.push(w(r));
    }
    sp.extend(abs_variants(&edit::plain(&std::fs::canonicalize(&o).unwrap())));
    matrix(&ws, "outside file", &sp, MARK);
    // Control: it does exist and it is a question.
    assert!(matches!(decide(&ws, "../outside/o.txt"), Out::Ask));
}

#[test]
fn note_what_canonicalize_does_with_streams_short_names_and_trailing_dots() {
    let s = Sandbox::new("canon");
    let p = s.write(".env", "x\n");
    s.write("dir/f.txt", "x\n");
    let ws = s.ws();
    let root = ws.root_display();
    let bs = '\u{5c}';
    let mut seen = String::new();
    for spelling in [
        format!("{root}{bs}.env"),
        format!("{root}{bs}.env::$DATA"),
        format!("{root}{bs}.env."),
        format!("{root}{bs}.env:x"),
        format!("{root}{bs}dir::$INDEX_ALLOCATION{bs}f.txt"),
        format!("{root}{bs}DIR.{bs}f.txt"),
        format!("{root}{bs}ENV~1"),
        w(&format!("//?/{root}/.env.")),
        w(&format!("//?/{root}/ENV~1")),
        short(&p).unwrap_or_default(),
    ] {
        let c = std::fs::canonicalize(&spelling);
        seen.push_str(&format!("  canonicalize({spelling:?}) = {c:?}\n"));
        seen.push_str(&format!("      locate = {:?}\n", ws.locate(&spelling)));
    }
    eprintln!("{seen}");
}

// ═══ 2. Reads the model can make without any card, by way of other tools ═══

/// `edit_file` is planned BEFORE any card, and a plan that fails goes straight back to the model
/// (apps/mimir/src-tauri/src/lib.rs, `Request::Edit` -> `plan_replace` -> `Err(e) => reject(..)`).
/// A plan that misses its text answers with the closest lines of the file. That is a read of the
/// file with no card, so it must not work on a file whose direct read would ask.
#[test]
fn defect_edit_planning_returns_lines_of_a_secret_file_without_a_card() {
    let s = Sandbox::new("edit-leak");
    s.write(".env", "DB_HOST=prod-db.internal\nDB_USER=admin\nDB_PASSWORD=hunter2-ODINSECRET\n");
    // An Ed25519 PKCS#8 key is exactly three lines; every model knows its first characters.
    s.write(
        "keys/server.pem",
        "-----BEGIN PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEIODINSECRETKEYMATERIAL0123456789abcdefghijklm\n-----END PRIVATE KEY-----\n",
    );
    s.write(".gitignore", "private/\n");
    s.write("private/notes.txt", "the launch code is 0451-ODINSECRET\n");
    let ws = s.ws();
    // None of these three would run unasked as a read: that is the premise.
    for p in [".env", "keys/server.pem", "private/notes.txt"] {
        assert!(matches!(decide(&ws, p), Out::Ask), "{p} should ask when read directly");
    }
    let tries = [
        (".env", "DB_USER=admin\nDB_PASSWORD=wrongpass"),
        ("keys/server.pem", "-----BEGIN PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEI\n-----END PRIVATE KEY-----"),
        ("private/notes.txt", "the launch code is 1234-X"),
    ];
    let mut leaked = Vec::new();
    for (path, old) in tries {
        let out = edit::plan_replace(&ws, path, old, "x");
        eprintln!("plan_replace({path:?}) -> {out:?}");
        if let Err(e) = out {
            if e.contains("ODINSECRET") {
                leaked.push(path);
            }
        }
    }
    assert!(leaked.is_empty(), "edit planning handed secret lines to the model with no card for {leaked:?}");
}

/// The same planner also answers "how many times does this text occur" for any file in the
/// folder, secret or ignored, with no card. A weaker oracle than the excerpt above.
// Updated in review round 2: a failed plan on a protected file now returns one fixed message with no count, no text and no reason (R-01).
#[test]
fn note_edit_planning_counts_occurrences_in_a_secret_file() {
    let s = Sandbox::new("edit-oracle");
    s.write(".env", "A=1\nB=1\nC=2\n");
    let ws = s.ws();
    let two = edit::plan_replace(&ws, ".env", "=1", "=9");
    eprintln!("{two:?}");
    assert_eq!(
        two.as_ref().err().map(String::as_str),
        Some(
            "old_text does not match exactly one place in .env. It is a protected file, so its text is \
             not shown; ask the user to allow a read of it first."
        ),
        "{two:?}"
    );
    let none = edit::plan_replace(&ws, ".env", "=7", "=9");
    assert_eq!(none.as_ref().err(), two.as_ref().err(), "a miss and an ambiguous match read the same: {none:?}");
    assert!(!format!("{two:?}").contains("times") && !format!("{two:?}").contains("A=1"), "{two:?}");
}

// ═══ 3. Config: the off switch ═════════════════════════════════════════════

fn parse_ask(json: &str) -> bool {
    serde_json::from_str::<config::Config>(json).unwrap().ask_every_call
}

#[test]
fn holds_ask_every_call_is_read_in_the_stricter_direction() {
    for on in ["true", "\"true\"", "\"TRUE\"", "1", "\"yes\"", "\"0\"", "\" false\"", "2", "0.0", "null", "[]", "{}", "[false]"] {
        assert!(parse_ask(&format!("{{\"ask_every_call\":{on}}}")), "{on} should mean ask");
    }
    for off in ["false", "\"false\"", "\"False\"", "0"] {
        assert!(!parse_ask(&format!("{{\"ask_every_call\":{off}}}")), "{off} should mean off");
    }
    assert!(!parse_ask("{}"), "a missing field is off by default");
}

/// `config::load()` answers a file that does not parse with `Config::default()`, whose
/// `ask_every_call` is false. The lenient field reader only helps when the file is valid JSON.
#[test]
fn defect_an_unparseable_config_silently_turns_ask_every_call_off() {
    let _guard = CONFIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    init();
    let dir = PathBuf::from(std::env::var_os("APPDATA").unwrap()).join("odin");
    assert!(dir.starts_with(home()), "must be the redirected APPDATA, never the real one");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.json");
    let bom = [0xEFu8, 0xBB, 0xBF];
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("UTF-8 BOM, as PowerShell 5 Set-Content -Encoding UTF8 writes", [&bom[..], br#"{"ask_every_call": true}"#].concat()),
        ("a trailing comma", br#"{"ask_every_call": true, "model": "x",}"#.to_vec()),
        ("a single-backslash Windows path", br#"{"ask_every_call": true, "workspace": "C:\Users\me\proj"}"#.to_vec()),
        ("a // comment", b"{\"ask_every_call\": true}\n// remember to keep this\n".to_vec()),
        ("a truncated write", br#"{"ask_every_call": true, "mo"#.to_vec()),
    ];
    let mut fail_open = Vec::new();
    for (what, bytes) in &cases {
        std::fs::write(&path, bytes).unwrap();
        let c = config::load();
        eprintln!("{what}: load().ask_every_call = {}", c.ask_every_call);
        if !c.ask_every_call {
            fail_open.push(*what);
        }
    }
    // Control: the same setting in a valid file is honoured.
    std::fs::write(&path, br#"{"ask_every_call": true}"#).unwrap();
    assert!(config::load().ask_every_call, "control");
    let _ = std::fs::remove_file(&path);
    assert!(fail_open.is_empty(), "a file that does not parse resets ask_every_call to false: {fail_open:?}");
}

/// Edit planning through a hard link reads the file the link points at, wherever it lives.
// Updated in review round 2: a failed plan on a protected file now returns one fixed message with no file text (R-01), where it used to return the closest lines.
#[test]
fn note_edit_planning_reads_through_a_hard_link_to_an_outside_file() {
    let s = Sandbox::new("edit-hardlink");
    let o = s.write_outside("o.txt", "the outside line is ODINSECRET-OUTSIDE\n");
    if std::fs::hard_link(&o, s.project().join("innocent.txt")).is_err() {
        eprintln!("could not create a hard link here; skipping");
        return;
    }
    let ws = s.ws();
    let out = edit::plan_replace(&ws, "innocent.txt", "the outside line is wrong", "x");
    eprintln!("{out:?}");
    assert_eq!(
        out.as_ref().err().map(String::as_str),
        Some(
            "old_text does not match exactly one place in innocent.txt. It is a protected file, so its text is \
             not shown; ask the user to allow a read of it first."
        ),
        "{out:?}"
    );
    assert!(!format!("{out:?}").contains("ODINSECRET-OUTSIDE"), "{out:?}");
}

// ═══ 3b. The approval table ════════════════════════════════════════════════

#[test]
fn holds_every_change_and_every_check_asks_whatever_the_switch_or_the_folder() {
    let s = Sandbox::new("table");
    s.write("a.rs", "x\n");
    let ws = s.ws();
    let reqs = vec![
        Request::Edit { path: "a.rs".into(), old_text: "x".into(), new_text: "y".into() },
        Request::Edit { path: "../outside.txt".into(), old_text: "x".into(), new_text: "y".into() },
        Request::Create { path: "b.rs".into(), content: "x".into() },
        Request::Create { path: "a/b/c/d.rs".into(), content: "x".into() },
        Request::Delete { path: "a.rs".into() },
        Request::Move { path: "a.rs".into(), to: "b.rs".into() },
        Request::Check,
        Request::Rewrite { path: "a.rs".into(), content: "y".into() },
    ];
    for req in &reqs {
        for ask_every in [false, true] {
            assert_eq!(tools::approval(Some(&ws), req, ask_every), Approval::Ask, "{req:?} ask_every={ask_every}");
        }
        assert_eq!(tools::approval(None, req, false), Approval::Ask, "no folder: {req:?}");
    }
    // A task list touches nothing and never asks; reads, lists and searches ask with the switch on.
    let plan = Request::Plan { steps: tools::parse_plan("[ ] a") };
    assert!(matches!(tools::approval(Some(&ws), &plan, true), Approval::Auto(Auto::Plan { .. })));
    for req in [read_req("a.rs"), Request::List { pattern: None }, Request::Search { text: "x".into(), pattern: None }] {
        assert_eq!(tools::approval(Some(&ws), &req, true), Approval::Ask, "{req:?}");
    }
}

// ═══ 4. Broad roots and paths on other machines ════════════════════════════

#[test]
fn holds_broad_roots_ask_for_reads_listings_and_searches() {
    init();
    let profile = home();
    let ancestor = base();
    let drive_root = PathBuf::from("C:/");
    for root in [profile.clone(), ancestor, drive_root] {
        let ws = Workspace::open(&root).unwrap();
        let reqs = vec![
            read_req("anything.txt"),
            read_req("C:/Windows/win.ini"),
            Request::List { pattern: None },
            Request::Search { text: "x".into(), pattern: None },
        ];
        for r in reqs {
            assert_eq!(tools::approval(Some(&ws), &r, false), Approval::Ask, "{root:?}: {r:?}");
        }
    }
    // Control: a folder inside the profile is an ordinary project.
    let proj = profile.join("proj-review");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(proj.join("a.txt"), "hello\n").unwrap();
    let ws = Workspace::open(&proj).unwrap();
    let got = decide(&ws, "a.txt");
    let _ = std::fs::remove_dir_all(&proj);
    assert!(matches!(got, Out::Ran(t) if t.contains("hello")), "control");
}

#[test]
fn holds_broad_root_table() {
    let p = Some(Path::new("C:/Users/odin-test-user"));
    for broad in ["C:/", "D:/", "C:/Users", "C:/Users/odin-test-user", "c:/users/ODIN-TEST-USER"] {
        assert!(tools::broad_root(Path::new(broad), p), "{broad}");
    }
    for fine in ["C:/work", "C:/Users/odin-test-user/proj", "C:/Users/Public", "C:/Windows"] {
        assert!(!tools::broad_root(Path::new(fine), p), "{fine}");
    }
    // No profile known: only a drive root is broad.
    assert!(tools::broad_root(Path::new("C:/"), None));
    assert!(!tools::broad_root(Path::new("C:/Users/odin-test-user"), None));
}

/// Folders inside the profile that hold other programs' secrets are not "broad". Recorded, not asserted.
#[test]
fn note_profile_subfolders_such_as_appdata_and_dot_ssh_are_ordinary_projects() {
    let p = Some(Path::new("C:/Users/odin-test-user"));
    for sub in ["AppData", "AppData/Roaming", ".ssh", ".aws", "Documents", "OneDrive"] {
        let root = format!("C:/Users/odin-test-user/{sub}");
        eprintln!("broad_root({root}) = {}", tools::broad_root(Path::new(&root), p));
    }
}

/// Run `f` on another thread and give up after `secs`: a call that resolves a path on another
/// machine blocks on an SMB connection, which is how the leak would show.
fn within<T: Send + 'static>(secs: u64, f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(Duration::from_secs(secs)).ok()
}

#[test]
fn holds_paths_on_other_machines_are_never_resolved_before_approval() {
    let s = Sandbox::new("remote");
    s.write("a.txt", "x\n");
    let ws = s.ws();
    // 192.0.2.0/24 is reserved for documentation: nothing answers, so a connection attempt would hang.
    let mut forms: Vec<String> = Vec::new();
    for h in ["192.0.2.55", "odin-review-no-such-host"] {
        for tail in ["share/x.txt", "share/sub/..", "ipc$/x", "share/"] {
            forms.push(w(&format!("//{h}/{tail}")));
            forms.push(format!("//{h}/{tail}"));
            forms.push(w(&format!("//?/UNC/{h}/{tail}")));
            forms.push(w(&format!("//./UNC/{h}/{tail}")));
        }
        forms.push(w(&format!("//{h}@SSL/DavWWWRoot/x.txt")));
        forms.push(w(&format!("//{h}/share")));
        forms.push(w(&format!("//{h}")));
    }
    let mut slow = Vec::new();
    for f in forms {
        let (ws2, f2) = (ws.clone(), f.clone());
        let t0 = Instant::now();
        let r = within(8, move || {
            let located = format!("{:?}", ws2.locate(&f2));
            let shown = tools::read_target(&f2, Some(&ws2)).1;
            let approval = tools::approval(Some(&ws2), &read_req(&f2), false);
            let errs = [
                ws2.resolve_existing(&f2).is_err(),
                ws2.resolve_target(&f2, "").is_err(),
                edit::plan_create(&ws2, &f2, "x").is_err(),
                edit::plan_delete(&ws2, &f2).is_err(),
                edit::plan_move(&ws2, &f2, "moved.txt").is_err(),
                edit::plan_move(&ws2, "a.txt", &f2).is_err(),
                edit::plan_replace(&ws2, &f2, "a", "b").is_err(),
            ];
            (located, shown, approval, errs)
        });
        let took = t0.elapsed();
        eprintln!("{f:?}: {took:?}");
        match r {
            None => slow.push(format!("{f} blocked for more than 8 s")),
            Some((located, _shown, approval, errs)) => {
                if matches!(approval, Approval::Auto(_)) {
                    slow.push(format!("{f}: approval was automatic ({located})"));
                }
                if errs.iter().any(|e| !e) {
                    slow.push(format!("{f}: an edit-side function accepted it: {errs:?}"));
                }
            }
        }
    }
    assert!(slow.is_empty(), "{slow:#?}");
}

// ═══ 5. Links ══════════════════════════════════════════════════════════════

#[test]
fn holds_a_junction_pointing_out_is_never_followed() {
    let s = Sandbox::new("junction-out");
    let bs = '\u{5c}';
    s.write("src/main.rs", "fn main() {}\n");
    s.write("sub/placeholder.txt", "x\n");
    s.write_outside("secret.txt", &format!("{MARK}\n"));
    s.write_outside("deep/er/secret2.txt", &format!("{MARK}\n"));
    if !s.junction("link", &s.outside()) {
        return;
    }
    let ws = s.ws();
    let mut sp: Vec<String> = Vec::new();
    for rel in ["link/secret.txt", "link/deep/er/secret2.txt", "link/../link/secret.txt"] {
        sp.extend(rel_variants(rel));
        sp.push(w(rel));
    }
    let abs = format!("{}{bs}link{bs}secret.txt", ws.root_display());
    sp.extend(abs_variants(&abs));
    matrix(&ws, "through a junction that points out", &sp, MARK);
    assert!(matches!(decide(&ws, "link/secret.txt"), Out::Ask));
    assert!(matches!(decide(&ws, "link/missing.txt"), Out::Ask), "missing, under the same link: still outside");
    // Not listed, not searched, not reachable by pattern.
    let l = search::list(&ws, None, || false).unwrap();
    assert!(l.contains("src/main.rs") && !l.contains("secret") && !l.contains("link"), "{l}");
    for p in ["link", "link/**", "link/*", "**/secret.txt", "**/secret2.txt"] {
        let l = search::list(&ws, Some(p), || false).unwrap();
        assert!(l.starts_with("No files match"), "{p}: {l}");
    }
    let r = search::search(&ws, MARK, None, || false).unwrap();
    assert!(r.starts_with("No matches"), "{r}");
}

#[test]
fn note_junctions_that_point_back_inside_or_loop() {
    let s = Sandbox::new("junction-in");
    s.write("src/main.rs", &format!("inside {MARK}\n"));
    s.write(".gitignore", "hidden_dir/\n");
    s.write("hidden_dir/x.txt", &format!("{MARK}\n"));
    if !s.junction("alias", &s.project().join("src")) {
        return;
    }
    assert!(s.junction("alias_hidden", &s.project().join("hidden_dir")));
    assert!(s.junction("loop", &s.project()));
    let ws = s.ws();
    for p in ["alias/main.rs", "alias_hidden/x.txt", "loop/loop/loop/src/main.rs", "loop/hidden_dir/x.txt"] {
        eprintln!("{p} -> {}", brief(&decide(&ws, p)));
    }
    // What is ignored stays ignored when reached by another name.
    for p in ["alias_hidden/x.txt", "loop/hidden_dir/x.txt"] {
        assert!(!matches!(decide(&ws, p), Out::Ran(_)), "{p} ran unasked");
    }
    // The listing and the search never go through any of them, and they end.
    let t0 = Instant::now();
    let l = search::list(&ws, None, || false).unwrap();
    let r = search::search(&ws, MARK, None, || false).unwrap();
    assert!(t0.elapsed() < Duration::from_secs(10), "a loop made the walk slow");
    assert!(!l.contains("alias") && !l.contains("loop") && !l.contains("hidden_dir"), "{l}");
    assert!(r.starts_with("1 match") && r.contains("src/main.rs:1:"), "{r}");
}

// Updated in review round 2: approval now asks up front for a hard-linked file (R-01: one shared protection test, `tools::protected`, which runs the checked open), instead of "automatic, then the open refuses".
#[test]
fn holds_a_hard_link_to_an_outside_file_is_never_read_or_searched_unasked() {
    let s = Sandbox::new("hardlink");
    let o = s.write_outside("o.txt", &format!("{MARK}\n"));
    s.write("src/main.rs", "fn main() {}\n");
    if std::fs::hard_link(&o, s.project().join("innocent.txt")).is_err() {
        eprintln!("could not create a hard link here; skipping");
        return;
    }
    let ws = s.ws();
    // Its name is ordinary, but `tools::protected` runs the checked open, so approval asks up front.
    assert!(matches!(decide(&ws, "innocent.txt"), Out::Ask), "{}", brief(&decide(&ws, "innocent.txt")));
    let r = search::search(&ws, MARK, None, || false).unwrap();
    assert!(r.starts_with("No matches") && r.contains("second name"), "{r}");
    // Names only: the listing shows it.
    assert!(search::list(&ws, None, || false).unwrap().contains("innocent.txt"));
}

/// The decision and the open are two steps. Swapping the file for a link in between must not read it.
#[test]
fn holds_a_swap_between_the_decision_and_the_open_is_caught() {
    let s = Sandbox::new("toctou");
    let o = s.write_outside("o.txt", &format!("{MARK}\n"));
    s.write("a.txt", "fine\n");
    s.write("d/f.txt", "fine\n");
    let ws = s.ws();
    let Approval::Auto(Auto::Read { real, range }) = tools::approval(Some(&ws), &read_req("a.txt"), false) else {
        panic!("a.txt should be automatic");
    };
    assert_eq!(tools::read_unasked(&real, range).as_deref(), Some("fine\n"));
    std::fs::remove_file(&real).unwrap();
    if std::fs::hard_link(&o, &real).is_err() {
        eprintln!("could not create a hard link here; skipping");
        return;
    }
    assert_eq!(tools::read_unasked(&real, range), None, "a hard link swapped in was read");
    // The folder swapped for a junction after the decision.
    let Approval::Auto(Auto::Read { real, range }) = tools::approval(Some(&ws), &read_req("d/f.txt"), false) else {
        panic!("d/f.txt should be automatic");
    };
    s.write_outside("f.txt", &format!("{MARK}\n"));
    std::fs::remove_dir_all(s.project().join("d")).unwrap();
    if !s.junction("d", &s.outside()) {
        return;
    }
    assert_eq!(tools::read_unasked(&real, range), None, "a junction swapped in was read");
}

// ═══ 6. What counts as ignored ═════════════════════════════════════════════

/// Git itself hides these (checked with `git check-ignore -v` on this machine, core.ignorecase=true).
#[test]
fn defect_git_info_exclude_is_not_honoured() {
    let s = Sandbox::new("info-exclude");
    s.write(".git/info/exclude", "local-only.txt\nprivate_dir/\n");
    s.write("local-only.txt", &format!("{MARK}\n"));
    s.write("private_dir/a.txt", &format!("{MARK}\n"));
    s.write("src/lib.rs", "x\n");
    let ws = s.ws();
    let found = search::search(&ws, MARK, None, || false).unwrap();
    let listed = search::list(&ws, None, || false).unwrap();
    let a = decide(&ws, "local-only.txt");
    let b = decide(&ws, "private_dir/a.txt");
    eprintln!("read: {} / {}\nlisted: {listed}\nsearched: {found}", brief(&a), brief(&b));
    assert!(!matches!(a, Out::Ran(_)) && !matches!(b, Out::Ran(_)), "files in .git/info/exclude ran unasked");
    assert!(found.starts_with("No matches"), "a search read files in .git/info/exclude: {found}");
}

#[test]
fn defect_a_gitignore_above_the_project_folder_is_not_honoured() {
    let s = Sandbox::new("parent-ignore");
    // The repo is the sandbox folder; the project is its `project` subfolder.
    std::fs::write(s.dir.join(".gitignore"), "private.txt\n*.secret\n").unwrap();
    s.write("private.txt", &format!("{MARK}\n"));
    s.write("conf/db.secret", &format!("{MARK}\n"));
    s.write("src/lib.rs", "x\n");
    let ws = s.ws();
    let a = decide(&ws, "private.txt");
    let b = decide(&ws, "conf/db.secret");
    let found = search::search(&ws, MARK, None, || false).unwrap();
    eprintln!("read: {} / {}\nsearched: {found}", brief(&a), brief(&b));
    assert!(!matches!(a, Out::Ran(_)) && !matches!(b, Out::Ran(_)), "files the repo's .gitignore hides ran unasked");
    assert!(found.starts_with("No matches"), "{found}");
}

#[test]
fn defect_gitignore_patterns_are_case_sensitive_but_git_on_windows_is_not() {
    let s = Sandbox::new("case-ignore");
    s.write(".gitignore", "Private.TXT\nSecrets/\n");
    s.write("private.txt", &format!("{MARK}\n"));
    s.write("secrets/a.txt", &format!("{MARK}\n"));
    s.write("src/lib.rs", "x\n");
    let ws = s.ws();
    let a = decide(&ws, "private.txt");
    let b = decide(&ws, "secrets/a.txt");
    let found = search::search(&ws, MARK, None, || false).unwrap();
    eprintln!("read: {} / {}\nsearched: {found}", brief(&a), brief(&b));
    assert!(!matches!(a, Out::Ran(_)) && !matches!(b, Out::Ran(_)), "case-different ignore patterns were not applied");
    assert!(found.starts_with("No matches"), "{found}");
}

#[test]
fn holds_dot_ignore_files_and_negations_and_ignored_directories() {
    let s = Sandbox::new("ignore-rules");
    s.write(".ignore", "from_dot_ignore.txt\n");
    s.write("from_dot_ignore.txt", &format!("{MARK}\n"));
    s.write(".gitignore", "logs/\n!logs/keep.txt\n*.tmp\n!/allowed.tmp\n");
    s.write("logs/keep.txt", &format!("{MARK}\n"));
    s.write("logs/other.txt", &format!("{MARK}\n"));
    s.write("a.tmp", &format!("{MARK}\n"));
    s.write("allowed.tmp", "allowed\n");
    s.write("deep/er/.gitignore", "only_here.txt\n");
    s.write("deep/er/only_here.txt", &format!("{MARK}\n"));
    s.write("deep/only_here.txt", "elsewhere\n");
    let ws = s.ws();
    for p in ["from_dot_ignore.txt", "logs/keep.txt", "logs/other.txt", "a.tmp", "deep/er/only_here.txt"] {
        assert!(!matches!(decide(&ws, p), Out::Ran(_)), "{p} should ask");
    }
    // A negation cannot re-include a file in an ignored directory, as in git; `allowed.tmp` can.
    assert!(matches!(decide(&ws, "allowed.tmp"), Out::Ran(_)));
    assert!(matches!(decide(&ws, "deep/only_here.txt"), Out::Ran(_)), "a nested rule is scoped to its folder");
    let found = search::search(&ws, MARK, None, || false).unwrap();
    assert!(found.starts_with("No matches"), "{found}");
}

// ═══ 7. Secret names and search ════════════════════════════════════════════

#[test]
fn holds_the_secret_names_the_docs_list() {
    for name in [
        ".env", ".ENV", ".env.local", ".env.production", "prod.env", ".envrc", ".npmrc", ".netrc", "_netrc",
        ".pypirc", ".pgpass", ".git-credentials", ".htpasswd", ".dockercfg", ".bash_history", ".my.cnf", ".boto",
        ".s3cfg", "credentials", "credentials.json", "secrets.json", "service-account.json", "service-account-prod.json",
        "id_rsa", "id_rsa.pub", "id_dsa", "id_ecdsa", "id_ed25519", "x.tfstate.backup", "a.pem", "a.KEY", "a.pfx",
        "a.p12", "a.p8", "a.ppk", "a.kdbx", "a.jks", "a.keystore", "a.tfstate", "a.tfvars", "a.gpg", "a.pgp", "a.asc",
    ] {
        assert!(search::secret_name(Path::new(name)), "{name}");
    }
    for name in ["C:/p/.kube/config", "C:/p/.aws/config", "C:/p/.docker/config.json", "C:/p/.azure/accessTokens.json"] {
        assert!(search::secret_name(Path::new(name)), "{name}");
    }
}

/// H5-results.md lists `secrets.*`; the code names four files (`secrets.json|yaml|yml|toml`) and
/// the extension rule. Recorded, with the names a reader of the doc would expect to be covered.
#[test]
fn note_secrets_star_in_the_docs_is_four_names_in_the_code() {
    let mut open = Vec::new();
    for name in ["secrets.txt", "secrets.ini", "secrets.conf", "secrets.cfg", "secrets.properties", "secrets.xml", "secrets.py", "secrets.rs", "secrets"] {
        if !search::secret_name(Path::new(name)) {
            open.push(name);
        }
    }
    eprintln!("not secret by name, though the docs say secrets.*: {open:?}");
    assert!(!open.is_empty());
}

/// Names that plausibly hold secrets and that the list does not cover. Recorded, not asserted.
#[test]
fn note_plausible_secret_files_the_name_list_does_not_cover() {
    let mut open = Vec::new();
    for name in [
        ".env-production", ".env_local", "env.local", ".dev.vars", "local.settings.json", "wp-config.php", "kubeconfig",
        "admin.conf", ".terraformrc", "terraform.rc", "hosts.yml", "application_default_credentials.json", ".vault_pass",
        ".mylogin.cnf", "client.ovpn", "vars.tfvars.json", "master.key.enc", "settings.local.json",
    ] {
        if !search::secret_name(Path::new(name)) {
            open.push(name);
        }
    }
    eprintln!("not covered: {open:?}");
}

#[test]
fn holds_search_never_returns_lines_from_secret_ignored_or_linked_files() {
    let s = Sandbox::new("search-leak");
    s.write("src/lib.rs", &format!("// {MARK} visible\n"));
    s.write(".gitignore", "ignored.txt\nbuild_out/\n");
    s.write("nested/.gitignore", "*.cache\n");
    for rel in [
        ".env", ".env.production", "prod.env", "keys/server.pem", "keys/id_rsa", ".npmrc", "credentials", "secrets.json",
        "ignored.txt", "build_out/x.txt", "target/debug/a.txt", "node_modules/m/i.js", ".git/config", "nested/a.cache",
    ] {
        s.write(rel, &format!("{MARK}\n"));
    }
    let o = s.write_outside("o.txt", &format!("{MARK}\n"));
    s.write_outside("viaj.txt", &format!("{MARK}\n"));
    let linked = std::fs::hard_link(&o, s.project().join("linked.txt")).is_ok();
    let jn = s.junction("link", &s.outside());
    let ws = s.ws();
    let protected = [
        ".env:", ".env.production:", "prod.env:", "server.pem:", "id_rsa:", ".npmrc:", "credentials:", "secrets.json:",
        "ignored.txt:", "build_out/", "target/", "node_modules/", ".git/", "a.cache:", "linked.txt:", "link/", "viaj.txt",
    ];
    let patterns: Vec<Option<&str>> = vec![
        None, Some("**"), Some("*"), Some(".*"), Some("*.txt"), Some("keys"), Some("keys/**"), Some(".env"), Some("*.pem"),
        Some("id_rsa"), Some("**/*"), Some("build_out"), Some("target"), Some("node_modules"), Some(".git"),
        Some("{.env,id_rsa,*.pem}"), Some("link"), Some("linked.txt"), Some("secrets.json"), Some("*.cache"),
    ];
    for pattern in patterns {
        let out = search::search(&ws, MARK, pattern, || false).unwrap();
        for p in protected {
            assert!(!out.contains(p), "pattern {pattern:?} returned something from {p}:\n{out}");
        }
    }
    let all = search::search(&ws, MARK, None, || false).unwrap();
    assert!(all.starts_with("1 match") && all.contains("src/lib.rs:1:"), "control:\n{all}");
    eprintln!("hard link made: {linked}, junction made: {jn}");
}

// ═══ 8. H9 and H1: new paths and the files that decide what a check runs ═══

/// Feed every spelling to `f` on a worker thread (a device name must not hang the run); return
/// the ones `f` accepted. Prints the table.
fn accepted_by(what: &str, spellings: &[String], f: impl Fn(&str) -> Result<String, String> + Send + Sync + 'static) -> Vec<String> {
    let f = std::sync::Arc::new(f);
    let mut table = String::new();
    let mut accepted = Vec::new();
    for s in spellings {
        let (f2, s2) = (f.clone(), s.clone());
        match within(15, move || f2(&s2)) {
            None => {
                table.push_str(&format!("  {s:?} -> HUNG\n"));
                accepted.push(format!("{s} (hung)"));
            }
            Some(Ok(shown)) => {
                table.push_str(&format!("  {s:?} -> ACCEPTED ({shown})\n"));
                accepted.push(s.clone());
            }
            Some(Err(e)) => table.push_str(&format!("  {s:?} -> refused: {e}\n")),
        }
    }
    eprintln!("== {what} ==\n{table}");
    accepted
}

/// A project with the folders and files that must never be written, each holding the same marker.
fn protected_project(tag: &str) -> Sandbox {
    let s = Sandbox::new(tag);
    for rel in [".git/config", ".git/hooks/keep", ".cargo/config.toml", ".npmrc", "rust-toolchain.toml", "sub/.npmrc"] {
        s.write(rel, "PROTECTED-FILE\n");
    }
    s.write("a.txt", "x\n");
    s.write("sub/b.txt", "x\n");
    s
}

#[test]
fn holds_existing_files_that_decide_what_a_check_runs_are_refused_in_every_spelling() {
    let s = protected_project("prot-existing");
    let ws = s.ws();
    let mut all: Vec<String> = Vec::new();
    for rel in [".git/config", ".cargo/config.toml", ".npmrc", "rust-toolchain.toml", "sub/.npmrc"] {
        all.extend(all_variants(&ws, rel));
    }
    let wsa = ws.clone();
    let a = accepted_by("resolve_existing", &all, move |p| wsa.resolve_existing(p).map(|r| edit::plain(&r)));
    let wsb = ws.clone();
    let b = accepted_by("plan_replace", &all, move |p| {
        edit::plan_replace(&wsb, p, "PROTECTED-FILE", "changed").map(|pl| pl.display)
    });
    let wsc = ws.clone();
    let c = accepted_by("plan_delete", &all, move |p| edit::plan_delete(&wsc, p).map(|pl| pl.display));
    let wsd = ws.clone();
    let d = accepted_by("plan_move from", &all, move |p| edit::plan_move(&wsd, p, "moved-away.txt").map(|pl| pl.display));
    assert!(a.is_empty() && b.is_empty() && c.is_empty() && d.is_empty(), "accepted: {a:?} {b:?} {c:?} {d:?}");
}

#[test]
fn holds_new_files_in_protected_places_are_refused_in_every_spelling() {
    let s = protected_project("prot-new");
    let ws = s.ws();
    let bs = '\u{5c}';
    let mut all: Vec<String> = Vec::new();
    for rel in [
        ".git/hooks/pre-commit", ".git/new", ".GIT/new", "sub/.git/new", ".cargo/config", ".cargo/new.toml",
        ".CARGO/Config.TOML", "sub/.cargo/x", "rust-toolchain", "RUST-TOOLCHAIN.TOML", "sub/rust-toolchain", ".NPMRC",
        "sub/.npmrc2/../.npmrc", ".npmrc.", ".npmrc::$DATA", ".npmrc:x", ".cargo::$INDEX_ALLOCATION/config.toml",
        ".git::$INDEX_ALLOCATION/new",
    ] {
        all.push(rel.to_string());
        all.push(w(rel));
        let long = format!("{}{bs}{}", ws.root_display(), w(rel));
        all.extend(abs_variants(&long).into_iter().take(12));
    }
    // The 8.3 aliases of the folders that exist.
    for (dir, rest) in [(".git", "hooks/new"), (".git", "new"), (".cargo", "config.toml"), (".cargo", "deeper/x.toml")] {
        if let Some(alias) = short(&s.project().join(dir)) {
            all.push(format!("{alias}{bs}{}", w(rest)));
            if let Some(tail) = alias.rsplit(bs).next() {
                all.push(format!("{tail}/{rest}"));
            }
        }
    }
    let wsa = ws.clone();
    let a = accepted_by("resolve_target", &all, move |p| wsa.resolve_target(p, "").map(|t| edit::plain(&t.path)));
    let wsb = ws.clone();
    let b = accepted_by("plan_create", &all, move |p| edit::plan_create(&wsb, p, "x").map(|pl| pl.display));
    let wsc = ws.clone();
    let c = accepted_by("plan_move to", &all, move |p| edit::plan_move(&wsc, "a.txt", p).map(|pl| pl.display));
    assert!(a.is_empty() && b.is_empty() && c.is_empty(), "accepted: {a:?} {b:?} {c:?}");
}

#[test]
fn holds_device_names_are_refused_everywhere_and_never_hang() {
    let s = Sandbox::new("devices");
    s.write("a.txt", "x\n");
    let ws = s.ws();
    let bs = '\u{5c}';
    let root = ws.root_display();
    let mut all: Vec<String> = Vec::new();
    for name in ["NUL", "CON", "COM1", "COM1.txt", "LPT1", "AUX", "PRN", "con", "Nul.txt", "CONIN$", "CONOUT$", "sub/NUL", "NUL/x.txt", "x/CON/y.txt"] {
        all.push(name.to_string());
        all.push(format!("{root}{bs}{}", w(name)));
        all.push(w(&format!("//?/{root}/{name}")));
        all.push(w(&format!("//./{name}")));
    }
    let wsa = ws.clone();
    let a = accepted_by("resolve_existing", &all, move |p| wsa.resolve_existing(p).map(|r| edit::plain(&r)));
    let wsb = ws.clone();
    let b = accepted_by("resolve_target", &all, move |p| wsb.resolve_target(p, "").map(|t| edit::plain(&t.path)));
    let wsc = ws.clone();
    let c = accepted_by("plan_delete", &all, move |p| edit::plan_delete(&wsc, p).map(|pl| pl.display));
    let wsd = ws.clone();
    let d = accepted_by("plan_replace", &all, move |p| edit::plan_replace(&wsd, p, "a", "b").map(|pl| pl.display));
    let wse = ws.clone();
    let e = accepted_by("plan_move from", &all, move |p| edit::plan_move(&wse, p, "gone.txt").map(|pl| pl.display));
    let wsf = ws.clone();
    let f = accepted_by("approval read", &all, move |p| match tools::approval(Some(&wsf), &read_req(p), false) {
        Approval::Auto(a) => Ok(format!("{a:?}")),
        _ => Err("not automatic".into()),
    });
    assert!(a.is_empty() && b.is_empty() && c.is_empty() && d.is_empty() && e.is_empty() && f.is_empty(),
        "accepted: {a:?} {b:?} {c:?} {d:?} {e:?} {f:?}");
}


/// Windows lists COM and LPT with a superscript digit as reserved too. Recorded, not asserted.
#[test]
fn note_superscript_device_names_and_clock_are_not_refused() {
    let s = Sandbox::new("names-more");
    let ws = s.ws();
    for name in ["COM\u{b9}", "COM\u{b2}.txt", "LPT\u{b3}", "CLOCK$", "COM0", "LPT0.txt"] {
        eprintln!("{name:?} -> accepted: {}", ws.resolve_target(name, "").is_ok());
    }
}

#[test]
fn note_dot_dot_in_the_part_that_does_not_exist_yet() {
    let s = Sandbox::new("dotdot");
    s.write("sub/placeholder.txt", "x\n");
    let ws = s.ws();
    let bs = '\u{5c}';
    let root = ws.root_display();
    let cases = [
        "nope/../x.txt".to_string(),
        w("nope/../x.txt"),
        "nope/../../x.txt".to_string(),
        "sub/../x.txt".to_string(),
        "a/b/../../c.txt".to_string(),
        format!("{root}{bs}nope{bs}..{bs}x.txt"),
        format!("{root}{bs}sub{bs}..{bs}x.txt"),
        w(&format!("//?/{root}/nope/../x.txt")),
    ];
    for c in cases {
        eprintln!("{c:?} -> {:?}", ws.resolve_target(&c, "").map(|t| edit::plain(&t.path)));
    }
    // Whatever the spelling, nothing may land outside.
    for c in ["nope/../../x.txt", "sub/../../x.txt", "../x.txt"] {
        assert!(ws.resolve_target(c, "").is_err(), "{c}");
    }
}

#[test]
fn holds_the_cap_on_new_folders() {
    let s = Sandbox::new("cap");
    s.write("exists/a.txt", "x\n");
    let ws = s.ws();
    assert_eq!(ws.resolve_target("a/b/c/d/e/f/g/h/x.rs", "").unwrap().new_dirs.len(), 8);
    assert!(ws.resolve_target("a/b/c/d/e/f/g/h/i/x.rs", "").is_err());
    assert_eq!(ws.resolve_target("exists/a/b/c/d/e/f/g/h/x.rs", "").unwrap().new_dirs.len(), 8);
    assert!(ws.resolve_target("exists/a/b/c/d/e/f/g/h/i/x.rs", "").is_err());
    assert!(edit::plan_create(&ws, "a/b/c/d/e/f/g/h/i/x.rs", "x").is_err());
    assert!(edit::plan_move(&ws, "exists/a.txt", "a/b/c/d/e/f/g/h/i/x.rs").is_err());
}

#[test]
fn holds_move_and_delete_refuse_links_read_only_case_only_and_outside() {
    let s = Sandbox::new("move-delete");
    s.write("a.txt", "a\n");
    s.write("sub/b.txt", "b\n");
    s.write("a-rather-long-file-name.txt", "long\n");
    let ro = s.write("readonly.txt", "ro\n");
    let o = s.write_outside("o.txt", "outside\n");
    let mut perm = std::fs::metadata(&ro).unwrap().permissions();
    perm.set_readonly(true);
    std::fs::set_permissions(&ro, perm).unwrap();
    let jin = s.junction("jin", &s.project().join("sub"));
    let jout = s.junction("jout", &s.outside());
    let ws = s.ws();
    let refused = |what: &str, r: Result<edit::Plan, String>| {
        eprintln!("{what}: {:?}", r.as_ref().map(|p| p.display.clone()));
        assert!(r.is_err(), "{what} was accepted");
    };
    refused("delete a folder", edit::plan_delete(&ws, "sub"));
    refused("delete outside", edit::plan_delete(&ws, "../outside/o.txt"));
    refused("delete outside, absolute", edit::plan_delete(&ws, &edit::plain(&o)));
    refused("delete read-only", edit::plan_delete(&ws, "readonly.txt"));
    refused("move read-only", edit::plan_move(&ws, "readonly.txt", "x.txt"));
    refused("move from outside", edit::plan_move(&ws, "../outside/o.txt", "o.txt"));
    refused("move to outside", edit::plan_move(&ws, "a.txt", "../outside/a.txt"));
    refused("case-only rename", edit::plan_move(&ws, "a.txt", "A.TXT"));
    refused("rename onto itself by dots", edit::plan_move(&ws, "a.txt", "sub/../a.txt"));
    refused("onto an existing file", edit::plan_move(&ws, "a.txt", "sub/b.txt"));
    refused("onto an existing folder", edit::plan_move(&ws, "a.txt", "sub"));
    refused("a folder as the source", edit::plan_move(&ws, "sub", "sub2"));
    if jin {
        refused("delete through a link that points back in", edit::plan_delete(&ws, "jin/b.txt"));
        refused("move through a link that points back in", edit::plan_move(&ws, "jin/b.txt", "c.txt"));
        refused("move onto a path through a link", edit::plan_move(&ws, "a.txt", "jin/b.txt"));
    }
    if jout {
        refused("delete through a link that points out", edit::plan_delete(&ws, "jout/o.txt"));
        refused("move through a link that points out", edit::plan_move(&ws, "jout/o.txt", "o.txt"));
        refused("move into a link that points out", edit::plan_move(&ws, "a.txt", "jout/new.txt"));
        refused("create through a link that points out", edit::plan_create(&ws, "jout/new/x.txt", "x"));
    }
    // The 8.3 alias of a real file is another name for it: refused as "reached through a link".
    if let Some(alias) = short(&s.project().join("a-rather-long-file-name.txt")) {
        let name = alias.rsplit('\u{5c}').next().unwrap().to_string();
        eprintln!("alias {name}");
        refused("delete by 8.3 alias", edit::plan_delete(&ws, &name));
        refused("move by 8.3 alias", edit::plan_move(&ws, &name, "z.txt"));
    }
    // Controls: the plain operations plan.
    assert!(edit::plan_delete(&ws, "a.txt").is_ok());
    assert!(edit::plan_move(&ws, "a.txt", "fresh/dir/a.txt").is_ok());
    let mut perm = std::fs::metadata(&ro).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perm.set_readonly(false);
    std::fs::set_permissions(&ro, perm).unwrap();
}

/// A hard-linked file is a second name for one file. Deleting or moving one name is not refused;
/// recorded so the report can say what happens.
#[test]
fn note_delete_and_move_of_a_hard_linked_name() {
    let s = Sandbox::new("move-hardlink");
    let o = s.write_outside("o.txt", "outside\n");
    if std::fs::hard_link(&o, s.project().join("twin.txt")).is_err() {
        return;
    }
    let ws = s.ws();
    eprintln!("delete twin: {:?}", edit::plan_delete(&ws, "twin.txt").map(|p| p.display));
    eprintln!("move twin: {:?}", edit::plan_move(&ws, "twin.txt", "moved.txt").map(|p| p.display));
}

#[test]
fn holds_mimirs_own_folder_is_never_written_when_the_project_contains_it() {
    init();
    let ws = Workspace::open(home()).unwrap();
    let appdata = PathBuf::from(std::env::var_os("APPDATA").unwrap());
    let odin = appdata.join("odin");
    std::fs::write(odin.join("settings-probe.json"), "PROTECTED-FILE\n").unwrap();
    std::fs::write(home().join("probe-source.txt"), "x\n").unwrap();
    let bs = '\u{5c}';
    let long = format!("{}{bs}AppData{bs}Roaming{bs}odin{bs}settings-probe.json", ws.root_display());
    let mut all: Vec<String> = vec![
        "AppData/Roaming/odin/settings-probe.json".into(),
        "appdata/ROAMING/Odin/SETTINGS-PROBE.JSON".into(),
        "AppData/Roaming/odin/new.json".into(),
        "AppData/Roaming/odin/backups/new/deeper.json".into(),
        "AppData/Roaming/odin/../odin/new.json".into(),
        "AppData/Roaming/odin/settings-probe.json::$DATA".into(),
    ];
    all.extend(abs_variants(&long).into_iter().take(10));
    if let Some(a) = short(&home().join("AppData")) {
        all.push(format!("{a}{bs}Roaming{bs}odin{bs}settings-probe.json"));
        all.push(format!("{a}{bs}Roaming{bs}odin{bs}new.json"));
    }
    let wsa = ws.clone();
    let a = accepted_by("create in app data", &all, move |p| edit::plan_create(&wsa, p, "x").map(|pl| pl.display));
    let wsb = ws.clone();
    let b = accepted_by("edit in app data", &all, move |p| edit::plan_replace(&wsb, p, "PROTECTED-FILE", "x").map(|pl| pl.display));
    let wsc = ws.clone();
    let c = accepted_by("delete in app data", &all, move |p| edit::plan_delete(&wsc, p).map(|pl| pl.display));
    let wsd = ws.clone();
    let d = accepted_by("move into app data", &all, move |p| {
        edit::plan_move(&wsd, "probe-source.txt", p).map(|pl| pl.display)
    });
    let _ = std::fs::remove_file(odin.join("settings-probe.json"));
    let _ = std::fs::remove_file(home().join("probe-source.txt"));
    assert!(a.is_empty() && b.is_empty() && c.is_empty() && d.is_empty(), "accepted: {a:?} {b:?} {c:?} {d:?}");
}

// ═══ 9. H10: the project's notes file ══════════════════════════════════════

fn put(s: &Sandbox, rel: &str, bytes: &[u8]) {
    let p = s.project().join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, bytes).unwrap();
}


/// Exactly at the edge of the cap.
#[test]
fn note_the_edge_of_the_notes_cap() {
    let cap = instructions::MAX_NOTES_BYTES;
    for (what, bytes) in [
        ("exactly the cap, no newline", "a".repeat(cap).into_bytes()),
        ("cap bytes and a final newline", format!("{}\n", "a".repeat(cap)).into_bytes()),
        ("two lines, the second ending exactly on the cap, then a newline", format!("{}\n{}\n", "a".repeat(10), "b".repeat(cap - 12)).into_bytes()),
    ] {
        let s = Sandbox::new("notes-edge");
        put(&s, "AGENTS.md", &bytes);
        let n = instructions::find(&s.ws());
        eprintln!("{what}: cut={:?} kept={:?}", n.as_ref().map(|n| n.cut), n.as_ref().map(|n| n.text.len()));
    }
}

#[test]
fn holds_notes_come_only_from_the_root_and_skip_hard_links_and_outside_links() {
    let s = Sandbox::new("notes-where");
    put(&s, "sub/AGENTS.md", b"not the root");
    put(&s, "docs/CLAUDE.md", b"not the root");
    assert_eq!(instructions::find(&s.ws()), None, "only the folder root counts");
    // A folder named like the file.
    std::fs::create_dir_all(s.project().join("AGENTS.md")).unwrap();
    assert_eq!(instructions::find(&s.ws()), None);
    std::fs::remove_dir(s.project().join("AGENTS.md")).unwrap();
    // A second name for a file outside.
    let o = s.write_outside("elsewhere.md", "outside notes");
    if std::fs::hard_link(&o, s.project().join("AGENTS.md")).is_ok() {
        put(&s, "CLAUDE.md", b"real notes");
        assert_eq!(instructions::find(&s.ws()).map(|n| n.name), Some("CLAUDE.md".to_string()));
    }
    // A junction at .github that points out.
    let s2 = Sandbox::new("notes-junction");
    s2.write_outside(".github/copilot-instructions.md", "outside notes");
    if s2.junction(".github", &s2.outside().join(".github")) {
        assert_eq!(instructions::find(&s2.ws()), None, "read through a junction that points out");
    }
    // The profile and a drive root are not projects.
    init();
    std::fs::write(home().join("AGENTS.md"), "profile notes").unwrap();
    let got = instructions::find(&Workspace::open(home()).unwrap());
    let _ = std::fs::remove_file(home().join("AGENTS.md"));
    assert_eq!(got, None);
}

/// The spec says "never through a link". A link that stays inside the folder is followed.
#[test]
fn defect_notes_are_read_through_a_link_that_points_back_inside() {
    let s = Sandbox::new("notes-inside-link");
    s.write("ghdocs/copilot-instructions.md", "notes found through a junction");
    if !s.junction(".github", &s.project().join("ghdocs")) {
        return;
    }
    let found = instructions::find(&s.ws());
    eprintln!("{found:?}");
    assert_eq!(found, None, "the notes file was read through a junction (spec: never through a link)");
}

/// With the privilege to make a file symlink: AGENTS.md -> .env must not send .env as the notes.
#[test]
fn note_a_notes_file_that_is_a_symlink_to_a_secret_in_the_folder() {
    let s = Sandbox::new("notes-symlink");
    s.write(".env", &format!("TOKEN={MARK}\n"));
    if std::os::windows::fs::symlink_file(s.project().join(".env"), s.project().join("AGENTS.md")).is_err() {
        eprintln!("no privilege to create a file symlink here; skipped (code reading: resolve_existing follows it)");
        return;
    }
    let found = instructions::find(&s.ws());
    eprintln!("{found:?}");
    assert!(found.is_none_or(|n| !n.text.contains(MARK)), "a secret-named file was sent as the notes");
}

// ═══ 10. Would the fixes work? ═════════════════════════════════════════════

/// The same walker settings as `search.rs`, with `info/exclude`, parent files and case folding switched
/// on one at a time, to check the fixes suggested in the report do what they should.
#[test]
fn note_the_suggested_walker_settings_hide_the_files_git_hides() {
    let s = Sandbox::new("walker-fix");
    s.write(".git/info/exclude", "local-only.txt\n");
    s.write("local-only.txt", "x\n");
    s.write(".gitignore", "Secrets/\n");
    s.write("secrets/a.txt", "x\n");
    s.write("private.txt", "x\n");
    s.write("src/lib.rs", "x\n");
    std::fs::write(s.dir.join(".gitignore"), "private.txt\n").unwrap();
    let root = PathBuf::from(s.ws().root_display());
    let seen = |parents: bool, exclude: bool, icase: bool| -> Vec<String> {
        let mut b = ignore::WalkBuilder::new(&root);
        b.hidden(false)
            .parents(parents)
            .require_git(false)
            .git_global(false)
            .git_exclude(exclude)
            .follow_links(false)
            .ignore_case_insensitive(icase);
        let mut v: Vec<String> = b
            .build()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
            .filter_map(|e| e.path().strip_prefix(&root).ok().map(|p| p.to_string_lossy().replace('\u{5c}', "/")))
            .filter(|p| !p.starts_with(".git/"))
            .collect();
        v.sort();
        v
    };
    let now = seen(false, false, false);
    let exclude = seen(false, true, false);
    let icase = seen(false, false, true);
    let parents = seen(true, false, false);
    eprintln!("as shipped: {now:?}\ngit_exclude(true): {exclude:?}\nignore_case_insensitive(true): {icase:?}\nparents(true): {parents:?}");
    assert!(now.contains(&"local-only.txt".to_string()) && !exclude.contains(&"local-only.txt".to_string()));
    assert!(now.contains(&"secrets/a.txt".to_string()) && !icase.contains(&"secrets/a.txt".to_string()));
    assert!(now.contains(&"private.txt".to_string()) && !parents.contains(&"private.txt".to_string()));
}

// ═══ 11. Corrected versions of two earlier tests ═══════════════════════════

#[test]
fn holds_names_windows_mishandles_are_refused_in_any_segment() {
    let s = Sandbox::new("names");
    let ws = s.ws();
    let mut bad: Vec<String> = Vec::new();
    for seg in [
        "con", "CON.txt", "Prn", "aux.rs", "NUL", "nul.tar.gz", "COM1", "com9.txt", "LPT1", "lpt9.c", "CONIN$", "CONOUT$.x",
        "con .txt", "x.", "x ", "a..", "a<b", "a>b", "a|b", "a?b", "a*b", "a\"b", "tab\there", "ctl\u{1}x",
    ] {
        // A request is trimmed as a whole, so a bare trailing space is not part of the name.
        if !seg.ends_with(' ') {
            bad.push(seg.to_string());
        }
        bad.push(format!("{seg}/x.rs"));
        bad.push(format!("src/{seg}/x.rs"));
    }
    let wsa = ws.clone();
    let accepted = accepted_by("resolve_target names", &bad, move |p| wsa.resolve_target(p, "").map(|t| edit::plain(&t.path)));
    assert!(accepted.is_empty(), "accepted: {accepted:?}");
    for ok in ["console.rs", "com10.txt", "com0.txt", "lpt0", "auxiliary/x.rs", ".gitignore", "a.b.c", "name with spaces.txt"] {
        assert!(ws.resolve_target(ok, "").is_ok(), "{ok}");
    }
}

#[test]
fn holds_the_notes_cut_lands_on_a_line_and_never_splits_a_character() {
    let cap = instructions::MAX_NOTES_BYTES;
    let note = instructions::CUT_NOTE;
    let rep = |unit: &str, n: usize| unit.repeat(n).into_bytes();
    let cases: Vec<(&str, Vec<u8>, bool)> = vec![
        ("60 lines of 100 bytes", rep(&format!("{}\n", "x".repeat(99)), 60), true),
        ("one 10 KB ASCII line", "y".repeat(10_000).into_bytes(), false),
        ("one line of 3-byte characters", "\u{20ac}".repeat(4000).into_bytes(), false),
        ("one line of 4-byte characters", "\u{1f600}".repeat(3000).into_bytes(), false),
        ("lines of 2-byte characters, the cut inside one", rep(&format!("{}\n", "\u{e9}".repeat(49)), 60), true),
        ("CRLF lines", rep(&format!("{}\r\n", "x".repeat(98)), 60), true),
        (
            "the cut between CR and LF",
            [rep(&format!("{}\r\n", "x".repeat(98)), 40), format!("{}\r\n", "z".repeat(95)).into_bytes(), rep("tail\r\n", 400)].concat(),
            true,
        ),
        ("a BOM and 5000 bytes", [&[0xEFu8, 0xBB, 0xBF][..], &rep("abcd\n", 1000)].concat(), true),
    ];
    for (what, bytes, has_lines) in &cases {
        let s = Sandbox::new("notes-cut");
        put(&s, "AGENTS.md", bytes);
        let n = instructions::find(&s.ws()).unwrap_or_else(|| panic!("{what}: no notes"));
        let raw = n.text.strip_suffix(note).unwrap_or_else(|| panic!("{what}: no cut note at the end"));
        let original = String::from_utf8_lossy(bytes.strip_prefix(&[0xEFu8, 0xBB, 0xBF][..]).unwrap_or(bytes)).replace("\r\n", "\n");
        // A note after a line that ends in a newline follows it directly; after a cut-off line it gets its own.
        let body = if original.starts_with(raw) { raw } else { raw.strip_suffix('\n').unwrap_or(raw) };
        eprintln!("{what}: file {} bytes, kept {} bytes, ends with a newline: {}", bytes.len(), body.len(), body.ends_with('\n'));
        assert!(n.cut && body.len() <= cap, "{what}: {} kept", body.len());
        assert!(!n.text.contains('\u{fffd}'), "{what}: a split character became U+FFFD");
        assert!(!n.text.contains('\r'), "{what}: a CR was sent");
        assert!(original.starts_with(body), "{what}: the kept text is not a prefix of the file");
        if *has_lines {
            assert!(body.ends_with('\n'), "{what}: not cut on a line");
        }
    }
}
