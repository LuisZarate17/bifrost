//! Independent review of file changes and undo (`odin_core::edit`): apply, backup
//! records, single undo, and "Undo all" for a turn.
//!
//! Written by a reviewer, not part of the product. Everything runs against folders
//! under `%TEMP%\odin-review-undo` and passes every backup folder explicitly, so
//! the user's real `%APPDATA%\odin` is never written to (`edit::backups_dir()` is
//! never called here).
//!
//! Tests named `defect_*` assert the behaviour the docs promise (or plain
//! correctness) and FAIL today; each is a confirmed finding. The rest pass and pin
//! what was checked. Set `REVIEW_SEQS=300` for a longer property run.
#![allow(clippy::too_many_arguments)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use odin_core::edit::{self, Op, TurnUndo, Workspace};

/// Where the fixtures go: under the system temp folder, never under `%APPDATA%\odin`.
fn scratch() -> PathBuf {
    std::env::temp_dir().join("odin-review-undo")
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

type Snap = BTreeMap<String, Option<Vec<u8>>>;

/// A project folder and a backups folder, both fresh, both under the scratch directory.
struct Sb {
    dir: PathBuf,
}

impl Sb {
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = scratch().join(format!("{tag}-{}-{n}", std::process::id()));
        // Refuse to run anywhere near the real data folder.
        if let Some(app) = std::env::var_os("APPDATA") {
            let app = PathBuf::from(app).join("odin");
            assert!(!dir.starts_with(&app), "scratch must not be inside the APPDATA odin folder");
        }
        fs::create_dir_all(dir.join("project")).unwrap();
        Sb { dir }
    }
    fn root(&self) -> PathBuf {
        self.dir.join("project")
    }
    fn ws(&self) -> Workspace {
        Workspace::open(self.root()).unwrap()
    }
    fn backups(&self) -> PathBuf {
        self.dir.join("backups")
    }
    fn put(&self, rel: &str, bytes: &[u8]) -> PathBuf {
        let p = self.root().join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, bytes).unwrap();
        p
    }
    fn get(&self, rel: &str) -> Vec<u8> {
        fs::read(self.root().join(rel)).unwrap_or_else(|e| panic!("reading {rel}: {e}"))
    }
    fn has(&self, rel: &str) -> bool {
        fs::symlink_metadata(self.root().join(rel)).is_ok()
    }
    fn snap(&self) -> Snap {
        fn walk(base: &Path, dir: &Path, out: &mut Snap) {
            let mut entries: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap()).collect();
            entries.sort_by_key(|e| e.file_name());
            for e in entries {
                let p = e.path();
                let rel = p.strip_prefix(base).unwrap().to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/");
                if e.file_type().unwrap().is_dir() {
                    out.insert(rel, None);
                    walk(base, &p, out);
                } else {
                    out.insert(rel, Some(fs::read(&p).unwrap()));
                }
            }
        }
        let mut out = Snap::new();
        walk(&self.root(), &self.root(), &mut out);
        out
    }
    /// Names in the backups folder, sorted.
    fn baks(&self) -> Vec<String> {
        let mut v: Vec<String> = match fs::read_dir(self.backups()) {
            Ok(rd) => rd.map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect(),
            Err(_) => Vec::new(),
        };
        v.sort();
        v
    }
    /// Relative paths of every file now in the project.
    fn files(&self) -> Vec<String> {
        self.snap().into_iter().filter(|(_, v)| v.is_some()).map(|(k, _)| k).collect()
    }
}

impl Drop for Sb {
    fn drop(&mut self) {
        // Keep the folder for inspection when a test failed.
        if std::thread::panicking() {
            return;
        }
        rm_rf(&self.dir);
    }
}

/// `remove_dir_all` that first clears read-only flags.
fn rm_rf(p: &Path) {
    fn clear(p: &Path) {
        if let Ok(md) = fs::symlink_metadata(p) {
            if md.is_dir() {
                if let Ok(rd) = fs::read_dir(p) {
                    for e in rd.flatten() {
                        clear(&e.path());
                    }
                }
            } else if md.permissions().readonly() {
                let mut perm = md.permissions();
                #[allow(clippy::permissions_set_readonly_false)]
                perm.set_readonly(false);
                let _ = fs::set_permissions(p, perm);
            }
        }
    }
    clear(p);
    let _ = fs::remove_dir_all(p);
}

fn diff_snaps(a: &Snap, b: &Snap) -> String {
    let mut out = Vec::new();
    for (k, v) in a {
        match b.get(k) {
            None => out.push(format!("  only in first : {k}{}", if v.is_none() { "/" } else { "" })),
            Some(w) if w != v => out.push(format!(
                "  differs       : {k} ({:?} vs {:?})",
                v.as_ref().map(|x| String::from_utf8_lossy(x).into_owned()),
                w.as_ref().map(|x| String::from_utf8_lossy(x).into_owned())
            )),
            _ => {}
        }
    }
    for (k, v) in b {
        if !a.contains_key(k) {
            out.push(format!("  only in second: {k}{}", if v.is_none() { "/" } else { "" }));
        }
    }
    out.join("\n")
}

fn assert_same(a: &Snap, b: &Snap, ctx: &str) {
    assert!(a == b, "{ctx}: project differs\n{}", diff_snaps(a, b));
}

// ── shorthand for the four kinds of change ─────────────────────────────────

fn edit(sb: &Sb, ws: &Workspace, rel: &str, old: &str, new: &str) -> String {
    edit::apply(&edit::plan_replace(ws, rel, old, new).unwrap(), &sb.backups()).unwrap().id
}
fn create(sb: &Sb, ws: &Workspace, rel: &str, content: &str) -> String {
    edit::apply(&edit::plan_create(ws, rel, content).unwrap(), &sb.backups()).unwrap().id
}
fn delete(sb: &Sb, ws: &Workspace, rel: &str) -> String {
    edit::apply(&edit::plan_delete(ws, rel).unwrap(), &sb.backups()).unwrap().id
}
fn mv(sb: &Sb, ws: &Workspace, from: &str, to: &str) -> String {
    edit::apply(&edit::plan_move(ws, from, to).unwrap(), &sb.backups()).unwrap().id
}
fn undo_all(sb: &Sb, ids: &[String]) -> Result<TurnUndo, String> {
    edit::undo_turn(ids, &sb.backups())
}

// ── a small deterministic generator (no external crates) ───────────────────

struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.next() % 100 < pct
    }
    fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            let j = self.below(i + 1);
            v.swap(i, j);
        }
    }
}

#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;


// ═══ Undo all: the cases the brief names ═══════════════════════════════════

#[test]
fn turn_two_files() {
    let sb = Sb::new("two");
    let ws = sb.ws();
    sb.put("a.txt", b"one\r\n");
    sb.put("b.txt", b"\xEF\xBB\xBFred\n");
    let init = sb.snap();
    let a = edit(&sb, &ws, "a.txt", "one", "two");
    let b = edit(&sb, &ws, "b.txt", "red", "blue");
    let out = undo_all(&sb, &[a.clone(), b.clone()]).unwrap();
    assert_eq!(out.restored, vec![b, a]);
    assert!(out.already.is_empty() && out.failed.is_none());
    assert_same(&sb.snap(), &init, "two files");
    assert!(sb.baks().is_empty(), "records retired as restored: {:?}", sb.baks());
}

#[test]
fn turn_one_file_edited_twice() {
    for reversed in [false, true] {
        let sb = Sb::new("twice");
        let ws = sb.ws();
        sb.put("a.txt", b"one two\r\n");
        let init = sb.snap();
        let first = edit(&sb, &ws, "a.txt", "one", "1");
        let second = edit(&sb, &ws, "a.txt", "two", "2");
        let ids = if reversed { vec![second.clone(), first.clone()] } else { vec![first.clone(), second.clone()] };
        let out = undo_all(&sb, &ids).unwrap();
        assert_eq!(out.restored, vec![second, first]);
        assert_same(&sb.snap(), &init, "edited twice");
        assert!(sb.baks().is_empty());
    }
}

#[test]
fn turn_create_then_edit() {
    let sb = Sb::new("create-edit");
    let ws = sb.ws();
    let init = sb.snap();
    let made = create(&sb, &ws, "new.txt", "hi\n");
    let edited = edit(&sb, &ws, "new.txt", "hi", "hello");
    undo_all(&sb, &[made, edited]).unwrap();
    assert_same(&sb.snap(), &init, "create then edit");
    assert!(sb.baks().is_empty());
}

#[test]
fn turn_create_then_move() {
    let sb = Sb::new("create-move");
    let ws = sb.ws();
    let init = sb.snap();
    let made = create(&sb, &ws, "n.txt", "hi\n");
    let moved = mv(&sb, &ws, "n.txt", "d/e/m.txt");
    assert!(sb.has("d/e/m.txt") && !sb.has("n.txt"));
    let out = undo_all(&sb, &[made, moved]).unwrap();
    assert!(out.failed.is_none() && out.restored.len() == 2);
    assert_same(&sb.snap(), &init, "create then move (folders d/e must go too)");
    assert!(sb.baks().is_empty());
}

#[test]
fn turn_edit_then_move() {
    let sb = Sb::new("edit-move");
    let ws = sb.ws();
    sb.put("a.txt", b"one two\r\n");
    let init = sb.snap();
    let edited = edit(&sb, &ws, "a.txt", "one", "1");
    let moved = mv(&sb, &ws, "a.txt", "sub/b.txt");
    undo_all(&sb, &[edited, moved]).unwrap();
    assert_same(&sb.snap(), &init, "edit then move");
    assert!(sb.baks().is_empty());
}

#[test]
fn turn_move_then_edit() {
    let sb = Sb::new("move-edit");
    let ws = sb.ws();
    sb.put("a.txt", b"one two\r\n");
    let init = sb.snap();
    let moved = mv(&sb, &ws, "a.txt", "sub/b.txt");
    let edited = edit(&sb, &ws, "sub/b.txt", "two", "2");
    undo_all(&sb, &[moved, edited]).unwrap();
    assert_same(&sb.snap(), &init, "move then edit");
    assert!(sb.baks().is_empty());
}

#[test]
fn turn_move_a_to_b_then_b_to_c() {
    let sb = Sb::new("move-move");
    let ws = sb.ws();
    sb.put("a.txt", b"x\n");
    let init = sb.snap();
    let one = mv(&sb, &ws, "a.txt", "b.txt");
    let two = mv(&sb, &ws, "b.txt", "deep/er/c.txt");
    let out = undo_all(&sb, &[one, two]).unwrap();
    assert_eq!(out.restored.len(), 2);
    assert_same(&sb.snap(), &init, "a to b to c");
    assert!(sb.baks().is_empty());
}

#[test]
fn turn_delete_then_create_same_path() {
    // different bytes, identical bytes (same hash), and a different spelling of the name
    for (variant, new_name, new_content) in [
        ("different", "a.txt", "replacement\n"),
        ("identical", "a.txt", "original\n"),
        ("other case", "A.txt", "replacement\n"),
    ] {
        let sb = Sb::new("del-create");
        let ws = sb.ws();
        sb.put("a.txt", b"original\r\n");
        let init = sb.snap();
        let gone = delete(&sb, &ws, "a.txt");
        let made = create(&sb, &ws, new_name, new_content);
        let out = undo_all(&sb, &[gone, made]).unwrap();
        assert_eq!(out.restored.len(), 2, "{variant}");
        assert_same(&sb.snap(), &init, variant);
        assert!(sb.baks().is_empty(), "{variant}");
    }
}

/// Four kinds of change in one turn.
fn mixed_turn(sb: &Sb, ws: &Workspace) -> Vec<String> {
    sb.put("e.txt", b"edit me\n");
    sb.put("del.txt", b"delete me\r\n");
    sb.put("mvfrom.txt", b"move me\n");
    vec![
        edit(sb, ws, "e.txt", "edit", "EDIT"),
        create(sb, ws, "n/c.txt", "created\n"),
        delete(sb, ws, "del.txt"),
        mv(sb, ws, "mvfrom.txt", "m/to.txt"),
    ]
}

fn append_to(sb: &Sb, rel: &str) -> Box<dyn Fn(&Sb)> {
    let orig = sb.get(rel);
    fs::write(sb.root().join(rel), [orig.clone(), b"mine\n".to_vec()].concat()).unwrap();
    let rel = rel.to_string();
    Box::new(move |sb| fs::write(sb.root().join(&rel), &orig).unwrap())
}

fn remove_by_hand(sb: &Sb, rel: &str) -> Box<dyn Fn(&Sb)> {
    let orig = sb.get(rel);
    fs::remove_file(sb.root().join(rel)).unwrap();
    let rel = rel.to_string();
    Box::new(move |sb| fs::write(sb.root().join(&rel), &orig).unwrap())
}

fn put_by_hand(sb: &Sb, rel: &str) -> Box<dyn Fn(&Sb)> {
    sb.put(rel, b"somebody elses\n");
    let rel = rel.to_string();
    Box::new(move |sb| fs::remove_file(sb.root().join(&rel)).unwrap())
}

fn folder_by_hand(sb: &Sb, rel: &str) -> Box<dyn Fn(&Sb)> {
    fs::create_dir(sb.root().join(rel)).unwrap();
    let rel = rel.to_string();
    Box::new(move |sb| fs::remove_dir(sb.root().join(&rel)).unwrap())
}

#[test]
fn turn_file_changed_since_refuses_the_whole_turn_and_touches_nothing() {
    // Every involved path, changed by hand in a different way, one at a time.
    type Mutate = fn(&Sb) -> Box<dyn Fn(&Sb)>;
    let cases: Vec<(&str, Mutate)> = vec![
        ("edited file appended to", |sb| append_to(sb, "e.txt")),
        ("created file appended to", |sb| append_to(sb, "n/c.txt")),
        ("created file deleted by hand", |sb| remove_by_hand(sb, "n/c.txt")),
        ("deleted file written again by hand", |sb| put_by_hand(sb, "del.txt")),
        ("deleted file path now a folder", |sb| folder_by_hand(sb, "del.txt")),
        ("move source written again by hand", |sb| put_by_hand(sb, "mvfrom.txt")),
        ("move destination appended to", |sb| append_to(sb, "m/to.txt")),
        ("move destination deleted by hand", |sb| remove_by_hand(sb, "m/to.txt")),
    ];
    let sb = Sb::new("changed-since");
    let ws = sb.ws();
    sb.put("e.txt", b"edit me\n");
    sb.put("del.txt", b"delete me\r\n");
    sb.put("mvfrom.txt", b"move me\n");
    let init = sb.snap();
    let ids = mixed_turn(&sb, &ws);
    let applied = sb.snap();
    let baks = sb.baks();
    for (name, mutate) in cases {
        let revert = mutate(&sb);
        let mutated = sb.snap();
        let err = undo_all(&sb, &ids).expect_err(&format!("{name}: the turn must be refused"));
        assert!(err.starts_with("Nothing was undone:"), "{name}: {err}");
        assert_same(&sb.snap(), &mutated, &format!("{name}: a refusal must touch nothing"));
        assert_eq!(sb.baks(), baks, "{name}: a refusal must keep every record");
        revert(&sb);
        assert_same(&sb.snap(), &applied, &format!("{name}: revert"));
    }
    let out = undo_all(&sb, &ids).unwrap();
    assert!(out.failed.is_none() && out.restored.len() == 4);
    assert_same(&sb.snap(), &init, "after the hand edits were reverted");
    assert!(sb.baks().is_empty());
}

#[test]
fn turn_partly_undone_skips_the_ones_already_undone() {
    // create (makes d/), move, edit; the newest undone by its own button.
    let sb = Sb::new("partly");
    let ws = sb.ws();
    let init = sb.snap();
    let made = create(&sb, &ws, "d/n.txt", "n\n");
    let moved = mv(&sb, &ws, "d/n.txt", "m.txt");
    let edited = edit(&sb, &ws, "m.txt", "n", "N");
    edit::undo(&edited, &sb.backups()).unwrap();
    let out = undo_all(&sb, &[made.clone(), moved.clone(), edited.clone()]).unwrap();
    assert_eq!(out.restored, vec![moved, made]);
    assert_eq!(out.already, vec![edited]);
    assert_same(&sb.snap(), &init, "partly undone");
    assert!(sb.baks().is_empty());

    // An independent file undone out of order in the middle of the turn.
    let sb = Sb::new("partly-mid");
    let ws = sb.ws();
    sb.put("a.txt", b"one two\n");
    sb.put("b.txt", b"red\n");
    let init = sb.snap();
    let r1 = edit(&sb, &ws, "a.txt", "one", "1");
    let r2 = edit(&sb, &ws, "b.txt", "red", "blue");
    let r3 = edit(&sb, &ws, "a.txt", "two", "2");
    edit::undo(&r2, &sb.backups()).unwrap();
    let out = undo_all(&sb, &[r1.clone(), r2.clone(), r3.clone()]).unwrap();
    assert_eq!((out.restored, out.already), (vec![r3, r1], vec![r2]));
    assert_same(&sb.snap(), &init, "middle one undone first");
}

#[test]
fn turn_created_folder_that_now_holds_another_file() {
    // a file of someone else in the deepest made folder
    let sb = Sb::new("folder-deep");
    let ws = sb.ws();
    let id = create(&sb, &ws, "n/a/x.txt", "x\n");
    sb.put("n/a/keep.txt", b"mine\n");
    let out = undo_all(&sb, &[id]).unwrap();
    assert!(out.failed.is_none());
    assert!(!sb.has("n/a/x.txt"));
    assert_eq!(sb.get("n/a/keep.txt"), b"mine\n");
    assert!(sb.baks().is_empty());

    // ... and in the outer one only: the inner empty folder goes, the outer stays
    let sb = Sb::new("folder-outer");
    let ws = sb.ws();
    let id = create(&sb, &ws, "n/a/x.txt", "x\n");
    sb.put("n/keep.txt", b"mine\n");
    undo_all(&sb, &[id]).unwrap();
    assert!(!sb.has("n/a"), "the inner folder was empty and was made by the create");
    assert_eq!(sb.get("n/keep.txt"), b"mine\n");

    // The same through a move that made the folder.
    let sb = Sb::new("folder-move");
    let ws = sb.ws();
    sb.put("a.txt", b"a\n");
    let id = mv(&sb, &ws, "a.txt", "q/r/a.txt");
    sb.put("q/r/keep.txt", b"mine\n");
    undo_all(&sb, &[id]).unwrap();
    assert_eq!(sb.get("a.txt"), b"a\n");
    assert_eq!(sb.get("q/r/keep.txt"), b"mine\n");
}

#[test]
fn turn_ids_from_another_turn_and_unknown_ids() {
    let sb = Sb::new("other-turn");
    let ws = sb.ws();
    sb.put("a.txt", b"one two\n");
    sb.put("b.txt", b"red\n");
    let init = sb.snap();
    // turn 1
    let t1 = edit(&sb, &ws, "a.txt", "one", "1");
    let after_t1 = sb.snap();
    // turn 2
    let t2a = edit(&sb, &ws, "a.txt", "two", "2");
    let t2b = edit(&sb, &ws, "b.txt", "red", "blue");
    let applied = sb.snap();

    // An id of the older turn with only part of the newer one: a.txt has moved on.
    let err = undo_all(&sb, &[t1.clone(), t2b.clone()]).unwrap_err();
    assert!(err.starts_with("Nothing was undone:") && err.contains("a.txt"), "{err}");
    assert_same(&sb.snap(), &applied, "refused");

    // Unknown ids are reported as already done and do not stop the rest.
    let out = undo_all(&sb, &[t2a, t2b, "deadbeef".to_string(), "0".to_string()]).unwrap();
    assert_eq!(out.already, vec!["deadbeef".to_string(), "0".to_string()]);
    assert_eq!(out.restored.len(), 2);
    assert_same(&sb.snap(), &after_t1, "turn 2 undone");

    // Only unknown ids: Ok, nothing done.
    let out = undo_all(&sb, &["deadbeef".to_string()]).unwrap();
    assert!(out.restored.is_empty() && out.failed.is_none());
    assert_same(&sb.snap(), &after_t1, "unknown ids touch nothing");

    undo_all(&sb, &[t1]).unwrap();
    assert_same(&sb.snap(), &init, "turn 1 undone last");
}

// ═══ A write that fails midway ═════════════════════════════════════════════

/// FILE_SHARE_READ | FILE_SHARE_WRITE, no DELETE: how many editors hold a file.
#[cfg(windows)]
const SHARE_RW: u32 = 1 | 2;

#[cfg(windows)]
fn hold_open(path: &Path, share: u32) -> fs::File {
    fs::OpenOptions::new().read(true).write(true).share_mode(share).open(path).unwrap()
}

fn leftovers(sb: &Sb) -> Vec<String> {
    sb.files().into_iter().filter(|f| f.contains(".odin-")).collect()
}

fn rec(id: &str) -> [String; 2] {
    [format!("{id}.bak"), format!("{id}.json")]
}

#[cfg(windows)]
#[test]
fn midway_locked_file_undo_all_reports_and_the_rest_stays_undoable() {
    let sb = Sb::new("lock-mid");
    let ws = sb.ws();
    sb.put("a.txt", b"one\r\n");
    sb.put("b.txt", b"red\n");
    let init = sb.snap();
    let ra = edit(&sb, &ws, "a.txt", "one", "two");
    let rb = edit(&sb, &ws, "b.txt", "red", "blue");
    let rc = create(&sb, &ws, "c.txt", "hi\n");
    let held = hold_open(&sb.root().join("b.txt"), SHARE_RW);
    let out = undo_all(&sb, &[ra.clone(), rb.clone(), rc.clone()]).unwrap();
    eprintln!("LOCKED midway: restored={:?} failed={:?}", out.restored, out.failed);
    assert_eq!(out.restored, vec![rc.clone()]);
    let f = out.failed.clone().expect("the locked file must be reported");
    assert_eq!((f.id.as_str(), f.display.as_str()), (rb.as_str(), "b.txt"));
    assert!(!sb.has("c.txt"), "newest change was put back");
    assert_eq!(sb.get("a.txt"), b"two\r\n");
    assert_eq!(sb.get("b.txt"), b"blue\n");
    assert!(leftovers(&sb).is_empty(), "{:?}", leftovers(&sb));
    let mut want: Vec<String> = [rec(&ra), rec(&rb)].concat();
    want.sort();
    assert_eq!(sb.baks(), want, "records of what is still applied are intact, the restored one is retired");
    assert!(edit::undo(&rb, &sb.backups()).is_err(), "still locked");
    drop(held);
    // H6 promised: the rest can be undone one at a time, newest first.
    edit::undo(&rb, &sb.backups()).unwrap();
    edit::undo(&ra, &sb.backups()).unwrap();
    assert_same(&sb.snap(), &init, "one at a time after the lock was released");
    assert!(sb.baks().is_empty());
}

#[cfg(windows)]
#[test]
fn midway_read_only_file_undo_all_reports_and_the_rest_stays_undoable() {
    let sb = Sb::new("ro-mid");
    let ws = sb.ws();
    sb.put("a.txt", b"one\r\n");
    let b = sb.put("b.txt", b"red\n");
    let init = sb.snap();
    let ra = edit(&sb, &ws, "a.txt", "one", "two");
    let rb = edit(&sb, &ws, "b.txt", "red", "blue");
    let mut perm = fs::metadata(&b).unwrap().permissions();
    perm.set_readonly(true);
    fs::set_permissions(&b, perm).unwrap();
    let out = undo_all(&sb, &[ra.clone(), rb.clone()]).unwrap();
    eprintln!("READ-ONLY midway: restored={:?} failed={:?}", out.restored, out.failed);
    let ro_after = fs::metadata(&b).map(|m| m.permissions().readonly()).unwrap_or(false);
    match &out.failed {
        Some(f) => {
            // The write was refused: nothing may have changed for b, a is still applied.
            assert_eq!(f.id, rb);
            assert!(out.restored.is_empty());
            assert_eq!(sb.get("b.txt"), b"blue\n");
            assert_eq!(sb.get("a.txt"), b"two\r\n");
            assert!(leftovers(&sb).is_empty(), "{:?}", leftovers(&sb));
            assert!(ro_after, "still read-only");
        }
        None => {
            // The undo went through a read-only file: report what became of the flag.
            eprintln!("READ-ONLY: undo replaced a read-only file; read-only flag afterwards = {ro_after}");
            assert_eq!(sb.get("b.txt"), b"red\n");
        }
    }
    let mut perm = fs::metadata(&b).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perm.set_readonly(false);
    fs::set_permissions(&b, perm).unwrap();
    let again = undo_all(&sb, &[ra, rb]).unwrap();
    assert!(again.failed.is_none());
    assert_same(&sb.snap(), &init, "after the flag was cleared");
}

#[cfg(windows)]
#[test]
fn apply_failures_on_held_files_leave_nothing_behind() {
    let sb = Sb::new("apply-fail");
    let ws = sb.ws();
    sb.put("e.txt", b"edit me\n");
    sb.put("d.txt", b"delete me\n");
    sb.put("m.txt", b"move me\n");
    let init = sb.snap();
    let plans = [
        ("edit", edit::plan_replace(&ws, "e.txt", "edit", "EDIT").unwrap(), "e.txt"),
        ("delete", edit::plan_delete(&ws, "d.txt").unwrap(), "d.txt"),
        ("move", edit::plan_move(&ws, "m.txt", "new/dir/m.txt").unwrap(), "m.txt"),
    ];
    let held: Vec<fs::File> = plans.iter().map(|(_, _, f)| hold_open(&sb.root().join(f), SHARE_RW)).collect();
    for (name, plan, _) in &plans {
        let err = edit::apply(plan, &sb.backups()).unwrap_err();
        eprintln!("HELD {name}: {err}");
        assert_same(&sb.snap(), &init, name);
        assert!(sb.baks().is_empty(), "{name}: {:?}", sb.baks());
    }
    drop(held);
    for (name, plan, _) in &plans {
        edit::apply(plan, &sb.backups()).unwrap_or_else(|e| panic!("{name} after release: {e}"));
    }
}

// ═══ Backup records: missing, corrupt, wrong ═══════════════════════════════

#[test]
fn records_corrupt_json_refuses_and_changes_nothing() {
    let sb = Sb::new("rec-corrupt");
    let ws = sb.ws();
    sb.put("a.txt", b"one\n");
    let id = edit(&sb, &ws, "a.txt", "one", "two");
    let json = sb.backups().join(format!("{id}.json"));
    for garbage in [&b"{ not json"[..], &b""[..], &b"{\"path\":1}"[..]] {
        fs::write(&json, garbage).unwrap();
        let e1 = edit::undo(&id, &sb.backups()).unwrap_err();
        let e2 = undo_all(&sb, std::slice::from_ref(&id)).unwrap_err();
        eprintln!("CORRUPT RECORD ({} bytes): single: {e1} | turn: {e2}", garbage.len());
        assert_eq!(sb.get("a.txt"), b"two\n");
        assert!(sb.backups().join(format!("{id}.bak")).exists());
    }
}

#[test]
fn records_missing_bak_refuses_for_edit_and_delete() {
    let sb = Sb::new("rec-nobak");
    let ws = sb.ws();
    sb.put("a.txt", b"one\n");
    sb.put("b.txt", b"bee\n");
    let ea = edit(&sb, &ws, "a.txt", "one", "two");
    let db = delete(&sb, &ws, "b.txt");
    fs::remove_file(sb.backups().join(format!("{ea}.bak"))).unwrap();
    fs::remove_file(sb.backups().join(format!("{db}.bak"))).unwrap();
    for id in [&ea, &db] {
        let single = edit::undo(id, &sb.backups()).unwrap_err();
        let turn = undo_all(&sb, std::slice::from_ref(id)).unwrap_err();
        eprintln!("MISSING BAK: single: {single} | turn: {turn}");
        assert!(turn.contains("backup of") && turn.contains("missing"), "{turn}");
    }
    assert_eq!(sb.get("a.txt"), b"two\n");
    assert!(!sb.has("b.txt"));
    assert!(sb.backups().join(format!("{ea}.json")).exists(), "the record stays for a later try");
}

#[test]
fn records_wrong_bak_for_a_delete_is_never_restored() {
    let sb = Sb::new("rec-wrong-del");
    let ws = sb.ws();
    sb.put("b.txt", b"bee\n");
    let db = delete(&sb, &ws, "b.txt");
    fs::write(sb.backups().join(format!("{db}.bak")), b"something else\n").unwrap();
    assert!(edit::undo(&db, &sb.backups()).unwrap_err().contains("not what was deleted"));
    assert!(undo_all(&sb, std::slice::from_ref(&db)).is_err());
    assert!(!sb.has("b.txt"));
}

#[test]
fn records_wrong_bak_for_an_edit_is_refused_by_undo_all() {
    let sb = Sb::new("rec-wrong-edit-turn");
    let ws = sb.ws();
    sb.put("a.txt", b"one two\n");
    let id = edit(&sb, &ws, "a.txt", "one", "1");
    fs::write(sb.backups().join(format!("{id}.bak")), b"one").unwrap();
    let err = undo_all(&sb, std::slice::from_ref(&id)).unwrap_err();
    eprintln!("WRONG BAK via undo all: {err}");
    assert_eq!(sb.get("a.txt"), b"1 two\n");
}

#[test]
fn defect_single_undo_restores_a_backup_that_is_not_the_original() {
    // The record keeps `before`, the hash of the original, "so a .bak can be checked
    // directly" (edit.rs Record.before). Undo all uses it; the single Undo does not,
    // so a truncated or swapped .bak is written over the file.
    let sb = Sb::new("rec-wrong-edit");
    let ws = sb.ws();
    sb.put("a.txt", b"one two\n");
    let id = edit(&sb, &ws, "a.txt", "one", "1");
    let edited = sb.get("a.txt");
    fs::write(sb.backups().join(format!("{id}.bak")), b"").unwrap();
    let r = edit::undo(&id, &sb.backups());
    assert!(
        r.is_err() && sb.get("a.txt") == edited,
        "single undo wrote a zero-byte backup over the file: result {r:?}, file now {:?}",
        String::from_utf8_lossy(&sb.get("a.txt"))
    );
}

#[cfg(windows)]
#[test]
fn defect_an_unreadable_record_is_reported_as_already_undone_by_undo_all() {
    // `already` is documented as "changes with no record left". A record that is
    // there but cannot be read right now (another handle on it, antivirus) is not that:
    // the app turns every id in `already` into an Undone card.
    let sb = Sb::new("rec-locked-turn");
    let ws = sb.ws();
    sb.put("a.txt", b"one\n");
    let id = edit(&sb, &ws, "a.txt", "one", "two");
    let json = sb.backups().join(format!("{id}.json"));
    let lock = fs::OpenOptions::new().read(true).share_mode(0).open(&json).unwrap();
    let out = undo_all(&sb, std::slice::from_ref(&id));
    drop(lock);
    eprintln!("UNREADABLE RECORD via undo all: {out:?}");
    assert_eq!(sb.get("a.txt"), b"two\n", "nothing was undone");
    assert!(json.exists(), "the record is still there");
    if let Ok(t) = &out {
        assert!(!t.already.contains(&id), "a record that exists but could not be read was reported as already undone: {t:?}");
    }
}

#[cfg(windows)]
#[test]
fn defect_an_unreadable_record_is_reported_as_already_undone_by_single_undo() {
    let sb = Sb::new("rec-locked-single");
    let ws = sb.ws();
    sb.put("a.txt", b"one\n");
    let id = edit(&sb, &ws, "a.txt", "one", "two");
    let json = sb.backups().join(format!("{id}.json"));
    let lock = fs::OpenOptions::new().read(true).share_mode(0).open(&json).unwrap();
    let err = edit::undo(&id, &sb.backups()).unwrap_err();
    drop(lock);
    eprintln!("UNREADABLE RECORD via single undo: {err}");
    assert!(!err.contains("already been undone"), "the record exists; the message says: {err}");
}

#[test]
fn ids_are_unique_and_increasing_in_quick_succession() {
    let sb = Sb::new("ids");
    let ws = sb.ws();
    sb.put("a.txt", b"x0\n");
    let mut ids = Vec::new();
    for i in 0..600 {
        ids.push(edit(&sb, &ws, "a.txt", &format!("x{i}"), &format!("x{}", i + 1)));
    }
    let nums: Vec<u128> = ids.iter().map(|s| u128::from_str_radix(s, 16).unwrap()).collect();
    assert!(nums.windows(2).all(|w| w[0] < w[1]), "ids must be strictly increasing");
    let gap = nums.windows(2).map(|w| w[1] - w[0]).min().unwrap();
    eprintln!("IDS: 600 sequential applies, smallest gap between ids = {gap} ns");
    assert_eq!(sb.baks().len(), 1200);
}

#[test]
fn ids_concurrent_applies_without_the_apps_lock_can_collide_informational() {
    // edit.rs says the caller serialises (the app holds EDITS). This shows what
    // happens if a caller does not: ids are 100 ns wall-clock ticks, so two threads can
    // draw the same one. Informational only; nothing asserted about collisions.
    let sb = Sb::new("ids-par");
    let ws = sb.ws();
    let backups = sb.backups();
    let threads = 6usize;
    let each = 120usize;
    for t in 0..threads {
        sb.put(&format!("t{t}.txt"), b"v0\n");
    }
    let mut handles = Vec::new();
    for t in 0..threads {
        let ws = ws.clone();
        let backups = backups.clone();
        handles.push(std::thread::spawn(move || {
            let mut ids = Vec::new();
            for i in 0..each {
                let plan = edit::plan_replace(&ws, &format!("t{t}.txt"), &format!("v{i}"), &format!("v{}", i + 1)).unwrap();
                if let Ok(a) = edit::apply(&plan, &backups) {
                    ids.push(a.id);
                }
            }
            ids
        }));
    }
    let all: Vec<String> = handles.into_iter().flat_map(|h| h.join().unwrap()).collect();
    let mut sorted = all.clone();
    sorted.sort();
    sorted.dedup();
    eprintln!("IDS parallel: {} applies, {} distinct ids, {} collisions", all.len(), sorted.len(), all.len() - sorted.len());
}

#[test]
fn records_are_global_not_scoped_to_a_workspace() {
    // One backups folder, two projects. An id carries everything: undo acts on the
    // project the record names, whichever folder the app has open.
    let a = Sb::new("ws-a");
    let b = Sb::new("ws-b");
    let (wa, wb) = (a.ws(), b.ws());
    a.put("f.txt", b"one\n");
    b.put("f.txt", b"uno\n");
    let shared = a.backups();
    let ia = edit::apply(&edit::plan_replace(&wa, "f.txt", "one", "two").unwrap(), &shared).unwrap().id;
    let ib = edit::apply(&edit::plan_replace(&wb, "f.txt", "uno", "dos").unwrap(), &shared).unwrap().id;
    let out = edit::undo_turn(std::slice::from_ref(&ib), &shared).unwrap();
    assert_eq!(out.restored, vec![ib]);
    assert_eq!((a.get("f.txt"), b.get("f.txt")), (b"two\n".to_vec(), b"uno\n".to_vec()));
    assert!(shared.join(format!("{ia}.json")).exists(), "the other project record is untouched");
    eprintln!("NOTE: undo/undo_turn take no workspace; a thread from project A undoes A files while B is open.");
}

// ═══ Bytes, sizes and refusals ═════════════════════════════════════════════

#[test]
fn bytes_crlf_bom_and_endings_survive_every_kind_of_change() {
    let sb = Sb::new("bytes");
    let ws = sb.ws();
    let crlf_bom: &[u8] = b"\xEF\xBB\xBFline one\r\nline two\r\nlast";
    let lf: &[u8] = b"alpha\nbeta\n";
    sb.put("crlf_bom.txt", crlf_bom);
    sb.put("lf.txt", lf);
    sb.put("del.txt", crlf_bom);
    sb.put("mv.txt", crlf_bom);
    let init = sb.snap();
    let e1 = edit(&sb, &ws, "crlf_bom.txt", "line two", "LINE\nTWO");
    assert_eq!(sb.get("crlf_bom.txt"), b"\xEF\xBB\xBFline one\r\nLINE\r\nTWO\r\nlast".to_vec(), "CRLF and BOM kept, no final newline added");
    let e2 = edit(&sb, &ws, "lf.txt", "beta", "BETA\ngamma");
    assert_eq!(sb.get("lf.txt"), b"alpha\nBETA\ngamma\n".to_vec(), "an LF file stays LF");
    let d = delete(&sb, &ws, "del.txt");
    let m = mv(&sb, &ws, "mv.txt", "x/y/mv.txt");
    assert_eq!(sb.get("x/y/mv.txt"), crlf_bom, "a move carries the exact bytes");
    let c = create(&sb, &ws, "made.txt", "a\nb\n");
    assert_eq!(sb.get("made.txt"), if cfg!(windows) { b"a\r\nb\r\n".to_vec() } else { b"a\nb\n".to_vec() });
    let out = undo_all(&sb, &[e1, e2, d, m, c]).unwrap();
    assert!(out.failed.is_none());
    assert_same(&sb.snap(), &init, "bytes");
    // mixed endings are refused rather than normalised
    sb.put("mixed.txt", b"a\r\nb\nc\r\n");
    assert!(edit::plan_replace(&ws, "mixed.txt", "b", "B").unwrap_err().contains("mixes CRLF and LF"));
}

#[test]
fn edit_size_caps() {
    let sb = Sb::new("caps-edit");
    let ws = sb.ws();
    let line = "abcdefghijklmno\n"; // 16 bytes
    let exact = format!("UNIQUEMARK00000{}{}", "\n", line.repeat(edit::MAX_EDIT_BYTES / 16 - 1));
    assert_eq!(exact.len(), edit::MAX_EDIT_BYTES);
    sb.put("exact.txt", exact.as_bytes());
    sb.put("over.txt", format!("{exact}x").as_bytes());
    assert!(edit::plan_replace(&ws, "exact.txt", "UNIQUEMARK00000", "X").is_ok());
    let err = edit::plan_replace(&ws, "over.txt", "UNIQUEMARK00000", "X").unwrap_err();
    assert!(err.contains("byte limit"), "{err}");
    // create: the cap is on the LF text, the file written is CRLF and can be twice as big
    assert!(edit::plan_create(&ws, "c1.txt", &"x".repeat(edit::MAX_EDIT_BYTES)).is_ok());
    assert!(edit::plan_create(&ws, "c2.txt", &"x".repeat(edit::MAX_EDIT_BYTES + 1)).is_err());
    let lines = "\n".repeat(edit::MAX_EDIT_BYTES);
    let plan = edit::plan_create(&ws, "c3.txt", &lines).unwrap();
    eprintln!("CAP: create of {} LF bytes writes {} bytes (over the {} edit cap, so it could not be edited afterwards)", lines.len(), plan.after.len(), edit::MAX_EDIT_BYTES);
}

#[test]
fn delete_cap_is_sixteen_mib_and_the_bytes_come_back() {
    let sb = Sb::new("caps-del");
    let ws = sb.ws();
    let n = edit::MAX_DELETE_BYTES as usize;
    let data: Vec<u8> = (0..n).map(|i| (i % 251) as u8).collect();
    sb.put("exact.bin", &data);
    sb.put("over.bin", &vec![7u8; n + 1]);
    assert!(edit::plan_delete(&ws, "over.bin").unwrap_err().contains("byte limit"));
    let plan = edit::plan_delete(&ws, "exact.bin").unwrap();
    assert_eq!(plan.size, n as u64);
    let done = edit::apply(&plan, &sb.backups()).unwrap();
    assert!(!sb.has("exact.bin"));
    edit::undo(&done.id, &sb.backups()).unwrap();
    assert!(sb.get("exact.bin") == data, "16 MiB restored byte for byte");
}

#[test]
fn empty_files_create_delete_move_and_restore() {
    let sb = Sb::new("empty");
    let ws = sb.ws();
    sb.put("zero.txt", b"");
    sb.put("zero2.txt", b"");
    let init = sb.snap();
    let c = create(&sb, &ws, "new_empty.txt", "");
    assert_eq!(sb.get("new_empty.txt"), b"");
    let d = delete(&sb, &ws, "zero.txt");
    assert!(!sb.has("zero.txt"));
    let m = mv(&sb, &ws, "zero2.txt", "sub/zero2.txt");
    assert_eq!(sb.get("sub/zero2.txt"), b"");
    let out = undo_all(&sb, &[c, d, m]).unwrap();
    assert!(out.failed.is_none() && out.restored.len() == 3);
    assert_same(&sb.snap(), &init, "empty files");
    assert_eq!(sb.get("zero.txt"), b"", "a 0-byte delete is restored as 0 bytes");
    assert!(sb.baks().is_empty());
}

#[test]
fn case_only_rename_and_directory_sources_are_refused() {
    let sb = Sb::new("refusals");
    let ws = sb.ws();
    sb.put("util.rs", b"x\n");
    sb.put("src/a.rs", b"y\n");
    for to in ["Util.rs", "UTIL.RS", "./util.rs", "src/../util.rs", "src/../Util.rs"] {
        let err = edit::plan_move(&ws, "util.rs", to).unwrap_err();
        assert!(err.contains("same file"), "{to}: {err}");
    }
    assert!(edit::plan_move(&ws, "src/a.rs", "SRC/a.rs").unwrap_err().contains("same file"));
    assert!(edit::plan_move(&ws, "src", "elsewhere").unwrap_err().contains("folder"));
    assert!(edit::plan_move(&ws, "src/", "elsewhere").is_err());
    assert!(edit::plan_delete(&ws, "src").unwrap_err().contains("folder"));
    assert!(edit::plan_delete(&ws, "src/").is_err());
    assert!(edit::plan_delete(&ws, ".").is_err());
    assert!(edit::plan_move(&ws, "util.rs", "src").unwrap_err().contains("existing folder"));
    assert!(edit::plan_move(&ws, "util.rs", "src/a.rs").unwrap_err().contains("already exists"));
}

#[cfg(windows)]
#[test]
fn move_across_drives_says_so_and_changes_nothing() {
    // move_new is public. D: is another volume on this machine; the destination name is
    // one nothing will have, and the move is expected to fail without creating it.
    let to = Path::new("D:/odin-review-undo-crossdrive-probe.txt");
    if !Path::new("D:/").exists() {
        eprintln!("CROSS-DRIVE: no D: drive here, skipped");
        return;
    }
    assert!(!to.exists());
    let sb = Sb::new("xdrive");
    let from = sb.put("a.txt", b"x\n");
    let r = edit::move_new(&from, to);
    let created = to.exists();
    if created {
        let _ = fs::remove_file(to); // tidy up if the move unexpectedly worked
    }
    eprintln!("CROSS-DRIVE: {r:?}");
    assert!(!created, "a cross-drive move must not fall back to copy and delete");
    assert!(from.exists(), "the source stays");
    let err = r.unwrap_err();
    assert!(err.contains("different drives"), "{err}");
}

#[cfg(windows)]
#[test]
fn edit_of_a_read_only_file_is_planned_then_fails_in_apply_observation() {
    let sb = Sb::new("ro-edit");
    let ws = sb.ws();
    let p = sb.put("a.txt", b"one\n");
    let mut perm = fs::metadata(&p).unwrap().permissions();
    perm.set_readonly(true);
    fs::set_permissions(&p, perm.clone()).unwrap();
    let planned = edit::plan_replace(&ws, "a.txt", "one", "two");
    let del = edit::plan_delete(&ws, "a.txt");
    let applied = planned.as_ref().map(|pl| edit::apply(pl, &sb.backups()));
    #[allow(clippy::permissions_set_readonly_false)]
    perm.set_readonly(false);
    fs::set_permissions(&p, perm).unwrap();
    eprintln!("READ-ONLY EDIT: plan ok = {}, apply = {:?}; delete plan = {:?}", planned.is_ok(), applied.as_ref().map(|r| r.as_ref().map(|a| &a.id)), del.as_ref().err());
    assert!(del.is_err(), "delete says read-only at planning");
    assert_eq!(sb.get("a.txt"), b"one\n");
    assert!(sb.baks().is_empty());
    assert!(leftovers(&sb).is_empty());
}

#[test]
fn defect_applied_folders_names_folders_this_apply_did_not_make() {
    // `Applied.folders` is "the folders this made". It is the plan list whenever at
    // least one folder was made, so a folder that appeared after the card was shown
    // is still reported (to the model and the card) as made by the create.
    let sb = Sb::new("folders-report");
    let ws = sb.ws();
    let plan = edit::plan_create(&ws, "n/m/x.txt", "x\n").unwrap();
    assert_eq!(plan.folders, vec!["n/".to_string(), "n/m/".to_string()]);
    fs::create_dir(sb.root().join("n")).unwrap(); // somebody made n/ after the card
    let done = edit::apply(&plan, &sb.backups()).unwrap();
    let undone = edit::undo(&done.id, &sb.backups());
    assert!(undone.is_ok());
    assert!(sb.has("n") && !sb.has("n/m"), "undo removed only the folder the create made");
    assert_eq!(done.folders, vec!["n/m/".to_string()], "reported as made: {:?}", done.folders);
}

// ═══ Nothing overwrites: races ═════════════════════════════════════════════

/// A thread that tries to create `path` (never replacing) the instant `barrier` opens.
fn racer(path: PathBuf, barrier: std::sync::Arc<std::sync::Barrier>) -> std::thread::JoinHandle<bool> {
    std::thread::spawn(move || {
        use std::io::Write;
        barrier.wait();
        match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => f.write_all(b"theirs\n").is_ok(),
            Err(_) => false,
        }
    })
}

/// Whoever wins, the loser must fail and the winner contents must stand.
fn judge(what: &str, i: usize, ours: bool, theirs: bool, now: &[u8], our_bytes: &[u8]) -> (u32, u32) {
    match (ours, theirs) {
        (true, true) => panic!("{what} #{i}: both claim to have created the file; it holds {:?}", String::from_utf8_lossy(now)),
        (true, false) => {
            assert_eq!(now, our_bytes, "{what} #{i}: ours won");
            (1, 0)
        }
        (false, true) => {
            assert_eq!(now, b"theirs\n", "{what} #{i}: theirs won and must be intact");
            (0, 1)
        }
        (false, false) => panic!("{what} #{i}: neither created it"),
    }
}

#[test]
fn nothing_overwrites_a_create_racing_a_writer() {
    use std::sync::{Arc, Barrier};
    let sb = Sb::new("race-create");
    let ws = sb.ws();
    let (mut mine, mut theirs) = (0, 0);
    for i in 0..200 {
        let name = format!("r{i}.txt");
        let plan = edit::plan_create(&ws, &name, "ours\n").unwrap();
        let b = Arc::new(Barrier::new(2));
        let h = racer(sb.root().join(&name), b.clone());
        b.wait();
        let ours = edit::apply(&plan, &sb.backups()).is_ok();
        let their = h.join().unwrap();
        let (m, t) = judge("create", i, ours, their, &sb.get(&name), &plan.after);
        mine += m;
        theirs += t;
    }
    eprintln!("RACE create: ours won {mine}, theirs won {theirs}");
    assert!(leftovers(&sb).is_empty(), "{:?}", leftovers(&sb));
}

#[test]
fn nothing_overwrites_a_move_racing_a_writer() {
    use std::sync::{Arc, Barrier};
    let sb = Sb::new("race-move");
    let ws = sb.ws();
    fs::create_dir_all(sb.root().join("sub")).unwrap();
    let (mut mine, mut theirs) = (0, 0);
    for i in 0..200 {
        let (from, to) = (format!("m{i}.txt"), format!("sub/m{i}.txt"));
        sb.put(&from, b"ours\n");
        let plan = edit::plan_move(&ws, &from, &to).unwrap();
        let b = Arc::new(Barrier::new(2));
        let h = racer(sb.root().join(&to), b.clone());
        b.wait();
        let ours = edit::apply(&plan, &sb.backups()).is_ok();
        let their = h.join().unwrap();
        let (m, t) = judge("move", i, ours, their, &sb.get(&to), b"ours\n");
        if t == 1 {
            assert_eq!(sb.get(&from), b"ours\n", "a refused move leaves the source");
        }
        mine += m;
        theirs += t;
    }
    eprintln!("RACE move: ours won {mine}, theirs won {theirs}");
}

#[test]
fn nothing_overwrites_an_undo_of_a_delete_racing_a_writer() {
    use std::sync::{Arc, Barrier};
    let sb = Sb::new("race-restore");
    let ws = sb.ws();
    let (mut mine, mut theirs) = (0, 0);
    for i in 0..200 {
        let name = format!("d{i}.txt");
        sb.put(&name, b"ours\n");
        let id = delete(&sb, &ws, &name);
        let b = Arc::new(Barrier::new(2));
        let h = racer(sb.root().join(&name), b.clone());
        b.wait();
        let ours = edit::undo(&id, &sb.backups()).is_ok();
        let their = h.join().unwrap();
        let (m, t) = judge("restore", i, ours, their, &sb.get(&name), b"ours\n");
        mine += m;
        theirs += t;
    }
    eprintln!("RACE restore: ours won {mine}, theirs won {theirs}");
    assert!(leftovers(&sb).is_empty(), "{:?}", leftovers(&sb));
}

#[test]
fn nothing_overwrites_an_undo_of_a_move_racing_a_writer() {
    use std::sync::{Arc, Barrier};
    let sb = Sb::new("race-moveback");
    let ws = sb.ws();
    for i in 0..200 {
        let (from, to) = (format!("u{i}.txt"), format!("moved{i}.txt"));
        sb.put(&from, b"ours\n");
        let id = mv(&sb, &ws, &from, &to);
        let b = Arc::new(Barrier::new(2));
        let h = racer(sb.root().join(&from), b.clone());
        b.wait();
        let ours = edit::undo(&id, &sb.backups()).is_ok();
        let their = h.join().unwrap();
        if ours {
            assert!(!their, "move-back #{i}: both claim the source path");
            assert_eq!(sb.get(&from), b"ours\n");
        } else {
            assert!(their, "move-back #{i}: neither");
            assert_eq!(sb.get(&from), b"theirs\n", "theirs must be intact");
            assert_eq!(sb.get(&to), b"ours\n", "the moved file stays where it is");
        }
    }
}

/// A thread that waits until a file whose name satisfies `pred` shows up in `watch`
/// (so the other side is between its checks and its act), then tries to create
/// `dest` without ever replacing. `done` ends the wait.
fn triggered(
    dest: PathBuf,
    watch: PathBuf,
    pred: fn(&str) -> bool,
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> std::thread::JoinHandle<Option<bool>> {
    std::thread::spawn(move || {
        use std::io::Write;
        let t0 = std::time::Instant::now();
        while !done.load(Ordering::SeqCst) && t0.elapsed().as_secs() < 5 {
            let hit = fs::read_dir(&watch)
                .map(|rd| rd.flatten().any(|e| pred(&e.file_name().to_string_lossy())))
                .unwrap_or(false);
            if hit {
                return Some(match fs::OpenOptions::new().write(true).create_new(true).open(&dest) {
                    Ok(mut f) => f.write_all(b"theirs\n").is_ok(),
                    Err(_) => false,
                });
            }
        }
        None
    })
}

/// Run `act` against a racer that fires in the window; return the outcome counts.
fn window_race(
    what: &str,
    sb: &Sb,
    n: usize,
    mut setup: impl FnMut(usize) -> (PathBuf, PathBuf, fn(&str) -> bool),
    mut act: impl FnMut(usize) -> Result<(), String>,
    ours: &[u8],
) -> (u32, u32, u32) {
    let (mut ours_won, mut theirs_won, mut idle) = (0, 0, 0);
    for i in 0..n {
        let (dest, watch, pred) = setup(i);
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let h = triggered(dest.clone(), watch, pred, done.clone());
        let r = act(i);
        done.store(true, Ordering::SeqCst);
        let t = h.join().unwrap();
        let now = fs::read(&dest).unwrap_or_default();
        match (r.is_ok(), t) {
            (true, Some(true)) => panic!("{what} #{i}: both created {}: {:?}", dest.display(), String::from_utf8_lossy(&now)),
            (true, _) => {
                assert_eq!(now, ours, "{what} #{i}");
                if t.is_none() { idle += 1 } else { ours_won += 1 }
            }
            (false, Some(true)) => {
                assert_eq!(now, b"theirs\n", "{what} #{i}: their file must be intact; error was {r:?}");
                theirs_won += 1;
            }
            (false, _) => panic!("{what} #{i}: failed with nothing in the way: {r:?}"),
        }
    }
    let _ = sb;
    eprintln!("WINDOW RACE {what}: ours {ours_won}, theirs {theirs_won}, racer never fired {idle}");
    (ours_won, theirs_won, idle)
}

#[test]
fn nothing_overwrites_a_create_when_the_file_appears_in_the_last_moment() {
    let sb = Sb::new("win-create");
    let ws = sb.ws();
    let plans: Vec<_> = (0..150).map(|i| edit::plan_create(&ws, &format!("w{i}.txt"), "ours\n").unwrap()).collect();
    let root = sb.root();
    let backups = sb.backups();
    let (_, theirs, _) = window_race(
        "create",
        &sb,
        150,
        |i| (root.join(format!("w{i}.txt")), root.clone(), |n: &str| n.ends_with(".odin-new")),
        |i| edit::apply(&plans[i], &backups).map(|_| ()),
        &plans[0].after,
    );
    assert!(theirs > 0, "the racer never landed in the window; the test proved nothing");
    assert!(leftovers(&sb).is_empty(), "{:?}", leftovers(&sb));
}

#[test]
fn nothing_overwrites_a_restore_when_the_file_appears_in_the_last_moment() {
    let sb = Sb::new("win-restore");
    let ws = sb.ws();
    let ids: Vec<String> = (0..150)
        .map(|i| {
            sb.put(&format!("w{i}.txt"), b"ours\n");
            delete(&sb, &ws, &format!("w{i}.txt"))
        })
        .collect();
    let root = sb.root();
    let backups = sb.backups();
    let (_, theirs, _) = window_race(
        "restore",
        &sb,
        150,
        |i| (root.join(format!("w{i}.txt")), root.clone(), |n: &str| n.ends_with(".odin-new")),
        |i| edit::undo(&ids[i], &backups).map(|_| ()),
        b"ours\n",
    );
    assert!(theirs > 0, "the racer never landed in the window; the test proved nothing");
    assert!(leftovers(&sb).is_empty(), "{:?}", leftovers(&sb));
}

#[test]
fn nothing_overwrites_a_move_when_the_destination_appears_in_the_last_moment() {
    let sb = Sb::new("win-move");
    let ws = sb.ws();
    fs::create_dir_all(sb.root().join("sub")).unwrap();
    let plans: Vec<_> = (0..150)
        .map(|i| {
            sb.put(&format!("m{i}.txt"), b"ours\n");
            edit::plan_move(&ws, &format!("m{i}.txt"), &format!("sub/m{i}.txt")).unwrap()
        })
        .collect();
    let root = sb.root();
    let backups = sb.backups();
    fs::create_dir_all(&backups).unwrap();
    // the record is written just before the rename, so its appearance marks the window
    let (_, theirs, _) = window_race(
        "move",
        &sb,
        150,
        |i| (root.join(format!("sub/m{i}.txt")), backups.clone(), |n: &str| n.ends_with(".json")),
        |i| edit::apply(&plans[i], &backups).map(|_| ()),
        b"ours\n",
    );
    assert!(theirs > 0, "the racer never landed in the window; the test proved nothing");
}

// ═══ The one write that does replace: an edit, and the undo of an edit ═════

/// Waits for a file named by `pred` in `watch`, then saves `path` the way an editor
/// does (replacing its bytes). Returns whether the save was made.
fn editor_save_on(
    path: PathBuf,
    watch: PathBuf,
    pred: fn(&str) -> bool,
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> std::thread::JoinHandle<bool> {
    std::thread::spawn(move || {
        let t0 = std::time::Instant::now();
        while !done.load(Ordering::SeqCst) && t0.elapsed().as_secs() < 5 {
            let hit = fs::read_dir(&watch)
                .map(|rd| rd.flatten().any(|e| pred(&e.file_name().to_string_lossy())))
                .unwrap_or(false);
            if hit {
                return fs::write(&path, b"one\nSAVED BY AN EDITOR AFTER THE CHECK\n").is_ok();
            }
        }
        false
    })
}

#[test]
fn defect_an_edit_replaces_a_save_that_landed_after_its_staleness_check() {
    // Guarantee 3 in the module doc: "Nothing is written over a file that changed since
    // you saw the diff." apply() compares the bytes at its start, then writes the .bak and
    // the record, then replaces the file; unlike delete/move it does not look again right
    // before the replacing rename. A save that lands in between is overwritten, and is in
    // neither the backup (taken earlier) nor the file.
    let attempts = 60;
    let (mut lost, mut refused, mut theirs_last) = (0, 0, 0);
    for i in 0..attempts {
        let sb = Sb::new("edit-race");
        let ws = sb.ws();
        let path = sb.put("a.txt", b"one\n");
        let plan = edit::plan_replace(&ws, "a.txt", "one", "two").unwrap();
        fs::create_dir_all(sb.backups()).unwrap();
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let h = editor_save_on(path.clone(), sb.backups(), |n| n.ends_with(".json"), done.clone());
        let r = edit::apply(&plan, &sb.backups());
        done.store(true, Ordering::SeqCst);
        let saved = h.join().unwrap();
        let now = sb.get("a.txt");
        match (&r, saved) {
            (Ok(_), true) if now == plan.after => lost += 1,
            (Ok(_), true) => theirs_last += 1,
            (Err(_), true) => refused += 1,
            _ => {}
        }
        let _ = i;
    }
    eprintln!("EDIT RACE over {attempts} attempts: editor save lost {lost}, refused {refused}, editor wrote last {theirs_last}");
    assert_eq!(lost, 0, "apply() returned Ok and replaced the file although a save had landed after its check ({lost}/{attempts})");
}

#[test]
fn defect_undo_of_an_edit_replaces_a_save_that_landed_after_its_check() {
    // The same window in undo(): look() compares the file with what the edit left, the
    // backup is read, a temporary file is written, then the rename replaces the file.
    // A save made after look() is overwritten by the backup. (The doc on undo():
    // "undo never throws away work done since".)
    let attempts = 60;
    let (mut lost, mut refused) = (0, 0);
    for _ in 0..attempts {
        let sb = Sb::new("undo-race");
        let ws = sb.ws();
        let path = sb.put("a.txt", b"one\n");
        let id = edit(&sb, &ws, "a.txt", "one", "two");
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let h = editor_save_on(path.clone(), sb.root(), |n| n.ends_with(".odin-tmp"), done.clone());
        let r = edit::undo(&id, &sb.backups());
        done.store(true, Ordering::SeqCst);
        let saved = h.join().unwrap();
        let now = sb.get("a.txt");
        match (&r, saved) {
            (Ok(_), true) if now == b"one\n" => lost += 1,
            (Err(_), true) => refused += 1,
            _ => {}
        }
    }
    eprintln!("UNDO RACE over {attempts} attempts: editor save lost {lost}, refused {refused}");
    assert_eq!(lost, 0, "undo() returned Ok and replaced the file although a save had landed after its check ({lost}/{attempts})");
}

// ═══ Property tests: random turns of edits, creates, deletes and moves ═════

#[derive(Clone, Debug)]
enum Step {
    Rewrite(String, String),
    Create(String, String),
    Delete(String),
    Move(String, String),
}

/// Paths the generator draws from. Two spellings of one file (a.txt / A.txt, d / D)
/// are in on purpose: NTFS has one file for both.
const UNIVERSE: &[&str] = &[
    "a.txt", "b.txt", "c.txt", "A.txt", "d/x.txt", "d/y.txt", "D/X.txt", "d/e/z.txt", "f/g/h.txt",
    "new/n1.txt", "new/sub/n2.txt", "empty/q.txt",
];

/// The same starting project for the same seed.
fn fixture(sb: &Sb, seed: u64) {
    let mut rng = Rng::new(seed);
    for rel in ["a.txt", "b.txt", "c.txt", "d/x.txt", "d/y.txt", "d/e/z.txt"] {
        if rng.chance(70) {
            let body = format!("{rel} version zero\nsecond line\nthird line");
            let bytes: Vec<u8> = match rng.below(5) {
                0 => body.replace('\n', "\r\n").into_bytes(),
                1 => [b"\xEF\xBB\xBF".to_vec(), body.replace('\n', "\r\n").into_bytes()].concat(),
                2 => format!("{body}\n").into_bytes(),
                3 => Vec::new(),
                _ => body.into_bytes(),
            };
            sb.put(rel, &bytes);
        }
    }
    if rng.chance(50) {
        fs::create_dir_all(sb.root().join("empty")).unwrap();
    }
    if rng.chance(30) {
        fs::create_dir_all(sb.root().join("d/e")).unwrap();
    }
}

fn run_step(sb: &Sb, ws: &Workspace, st: &Step) -> Result<String, String> {
    let plan = match st {
        Step::Rewrite(p, c) => edit::plan_rewrite(ws, p, c),
        Step::Create(p, c) => edit::plan_create(ws, p, c),
        Step::Delete(p) => edit::plan_delete(ws, p),
        Step::Move(a, b) => edit::plan_move(ws, a, b),
    }?;
    edit::apply(&plan, &sb.backups()).map(|a| a.id)
}

/// Draw steps that the real planner and applier accept.
fn gen_steps(sb: &Sb, ws: &Workspace, rng: &mut Rng, want: usize) -> Vec<(Step, String)> {
    let mut out = Vec::new();
    let mut tries = 0;
    while out.len() < want && tries < want * 10 {
        tries += 1;
        let files = sb.files();
        let pick = |rng: &mut Rng| files[rng.below(files.len())].clone();
        let uni = |rng: &mut Rng| UNIVERSE[rng.below(UNIVERSE.len())].to_string();
        let step = match rng.below(100) {
            0..=29 if !files.is_empty() => {
                let p = pick(rng);
                Step::Rewrite(p, format!("rewritten {:x}\nsecond line\n", rng.next()))
            }
            30..=54 => {
                let p = uni(rng);
                Step::Create(p, format!("created {:x}\n", rng.next()))
            }
            55..=74 if !files.is_empty() => Step::Delete(pick(rng)),
            75..=99 if !files.is_empty() => Step::Move(pick(rng), uni(rng)),
            _ => continue,
        };
        if let Ok(id) = run_step(sb, ws, &step) {
            out.push((step, id));
        }
    }
    out
}

/// Fresh project for `seed`, then either generate `want` steps or replay `steps`.
/// Returns the sandbox, the steps with their ids, and a snapshot after each step
/// (snaps[0] is the starting project).
fn build(seed: u64, tag: &str, steps: Option<&[Step]>, want: usize) -> (Sb, Workspace, Vec<(Step, String)>, Vec<Snap>) {
    let sb = Sb::new(tag);
    fixture(&sb, seed);
    let ws = sb.ws();
    let mut snaps = vec![sb.snap()];
    let done: Vec<(Step, String)> = match steps {
        None => {
            // generate one step at a time so each snapshot lines up
            let mut rng = Rng::new(seed ^ 0xA5A5_5A5A);
            let mut v = Vec::new();
            for _ in 0..want {
                let mut one = gen_steps(&sb, &ws, &mut rng, 1);
                match one.pop() {
                    Some(s) => {
                        v.push(s);
                        snaps.push(sb.snap());
                    }
                    None => break,
                }
            }
            v
        }
        Some(list) => list
            .iter()
            .map(|st| {
                let id = run_step(&sb, &ws, st).unwrap_or_else(|e| panic!("seed {seed}: replay of {st:?} failed: {e}"));
                snaps.push(sb.snap());
                (st.clone(), id)
            })
            .collect(),
    };
    (sb, ws, done, snaps)
}

fn seeds() -> std::ops::RangeInclusive<u64> {
    let n: u64 = std::env::var("REVIEW_SEQS").ok().and_then(|s| s.parse().ok()).unwrap_or(60);
    1..=n
}

fn ids_of(done: &[(Step, String)]) -> Vec<String> {
    done.iter().map(|(_, id)| id.clone()).collect()
}

fn steps_of(done: &[(Step, String)]) -> Vec<Step> {
    done.iter().map(|(s, _)| s.clone()).collect()
}

fn len_for(seed: u64) -> usize {
    3 + (seed as usize % 6)
}

#[test]
fn property_undo_all_restores_the_starting_project_byte_for_byte() {
    let mut total = 0usize;
    for seed in seeds() {
        let (sb, _ws, done, snaps) = build(seed, "prop-all", None, len_for(seed));
        if done.is_empty() {
            continue;
        }
        total += done.len();
        let steps = steps_of(&done);
        let mut ids = ids_of(&done);
        Rng::new(seed ^ 77).shuffle(&mut ids);
        let out = edit::undo_turn(&ids, &sb.backups())
            .unwrap_or_else(|e| panic!("seed {seed}: undo all refused an untouched turn: {e}\nsteps: {steps:#?}"));
        assert!(out.failed.is_none(), "seed {seed}: {:?}\nsteps: {steps:#?}", out.failed);
        assert_eq!((out.restored.len(), out.already.len()), (done.len(), 0), "seed {seed}");
        assert_same(&sb.snap(), &snaps[0], &format!("seed {seed}, steps {steps:#?}"));
        assert!(sb.baks().is_empty(), "seed {seed}: {:?}", sb.baks());
    }
    eprintln!("PROPERTY undo all: {total} steps");
}

#[test]
fn property_undoing_the_newest_changes_gives_back_the_state_after_the_rest() {
    let mut cases = 0usize;
    for seed in seeds() {
        let steps = {
            let (_sb, _ws, done, _) = build(seed, "prop-suffix-gen", None, len_for(seed));
            steps_of(&done)
        };
        let n = steps.len();
        if n < 2 {
            continue;
        }
        let mut keeps = vec![0, n / 2, n - 1];
        keeps.dedup();
        for keep in keeps {
            let (sb, _ws, done, snaps) = build(seed, "prop-suffix", Some(&steps), 0);
            let mut ids = ids_of(&done)[keep..].to_vec();
            Rng::new(seed ^ keep as u64).shuffle(&mut ids);
            let out = edit::undo_turn(&ids, &sb.backups())
                .unwrap_or_else(|e| panic!("seed {seed} keep {keep}: refused: {e}\nsteps: {steps:#?}"));
            assert!(out.failed.is_none(), "seed {seed} keep {keep}: {:?}", out.failed);
            assert_same(&sb.snap(), &snaps[keep], &format!("seed {seed}, kept the first {keep}, steps {steps:#?}"));
            cases += 1;
        }
    }
    eprintln!("PROPERTY newest-k: {cases} cases");
}

#[test]
fn property_single_undos_newest_first_walk_back_through_every_state() {
    let mut cases = 0usize;
    for seed in seeds() {
        let (sb, _ws, done, snaps) = build(seed, "prop-single", None, len_for(seed));
        let n = done.len();
        if n < 2 {
            continue;
        }
        let ids = ids_of(&done);
        let j = 1 + (seed as usize % n.min(3));
        for back in 0..j {
            let at = n - 1 - back;
            edit::undo(&ids[at], &sb.backups())
                .unwrap_or_else(|e| panic!("seed {seed}: single undo of step {at} ({:?}) refused: {e}", done[at].0));
            assert_same(&sb.snap(), &snaps[at], &format!("seed {seed}: after undoing step {at} ({:?})", done[at].0));
        }
        let out = edit::undo_turn(&ids, &sb.backups())
            .unwrap_or_else(|e| panic!("seed {seed}: undo all after {j} single undos: {e}"));
        assert_eq!((out.restored.len(), out.already.len()), (n - j, j), "seed {seed}");
        assert!(out.failed.is_none());
        assert_same(&sb.snap(), &snaps[0], &format!("seed {seed}: finished by undo all"));
        assert!(sb.baks().is_empty());
        cases += 1;
    }
    eprintln!("PROPERTY partly undone: {cases} cases");
}

fn touched(done: &[(Step, String)]) -> Vec<String> {
    let mut v = Vec::new();
    for (s, _) in done {
        match s {
            Step::Rewrite(p, _) | Step::Create(p, _) | Step::Delete(p) => v.push(p.clone()),
            Step::Move(a, b) => {
                v.push(a.clone());
                v.push(b.clone());
            }
        }
    }
    v
}

#[test]
fn property_a_hand_change_to_any_touched_path_refuses_the_turn_and_touches_nothing() {
    let mut cases = 0usize;
    let mut kinds = [0usize; 6];
    for seed in seeds() {
        let (sb, _ws, done, snaps) = build(seed, "prop-hand", None, len_for(seed));
        if done.is_empty() {
            continue;
        }
        let steps = steps_of(&done);
        let ids = ids_of(&done);
        let mut rng = Rng::new(seed ^ 991);
        let all = touched(&done);
        let p = all[rng.below(all.len())].clone();
        let full = sb.root().join(&p);
        let original = fs::read(&full).ok();
        let (kind, revert): (usize, Box<dyn Fn()>) = match (original.clone(), rng.below(3)) {
            (Some(orig), 0) => {
                fs::write(&full, [orig.clone(), b"!".to_vec()].concat()).unwrap();
                let (full, orig) = (full.clone(), orig);
                (0, Box::new(move || fs::write(&full, &orig).unwrap()))
            }
            (Some(orig), 1) => {
                fs::remove_file(&full).unwrap();
                let (full, orig) = (full.clone(), orig);
                (1, Box::new(move || fs::write(&full, &orig).unwrap()))
            }
            (Some(orig), _) => {
                fs::remove_file(&full).unwrap();
                fs::create_dir(&full).unwrap();
                let (full, orig) = (full.clone(), orig);
                (2, Box::new(move || {
                    fs::remove_dir(&full).unwrap();
                    fs::write(&full, &orig).unwrap()
                }))
            }
            (None, k) if full.parent().is_some_and(|d| d.is_dir()) && !full.exists() => {
                if k == 2 {
                    fs::create_dir(&full).unwrap();
                    let full = full.clone();
                    (3, Box::new(move || fs::remove_dir(&full).unwrap()))
                } else {
                    fs::write(&full, b"intruder\n").unwrap();
                    let full = full.clone();
                    (4, Box::new(move || fs::remove_file(&full).unwrap()))
                }
            }
            _ => continue,
        };
        kinds[kind] += 1;
        let applied = {
            revert();
            let s = sb.snap();
            // put the mutation back for the real test
            match kind {
                0 => fs::write(&full, [original.clone().unwrap(), b"!".to_vec()].concat()).unwrap(),
                1 => fs::remove_file(&full).unwrap(),
                2 => {
                    fs::remove_file(&full).unwrap();
                    fs::create_dir(&full).unwrap();
                }
                3 => fs::create_dir(&full).unwrap(),
                _ => fs::write(&full, b"intruder\n").unwrap(),
            }
            s
        };
        let mutated = sb.snap();
        let baks = sb.baks();
        let err = edit::undo_turn(&ids, &sb.backups());
        assert!(err.is_err(), "seed {seed}: a hand change (kind {kind}) to {p} must refuse the turn, got {err:?}\nsteps: {steps:#?}");
        assert_same(&sb.snap(), &mutated, &format!("seed {seed}: refusal touched something (kind {kind} on {p})\nsteps: {steps:#?}"));
        assert_eq!(sb.baks(), baks, "seed {seed}: refusal changed the records");
        revert();
        assert_same(&sb.snap(), &applied, &format!("seed {seed}: revert"));
        let out = edit::undo_turn(&ids, &sb.backups())
            .unwrap_or_else(|e| panic!("seed {seed}: after reverting the hand change: {e}\nsteps: {steps:#?}"));
        assert!(out.failed.is_none());
        assert_same(&sb.snap(), &snaps[0], &format!("seed {seed}: final\nsteps: {steps:#?}"));
        cases += 1;
    }
    eprintln!("PROPERTY hand change: {cases} cases; kinds [append, delete, dir-for-file, dir-for-absent, intruder-file] = {:?}", &kinds[..5]);
}

#[test]
fn property_a_made_folder_that_holds_someone_elses_file_is_kept_with_it() {
    let mut cases = 0usize;
    for seed in seeds() {
        let (sb, _ws, done, snaps) = build(seed, "prop-keep", None, len_for(seed));
        let steps = steps_of(&done);
        let lower = |m: &Snap| -> BTreeMap<String, bool> { m.iter().map(|(k, v)| (k.to_lowercase(), v.is_none())).collect() };
        let start = lower(&snaps[0]);
        let now = sb.snap();
        let made: Vec<String> = now.iter().filter(|(k, v)| v.is_none() && !start.contains_key(&k.to_lowercase())).map(|(k, _)| k.clone()).collect();
        if made.is_empty() {
            continue;
        }
        let dir = made[Rng::new(seed ^ 5).below(made.len())].clone();
        let keep = format!("{dir}/keep.txt");
        sb.put(&keep, b"mine\n");
        let ids = ids_of(&done);
        let out = edit::undo_turn(&ids, &sb.backups())
            .unwrap_or_else(|e| panic!("seed {seed}: refused although only a new file sits in a made folder: {e}\nsteps: {steps:#?}"));
        assert!(out.failed.is_none(), "seed {seed}: {:?}", out.failed);
        let after = sb.snap();
        assert_eq!(after.get(&keep), Some(&Some(b"mine\n".to_vec())), "seed {seed}: the other file must be kept");
        // everything the project started with is back, exactly
        for (k, v) in &snaps[0] {
            assert_eq!(after.get(k), Some(v), "seed {seed}: {k} must be as it started\nsteps: {steps:#?}");
        }
        // and nothing else is left except the folders that lead to the kept file
        let want = format!("{}/", keep.to_lowercase());
        for k in after.keys() {
            let kl = k.to_lowercase();
            let fine = snaps[0].contains_key(k) || kl == keep.to_lowercase() || want.starts_with(&format!("{kl}/"));
            assert!(fine, "seed {seed}: left behind {k}\nsteps: {steps:#?}");
        }
        assert!(sb.baks().is_empty());
        cases += 1;
    }
    eprintln!("PROPERTY kept folder: {cases} cases");
}

#[test]
fn property_any_subset_either_refuses_untouched_or_runs_through_cleanly() {
    let (mut refused, mut ran) = (0usize, 0usize);
    for seed in seeds() {
        let (sb, _ws, done, _snaps) = build(seed, "prop-subset", None, len_for(seed) + 2);
        if done.len() < 3 {
            continue;
        }
        let steps = steps_of(&done);
        let mut rng = Rng::new(seed ^ 4242);
        let ids: Vec<String> = ids_of(&done).into_iter().filter(|_| rng.chance(50)).collect();
        if ids.is_empty() {
            continue;
        }
        let before = sb.snap();
        let baks = sb.baks();
        match edit::undo_turn(&ids, &sb.backups()) {
            Err(e) => {
                assert!(e.starts_with("Nothing was undone"), "seed {seed}: {e}");
                assert_same(&sb.snap(), &before, &format!("seed {seed}: a refusal must touch nothing\nsteps: {steps:#?}"));
                assert_eq!(sb.baks(), baks, "seed {seed}");
                refused += 1;
            }
            Ok(out) => {
                assert!(out.failed.is_none(), "seed {seed}: the pre-check passed but the run stopped: {:?}\nids {ids:?}\nsteps: {steps:#?}", out.failed);
                assert_eq!(out.restored.len(), ids.len(), "seed {seed}");
                ran += 1;
            }
        }
    }
    eprintln!("PROPERTY subsets: refused {refused}, ran {ran}");
}

#[test]
fn property_coverage_of_what_the_generator_does_informational() {
    let (mut edits, mut creates, mut deletes, mut moves, mut in_new_dirs, mut same_path_twice) = (0, 0, 0, 0, 0, 0);
    let mut total = 0;
    for seed in seeds() {
        let (_sb, _ws, done, snaps) = build(seed, "prop-cover", None, len_for(seed));
        total += done.len();
        let lower = |m: &Snap| -> std::collections::BTreeSet<String> { m.iter().filter(|(_, v)| v.is_none()).map(|(k, _)| k.to_lowercase()).collect() };
        let end_dirs = lower(&_sb.snap());
        if end_dirs.difference(&lower(&snaps[0])).next().is_some() {
            in_new_dirs += 1;
        }
        let t: Vec<String> = touched(&done).into_iter().map(|s| s.to_lowercase()).collect();
        let uniq: std::collections::BTreeSet<_> = t.iter().collect();
        if uniq.len() < t.len() {
            same_path_twice += 1;
        }
        for (s, _) in &done {
            match s {
                Step::Rewrite(..) => edits += 1,
                Step::Create(..) => creates += 1,
                Step::Delete(..) => deletes += 1,
                Step::Move(..) => moves += 1,
            }
        }
    }
    eprintln!("COVERAGE: {total} steps = {edits} edits, {creates} creates, {deletes} deletes, {moves} moves; {in_new_dirs} turns left new folders; {same_path_twice} turns touched a path twice");
}

// ═══ Folders a change made, and single undos out of order ══════════════════

#[test]
fn defect_undoing_the_card_that_made_a_folder_strands_later_changes_that_need_it() {
    // create n/x.txt (makes n/), create n/y.txt, delete n/y.txt. n/ now holds x.txt only.
    // The user presses Undo on the FIRST card: x.txt goes and, being empty, so does n/.
    // The delete (needs n/ to put y.txt back into) and the create of y.txt can no longer
    // be undone from the app: "the folder for n/y.txt is gone". Nothing in the UI makes it.
    let sb = Sb::new("strand-delete");
    let ws = sb.ws();
    let r1 = create(&sb, &ws, "n/x.txt", "x\n");
    let r2 = create(&sb, &ws, "n/y.txt", "y\n");
    let r3 = delete(&sb, &ws, "n/y.txt");
    edit::undo(&r1, &sb.backups()).unwrap();
    let single = edit::undo(&r3, &sb.backups());
    let all = undo_all(&sb, &[r2.clone(), r3.clone()]);
    eprintln!("STRANDED: n exists = {}; undo of the delete: {single:?}; undo all of the rest: {all:?}", sb.has("n"));
    assert!(all.is_ok(), "the remaining two changes can no longer be undone: {all:?}");
}

#[test]
fn defect_undoing_the_card_that_made_a_folder_strands_a_later_move_out_of_it() {
    // The same through a move: create n/x.txt (makes n/), create n/y.txt, move n/y.txt out.
    let sb = Sb::new("strand-move");
    let ws = sb.ws();
    let r1 = create(&sb, &ws, "n/x.txt", "x\n");
    let r2 = create(&sb, &ws, "n/y.txt", "y\n");
    let r3 = mv(&sb, &ws, "n/y.txt", "z.txt");
    edit::undo(&r1, &sb.backups()).unwrap();
    let all = undo_all(&sb, &[r2, r3]);
    eprintln!("STRANDED (move): {all:?}");
    assert!(all.is_ok(), "the remaining two changes can no longer be undone: {all:?}");
}

// ═══ Smaller things ════════════════════════════════════════════════════════

#[test]
fn a_leftover_temporary_file_blocks_a_create_and_a_restore_observation() {
    // write_new uses create_new for its temporary file so a crash leftover is never
    // written over - at the price of blocking every later create or restore at that path.
    let sb = Sb::new("leftover");
    let ws = sb.ws();
    sb.put(".new.txt.odin-new", b"left by a crash");
    let plan = edit::plan_create(&ws, "new.txt", "hi\n").unwrap();
    let err = edit::apply(&plan, &sb.backups()).unwrap_err();
    eprintln!("LEFTOVER blocks create: {err}");
    assert!(!sb.has("new.txt") && sb.baks().is_empty());

    sb.put("gone.txt", b"bytes\n");
    let id = delete(&sb, &ws, "gone.txt");
    sb.put(".gone.txt.odin-new", b"left by a crash");
    let err = edit::undo(&id, &sb.backups()).unwrap_err();
    eprintln!("LEFTOVER blocks restore: {err}");
    assert!(!sb.has("gone.txt"));
    assert!(sb.backups().join(format!("{id}.bak")).exists(), "the backup stays");
}

#[test]
fn an_unusable_backups_folder_stops_every_kind_of_change_before_it_starts() {
    let sb = Sb::new("no-backups");
    let ws = sb.ws();
    sb.put("a.txt", b"one\n");
    sb.put("b.txt", b"bee\n");
    sb.put("c.txt", b"sea\n");
    fs::write(sb.backups(), b"a file where the folder should be").unwrap_or_else(|_| {
        fs::create_dir_all(sb.dir.clone()).unwrap();
        fs::write(sb.backups(), b"x").unwrap();
    });
    let init = sb.snap();
    for (name, plan) in [
        ("edit", edit::plan_replace(&ws, "a.txt", "one", "two").unwrap()),
        ("delete", edit::plan_delete(&ws, "b.txt").unwrap()),
        ("move", edit::plan_move(&ws, "c.txt", "new/c.txt").unwrap()),
        ("create", edit::plan_create(&ws, "d/e/new.txt", "x\n").unwrap()),
    ] {
        let err = edit::apply(&plan, &sb.backups()).unwrap_err();
        assert!(err.contains("creating"), "{name}: {err}");
        assert_same(&sb.snap(), &init, name);
    }
}

#[test]
fn sixty_four_edits_of_one_file_are_undone_in_one_go() {
    let sb = Sb::new("many");
    let ws = sb.ws();
    sb.put("a.txt", b"v0\r\n");
    let init = sb.snap();
    let ids: Vec<String> = (0..64).map(|i| edit(&sb, &ws, "a.txt", &format!("v{i}"), &format!("v{}", i + 1))).collect();
    let out = undo_all(&sb, &ids).unwrap();
    assert_eq!(out.restored.len(), 64);
    assert_same(&sb.snap(), &init, "64 edits");
    assert!(sb.baks().is_empty());
}

#[test]
fn creates_get_crlf_even_for_files_that_want_lf_observation() {
    // A shell script created on Windows is written with CRLF (new_file_style). This repo
    // keeps *.sh LF on purpose (.gitattributes, commit bd0a7fb).
    let sb = Sb::new("crlf-sh");
    let ws = sb.ws();
    let plan = edit::plan_create(&ws, "run.sh", "#!/bin/sh\necho hi\n").unwrap();
    eprintln!("CREATE run.sh bytes: {:?}", String::from_utf8_lossy(&plan.after));
    assert_eq!(plan.after, if cfg!(windows) { b"#!/bin/sh\r\necho hi\r\n".to_vec() } else { b"#!/bin/sh\necho hi\n".to_vec() });
}

#[test]
fn property_edit_keeps_every_byte_it_was_not_asked_to_change() {
    let mut n_empty_new = 0;
    for seed in seeds() {
        let mut rng = Rng::new(seed ^ 0xED17);
        let sb = Sb::new("prop-replace");
        let ws = sb.ws();
        let n = 6 + rng.below(10);
        let lines: Vec<String> = (0..n).map(|i| format!("line {i} token{:x} \u{e9} \u{fc}n\u{ef} \t end", rng.next() % 100000)).collect();
        let eol = if rng.chance(50) { "\r\n" } else { "\n" };
        let mut text = lines.join(eol);
        if rng.chance(70) {
            text.push_str(eol);
        }
        let mut bytes: Vec<u8> = if rng.chance(30) { b"\xEF\xBB\xBF".to_vec() } else { Vec::new() };
        let bom_len = bytes.len();
        bytes.extend_from_slice(text.as_bytes());
        sb.put("f.txt", &bytes);

        let i = rng.below(n - 1);
        let old_lf = match rng.below(3) {
            0 => format!("line {i} token"),
            1 => lines[i].clone(),
            _ => format!("{}\n{}", lines[i], lines[i + 1]),
        };
        let new_lf = match rng.below(4) {
            0 => String::new(),
            1 => "X".to_string(),
            2 => "A\nB\nC".to_string(),
            _ => "\u{fc}n\u{ef}\tcode".to_string(),
        };
        let plan = edit::plan_replace(&ws, "f.txt", &old_lf, &new_lf)
            .unwrap_or_else(|e| panic!("seed {seed}: plan refused {old_lf:?} -> {new_lf:?}: {e}"));
        let id = edit::apply(&plan, &sb.backups()).unwrap().id;
        let now = sb.get("f.txt");
        let crlf = eol == "\r\n";
        assert_eq!(now.starts_with(b"\xEF\xBB\xBF"), bom_len == 3, "seed {seed}: BOM kept as it was");
        if crlf {
            let s = String::from_utf8_lossy(&now[bom_len..]).into_owned();
            assert!(s.matches('\n').count() == s.matches("\r\n").count(), "seed {seed}: every line ending is still CRLF");
        } else {
            assert!(!now.contains(&b'\r'), "seed {seed}: an LF file stays LF");
        }
        if new_lf.is_empty() {
            n_empty_new += 1;
        } else {
            let old_b = old_lf.replace('\n', eol);
            let new_b = new_lf.replace('\n', eol);
            let at = bom_len + text.find(&old_b).expect("old occurs once");
            let want = [&bytes[..at], new_b.as_bytes(), &bytes[at + old_b.len()..]].concat();
            assert!(now == want, "seed {seed}: bytes differ from a plain splice\n got: {:?}\nwant: {:?}", String::from_utf8_lossy(&now), String::from_utf8_lossy(&want));
        }
        edit::undo(&id, &sb.backups()).unwrap();
        assert!(sb.get("f.txt") == bytes, "seed {seed}: undo gave back different bytes");
    }
    eprintln!("PROPERTY edit bytes: {n_empty_new} deletions checked for invariants only");
}

// ═══ Memory: what a move of a big file costs ═══════════════════════════════

mod tracking {
    //! Counts the bytes the CURRENT thread has allocated, and the most it held at once,
    //! so a test can say how much memory one call needed even while others run.
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    thread_local! {
        static CUR: Cell<usize> = const { Cell::new(0) };
        static PEAK: Cell<usize> = const { Cell::new(0) };
    }

    pub struct Track;

    fn grow(by: usize) {
        let _ = CUR.try_with(|c| {
            let n = c.get() + by;
            c.set(n);
            let _ = PEAK.try_with(|p| {
                if n > p.get() {
                    p.set(n)
                }
            });
        });
    }

    fn shrink(by: usize) {
        let _ = CUR.try_with(|c| c.set(c.get().saturating_sub(by)));
    }

    unsafe impl GlobalAlloc for Track {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            let p = unsafe { System.alloc(l) };
            if !p.is_null() {
                grow(l.size());
            }
            p
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            unsafe { System.dealloc(p, l) };
            shrink(l.size());
        }
        unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
            let q = unsafe { System.realloc(p, l, new) };
            if !q.is_null() {
                shrink(l.size());
                grow(new);
            }
            q
        }
    }

    /// Run `f`; return its result and the most bytes this thread held beyond what it held before.
    pub fn peak_of<T>(f: impl FnOnce() -> T) -> (T, usize) {
        let base = CUR.with(|c| c.get());
        PEAK.with(|p| p.set(base));
        let out = f();
        let peak = PEAK.with(|p| p.get());
        (out, peak.saturating_sub(base))
    }
}

#[global_allocator]
static ALLOC: tracking::Track = tracking::Track;

#[test]
fn defect_applying_a_move_reads_the_whole_file_into_memory() {
    // plan_move hashes in 64 KiB chunks ("moving a file needs its hash but not its bytes,
    // and has no size limit", edit.rs:587-589). apply() hashes it in chunks too, and then
    // reads the entire file with std::fs::read for its last-moment re-check (edit.rs:1436).
    let sb = Sb::new("big-move");
    let ws = sb.ws();
    let size: u64 = 128 * 1024 * 1024;
    let f = fs::File::create(sb.root().join("big.bin")).unwrap();
    f.set_len(size).unwrap();
    drop(f);
    let (plan, plan_peak) = tracking::peak_of(|| edit::plan_move(&ws, "big.bin", "sub/big.bin").unwrap());
    let (done, apply_peak) = tracking::peak_of(|| edit::apply(&plan, &sb.backups()).unwrap());
    eprintln!(
        "BIG MOVE of {} MiB: planning held at most {} KiB, applying held at most {} MiB",
        size >> 20,
        plan_peak >> 10,
        apply_peak >> 20
    );
    assert!(sb.has("sub/big.bin") && !sb.has("big.bin"));
    edit::undo(&done.id, &sb.backups()).unwrap();
    assert!(sb.has("big.bin"));
    assert!(plan_peak < 4 << 20, "planning is chunked");
    assert!(apply_peak < 4 << 20, "apply() held {} MiB for a move of {} MiB", apply_peak >> 20, size >> 20);
}

#[test]
fn a_clock_that_stepped_back_refuses_the_turn_and_writes_nothing() {
    // H6-results "Accepted": ids are wall-clock times; if the clock steps back during a
    // turn the order check refuses. Simulated by giving the second edit an earlier id.
    let sb = Sb::new("clock-back");
    let ws = sb.ws();
    sb.put("a.txt", b"one two\n");
    sb.put("b.txt", b"red\n");
    let r1 = edit(&sb, &ws, "a.txt", "one", "1");
    let r2 = edit(&sb, &ws, "a.txt", "two", "2");
    let r3 = mv(&sb, &ws, "b.txt", "sub/b.txt");
    let earlier = format!("{:x}", u128::from_str_radix(&r1, 16).unwrap() - 1_000_000_000);
    for ext in ["json", "bak"] {
        fs::rename(sb.backups().join(format!("{r2}.{ext}")), sb.backups().join(format!("{earlier}.{ext}"))).unwrap();
    }
    let applied = sb.snap();
    let err = undo_all(&sb, &[r1.clone(), earlier.clone(), r3.clone()]).unwrap_err();
    eprintln!("CLOCK STEPPED BACK: {err}");
    assert!(err.starts_with("Nothing was undone:"), "{err}");
    assert_same(&sb.snap(), &applied, "nothing written");
    // undoing them by hand, newest first, still works
    edit::undo(&r3, &sb.backups()).unwrap();
    edit::undo(&earlier, &sb.backups()).unwrap();
    edit::undo(&r1, &sb.backups()).unwrap();
    assert_eq!(sb.get("a.txt"), b"one two\n");
    assert_eq!(sb.get("b.txt"), b"red\n");
}

