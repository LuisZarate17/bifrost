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
//!    disk no longer matches it. It checks at the start, and once more with the new
//!    bytes already written beside the file, immediately before the rename that
//!    replaces it ([`write_replacing`]); [`undo`] does the same. On Windows that last
//!    read holds the file against *in-place* writers until the rename is done, so for
//!    them there is no gap: a save in that moment fails loudly instead of being
//!    replaced. An editor that saves by writing a temporary file and renaming it over
//!    the target (JetBrains safe-write, Vim, Sublime) is not stopped by that hold, and
//!    if its rename lands between our re-read and our rename, its save is replaced.
//!    Elsewhere "compare, then replace" cannot be one step on a plain file, and a save
//!    landing in the few microseconds between the read and the rename is still replaced.
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

/// The biggest file a delete will take. Its bytes are copied to `%APPDATA%` first
/// so undo can put it back, and a runaway request should not fill the disk.
pub const MAX_DELETE_BYTES: u64 = 16 * 1024 * 1024;

/// How many folders one create or move may make. Eight is deeper than any module
/// tree this app is pointed at, and a longer chain is a mistake in the path.
pub const MAX_NEW_FOLDERS: usize = 8;

// ── the folder ──────────────────────────────────────────────────────────────

/// The one folder the model may change files in.
///
/// Held canonicalised — on Windows that is the `\\?\C:\…` verbatim form, with
/// the on-disk case — so every containment check compares like with like.
#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
}

/// Where [`Workspace::locate`] found a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Located {
    /// An existing file or folder in the project, canonical.
    Inside(PathBuf),
    /// Exists, or would, somewhere else — canonical, or as near as could be told.
    Outside(PathBuf),
    /// Nothing is there, and the place it would be is in the project.
    MissingInside,
}

/// Where something that does not exist yet will go: the canonical path, and the
/// folders under the deepest existing one that have to be made first
/// (shallowest first — the order they are made in).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub path: PathBuf,
    pub new_dirs: Vec<PathBuf>,
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

    /// Where a path the model gave for a **read** lands, for deciding whether
    /// the read needs asking about (`tools::approval`).
    ///
    /// Shares [`join`](Self::join) and `canonicalize` with
    /// [`resolve_existing`](Self::resolve_existing), so `..`, symlinks and
    /// junctions resolve the same way. Inside or outside is the same component-wise
    /// `starts_with` test [`contain_with`](Self::contain_with) makes — and **none of
    /// its edit-only refusals** (`.git`, streams, `.cargo`, the toolchain files,
    /// `.npmrc`, app data) apply: whether a read of those is automatic is
    /// `search::visible` and `search::secret_name`'s call, not this one's.
    ///
    /// A path that does not exist is placed by its nearest existing ancestor,
    /// canonicalised, with the missing tail put back — so `linked/missing.txt`
    /// under a junction that points out is `Outside`, not a missing file in the
    /// project. A tail holding `..` is folded in lexically for the same reason.
    ///
    /// A network or device path (`\\host\share`, `\\.\…`) is `Outside` as given,
    /// without a filesystem call: resolving one is how Windows opens a connection to
    /// the host, and nothing has been approved yet. See [`remote_or_device`].
    pub fn locate(&self, requested: &str) -> Located {
        let Ok(joined) = self.join_any(requested) else {
            return Located::MissingInside;
        };
        if remote_or_device(&joined, &self.root) {
            return Located::Outside(joined);
        }
        if let Ok(real) = std::fs::canonicalize(&joined) {
            return if real.starts_with(&self.root) { Located::Inside(real) } else { Located::Outside(real) };
        }
        let mut tail = Vec::new();
        let mut base = joined.as_path();
        let real_base = loop {
            let Some(parent) = base.parent() else {
                break joined.clone();
            };
            if let Some(name) = base.file_name() {
                tail.push(name.to_owned());
            } else {
                tail.push(std::ffi::OsString::from(".."));
            }
            base = parent;
            if let Ok(real) = std::fs::canonicalize(base) {
                break real;
            }
        };
        let mut real = real_base;
        for part in tail.into_iter().rev() {
            if part == ".." {
                real.pop();
            } else {
                real.push(part);
            }
        }
        if real.starts_with(&self.root) { Located::MissingInside } else { Located::Outside(real) }
    }

    /// Resolve a path for a file that **must not exist yet** — a create, or the
    /// destination of a move — along with the folders it needs made.
    ///
    /// Folders that are missing are part of the plan, not an error: the card lists
    /// them and undo removes them again. (Until H9 this refused, because making
    /// directories on the model's say-so was one more thing to approve. It is not
    /// one more thing now — it is the same approval, and a module split into a
    /// sub-module cannot be done without it.)
    ///
    /// The walk goes up to the **deepest ancestor that exists**, judged with
    /// `symlink_metadata` so a dangling link counts as existing — and is then
    /// refused rather than created through. That ancestor is canonicalised and must
    /// be a folder inside the root, which is what a junction out fails. The missing
    /// tail below it is checked by name: no `..`, at most [`MAX_NEW_FOLDERS`] levels,
    /// and nothing Windows would mangle (see [`unsafe_name`]) — the `\\?\` root this
    /// workspace is held under turns the usual normalisation off, so `con` or `a.`
    /// would be created literally. Then [`contain`](Self::contain) runs on the whole
    /// final path, so `.git`, `.cargo`, streams and app data are refused for every
    /// new folder too.
    ///
    /// `exists` is appended to "already exists" when something is already there.
    pub fn resolve_target(&self, requested: &str, exists: &str) -> Result<Target, String> {
        let joined = self.join(requested)?;
        let shown = clean(requested);
        if let Ok(meta) = std::fs::symlink_metadata(&joined) {
            return Err(if meta.is_dir() {
                format!("{shown} is an existing folder{exists}")
            } else {
                format!("{shown} already exists{exists}")
            });
        }
        // A `..` is fine after a folder that exists — Windows folds it away before
        // it touches the disk — and refused after one that does not: `nope/../x`
        // would otherwise mean different things to the check and to the write.
        let mut walked = PathBuf::new();
        for c in joined.components() {
            match c {
                Component::ParentDir => {
                    if std::fs::symlink_metadata(&walked).is_err() {
                        return Err(format!("{shown} has `..` in the part of the path that does not exist yet"));
                    }
                    walked.pop();
                }
                Component::CurDir => {}
                other => walked.push(other.as_os_str()),
            }
        }
        let joined = lexical(&joined);
        joined.file_name().ok_or_else(|| format!("{shown} is not a file name"))?;

        // Up to the deepest ancestor that exists, collecting the names passed.
        let mut tail: Vec<std::ffi::OsString> = Vec::new();
        let mut base = joined.as_path();
        let real_base = loop {
            let Some(name) = base.file_name() else {
                return Err(format!("{shown} has `..` in the part of the path that does not exist yet"));
            };
            tail.push(name.to_owned());
            let Some(parent) = base.parent() else {
                return Err(format!("{shown} has no folder"));
            };
            base = parent;
            if std::fs::symlink_metadata(base).is_ok() {
                break std::fs::canonicalize(base).map_err(|_| {
                    format!("{shown} is in a folder that is a link that leads nowhere")
                })?;
            }
        };
        if !real_base.starts_with(&self.root) {
            return Err(format!(
                "{shown} is outside the project folder ({}). Only files inside it can be changed.",
                self.root_display()
            ));
        }
        if !real_base.is_dir() {
            return Err(format!("{shown} is inside {}, which is a file, not a folder", self.relative(&real_base)));
        }
        tail.reverse();
        if tail.len() - 1 > MAX_NEW_FOLDERS {
            return Err(format!(
                "{shown} would make {} new folders; at most {MAX_NEW_FOLDERS} are made at once",
                tail.len() - 1
            ));
        }
        for part in &tail {
            if let Some(why) = unsafe_name(&part.to_string_lossy()) {
                return Err(format!("{shown}: {why}"));
            }
        }
        let mut new_dirs = Vec::new();
        let mut real = real_base;
        for (i, part) in tail.iter().enumerate() {
            real.push(part);
            if i + 1 < tail.len() {
                new_dirs.push(real.clone());
            }
        }
        self.contain(&real, requested)?;
        Ok(Target { path: real, new_dirs })
    }

    /// Resolve a path for a file that **must exist and may be deleted or moved**:
    /// a real file, reached by its own name.
    ///
    /// [`resolve_existing`](Self::resolve_existing) follows links to where they
    /// point, which is right for reading and editing and wrong here: `link/a.rs`
    /// would name a file that lives somewhere else. So the canonical path must be the
    /// requested one with `..` folded in, compared case-insensitively — a link
    /// anywhere along the way, or the link itself, makes them differ and is refused.
    /// (A short `PROGRA~1` name differs too, which is refused the same way, and is
    /// the safe side.) Read-only files are refused, since removing one is a decision
    /// the user did not make by asking.
    ///
    /// `done` is how the refusal reads: "deleted" or "moved".
    pub fn resolve_source(&self, requested: &str, done: &str) -> Result<PathBuf, String> {
        let joined = self.join(requested)?;
        let shown = clean(requested);
        if std::fs::symlink_metadata(&joined).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(format!("{shown} is a link, so it is never {done}"));
        }
        let real = self.resolve_existing(requested)?;
        if plain(&real).to_lowercase() != plain(&lexical(&joined)).to_lowercase() {
            return Err(format!(
                "{shown} is reached through a link, so it is never {done}. Use the file's own path: {}",
                self.relative(&real)
            ));
        }
        let meta = std::fs::symlink_metadata(&real).map_err(|e| format!("cannot look at {shown}: {e}"))?;
        if meta.permissions().readonly() {
            return Err(format!("{shown} is read-only, so it is never {done}"));
        }
        Ok(real)
    }

    /// [`join_any`](Self::join_any), refusing a network or device path outright.
    /// Callers canonicalise what this returns, and canonicalising `\\host\share`
    /// reaches out to `host` — before [`contain`] has had a say.
    fn join(&self, requested: &str) -> Result<PathBuf, String> {
        let joined = self.join_any(requested)?;
        if remote_or_device(&joined, &self.root) {
            return Err(format!(
                "{} is outside the project folder ({}). Only files inside it can be changed.",
                clean(requested),
                self.root_display()
            ));
        }
        Ok(joined)
    }

    /// Relative paths are relative to the workspace; absolute ones stand as given
    /// and are checked by [`contain`] like any other.
    fn join_any(&self, requested: &str) -> Result<PathBuf, String> {
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

/// Whether `path` names a network share or a device (`\\host\share`, `\\?\UNC\…`,
/// `\\.\…`, `\\?\GLOBALROOT\…`) that is not the project folder's own place.
///
/// **A pure function of the text**, because it exists to run before anything
/// touches the filesystem: `canonicalize` on a UNC path makes Windows open an SMB
/// connection and send the user's NTLM hash to whoever runs the host. Such a path
/// is only trusted when it lies lexically under `root` (compared in plain form,
/// case-insensitively, component by component, with no `..`) — a project folder
/// that itself lives on a share must keep working.
pub fn remote_or_device(path: &Path, root: &Path) -> bool {
    use std::path::Prefix;
    let remote = match path.components().next() {
        Some(Component::Prefix(p)) => match p.kind() {
            Prefix::UNC(..) | Prefix::VerbatimUNC(..) | Prefix::DeviceNS(_) | Prefix::Verbatim(_) => true,
            Prefix::VerbatimDisk(_) | Prefix::Disk(_) => false,
        },
        _ => false,
    };
    if !remote {
        return false;
    }
    let parts = |p: &Path| -> Vec<String> {
        PathBuf::from(plain(p))
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
            .collect()
    };
    let (under, base) = (parts(path), parts(root));
    let lexically_inside = !path.components().any(|c| c == Component::ParentDir)
        && !base.is_empty()
        && under.len() >= base.len()
        && under[..base.len()] == base[..];
    !lexically_inside
}

/// Open `real` for an automatic read, only if it is still the file that was
/// approved — `None` means "do not read this unasked".
///
/// A path is not a file: between the decision and the open, a link can be swapped
/// in, and a hard link makes a file inside the folder *be* a file outside it
/// (`mklink /H` needs no admin rights), which no check of its name can see. So, on
/// Windows:
///
/// 1. canonicalise `real` again and require it is still the same path;
/// 2. open it, and ask the **open handle** where it really is
///    (`GetFinalPathNameByHandleW`) and require that is `real`;
/// 3. ask the handle how many names the file has (`GetFileInformationByHandle`)
///    and require exactly one — a second name is a hard link.
///
/// The caller reads from the returned handle, so nothing can change between the
/// checks and the read. Where this is not Windows nothing is checkable, so nothing
/// is automatic.
#[cfg(windows)]
pub fn open_checked(real: &Path) -> Option<std::fs::File> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_NAME_NORMALIZED, GetFileInformationByHandle,
        GetFinalPathNameByHandleW,
    };

    if std::fs::canonicalize(real).ok().as_deref() != Some(real) {
        return None;
    }
    let file = std::fs::File::open(real).ok()?;
    let handle = HANDLE(file.as_raw_handle());

    let mut buf = vec![0u16; 1024];
    let at = loop {
        // SAFETY: `handle` belongs to `file`, which is open until the end of this
        // function, and `buf` is a live buffer whose length is passed with it.
        // FILE_NAME_NORMALIZED is 0, which is also the DOS volume form `canonicalize` gives.
        let n = unsafe { GetFinalPathNameByHandleW(handle, &mut buf, FILE_NAME_NORMALIZED) } as usize;
        if n == 0 {
            return None;
        }
        if n <= buf.len() {
            break PathBuf::from(String::from_utf16_lossy(&buf[..n]));
        }
        buf.resize(n + 1, 0);
    };
    if at != real {
        return None;
    }

    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: as above; `info` is a live, correctly sized out-parameter.
    if unsafe { GetFileInformationByHandle(handle, &mut info) }.is_err() || info.nNumberOfLinks != 1 {
        return None;
    }
    Some(file)
}

#[cfg(not(windows))]
pub fn open_checked(_real: &Path) -> Option<std::fs::File> {
    None
}

/// `.cargo/…`, `rust-toolchain(.toml)` and `.npmrc`, anywhere in the folder.
fn decides_what_the_check_runs(inside: &Path) -> bool {
    let named = |n: &std::ffi::OsStr, what: &str| n.eq_ignore_ascii_case(what);
    inside.components().any(|c| matches!(c, Component::Normal(n) if named(n, ".cargo")))
        || inside.file_name().is_some_and(|n| {
            named(n, "rust-toolchain") || named(n, "rust-toolchain.toml") || named(n, ".npmrc")
        })
}

/// `%APPDATA%\odin`, canonical, if it exists. [`crate::search`] skips it too.
pub(crate) fn app_data() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("APPDATA")?).join("odin");
    std::fs::canonicalize(dir).ok()
}

/// Models wrap paths in quotes or backticks surprisingly often.
pub(crate) fn clean(requested: &str) -> String {
    requested
        .trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == '`')
        .trim()
        .to_string()
}

/// `\\?\C:\x` → `C:\x`, and `\\?\UNC\server\share` → `\\server\share`, for
/// anything a person reads — and anything handed to a program as a folder,
/// which the verbatim form of a network share would reach as a relative path.
pub fn plain(p: &Path) -> String {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or_else(|| s.into_owned())
}

/// `p` with `.` and `..` folded in by text alone, touching no filesystem.
fn lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Why Windows would not keep `name` as written, if it would not.
///
/// The workspace root is the `\\?\` form, which switches Win32's name clean-up
/// off: `notes.` and `notes ` are created literally (and then cannot be opened or
/// deleted by most tools), `con` and `nul` name devices, and `<>"|?*` are not
/// allowed. None of them is a file a project is made of.
fn unsafe_name(name: &str) -> Option<String> {
    if name.ends_with('.') || name.ends_with(' ') {
        return Some(format!("\"{name}\" ends with a dot or a space, which Windows cannot handle"));
    }
    if name.chars().any(|c| matches!(c, '<' | '>' | '"' | '|' | '?' | '*') || (c as u32) < 32) {
        return Some(format!("\"{name}\" has a character Windows does not allow in names"));
    }
    let stem = name.split('.').next().unwrap_or(name).trim_end().to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0');
    device.then(|| format!("\"{name}\" is the name of a Windows device, not a file"))
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

/// [`fingerprint`] of a file's contents, read in chunks — what a move compares,
/// since moving a file needs its hash but not its bytes, and has no size limit.
pub fn fingerprint_file(path: &Path) -> std::io::Result<u64> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            return Ok(h);
        }
        for b in &buf[..n] {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
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

/// What a change does to the project. Written into every undo record, so it must
/// keep its spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    Edit,
    Create,
    Delete,
    Move,
}

/// An edit worked out in full, before anyone is asked about it.
///
/// Holds the exact bytes that will be written. Nothing downstream re-derives
/// them — [`apply`] writes `after` or nothing — so the diff you approve and the
/// file you get cannot disagree.
///
/// Since H9 a plan can also be a delete or a move. A delete has no `after`; a
/// move has no bytes at all, only the hash of the file it carries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    /// The file changed, created or deleted — and the *source* of a move.
    pub path: PathBuf,
    /// Relative to the workspace, with forward slashes. What the card shows.
    pub display: String,
    /// True when this creates a file rather than changing one.
    pub create: bool,
    pub op: Op,
    /// Hash of the file as it was when planned; `None` for a new file.
    pub before: Option<u64>,
    pub after: Vec<u8>,
    pub hunks: Vec<Hunk>,
    pub added: usize,
    pub removed: usize,
    /// Where a move puts the file, and how to show it. `None` for everything else.
    pub to: Option<PathBuf>,
    pub to_display: Option<String>,
    /// Folders to make first, shallowest first, and how to show them.
    pub make_dirs: Vec<PathBuf>,
    pub folders: Vec<String>,
    /// The size of the file a delete or a move carries; 0 otherwise.
    pub size: u64,
}

impl Plan {
    /// A plan for `path` that changes nothing yet; the planners fill in the rest.
    fn new(op: Op, path: PathBuf, display: String) -> Self {
        Self {
            path,
            display,
            create: op == Op::Create,
            op,
            before: None,
            after: Vec::new(),
            hunks: Vec::new(),
            added: 0,
            removed: 0,
            to: None,
            to_display: None,
            make_dirs: Vec::new(),
            folders: Vec::new(),
            size: 0,
        }
    }
}

/// Replace one exact piece of text with another.
///
/// `old` must occur **exactly once**. None means the model misremembered the
/// file; more than one means it did not say which. Either way nothing is
/// written, and the message says how to fix the request — it goes back to the
/// model, which can read the file again and retry.
pub fn plan_replace(ws: &Workspace, path: &str, old: &str, new: &str) -> Result<Plan, String> {
    plan_replace_in(ws, path, old, new, false)
}

/// The one thing a failed edit of a protected file says, whatever the reason:
/// no text of it, no count, no size, no encoding (review R-01).
fn protected_miss(ws: &Workspace, real: &Path) -> String {
    format!(
        "old_text does not match exactly one place in {}. It is a protected file, so its text is \
         not shown; ask the user to allow a read of it first.",
        ws.relative(real)
    )
}

/// [`plan_replace`], for a file whose text must not reach the model unasked
/// (`protected`, decided by [`crate::tools::protected`]).
///
/// **Found in review (R-01):** an edit is planned before any card, and a plan
/// that fails goes straight back to the model. On a miss the error carried the
/// closest lines of the file (up to 60), and on several matches how many there
/// were, so `edit_file` on `.env` with a wrong `old_text` read the file with
/// no card, where a direct read would have asked. A protected file's
/// errors therefore carry no text of it and no count. A plan that succeeds is
/// unchanged: its diff card is the approval.
///
/// Callers do not choose `protected` themselves; they go through
/// [`crate::tools::plan_edit`], which is shared by the app and the probes.
pub fn plan_replace_in(
    ws: &Workspace,
    path: &str,
    old: &str,
    new: &str,
    protected: bool,
) -> Result<Plan, String> {
    let real = ws.resolve_existing(path)?;
    // Decided here, on the data, not only by the callers that remember to ask:
    // whatever plans an edit — the app, a probe, a test — a secret-named,
    // hidden or hard-linked file never answers a miss with its own text. The
    // caller can only add to this (`ask_every_call`), never take it away.
    let protected = protected || crate::tools::protected(Some(ws), &real, false);
    let bytes = std::fs::read(&real).map_err(|e| format!("cannot read {}: {e}", clean(path)))?;
    // Why a file cannot be edited — its exact size, binary, not UTF-8, mixed
    // line endings — is itself a fact about its content; a protected file
    // gives the same one answer as any other miss.
    let text = Text::parse(&bytes).map_err(|e| if protected { protected_miss(ws, &real) } else { e })?;

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

    // A protected file (H5, review R-01) says one thing whenever the edit cannot
    // be planned — never which way it missed: "not found" against "more than one
    // place" would itself tell the model something about text it was not allowed
    // to read.
    if count != 1 && protected {
        return Err(protected_miss(ws, &real));
    }

    if count == 0 {
        // The header of a ranged read is not in the file. Never stripped silently:
        // `new_text` may carry it too, and guessing which half to clean is how a
        // header ends up written into someone's source.
        if old.lines().next().is_some_and(crate::tools::is_read_header) {
            return Err(format!(
                "old_text starts with the read's header line, which is not part of {}. Copy only \
                 the file's own text, starting after that line.",
                ws.relative(&real)
            ));
        }
        // Past the size `closest` gives up at, "read the file again" is the very
        // thing that cannot work, so the model is told how to read part of it.
        let again = if text.body.len() > CLOSEST_MAX_BODY {
            " Read those lines again with start_line and end_line, and copy the text from them."
        } else {
            " Read the file again and copy the text from it."
        };
        return Err(format!(
            "old_text was not found in {}. It must match the file exactly, including \
             indentation.{}",
            ws.relative(&real),
            closest(&text.body, &old)
                .map(|(from, to, s)| format!(
                    " The closest text in the file is lines {from}–{to}:\n{s}\nCopy it exactly \
                     as old_text."
                ))
                .unwrap_or_else(|| again.to_string())
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
        before: Some(fingerprint(&bytes)),
        after: text.render(&body),
        hunks,
        added,
        removed,
        ..Plan::new(Op::Edit, real.clone(), ws.relative(&real))
    })
}

/// The largest file [`closest`] compares against.
const CLOSEST_MAX_BODY: usize = 64 * 1024;

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
    const MAX_LINES: usize = 60;
    const MIN_RATIO: f32 = 0.6;

    let want_lines = wanted.trim_end_matches('\n').lines().count().max(1);
    let lines: Vec<&str> = body.lines().collect();
    if body.len() > CLOSEST_MAX_BODY || want_lines > MAX_LINES || lines.len() < want_lines {
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

/// Create a file that does not exist yet, in a folder that may not either.
pub fn plan_create(ws: &Workspace, path: &str, content: &str) -> Result<Plan, String> {
    let target = ws.resolve_target(path, " — change it with edit_file rather than creating it")?;
    let body = from_model(content);
    if body.len() > MAX_EDIT_BYTES {
        return Err(format!("that content is over the {MAX_EDIT_BYTES} byte limit"));
    }
    let (hunks, added, removed) = diff("", &body);
    Ok(Plan {
        after: new_file_style().render(&body),
        hunks,
        added,
        removed,
        folders: folders_of(ws, &target),
        make_dirs: target.new_dirs,
        ..Plan::new(Op::Create, target.path.clone(), ws.relative(&target.path))
    })
}

/// The new folders of a target as the card lists them: relative, with a trailing slash.
fn folders_of(ws: &Workspace, target: &Target) -> Vec<String> {
    target.new_dirs.iter().map(|d| format!("{}/", ws.relative(d))).collect()
}

/// Delete a file — **only a file, only a real one, and only after its bytes are
/// kept**.
///
/// What is refused, and why:
/// - a folder: no recursive delete exists anywhere in this module, and a model that
///   means a folder can name its files;
/// - a link, or a file reached through one ([`Workspace::resolve_source`]);
/// - a read-only file;
/// - anything over [`MAX_DELETE_BYTES`], since the backup is a copy in `%APPDATA%`.
///
/// Whatever the delete refuses is refused here, at planning, before anyone is asked.
pub fn plan_delete(ws: &Workspace, path: &str) -> Result<Plan, String> {
    let real = ws.resolve_source(path, "deleted")?;
    let shown = clean(path);
    let size = std::fs::metadata(&real).map_err(|e| format!("cannot read {shown}: {e}"))?.len();
    if size > MAX_DELETE_BYTES {
        // Not the exact size: before any card, that would say something about a
        // file the model may not be allowed to read (review R-01).
        return Err(format!(
            "{shown} is over the {MAX_DELETE_BYTES} byte limit for a delete, which has to keep a copy"
        ));
    }
    let bytes = std::fs::read(&real).map_err(|e| format!("cannot read {shown}: {e}"))?;
    Ok(Plan {
        before: Some(fingerprint(&bytes)),
        size,
        ..Plan::new(Op::Delete, real.clone(), ws.relative(&real))
    })
}

/// Move a file to a path that does not exist yet, making its folders.
///
/// The destination is a **file path**: a destination that is an existing folder is
/// refused, with the whole path suggested, because "put it in that folder" and
/// "replace it with that folder" look the same to a model. A destination that
/// differs from the source only in case is refused as the same file — Windows would
/// either rename it or fail, and neither is what was asked. Nothing is ever
/// overwritten: [`apply`] moves with [`move_new`].
pub fn plan_move(ws: &Workspace, from: &str, to: &str) -> Result<Plan, String> {
    let real = ws.resolve_source(from, "moved")?;
    let (shown_from, shown_to) = (clean(from), clean(to));
    let dest = ws.join(to)?;
    if plain(&real).to_lowercase() == plain(&lexical(&dest)).to_lowercase() {
        return Err(format!(
            "{shown_to} is the same file as {shown_from} on Windows, which ignores case — give a different name"
        ));
    }
    if std::fs::symlink_metadata(&dest).is_ok_and(|m| m.is_dir()) {
        let name = real.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        return Err(format!(
            "{shown_to} is an existing folder. To move the file into it, give the whole path: {}/{name}",
            shown_to.trim_end_matches(['/', '\\'])
        ));
    }
    let target = ws.resolve_target(to, " — nothing is overwritten, so choose another name")?;
    let size = std::fs::metadata(&real).map_err(|e| format!("cannot read {shown_from}: {e}"))?.len();
    let before = fingerprint_file(&real).map_err(|e| format!("cannot read {shown_from}: {e}"))?;
    Ok(Plan {
        before: Some(before),
        size,
        to: Some(target.path.clone()),
        to_display: Some(ws.relative(&target.path)),
        folders: folders_of(ws, &target),
        make_dirs: target.new_dirs,
        ..Plan::new(Op::Move, real.clone(), ws.relative(&real))
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
        before: Some(fingerprint(&bytes)),
        after: text.render(&body),
        hunks,
        added,
        removed,
        ..Plan::new(Op::Edit, real.clone(), ws.relative(&real))
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
    pub op: Op,
    /// Where a move put the file, relative to the project folder.
    pub to: Option<String>,
    /// The folders this made, as the card listed them.
    pub folders: Vec<String>,
}

/// Kept beside each backup so [`undo`] knows what it is restoring and can tell
/// whether the file has moved on since.
///
/// **Written so an older build refuses these records rather than misreading
/// them.** `create` stays, and `path` is still the file the change was about — the
/// source of a move, the file a delete removed — so an older [`undo`] looks for a
/// file that is no longer there and stops. A delete writes `create: false` and the
/// deleted file's hash as `after`; a move writes `create: false` and the moved
/// file's. With `create: true` an older build that found an identical file back at
/// `path` would delete it, which is the one mistake this layout avoids.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    path: PathBuf,
    display: String,
    create: bool,
    /// Hash of what the change left at `path` (at `to` for a move, which carries the
    /// file's own hash; the deleted file's for a delete). Undo only proceeds if the
    /// file still is this.
    after: u64,
    /// Hash of the file before the edit, so [`undo_turn`] can check a `.bak`
    /// directly — the oldest edit of a chain has no earlier record to compare it
    /// with. `None` for a create, and for records written before this existed.
    #[serde(default)]
    before: Option<u64>,
    /// What the change was. `None` in records written before H9: an edit, or a
    /// create where `create` says so.
    #[serde(default)]
    kind: Option<Op>,
    /// Where a move put the file, and how it was shown.
    #[serde(default)]
    to: Option<PathBuf>,
    #[serde(default)]
    to_display: Option<String>,
    /// The folders this change made, **deepest first** — the order they are removed
    /// in, and only if each is empty.
    #[serde(default)]
    made_dirs: Vec<PathBuf>,
}

impl Record {
    fn op(&self) -> Op {
        self.kind.unwrap_or(if self.create { Op::Create } else { Op::Edit })
    }

    fn to_shown(&self) -> String {
        self.to_display
            .clone()
            .or_else(|| self.to.as_deref().map(plain))
            .unwrap_or_default()
    }
}

/// `%APPDATA%\odin\backups\`, beside the conversations.
pub fn backups_dir() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join("odin").join("backups"))
}

/// Write `bytes` over `path` — **only if it is still exactly what the caller saw**
/// (it hashes to `expect`) — via a temporary file in the same folder and a rename,
/// so a crash leaves either the old file or the new one, never half.
///
/// **Replaces what is there**, which is what an edit means — and why a create, a
/// restore and a move never use it: see [`write_new`] and [`move_new`].
///
/// An edit checks its file when it starts, and then makes folders, writes a backup
/// and a record before it replaces anything — long enough for an editor's save to
/// land in between, and that save would be replaced by bytes made from the older
/// file, in neither the backup nor the result. Undo has the same stretch between
/// its check and its rename. So the check is repeated as late as it can be: after
/// the temporary file is written (the slow part) and immediately before the rename.
/// On a mismatch the temporary file is removed and the file is left as the other
/// writer made it.
///
/// **On Windows the gap between that read and the rename is closed for writers
/// that write in place, and only for them.** The file is read through a handle that
/// allows others to read and rename but not to write, and the handle stays open
/// across the rename (which replaces by name, so it does not need the file closed).
/// A program writing into the file in that moment gets a sharing error — it fails
/// loudly and can retry — instead of its bytes being silently replaced. An editor that
/// saves by writing a temporary file and renaming it over the target (JetBrains
/// safe-write, Vim, Sublime) is allowed to rename, so its save can still land between
/// our read and our rename and be replaced. Elsewhere there is no such handle, and
/// what is left is the few microseconds between the read and the rename.
///
/// The same no-write-sharing open fails when another program already has the file
/// open for writing (a log tailer, a syncing tool). That is tried a few times, since
/// such holds are usually brief, and then reported as what it is rather than as a
/// failed read.
fn write_replacing(path: &Path, bytes: &[u8], expect: u64) -> Result<(), String> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.odin-tmp"));
    if let Err(e) = std::fs::write(&tmp, bytes) {
        // A write that failed part way (disk full) leaves a partial file behind.
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("writing {}: {e}", plain(path)));
    }
    let held = fingerprint_held_retrying(path);
    match &held {
        Ok((h, _)) if *h == expect => {}
        Ok(_) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("{} changed just now — nothing was changed", plain(path)));
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            if is_sharing_violation(e) {
                return Err(format!("{} is open for writing in another program; nothing was changed", plain(path)));
            }
            return Err(format!("reading {}: {e}", plain(path)));
        }
    }
    let replaced = std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("replacing {}: {e}", plain(path))
    });
    drop(held);
    replaced
}

/// ERROR_SHARING_VIOLATION: the file is open in another program in a way that
/// forbids ours. Only Windows has it.
fn is_sharing_violation(e: &std::io::Error) -> bool {
    cfg!(windows) && e.raw_os_error() == Some(32)
}

/// [`fingerprint_held`], tried up to three more times, 50 ms apart, while the only
/// thing wrong is a sharing violation: a program that has the file open for writing
/// usually lets go within a moment.
fn fingerprint_held_retrying(path: &Path) -> std::io::Result<(u64, std::fs::File)> {
    let mut tries = 0;
    loop {
        match fingerprint_held(path) {
            Err(e) if is_sharing_violation(&e) && tries < 3 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            r => return r,
        }
    }
}

/// [`fingerprint_file`], keeping the file open so the caller can hold it: on
/// Windows with no write sharing, so nobody can save to it until the handle is
/// dropped; elsewhere an ordinary read.
fn fingerprint_held(path: &Path) -> std::io::Result<(u64, std::fs::File)> {
    use std::io::Read;
    #[cfg(windows)]
    let mut f = {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_SHARE_READ | FILE_SHARE_DELETE: others may read it and rename over it.
        std::fs::OpenOptions::new().read(true).share_mode(1 | 4).open(path)?
    };
    #[cfg(not(windows))]
    let mut f = std::fs::File::open(path)?;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            return Ok((h, f));
        }
        for b in &buf[..n] {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
}

/// Rename `from` to `to`, **failing if `to` exists**.
///
/// `std::fs::rename` on Windows is `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`:
/// it silently replaces the destination, so a plan that checked "nothing is there"
/// and an apply that renames are one racing process away from destroying a file
/// nobody planned to touch. This is the same call without that flag — and without
/// `MOVEFILE_COPY_ALLOWED`, so a move to another drive (a folder mounted from one)
/// fails with "cannot move across drives" instead of quietly becoming copy plus
/// delete, which can half-finish. Used for the move, for moving back on undo, and by
/// [`write_new`].
pub fn move_new(from: &Path, to: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::Storage::FileSystem::{MOVE_FILE_FLAGS, MoveFileExW};
        use windows::core::PCWSTR;

        let wide = |p: &Path| -> Vec<u16> { p.as_os_str().encode_wide().chain(Some(0)).collect() };
        let (a, b) = (wide(from), wide(to));
        // SAFETY: both buffers are NUL-terminated and live until the call returns.
        let moved = unsafe { MoveFileExW(PCWSTR(a.as_ptr()), PCWSTR(b.as_ptr()), MOVE_FILE_FLAGS(0)) };
        moved.map_err(|e| {
            // ERROR_FILE_EXISTS, ERROR_ALREADY_EXISTS, ERROR_NOT_SAME_DEVICE.
            match (e.code().0 as u32) & 0xFFFF {
                80 | 183 => format!("{} already exists, so nothing was overwritten", plain(to)),
                17 => format!(
                    "cannot move {} to {}: they are on different drives, and a move never falls back to copy and delete",
                    plain(from),
                    plain(to)
                ),
                _ => format!("moving {} to {}: {e}", plain(from), plain(to)),
            }
        })
    }
    #[cfg(not(windows))]
    {
        // A hard link fails if `to` exists, and the old name is removed after.
        std::fs::hard_link(from, to).map_err(|e| format!("moving {} to {}: {e}", plain(from), plain(to)))?;
        std::fs::remove_file(from).map_err(|e| format!("removing {}: {e}", plain(from)))
    }
}

/// Write `bytes` to a path **where nothing exists**: a temporary file beside it,
/// then [`move_new`]. For creating a file and for putting a deleted one back —
/// both cases where something appearing at the path meanwhile is somebody's work.
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.odin-new"));
    // `create_new`: a leftover from a crash is never written over either.
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|e| format!("writing {}: {e}", plain(path)))?;
    if let Err(e) = f.write_all(bytes).and_then(|()| f.flush()) {
        drop(f);
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("writing {}: {e}", plain(path)));
    }
    drop(f);
    move_new(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

fn new_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("{nanos:x}")
}

/// Whether what a plan was made from is still true, and if not, how to say so.
fn stale(plan: &Plan, current: Option<&[u8]>) -> Option<String> {
    let (what, did) = match plan.op {
        Op::Delete => ("delete", "deleted"),
        Op::Move => ("move", "moved"),
        _ => ("edit", "written"),
    };
    let d = &plan.display;
    match (plan.before, current) {
        (None, Some(_)) => {
            Some(format!("{d} was created by something else after this {what} was shown — nothing was {did}"))
        }
        (Some(_), None) => {
            Some(format!("{d} was deleted after this {what} was shown — nothing was {did}"))
        }
        (Some(want), Some(bytes)) if fingerprint(bytes) != want => Some(format!(
            "{d} changed after this {what} was shown, so what you approved no longer describes it — \
             nothing was {did}"
        )),
        _ => None,
    }
}

/// Apply a plan — **only if what it was made from is still exactly as it was.**
/// Backs the original up first.
///
/// **Never overwrites a file it did not plan to.** Edits replace the file they
/// read ([`write_replacing`]); every other way a path gets bytes goes through
/// [`move_new`], which refuses an existing destination.
///
/// Order, so a failure at any step leaves the project as it was:
/// 1. make the planned folders one level at a time with `create_dir`, noting each
///    one it made. A planned folder that already exists must canonicalise to the
///    planned path, which catches a junction put there since planning;
/// 2. require that the folder the file goes in is still the one planned;
/// 3. write the `.bak` (an edit's or a delete's bytes) and the `.json`;
/// 4. do it.
///
/// If any step fails, the record and the folders this call made are removed again.
pub fn apply(plan: &Plan, backups: &Path) -> Result<Applied, String> {
    let current = match plan.op {
        Op::Move => None,
        _ => std::fs::read(&plan.path).ok(),
    };
    if plan.op == Op::Move {
        let to = plan.to.as_deref().ok_or("a move with no destination")?;
        match fingerprint_file(&plan.path) {
            Ok(h) if Some(h) == plan.before => {}
            Ok(_) => {
                return Err(format!(
                    "{} changed after this move was shown, so what you approved no longer describes \
                     it — nothing was moved",
                    plan.display
                ));
            }
            Err(_) => {
                return Err(format!(
                    "{} was deleted after this move was shown — nothing was moved",
                    plan.display
                ));
            }
        }
        if std::fs::symlink_metadata(to).is_ok() {
            return Err(format!(
                "{} was created by something else after this move was shown — nothing was moved",
                plan.to_display.as_deref().unwrap_or(&plan.display)
            ));
        }
    } else {
        // A folder or a link at a create's path reads as "nothing there", and is
        // not.
        if plan.op == Op::Create && std::fs::symlink_metadata(&plan.path).is_ok() {
            return Err(format!(
                "{} was created by something else after this edit was shown — nothing was written",
                plan.display
            ));
        }
        if let Some(why) = stale(plan, current.as_deref()) {
            return Err(why);
        }
    }

    std::fs::create_dir_all(backups).map_err(|e| format!("creating {}: {e}", plain(backups)))?;
    let id = new_id();
    let mut made: Vec<PathBuf> = Vec::new();
    match apply_steps(plan, backups, &id, current.as_deref(), &mut made) {
        Ok(()) => Ok(Applied {
            id,
            display: plan.display.clone(),
            added: plan.added,
            removed: plan.removed,
            create: plan.create,
            op: plan.op,
            to: plan.to_display.clone(),
            // Only the folders this call made: one that appeared since the card was
            // shown is somebody else's, and undo will leave it too.
            folders: plan
                .make_dirs
                .iter()
                .zip(&plan.folders)
                .filter(|(dir, _)| made.contains(dir))
                .map(|(_, shown)| shown.clone())
                .collect(),
        }),
        Err(e) => {
            let _ = std::fs::remove_file(backups.join(format!("{id}.json")));
            let _ = std::fs::remove_file(backups.join(format!("{id}.bak")));
            for dir in made.iter().rev() {
                let _ = std::fs::remove_dir(dir);
            }
            Err(e)
        }
    }
}

fn apply_steps(
    plan: &Plan,
    backups: &Path,
    id: &str,
    current: Option<&[u8]>,
    made: &mut Vec<PathBuf>,
) -> Result<(), String> {
    // Every folder this touches must still be where it was planned, checked
    // before anything is made or moved: the folder the first new one goes in,
    // and — for a delete or a move — the folder the source sits in. A junction
    // swapped in since planning would otherwise point the action elsewhere.
    let still_there = |folder: &Path| -> Result<(), String> {
        if std::fs::canonicalize(folder).ok().as_deref() == Some(folder) {
            Ok(())
        } else {
            Err(format!("the folder {} is not where it was planned — nothing was changed", plain(folder)))
        }
    };
    if let Some(first) = plan.make_dirs.first()
        && let Some(base) = first.parent()
    {
        still_there(base)?;
    }
    if matches!(plan.op, Op::Delete | Op::Move)
        && let Some(source_folder) = plan.path.parent()
    {
        still_there(source_folder)?;
    }
    for dir in &plan.make_dirs {
        if std::fs::symlink_metadata(dir).is_ok() {
            if !dir.is_dir() || std::fs::canonicalize(dir).ok().as_deref() != Some(dir.as_path()) {
                return Err(format!(
                    "{} is not the folder that was planned (a link, or a file, is there now) — \
                     nothing was changed",
                    plain(dir)
                ));
            }
        } else {
            std::fs::create_dir(dir).map_err(|e| format!("making the folder {}: {e}", plain(dir)))?;
            made.push(dir.clone());
        }
    }
    if plan.op != Op::Edit {
        let dest = plan.to.as_deref().unwrap_or(&plan.path);
        let parent = dest.parent().ok_or_else(|| format!("{} has no folder", plan.display))?;
        if std::fs::canonicalize(parent).ok().as_deref() != Some(parent) {
            return Err(format!(
                "the folder {} is not where it was planned — nothing was changed",
                plain(parent)
            ));
        }
    }

    if let (Op::Edit | Op::Delete, Some(original)) = (plan.op, current) {
        std::fs::write(backups.join(format!("{id}.bak")), original)
            .map_err(|e| format!("backing up {}: {e}", plan.display))?;
    }
    let record = Record {
        path: plan.path.clone(),
        display: plan.display.clone(),
        create: plan.op == Op::Create,
        after: match plan.op {
            Op::Edit | Op::Create => fingerprint(&plan.after),
            Op::Delete | Op::Move => plan.before.unwrap_or_default(),
        },
        before: plan.before,
        kind: Some(plan.op),
        to: plan.to.clone(),
        to_display: plan.to_display.clone(),
        made_dirs: made.iter().rev().cloned().collect(),
    };
    std::fs::write(
        backups.join(format!("{id}.json")),
        serde_json::to_vec_pretty(&record).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("recording the backup: {e}"))?;

    // A delete or a move acts on the file that was approved, checked once more
    // right before acting: an editor saving it since the check above would
    // otherwise have its newest bytes deleted, with only the older ones backed up.
    if matches!(plan.op, Op::Delete | Op::Move) {
        // By chunks: a move has no size limit, and its bytes are not needed.
        let now = fingerprint_file(&plan.path).map_err(|e| format!("reading {}: {e}", plan.display))?;
        if Some(now) != plan.before {
            return Err(format!("{} changed just now — nothing was changed", plan.display));
        }
    }
    match plan.op {
        Op::Edit => write_replacing(&plan.path, &plan.after, plan.before.ok_or("an edit with no original")?),
        Op::Create => write_new(&plan.path, &plan.after),
        Op::Delete => std::fs::remove_file(&plan.path)
            .map_err(|e| format!("deleting {}: {e}", plan.display)),
        Op::Move => move_new(&plan.path, plan.to.as_deref().ok_or("a move with no destination")?),
    }
}

/// What is at a path, as undo cares about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Absent,
    /// A regular file, and the hash of what it holds.
    File(u64),
    /// A folder, a link, or something that could not be read.
    Other,
}

fn look(path: &Path) -> State {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::Absent,
        Ok(m) if m.file_type().is_file() => fingerprint_file(path).map(State::File).unwrap_or(State::Other),
        _ => State::Other,
    }
}

/// Why a record cannot be undone against the state of the files.
#[derive(Debug)]
enum Why {
    /// The file is there but is not what the change left.
    Changed,
    /// The file the change left is not there.
    Gone,
    /// The path a delete or a move came from is occupied again.
    Occupied,
    /// The folder a file would go back into is gone.
    NoFolder,
}

#[derive(Debug)]
struct Refusal {
    path: PathBuf,
    display: String,
    why: Why,
}

impl Refusal {
    /// As [`undo`] says it, about one change.
    fn single(&self) -> String {
        let d = &self.display;
        match self.why {
            Why::Changed => {
                format!("{d} has changed since the edit, so undoing it would lose those changes — left as is")
            }
            Why::Gone => format!("{d} no longer exists, so there is nothing to undo"),
            Why::Occupied => {
                format!("{d} exists again, so undoing would overwrite it — left as is")
            }
            Why::NoFolder => format!("the folder for {d} is gone, so it cannot be put back"),
        }
    }

    /// As [`undo_turn`] says it, about the first change in the turn to find the
    /// file as somebody else left it.
    fn in_turn(&self) -> String {
        let d = &self.display;
        match self.why {
            Why::Changed => format!(
                "{d} has changed since this turn's edit, so putting it back would lose those changes"
            ),
            Why::Gone => format!("{d} no longer exists, so there is nothing to put back"),
            Why::Occupied => format!("{d} exists again, so putting the turn back would overwrite it"),
            Why::NoFolder => format!("the folder for {d} is gone, so it cannot be put back"),
        }
    }
}

/// **The one rule for whether a record can be undone**, against whatever `look`
/// says is at each path — the real files for [`undo`], a simulation for
/// [`undo_turn`], so they cannot disagree.
///
/// - Edit, create: the file is what the change left.
/// - Delete: nothing is at the path, and its folder is there to put it back in.
/// - Move: the file at `to` is what the move left, and nothing is at the source
///   (whose folder must still exist).
fn check_undo(record: &Record, look: &dyn Fn(&Path) -> State) -> Result<(), Refusal> {
    let refuse = |path: &Path, display: &str, why| {
        Err(Refusal { path: path.to_path_buf(), display: display.to_string(), why })
    };
    let holds = |path: &Path, display: &str| match look(path) {
        State::File(h) if h == record.after => Ok(()),
        State::Absent => refuse(path, display, Why::Gone),
        _ => refuse(path, display, Why::Changed),
    };
    let free = |path: &Path, display: &str| {
        if look(path) != State::Absent {
            return refuse(path, display, Why::Occupied);
        }
        // Its folder must still be the folder it was — canonical as recorded,
        // not a junction swapped in since that would send the restore outside.
        if !path
            .parent()
            .is_some_and(|p| p.is_dir() && std::fs::canonicalize(p).ok().as_deref() == Some(p))
        {
            return refuse(path, display, Why::NoFolder);
        }
        Ok(())
    };
    match record.op() {
        Op::Edit | Op::Create => holds(&record.path, &record.display),
        Op::Delete => free(&record.path, &record.display),
        Op::Move => {
            let to = record.to.as_deref().unwrap_or(&record.path);
            holds(to, &record.to_shown())?;
            free(&record.path, &record.display)
        }
    }
}

/// Whether some record in `backups` other than `own_id` — a change not yet
/// undone, since undoing deletes its record — has its `path` or its `to` inside
/// `dir`. Such a change still needs the folder: a delete would have nowhere to be
/// put back, a move nowhere to return to.
///
/// **Errors count as "needed".** If the backups folder cannot be listed, or another
/// record's file is there but cannot be read or parsed (a transient lock, a half
/// write), there is no telling that it does not lie inside `dir`, and removing the
/// folder on a guess could strand a change with nowhere to go back to. An empty
/// folder left behind is harmless; a stranded change is not.
fn folder_still_needed(dir: &Path, own_id: &str, backups: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(backups) else { return true };
    let dir = PathBuf::from(key(dir));
    entries.flatten().any(|entry| {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(id) = name.strip_suffix(".json") else { return false };
        if id == own_id {
            return false;
        }
        let Some(other) = std::fs::read(entry.path()).ok().and_then(|b| serde_json::from_slice::<Record>(&b).ok()) else {
            return true;
        };
        std::iter::once(&other.path)
            .chain(other.to.as_ref())
            .any(|p| PathBuf::from(key(p)).starts_with(&dir))
    })
}

/// Take away the folders a change made — each only if it is empty, deepest
/// first, stopping quietly at the first that is not. Never recursive.
///
/// **And not while another change that is still live lies inside it.** Create
/// `n/x.txt` (which makes `n/`), create `n/y.txt`, delete `n/y.txt`: undoing the
/// first card empties `n/`, and taking the folder away would leave the delete with
/// nowhere to be put back — its bytes would exist only in the backups. So a single
/// undo leaves the folder; [`undo_turn`] undoes newest first, so by the time it
/// reaches the change that made the folder nothing else is live and the folder goes.
fn remove_made(record: &Record, own_id: &str, backups: &Path) {
    for dir in &record.made_dirs {
        if folder_still_needed(dir, own_id, backups) || std::fs::remove_dir(dir).is_err() {
            break;
        }
    }
}

/// Put a file back the way it was before a change.
///
/// **Refuses if the file has changed since** — undo restores by overwriting (an
/// edit) or by taking a file away (a create, a move), and doing that over later work
/// would lose it. What undo does depends on what was done:
/// - **edit:** the original bytes go back over the file;
/// - **create:** the file is removed, then each folder the create made, if it is
///   empty. Two creates that shared a new folder: it belongs to the first, so
///   undoing the second alone leaves it, and undoing them newest first (as
///   [`undo_turn`] does) removes it;
/// - **delete:** the bytes come back from the backup, to a path where nothing is —
///   never over something that appeared there;
/// - **move:** the file goes back by [`move_new`], and the folders the move made
///   are removed if empty.
pub fn undo(id: &str, backups: &Path) -> Result<String, String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("not an edit id: {id}"));
    }
    let meta = backups.join(format!("{id}.json"));
    // Only a record that is not there means "already undone". One that is there but
    // cannot be read (a lock, antivirus) is a change still on disk, and saying
    // otherwise would mark its card Undone.
    let bytes = std::fs::read(&meta).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "that edit has already been undone, or was never made".to_string()
        } else {
            format!("reading the backup record for {id}: {e}")
        }
    })?;
    let record: Record =
        serde_json::from_slice(&bytes).map_err(|e| format!("reading the backup record: {e}"))?;

    check_undo(&record, &look).map_err(|r| r.single())?;

    let bak = backups.join(format!("{id}.bak"));
    match record.op() {
        Op::Edit => {
            let original = std::fs::read(&bak).map_err(|e| format!("reading the backup: {e}"))?;
            // As undo all checks (see `simulate`): a truncated or swapped backup is
            // not the file the edit started from, and would replace the user's file.
            if record.before.is_some_and(|b| b != fingerprint(&original)) {
                return Err(format!(
                    "the backup of {} is not what the file was before the edit, so it is not put back",
                    record.display
                ));
            }
            write_replacing(&record.path, &original, record.after)?;
        }
        Op::Create => {
            std::fs::remove_file(&record.path).map_err(|e| format!("removing {}: {e}", record.display))?;
            remove_made(&record, id, backups);
        }
        Op::Delete => {
            let original = std::fs::read(&bak).map_err(|e| format!("reading the backup: {e}"))?;
            if fingerprint(&original) != record.after {
                return Err(format!(
                    "the backup of {} is not what was deleted, so it is not put back",
                    record.display
                ));
            }
            write_new(&record.path, &original)?;
        }
        Op::Move => {
            let to = record.to.as_deref().ok_or("that move has no destination on record")?;
            move_new(to, &record.path)?;
            remove_made(&record, id, backups);
        }
    }
    let _ = std::fs::remove_file(&meta);
    let _ = std::fs::remove_file(&bak);
    Ok(record.display)
}

/// What [`undo_turn`] did. `Err` from `undo_turn` means it refused and wrote
/// nothing; `Ok` with no `failed` means every change is back; `Ok` with `failed`
/// means it stopped partway and the rest are still applied.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnUndo {
    /// Changes put back, newest first.
    pub restored: Vec<String>,
    /// Changes with no record left — undone one at a time already, or never made.
    pub already: Vec<String>,
    pub failed: Option<TurnFailure>,
}

/// The change a turn's undo stopped at, and why.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnFailure {
    pub id: String,
    pub display: String,
    pub error: String,
}

/// A path as a key for a turn's simulated state, case-insensitively — NTFS treats
/// `Src\A.rs` and `src\a.rs` as one file, so two spellings must be one entry.
fn key(p: &Path) -> String {
    plain(p).to_lowercase()
}

/// Walk a turn's records **newest first against the real files, rolling each back
/// in a copy of their state**, and say what would stop it.
///
/// This is what [`undo`] would meet if it ran the whole turn — without running it.
/// A record's check is the same [`check_undo`], against the simulated state; then its
/// effect is undone in the simulation so the next record down sees the state it
/// would see for real. That handles every mix of edits, creates, deletes and moves
/// the way a chain of edits on one file used to be handled by hand.
///
/// **One problem per path.** The first record to fail on a path poisons it and every
/// record after that touches it is skipped, so one hand edit is one line, not one
/// per change underneath it. A failure on the path's real state reads "has changed";
/// after an earlier rollback in the walk it reads "do not line up", because by then
/// the disagreement is between two changes, not with the user.
fn simulate(found: &[(u128, String, Record)], backups: &Path) -> Vec<String> {
    use std::collections::{HashMap, HashSet};

    let paths = |r: &Record| -> Vec<(PathBuf, String)> {
        let mut v = vec![(r.path.clone(), r.display.clone())];
        if let Some(to) = &r.to {
            v.push((to.clone(), r.to_shown()));
        }
        v
    };
    let mut state: HashMap<String, State> = HashMap::new();
    for (_, _, r) in found {
        for (p, _) in paths(r) {
            state.entry(key(&p)).or_insert_with(|| look(&p));
        }
    }
    let mut rolled: HashSet<String> = HashSet::new();
    let mut poisoned: HashSet<String> = HashSet::new();
    let mut problems: Vec<String> = Vec::new();

    for (_, id, r) in found {
        let mine = paths(r);
        let lines_up = |display: &str| {
            format!("{display}'s edits in this turn do not line up, so they cannot be put back safely")
        };
        let problem: Option<String> = 'record: {
            if mine.iter().any(|(p, _)| poisoned.contains(&key(p))) {
                break 'record None;
            }
            let seen = |p: &Path| state.get(&key(p)).copied().unwrap_or(State::Absent);
            if let Err(refusal) = check_undo(r, &seen) {
                break 'record Some(if rolled.contains(&key(&refusal.path)) {
                    lines_up(&refusal.display)
                } else {
                    refusal.in_turn()
                });
            }
            let bak = || std::fs::read(backups.join(format!("{id}.bak")));
            let missing = || format!("the backup of {} is missing", r.display);
            match r.op() {
                Op::Edit => {
                    let Ok(bytes) = bak() else { break 'record Some(missing()) };
                    let hash = fingerprint(&bytes);
                    if r.before.is_some_and(|b| b != hash) {
                        break 'record Some(lines_up(&r.display));
                    }
                    state.insert(key(&r.path), State::File(hash));
                }
                Op::Create => {
                    state.insert(key(&r.path), State::Absent);
                }
                Op::Delete => {
                    let Ok(bytes) = bak() else { break 'record Some(missing()) };
                    if fingerprint(&bytes) != r.after {
                        break 'record Some(lines_up(&r.display));
                    }
                    state.insert(key(&r.path), State::File(r.after));
                }
                Op::Move => {
                    if let Some(to) = &r.to {
                        state.insert(key(to), State::Absent);
                    }
                    state.insert(key(&r.path), State::File(r.after));
                }
            }
            rolled.extend(mine.iter().map(|(p, _)| key(p)));
            None
        };
        if let Some(p) = problem {
            problems.push(p);
            poisoned.extend(mine.iter().map(|(p, _)| key(p)));
        }
    }
    problems
}

/// Put back every change of one turn — or, if any of them cannot be put back
/// safely, none.
///
/// Undoing newest first with [`undo`] already unwinds a chain of changes, because
/// each one's before-state is the next one's after-state. What a loop of `undo`
/// cannot do is refuse *before* writing: change three of four failing would leave
/// the turn half undone. So this first [`simulate`]s the whole turn — every record's
/// own precondition, newest first, against the real files with each rolled back in
/// turn — collecting every problem before refusing. Then it restores newest first
/// across all files, each through [`undo`], whose own check guards against a change
/// after the pre-check.
///
/// **Not atomic across files.** If a write fails partway, what is left is a
/// state that really existed during the turn, the records of what remains are
/// intact, and calling this again with the same ids finishes the job. Ids are
/// wall-clock nanoseconds, so a clock that steps back during a turn makes the
/// chains disagree and gets a refusal, never a wrong write.
///
/// The caller serialises this against every other edit and undo (the app holds a
/// lock); nothing in here does.
pub fn undo_turn(ids: &[String], backups: &Path) -> Result<TurnUndo, String> {
    // id → (numeric id, record), in the order given, without repeats.
    let mut seen: Vec<u128> = Vec::new();
    let mut already: Vec<String> = Vec::new();
    let mut found: Vec<(u128, String, Record)> = Vec::new();
    for id in ids {
        if id.is_empty() || id.len() > 32 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("not an edit id: {id}"));
        }
        let n = u128::from_str_radix(id, 16).map_err(|_| format!("not an edit id: {id}"))?;
        if seen.contains(&n) {
            continue;
        }
        seen.push(n);
        // Only a record that is not there is "already undone"; one that cannot be
        // read is still a change on disk, so the whole turn is refused, naming it.
        let bytes = match std::fs::read(backups.join(format!("{id}.json"))) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                already.push(id.clone());
                continue;
            }
            Err(e) => return Err(format!("reading the backup record for {id}: {e}")),
        };
        let record: Record =
            serde_json::from_slice(&bytes).map_err(|e| format!("reading the backup record for {id}: {e}"))?;
        found.push((n, id.clone(), record));
    }
    if found.is_empty() {
        if ids.is_empty() {
            return Err("there are no edits to undo".to_string());
        }
        return Ok(TurnUndo { restored: Vec::new(), already, failed: None });
    }

    // Newest first across every file.
    found.sort_by_key(|f| std::cmp::Reverse(f.0));
    let problems = simulate(&found, backups);
    if !problems.is_empty() {
        return Err(format!("Nothing was undone: {}.", problems.join("; ")));
    }

    let mut restored = Vec::new();
    for (_, id, record) in &found {
        match undo(id, backups) {
            Ok(_) => restored.push(id.clone()),
            Err(error) => {
                let failed = TurnFailure { id: id.clone(), display: record.display.clone(), error };
                return Ok(TurnUndo { restored, already, failed: Some(failed) });
            }
        }
    }
    Ok(TurnUndo { restored, already, failed: None })
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
        let err = ws.resolve_target("link/new.txt", "").unwrap_err();
        assert!(err.contains("outside"), "{err}");
    }

    // ── locate ──────────────────────────────────────────────────────────────

    /// A junction `ws/link` pointing at `s.dir/target`, or `None` where junctions
    /// cannot be made — the caller skips its assertions, like the test above.
    fn junction_out(s: &Sandbox, ws: &Workspace) -> Option<PathBuf> {
        let target = s.dir.join("target");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("there.txt"), b"x").unwrap();
        let link = ws.root().join("linked");
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(plain(&link))
            .arg(plain(&target))
            .output();
        if !matches!(made, Ok(ref o) if o.status.success()) {
            eprintln!("could not create a junction here; skipping");
            return None;
        }
        Some(target)
    }

    #[test]
    fn locate_finds_a_file_inside() {
        let s = Sandbox::new("loc-in");
        let ws = s.ws();
        s.write("src/a.rs", b"x");
        match ws.locate("src/a.rs") {
            Located::Inside(p) => assert_eq!(ws.relative(&p), "src/a.rs"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn locate_puts_dot_dot_and_absolute_paths_elsewhere_outside() {
        let s = Sandbox::new("loc-out");
        let ws = s.ws();
        std::fs::write(s.dir.join("x"), b"x").unwrap();
        assert!(matches!(ws.locate("../x"), Located::Outside(_)));
        let abs = s.dir.join("x");
        assert!(matches!(ws.locate(abs.to_str().unwrap()), Located::Outside(_)));
        // Missing, and still outside: a `..` in the tail cannot climb back in.
        assert!(matches!(ws.locate("nope/../../missing.txt"), Located::Outside(_)));
    }

    #[test]
    fn locate_follows_a_junction_out_even_to_a_missing_file() {
        let s = Sandbox::new("loc-junction");
        let ws = s.ws();
        let Some(target) = junction_out(&s, &ws) else { return };
        match ws.locate("linked/there.txt") {
            Located::Outside(p) => assert_eq!(p, std::fs::canonicalize(target.join("there.txt")).unwrap()),
            other => panic!("{other:?}"),
        }
        assert!(matches!(ws.locate("linked/missing.txt"), Located::Outside(_)));
    }

    #[test]
    fn locate_says_a_missing_file_inside_is_missing() {
        let s = Sandbox::new("loc-missing");
        let ws = s.ws();
        s.write("src/a.rs", b"x");
        assert_eq!(ws.locate("src/none.rs"), Located::MissingInside);
        assert_eq!(ws.locate("no/such/dir/none.rs"), Located::MissingInside);
        assert_eq!(ws.locate("src/../other/none.rs"), Located::MissingInside);
    }

    /// Editing refuses `.cargo/`; reading is decided by `search`, not here.
    #[test]
    fn locate_applies_none_of_the_edit_only_refusals() {
        let s = Sandbox::new("loc-cargo");
        let ws = s.ws();
        s.write(".cargo/config.toml", b"[build]
");
        s.write(".git/config", b"[core]
");
        assert!(ws.resolve_existing(".cargo/config.toml").is_err());
        assert!(matches!(ws.locate(".cargo/config.toml"), Located::Inside(_)));
        assert!(matches!(ws.locate(".git/config"), Located::Inside(_)));
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
        assert!(ws.resolve_target(".cargo/config", "").is_err());
        assert!(ws.resolve_target("RUST-TOOLCHAIN", "").is_err());
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
        let err = ws.resolve_target(".npmrc::$DATA", "").unwrap_err();
        assert!(err.contains("file stream"), "{err}");
        let err = ws.resolve_target("notes.txt:hidden", "").unwrap_err();
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
        assert!(ws.resolve_target(".git/new", "").unwrap_err().contains(".git"));
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
    fn new_files_get_the_folders_they_need_inside() {
        let s = Sandbox::new("new");
        let ws = s.ws();
        assert!(ws.resolve_target("fresh.txt", "").unwrap().new_dirs.is_empty());
        let t = ws.resolve_target("no/such/dir/f.txt", "").unwrap();
        let shown: Vec<String> = t.new_dirs.iter().map(|d| ws.relative(d)).collect();
        assert_eq!(shown, ["no", "no/such", "no/such/dir"], "shallowest first");
        assert_eq!(ws.relative(&t.path), "no/such/dir/f.txt");
        // Only the missing part is new: an existing folder is not made again.
        s.write("src/a.rs", b"x");
        let t = ws.resolve_target("src/deeper/f.rs", "").unwrap();
        assert_eq!(t.new_dirs.len(), 1);
        s.write("taken.txt", b"x");
        assert!(ws.resolve_target("taken.txt", "").unwrap_err().contains("already exists"));
        assert!(ws.resolve_target("../escape.txt", "").unwrap_err().contains("outside"));
        // A file in the way is not a folder to make a path under.
        let err = ws.resolve_target("taken.txt/inside/f.txt", "").unwrap_err();
        assert!(err.contains("a file, not a folder"), "{err}");
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
    fn a_read_header_pasted_into_old_text_is_named_as_the_mistake() {
        let s = Sandbox::new("header");
        let ws = s.ws();
        s.write("a.rs", b"fn one() {}\nfn two() {}\n");
        for old in [
            "[lines 1–2 of 2]\nfn one() {}\n",
            "[lines 1–2 of 2; cut at 65536 bytes — read on from line 3]\nfn one() {}",
        ] {
            let err = plan_replace(&ws, "a.rs", old, "fn uno() {}\n").unwrap_err();
            assert!(
                err.contains("old_text starts with the read's header line") && err.contains("a.rs"),
                "{err}"
            );
        }
        // Never stripped for the model: nothing was written.
        assert_eq!(s.read("a.rs"), b"fn one() {}\nfn two() {}\n");
        // A header-looking line that is really in the file is just text.
        s.write("b.rs", b"[lines 1-2 of 2]\nx\n");
        assert!(plan_replace(&ws, "b.rs", "[lines 1-2 of 2]\nx\n", "y\n").is_ok());
    }

    #[test]
    fn a_miss_in_a_big_file_says_to_read_those_lines_again() {
        // Past 64 KB there is no "closest text" hint, and "read the file again"
        // would get the model a refusal — so it is told to read a range.
        let s = Sandbox::new("bigmiss");
        let ws = s.ws();
        let body: String = (1..=4000).map(|i| format!("    let value_{i} = {i};\n")).collect();
        assert!(body.len() > CLOSEST_MAX_BODY);
        s.write("big.rs", body.as_bytes());
        let err = plan_replace(&ws, "big.rs", "    let value_3 = 4;\n", "x\n").unwrap_err();
        assert!(err.contains("Read those lines again with start_line and end_line"), "{err}");
        assert!(!err.contains("Read the file again"), "{err}");
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

    // ── undoing a whole turn ────────────────────────────────────────────────

    fn replace(s: &Sandbox, ws: &Workspace, rel: &str, old: &str, new: &str) -> String {
        apply(&plan_replace(ws, rel, old, new).unwrap(), &s.backups()).unwrap().id
    }

    fn backup_files(s: &Sandbox) -> Vec<String> {
        match std::fs::read_dir(s.backups()) {
            Ok(rd) => rd.map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect(),
            Err(_) => Vec::new(),
        }
    }

    #[test]
    fn a_turn_across_two_files_is_put_back() {
        let s = Sandbox::new("turn-two");
        let ws = s.ws();
        s.write("a.txt", b"one\r\n");
        s.write("b.txt", b"\xEF\xBB\xBFred\n");
        let a = replace(&s, &ws, "a.txt", "one", "two");
        let b = replace(&s, &ws, "b.txt", "red", "blue");
        let out = undo_turn(&[a.clone(), b.clone()], &s.backups()).unwrap();
        assert_eq!(out.restored, vec![b, a]);
        assert!(out.already.is_empty() && out.failed.is_none());
        assert_eq!(s.read("a.txt"), b"one\r\n");
        assert_eq!(s.read("b.txt"), b"\xEF\xBB\xBFred\n");
    }

    #[test]
    fn one_file_edited_twice_is_unwound_newest_first() {
        let s = Sandbox::new("turn-twice");
        let ws = s.ws();
        s.write("a.txt", b"one two\n");
        let first = replace(&s, &ws, "a.txt", "one", "1");
        let second = replace(&s, &ws, "a.txt", "two", "2");
        let out = undo_turn(&[first.clone(), second.clone()], &s.backups()).unwrap();
        assert_eq!(out.restored, vec![second, first]);
        assert_eq!(s.read("a.txt"), b"one two\n");
    }

    #[test]
    fn ids_given_in_reverse_order_make_no_difference() {
        let s = Sandbox::new("turn-reverse");
        let ws = s.ws();
        s.write("a.txt", b"one two\n");
        let first = replace(&s, &ws, "a.txt", "one", "1");
        let second = replace(&s, &ws, "a.txt", "two", "2");
        undo_turn(&[second, first], &s.backups()).unwrap();
        assert_eq!(s.read("a.txt"), b"one two\n");
    }

    #[test]
    fn a_create_then_an_edit_of_it_is_removed() {
        let s = Sandbox::new("turn-create");
        let ws = s.ws();
        let made = apply(&plan_create(&ws, "new.txt", "hi\n").unwrap(), &s.backups()).unwrap().id;
        let edited = replace(&s, &ws, "new.txt", "hi", "hello");
        undo_turn(&[made, edited], &s.backups()).unwrap();
        assert!(!ws.root().join("new.txt").exists());
        assert!(backup_files(&s).is_empty());
    }

    #[test]
    fn a_file_changed_since_refuses_the_whole_turn_and_touches_nothing() {
        let s = Sandbox::new("turn-changed");
        let ws = s.ws();
        s.write("a.txt", b"one\n");
        s.write("b.txt", b"red\n");
        let a = replace(&s, &ws, "a.txt", "one", "two");
        let b = replace(&s, &ws, "b.txt", "red", "blue");
        s.write("a.txt", b"two\nmy own line\n");
        let before = backup_files(&s);
        let err = undo_turn(&[a, b], &s.backups()).unwrap_err();
        assert!(err.starts_with("Nothing was undone:") && err.contains("a.txt has changed"), "{err}");
        assert_eq!(s.read("a.txt"), b"two\nmy own line\n");
        assert_eq!(s.read("b.txt"), b"blue\n");
        let mut after = backup_files(&s);
        let mut before = before;
        before.sort();
        after.sort();
        assert_eq!(before, after);
    }

    #[test]
    fn every_problem_file_is_named() {
        let s = Sandbox::new("turn-names");
        let ws = s.ws();
        s.write("a.txt", b"one\n");
        s.write("b.txt", b"red\n");
        let a = replace(&s, &ws, "a.txt", "one", "two");
        let b = replace(&s, &ws, "b.txt", "red", "blue");
        s.write("a.txt", b"x\n");
        s.write("b.txt", b"y\n");
        let err = undo_turn(&[a, b], &s.backups()).unwrap_err();
        assert!(err.contains("a.txt") && err.contains("b.txt"), "{err}");
    }

    #[test]
    fn a_turn_partly_undone_by_hand_finishes_the_rest() {
        let s = Sandbox::new("turn-partly");
        let ws = s.ws();
        s.write("a.txt", b"one two\n");
        let first = replace(&s, &ws, "a.txt", "one", "1");
        let second = replace(&s, &ws, "a.txt", "two", "2");
        undo(&second, &s.backups()).unwrap();
        let out = undo_turn(&[first.clone(), second.clone()], &s.backups()).unwrap();
        assert_eq!(out.restored, vec![first]);
        assert_eq!(out.already, vec![second]);
        assert_eq!(s.read("a.txt"), b"one two\n");
    }

    #[test]
    fn a_hand_edit_between_two_of_the_turns_edits_refuses() {
        let s = Sandbox::new("turn-between");
        let ws = s.ws();
        s.write("a.txt", b"one two\n");
        let first = replace(&s, &ws, "a.txt", "one", "1");
        s.write("a.txt", b"1 two\nmine\n");
        let second = replace(&s, &ws, "a.txt", "two", "2");
        let err = undo_turn(&[first, second], &s.backups()).unwrap_err();
        assert!(err.contains("do not line up"), "{err}");
        assert_eq!(s.read("a.txt"), b"1 2\nmine\n");
    }

    #[test]
    fn an_older_turn_under_a_newer_edit_refuses_until_the_newer_goes_first() {
        let s = Sandbox::new("turn-layers");
        let ws = s.ws();
        s.write("a.txt", b"one two\n");
        let older = replace(&s, &ws, "a.txt", "one", "1");
        let newer = replace(&s, &ws, "a.txt", "two", "2");
        let err = undo_turn(std::slice::from_ref(&older), &s.backups()).unwrap_err();
        assert!(err.contains("has changed"), "{err}");
        undo_turn(&[newer], &s.backups()).unwrap();
        undo_turn(&[older], &s.backups()).unwrap();
        assert_eq!(s.read("a.txt"), b"one two\n");
    }

    #[test]
    fn a_created_file_deleted_by_hand_refuses() {
        let s = Sandbox::new("turn-vanished");
        let ws = s.ws();
        let made = apply(&plan_create(&ws, "new.txt", "hi\n").unwrap(), &s.backups()).unwrap().id;
        let edited = replace(&s, &ws, "new.txt", "hi", "hello");
        std::fs::remove_file(ws.root().join("new.txt")).unwrap();
        let err = undo_turn(&[made, edited], &s.backups()).unwrap_err();
        assert!(err.contains("no longer exists"), "{err}");
    }

    #[test]
    fn a_file_created_deleted_and_created_again_refuses() {
        let s = Sandbox::new("turn-recreated");
        let ws = s.ws();
        let one = apply(&plan_create(&ws, "new.txt", "hi\n").unwrap(), &s.backups()).unwrap().id;
        std::fs::remove_file(ws.root().join("new.txt")).unwrap();
        let two = apply(&plan_create(&ws, "new.txt", "hi\n").unwrap(), &s.backups()).unwrap().id;
        let err = undo_turn(&[one, two], &s.backups()).unwrap_err();
        assert!(err.contains("do not line up"), "{err}");
        assert!(ws.root().join("new.txt").exists());
    }

    #[test]
    fn a_missing_backup_refuses() {
        let s = Sandbox::new("turn-nobak");
        let ws = s.ws();
        s.write("a.txt", b"one two\n");
        let first = replace(&s, &ws, "a.txt", "one", "1");
        let second = replace(&s, &ws, "a.txt", "two", "2");
        std::fs::remove_file(s.backups().join(format!("{first}.bak"))).unwrap();
        let err = undo_turn(&[first, second], &s.backups()).unwrap_err();
        assert!(err.contains("backup of a.txt is missing"), "{err}");
        assert_eq!(s.read("a.txt"), b"1 2\n");
    }

    #[test]
    fn a_turn_already_undone_is_ok_and_does_nothing() {
        let s = Sandbox::new("turn-already");
        let ws = s.ws();
        s.write("a.txt", b"one\n");
        let a = replace(&s, &ws, "a.txt", "one", "two");
        undo(&a, &s.backups()).unwrap();
        let out = undo_turn(std::slice::from_ref(&a), &s.backups()).unwrap();
        assert_eq!(out, TurnUndo { restored: Vec::new(), already: vec![a], failed: None });
    }

    #[test]
    fn a_write_failing_partway_leaves_the_rest_intact_and_retryable() {
        let s = Sandbox::new("turn-fail");
        let ws = s.ws();
        s.write("a.txt", b"one\n");
        s.write("b.txt", b"red\n");
        let a = replace(&s, &ws, "a.txt", "one", "two");
        let b = replace(&s, &ws, "b.txt", "red", "blue");
        // A directory where write_replacing wants its temporary file makes the
        // write to a.txt fail on any OS.
        let block = ws.root().join(".a.txt.odin-tmp");
        std::fs::create_dir(&block).unwrap();

        let out = undo_turn(&[a.clone(), b.clone()], &s.backups()).unwrap();
        assert_eq!(out.restored, vec![b.clone()]);
        let failed = out.failed.unwrap();
        assert_eq!((failed.id.as_str(), failed.display.as_str()), (a.as_str(), "a.txt"));
        assert_eq!(s.read("b.txt"), b"red\n");
        assert_eq!(s.read("a.txt"), b"two\n");
        assert!(s.backups().join(format!("{a}.json")).exists());
        assert!(s.backups().join(format!("{a}.bak")).exists());

        std::fs::remove_dir(&block).unwrap();
        let again = undo_turn(&[a.clone(), b.clone()], &s.backups()).unwrap();
        assert_eq!(again.restored, vec![a]);
        assert_eq!(again.already, vec![b]);
        assert_eq!(s.read("a.txt"), b"one\n");
    }

    #[test]
    fn turn_ids_that_are_not_ids_are_refused() {
        let s = Sandbox::new("turn-badids");
        assert!(undo_turn(&["../../config".to_string()], &s.backups()).unwrap_err().contains("not an edit id"));
        assert!(undo_turn(&["zz".to_string()], &s.backups()).is_err());
        assert!(undo_turn(&["1".repeat(33)], &s.backups()).is_err());
        assert!(undo_turn(&[String::new()], &s.backups()).is_err());
    }

    #[test]
    fn an_empty_turn_is_refused() {
        let s = Sandbox::new("turn-empty");
        assert!(undo_turn(&[], &s.backups()).is_err());
    }

    #[test]
    fn a_repeated_id_is_undone_once() {
        let s = Sandbox::new("turn-dup");
        let ws = s.ws();
        s.write("a.txt", b"one\n");
        let a = replace(&s, &ws, "a.txt", "one", "two");
        let out = undo_turn(&[a.clone(), a.clone()], &s.backups()).unwrap();
        assert_eq!(out.restored, vec![a]);
        assert!(out.already.is_empty());
    }

    #[test]
    fn undoing_a_turn_leaves_no_record_or_backup_behind() {
        let s = Sandbox::new("turn-clean");
        let ws = s.ws();
        s.write("a.txt", b"one\n");
        let a = replace(&s, &ws, "a.txt", "one", "two");
        let b = apply(&plan_create(&ws, "new.txt", "hi\n").unwrap(), &s.backups()).unwrap().id;
        undo_turn(&[a, b], &s.backups()).unwrap();
        assert!(backup_files(&s).is_empty());
    }

    #[test]
    fn a_record_written_before_the_hash_of_the_original_was_kept_still_loads() {
        let old = br#"{"path":"C:\\x\\a.txt","display":"a.txt","create":false,"after":42}"#;
        let r: Record = serde_json::from_slice(old).unwrap();
        assert_eq!(r.before, None);
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

    #[test]
    fn a_network_or_device_path_is_recognised_from_its_text_alone() {
        let local = Path::new(r"C:\proj");
        for p in [
            r"\\host\share\x.txt",
            "//host/share/x.txt",
            r"\\?\UNC\host\share\x.txt",
            r"\\.\PhysicalDrive0",
            r"\\?\GLOBALROOT\Device\x",
        ] {
            assert!(remote_or_device(Path::new(p), local), "{p} should be treated as remote");
        }
        for p in [r"C:\proj\a.txt", r"C:\elsewhere\a.txt", r"\\?\C:\proj\a.txt", "a.txt"] {
            assert!(!remote_or_device(Path::new(p), local), "{p} is a local path");
        }
    }

    #[test]
    fn a_project_on_a_share_keeps_its_own_paths_and_nothing_else() {
        let root = Path::new(r"\\?\UNC\srv\share\proj");
        assert!(!remote_or_device(Path::new(r"\\srv\share\proj\a.txt"), root));
        assert!(!remote_or_device(Path::new(r"\\SRV\Share\Proj\src\b.rs"), root), "case-insensitive");
        assert!(remote_or_device(Path::new(r"\\srv\share\proj\..\other\a.txt"), root), "no climbing out");
        assert!(remote_or_device(Path::new(r"\\srv\share\other\a.txt"), root));
        assert!(remote_or_device(Path::new(r"\\evil\share\proj\a.txt"), root));
        // With no project folder there is nothing for a remote path to be under.
        assert!(remote_or_device(Path::new(r"\\srv\share\proj\a.txt"), Path::new("")));
    }

    #[test]
    fn locate_never_resolves_a_path_on_another_machine() {
        // A host that cannot exist: had `locate` canonicalised it, Windows would
        // have tried an SMB connection first. It comes back as given, outside.
        let s = Sandbox::new("loc-remote");
        let ws = s.ws();
        let given = r"\\odin-test-no-such-host\share\x.txt";
        match ws.locate(given) {
            Located::Outside(p) => assert_eq!(p, PathBuf::from(given)),
            other => panic!("{other:?}"),
        }
    }

    // ── H9: resolving a path that does not exist yet ────────────────────────

    /// A junction `link` → `target`, or false where junctions cannot be made.
    fn junction(link: &Path, target: &Path) -> bool {
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(plain(link))
            .arg(plain(target))
            .output();
        let ok = matches!(made, Ok(ref o) if o.status.success());
        if !ok {
            eprintln!("could not create a junction here; skipping");
        }
        ok
    }

    #[test]
    fn dot_dot_in_the_part_that_does_not_exist_is_refused() {
        let s = Sandbox::new("tgt-dotdot");
        let ws = s.ws();
        // Absolute and non-verbatim, so the `..` survives to be seen.
        let given = format!("{}\\nope\\..\\x.txt", ws.root_display());
        let err = ws.resolve_target(&given, "").unwrap_err();
        assert!(err.contains(".."), "{err}");
    }

    #[test]
    fn a_new_path_through_a_junction_out_is_refused() {
        let s = Sandbox::new("tgt-junction");
        let ws = s.ws();
        let Some(_) = junction_out(&s, &ws) else { return };
        let err = ws.resolve_target("linked/new/deeper/x.rs", "").unwrap_err();
        assert!(err.contains("outside"), "{err}");
    }

    #[test]
    fn a_new_path_through_a_link_that_leads_nowhere_is_refused() {
        let s = Sandbox::new("tgt-dangling");
        let ws = s.ws();
        let target = s.dir.join("gone");
        std::fs::create_dir_all(&target).unwrap();
        if !junction(&ws.root().join("dangling"), &target) {
            return;
        }
        std::fs::remove_dir(&target).unwrap();
        let err = ws.resolve_target("dangling/x.rs", "").unwrap_err();
        assert!(err.contains("leads nowhere"), "{err}");
    }

    #[test]
    fn a_missing_project_folder_is_not_made_up() {
        let s = Sandbox::new("tgt-noroot");
        let ws = s.ws();
        std::fs::remove_dir_all(ws.root()).unwrap();
        let err = ws.resolve_target("src/new.rs", "").unwrap_err();
        assert!(err.contains("outside"), "{err}");
    }

    #[test]
    fn at_most_eight_new_folders_are_made_at_once() {
        let s = Sandbox::new("tgt-cap");
        let ws = s.ws();
        let eight = "a/b/c/d/e/f/g/h/x.rs";
        assert_eq!(ws.resolve_target(eight, "").unwrap().new_dirs.len(), 8);
        let err = ws.resolve_target("a/b/c/d/e/f/g/h/i/x.rs", "").unwrap_err();
        assert!(err.contains("at most 8"), "{err}");
    }

    #[test]
    fn names_windows_cannot_handle_are_refused_in_files_and_folders() {
        let s = Sandbox::new("tgt-names");
        let ws = s.ws();
        for p in ["notes.", "src /x.rs", "src/con.rs", "NUL", "src/aux/x.rs", "a/COM1.txt", "lpt9", "what?.rs", "src./x.rs"] {
            let err = ws.resolve_target(p, "").unwrap_err();
            assert!(err.contains("Windows"), "{p}: {err}");
        }
        // Lookalikes are ordinary names.
        for p in ["console.rs", "src/auxiliary/x.rs", "com10.txt", "com0.txt", ".gitignore", "a.b.c"] {
            assert!(ws.resolve_target(p, "").is_ok(), "{p}");
        }
    }

    #[test]
    fn dot_git_and_dot_cargo_are_refused_as_new_folders() {
        let s = Sandbox::new("tgt-git");
        let ws = s.ws();
        assert!(ws.resolve_target(".git/hooks/pre-commit", "").unwrap_err().contains(".git"));
        assert!(ws.resolve_target("sub/.GIT/x", "").unwrap_err().contains(".git"));
        assert!(ws.resolve_target(".cargo/deeper/x.toml", "").is_err());
    }

    // ── H9: creating in new folders ─────────────────────────────────────────

    fn create(s: &Sandbox, ws: &Workspace, rel: &str, content: &str) -> String {
        apply(&plan_create(ws, rel, content).unwrap(), &s.backups()).unwrap().id
    }

    fn delete(s: &Sandbox, ws: &Workspace, rel: &str) -> String {
        apply(&plan_delete(ws, rel).unwrap(), &s.backups()).unwrap().id
    }

    fn mv(s: &Sandbox, ws: &Workspace, from: &str, to: &str) -> String {
        apply(&plan_move(ws, from, to).unwrap(), &s.backups()).unwrap().id
    }

    #[test]
    fn a_file_in_new_folders_is_made_and_undo_takes_the_folders_away() {
        let s = Sandbox::new("mk-undo");
        let ws = s.ws();
        s.write("src/main.rs", b"fn main() {}\n");
        let plan = plan_create(&ws, "src/net/http/client.rs", "pub fn get() {}\n").unwrap();
        assert_eq!(plan.folders, ["src/net/", "src/net/http/"]);
        let done = apply(&plan, &s.backups()).unwrap();
        assert_eq!(done.folders, ["src/net/", "src/net/http/"]);
        assert!(ws.root().join("src/net/http/client.rs").exists());
        assert_eq!(undo(&done.id, &s.backups()).unwrap(), "src/net/http/client.rs");
        assert!(!ws.root().join("src/net").exists());
        assert!(ws.root().join("src").exists(), "the folder that was there already stays");
        assert!(backup_files(&s).is_empty());
    }

    #[test]
    fn a_neighbour_in_a_made_folder_keeps_the_folder_and_undo_still_works() {
        let s = Sandbox::new("mk-neighbour");
        let ws = s.ws();
        let id = create(&s, &ws, "a/b/x.rs", "x\n");
        s.write("a/b/mine.txt", b"mine\n");
        undo(&id, &s.backups()).unwrap();
        assert!(!ws.root().join("a/b/x.rs").exists());
        assert_eq!(s.read("a/b/mine.txt"), b"mine\n");
    }

    #[test]
    fn a_single_undo_keeps_a_made_folder_a_later_delete_needs() {
        let s = Sandbox::new("mk-needed-delete");
        let ws = s.ws();
        let first = create(&s, &ws, "n/x.txt", "x\n");
        let second = create(&s, &ws, "n/y.txt", "y\n");
        let deleted = apply(&plan_delete(&ws, "n/y.txt").unwrap(), &s.backups()).unwrap().id;
        // The first card made `n/`, but the delete of y.txt still has to put it back.
        undo(&first, &s.backups()).unwrap();
        assert!(!ws.root().join("n/x.txt").exists());
        assert!(ws.root().join("n").is_dir(), "the delete's folder was taken away");
        undo(&deleted, &s.backups()).unwrap();
        assert!(ws.root().join("n/y.txt").exists(), "the deleted file came back");
        undo(&second, &s.backups()).unwrap();
    }

    #[test]
    fn a_single_undo_keeps_a_made_folder_when_another_record_cannot_be_read() {
        let s = Sandbox::new("mk-needed-unreadable");
        let ws = s.ws();
        let first = create(&s, &ws, "n/x.txt", "x\n");
        // A record that is there but cannot be parsed: there is no telling it does
        // not lie inside `n/`, so the folder stays.
        std::fs::write(s.backups().join("broken.json"), b"{ half a write").unwrap();
        undo(&first, &s.backups()).unwrap();
        assert!(!ws.root().join("n/x.txt").exists());
        assert!(ws.root().join("n").is_dir(), "the folder was removed on a guess");
    }

    #[test]
    fn a_single_undo_keeps_a_made_folder_a_later_move_came_from() {
        let s = Sandbox::new("mk-needed-move");
        let ws = s.ws();
        let first = create(&s, &ws, "n/x.txt", "x\n");
        s.write("n/mine.txt", b"mine\n");
        let moved = apply(&plan_move(&ws, "n/mine.txt", "out.txt").unwrap(), &s.backups()).unwrap().id;
        undo(&first, &s.backups()).unwrap();
        assert!(ws.root().join("n").is_dir());
        undo(&moved, &s.backups()).unwrap();
        assert_eq!(s.read("n/mine.txt"), b"mine\n");
    }

    #[test]
    fn undo_all_removes_a_made_folder_once_nothing_needs_it() {
        let s = Sandbox::new("mk-needed-turn");
        let ws = s.ws();
        let first = create(&s, &ws, "n/x.txt", "x\n");
        let second = create(&s, &ws, "n/y.txt", "y\n");
        let deleted = apply(&plan_delete(&ws, "n/y.txt").unwrap(), &s.backups()).unwrap().id;
        let out = undo_turn(&[first, second, deleted], &s.backups()).unwrap();
        assert!(out.failed.is_none(), "{out:?}");
        assert!(!ws.root().join("n").exists());
        assert!(backup_files(&s).is_empty());
    }

    #[test]
    fn a_made_folder_is_still_removed_while_unrelated_changes_are_live() {
        let s = Sandbox::new("mk-needed-other");
        let ws = s.ws();
        s.write("other/keep.txt", b"k\n");
        let _edit = apply(&plan_delete(&ws, "other/keep.txt").unwrap(), &s.backups()).unwrap().id;
        let made = create(&s, &ws, "n/x.txt", "x\n");
        undo(&made, &s.backups()).unwrap();
        assert!(!ws.root().join("n").exists());
    }

    #[test]
    fn two_creates_sharing_a_new_folder_leave_it_to_the_first() {
        let s = Sandbox::new("mk-shared");
        let ws = s.ws();
        // In order: undo all newest first, and the folder goes with the first.
        let first = create(&s, &ws, "pkg/one.rs", "1\n");
        let second = create(&s, &ws, "pkg/two.rs", "2\n");
        undo_turn(&[first, second], &s.backups()).unwrap();
        assert!(!ws.root().join("pkg").exists());

        // Out of order: undoing the first alone leaves the folder, and so does
        // undoing the second afterwards, which never made it. Documented, pinned.
        let first = create(&s, &ws, "pkg/one.rs", "1\n");
        let second = create(&s, &ws, "pkg/two.rs", "2\n");
        undo(&first, &s.backups()).unwrap();
        assert!(ws.root().join("pkg/two.rs").exists());
        undo(&second, &s.backups()).unwrap();
        assert!(ws.root().join("pkg").exists(), "an empty folder is left behind, never removed by the wrong undo");
    }

    #[test]
    fn a_junction_swapped_in_for_a_planned_folder_is_not_written_through() {
        let s = Sandbox::new("mk-swap");
        let ws = s.ws();
        let target = s.dir.join("elsewhere");
        std::fs::create_dir_all(&target).unwrap();
        let plan = plan_create(&ws, "sub/x.rs", "x\n").unwrap();
        if !junction(&ws.root().join("sub"), &target) {
            return;
        }
        let err = apply(&plan, &s.backups()).unwrap_err();
        assert!(err.contains("not the folder that was planned"), "{err}");
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0, "nothing was written through it");
        assert!(backup_files(&s).is_empty());
    }

    #[test]
    fn a_failed_apply_leaves_no_record_and_no_folder() {
        let s = Sandbox::new("mk-fail");
        let ws = s.ws();
        let mut plan = plan_create(&ws, "a/b/x.rs", "x\n").unwrap();
        // Plan only the first of its two folders, so the second is missing when the
        // file's folder is checked — a failure after a folder has been made.
        plan.make_dirs.truncate(1);
        assert!(apply(&plan, &s.backups()).is_err());
        assert!(!ws.root().join("a").exists(), "the folder this apply made is taken away again");
        assert!(backup_files(&s).is_empty());
    }

    #[test]
    fn a_create_never_writes_over_a_folder_that_appeared() {
        let s = Sandbox::new("mk-folder");
        let ws = s.ws();
        let plan = plan_create(&ws, "x.rs", "x\n").unwrap();
        std::fs::create_dir(ws.root().join("x.rs")).unwrap();
        assert!(apply(&plan, &s.backups()).unwrap_err().contains("created by something else"));
        assert!(ws.root().join("x.rs").is_dir());
    }

    #[test]
    fn move_new_and_write_new_never_replace() {
        let s = Sandbox::new("no-replace");
        let a = s.write("a.txt", b"a\n");
        let b = s.write("b.txt", b"b\n");
        let err = move_new(&a, &b).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!((s.read("a.txt"), s.read("b.txt")), (b"a\n".to_vec(), b"b\n".to_vec()));
        assert!(write_new(&b, b"c\n").is_err());
        assert_eq!(s.read("b.txt"), b"b\n");
        let names: Vec<String> = std::fs::read_dir(s.dir.join("project"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 2, "no temporary file is left behind: {names:?}");
    }

    // ── H9: deleting ────────────────────────────────────────────────────────

    #[test]
    fn a_deleted_file_comes_back_byte_for_byte() {
        let s = Sandbox::new("del-bytes");
        let ws = s.ws();
        let crlf: &[u8] = b"line one\r\nline two\r\n";
        let bom: &[u8] = b"\xEF\xBB\xBFhello\r\n";
        let bin: &[u8] = b"\x00\x01\xff\xfe binary \x00";
        for (i, bytes) in [crlf, bom, bin].into_iter().enumerate() {
            let rel = format!("f{i}.dat");
            s.write(&rel, bytes);
            let plan = plan_delete(&ws, &rel).unwrap();
            assert_eq!((plan.op, plan.size, plan.after.len()), (Op::Delete, bytes.len() as u64, 0));
            let done = apply(&plan, &s.backups()).unwrap();
            assert!(!ws.root().join(&rel).exists());
            assert_eq!(undo(&done.id, &s.backups()).unwrap(), rel);
            assert_eq!(s.read(&rel), bytes);
        }
        assert!(backup_files(&s).is_empty());
    }

    #[test]
    fn a_delete_refuses_a_read_only_file_a_folder_a_link_and_a_big_file() {
        let s = Sandbox::new("del-refuse");
        let ws = s.ws();
        let p = s.write("locked.txt", b"x");
        let mut perm = std::fs::metadata(&p).unwrap().permissions();
        perm.set_readonly(true);
        std::fs::set_permissions(&p, perm.clone()).unwrap();
        let err = plan_delete(&ws, "locked.txt").unwrap_err();
        assert!(err.contains("read-only"), "{err}");
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        std::fs::set_permissions(&p, perm).unwrap();

        std::fs::create_dir_all(ws.root().join("src")).unwrap();
        assert!(plan_delete(&ws, "src").unwrap_err().contains("folder"));

        s.write("big.bin", &vec![0u8; MAX_DELETE_BYTES as usize + 1]);
        assert!(plan_delete(&ws, "big.bin").unwrap_err().contains("byte limit"));

        assert!(plan_delete(&ws, "nope.txt").unwrap_err().contains("does not exist"));
        assert!(plan_delete(&ws, ".git/config").is_err());
        assert!(plan_delete(&ws, "../outside.txt").is_err());
    }

    #[test]
    fn a_file_reached_through_a_link_is_never_deleted_or_moved() {
        let s = Sandbox::new("del-link");
        let ws = s.ws();
        let target = ws.root().join("real");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("a.txt"), b"x").unwrap();
        if !junction(&ws.root().join("alias"), &target) {
            return;
        }
        let err = plan_delete(&ws, "alias/a.txt").unwrap_err();
        assert!(err.contains("reached through a link") && err.contains("real/a.txt"), "{err}");
        assert!(plan_move(&ws, "alias/a.txt", "b.txt").unwrap_err().contains("link"));
        assert!(plan_delete(&ws, "real/a.txt").is_ok());
        assert!(plan_delete(&ws, "alias").is_err(), "the link itself is a folder");
    }

    #[test]
    fn undoing_a_delete_never_overwrites_what_is_there_now() {
        let s = Sandbox::new("del-occupied");
        let ws = s.ws();
        s.write("a.txt", b"original\n");
        let id = delete(&s, &ws, "a.txt");
        s.write("a.txt", b"someone wrote this\n");
        let err = undo(&id, &s.backups()).unwrap_err();
        assert!(err.contains("exists again"), "{err}");
        assert_eq!(s.read("a.txt"), b"someone wrote this\n");
        assert!(s.backups().join(format!("{id}.bak")).exists(), "the backup is kept for a later try");
    }

    #[test]
    fn undoing_a_delete_needs_its_folder() {
        let s = Sandbox::new("del-nofolder");
        let ws = s.ws();
        s.write("old/a.txt", b"x\n");
        let id = delete(&s, &ws, "old/a.txt");
        std::fs::remove_dir(ws.root().join("old")).unwrap();
        let err = undo(&id, &s.backups()).unwrap_err();
        assert!(err.contains("folder") && err.contains("gone"), "{err}");
        assert!(!ws.root().join("old").exists(), "no folder is made up for it");
    }

    #[test]
    fn a_delete_of_a_file_that_changed_since_it_was_shown_is_not_done() {
        let s = Sandbox::new("del-stale");
        let ws = s.ws();
        s.write("a.txt", b"one\n");
        let plan = plan_delete(&ws, "a.txt").unwrap();
        s.write("a.txt", b"one\nand more\n");
        let err = apply(&plan, &s.backups()).unwrap_err();
        assert!(err.contains("changed after this delete was shown"), "{err}");
        assert_eq!(s.read("a.txt"), b"one\nand more\n");
        assert!(backup_files(&s).is_empty());
    }

    // ── H9: moving ──────────────────────────────────────────────────────────

    #[test]
    fn a_move_goes_to_new_folders_and_undo_brings_it_back_and_tidies() {
        let s = Sandbox::new("mv-undo");
        let ws = s.ws();
        let bytes: &[u8] = b"\xEF\xBB\xBFpub fn f() {}\r\n";
        s.write("src/util.rs", bytes);
        let plan = plan_move(&ws, "src/util.rs", "src/helpers/util.rs").unwrap();
        assert_eq!((plan.display.as_str(), plan.to_display.as_deref()), ("src/util.rs", Some("src/helpers/util.rs")));
        assert_eq!(plan.folders, ["src/helpers/"]);
        let done = apply(&plan, &s.backups()).unwrap();
        assert_eq!((done.op, done.to.as_deref()), (Op::Move, Some("src/helpers/util.rs")));
        assert!(!ws.root().join("src/util.rs").exists());
        assert_eq!(s.read("src/helpers/util.rs"), bytes);
        undo(&done.id, &s.backups()).unwrap();
        assert_eq!(s.read("src/util.rs"), bytes);
        assert!(!ws.root().join("src/helpers").exists());
        assert!(backup_files(&s).is_empty());
    }

    #[test]
    fn a_move_onto_an_existing_file_or_folder_is_refused_with_advice() {
        let s = Sandbox::new("mv-exists");
        let ws = s.ws();
        s.write("a.rs", b"a\n");
        s.write("b.rs", b"b\n");
        s.write("lib/x.rs", b"x\n");
        let err = plan_move(&ws, "a.rs", "b.rs").unwrap_err();
        assert!(err.contains("already exists") && err.contains("nothing is overwritten"), "{err}");
        let err = plan_move(&ws, "a.rs", "lib").unwrap_err();
        assert!(err.contains("existing folder") && err.contains("lib/a.rs"), "{err}");
        let err = plan_move(&ws, "a.rs", "lib/").unwrap_err();
        assert!(err.contains("lib/a.rs"), "{err}");
        assert_eq!((s.read("a.rs"), s.read("b.rs")), (b"a\n".to_vec(), b"b\n".to_vec()));
    }

    #[test]
    fn a_destination_that_appears_after_planning_is_never_overwritten() {
        let s = Sandbox::new("mv-race");
        let ws = s.ws();
        s.write("a.rs", b"a\n");
        let plan = plan_move(&ws, "a.rs", "sub/b.rs").unwrap();
        s.write("sub/b.rs", b"theirs\n");
        let err = apply(&plan, &s.backups()).unwrap_err();
        assert!(err.contains("created by something else"), "{err}");
        assert_eq!((s.read("a.rs"), s.read("sub/b.rs")), (b"a\n".to_vec(), b"theirs\n".to_vec()));
        assert!(backup_files(&s).is_empty());
    }

    #[test]
    fn the_rename_itself_refuses_a_destination_that_appears_in_the_last_moment() {
        // The check in `apply` and the rename are two steps; the second must hold on
        // its own. A plan is edited to skip the first, as a race would.
        let s = Sandbox::new("mv-raw");
        let ws = s.ws();
        s.write("a.rs", b"a\n");
        let plan = plan_move(&ws, "a.rs", "b.rs").unwrap();
        s.write("b.rs", b"theirs\n");
        assert!(move_new(&plan.path, plan.to.as_deref().unwrap()).is_err());
        assert_eq!((s.read("a.rs"), s.read("b.rs")), (b"a\n".to_vec(), b"theirs\n".to_vec()));
    }

    #[test]
    fn a_case_only_rename_is_refused_as_the_same_file() {
        let s = Sandbox::new("mv-case");
        let ws = s.ws();
        s.write("util.rs", b"x\n");
        let err = plan_move(&ws, "util.rs", "Util.rs").unwrap_err();
        assert!(err.contains("same file"), "{err}");
        assert!(plan_move(&ws, "util.rs", "./util.rs").unwrap_err().contains("same file"));
    }

    #[test]
    fn undoing_a_move_refuses_if_either_end_has_changed() {
        let s = Sandbox::new("mv-changed");
        let ws = s.ws();
        s.write("a.txt", b"one\n");
        let id = mv(&s, &ws, "a.txt", "b.txt");
        s.write("b.txt", b"one\nedited since\n");
        let err = undo(&id, &s.backups()).unwrap_err();
        assert!(err.contains("b.txt has changed"), "{err}");
        assert_eq!(s.read("b.txt"), b"one\nedited since\n");

        s.write("b.txt", b"one\n");
        s.write("a.txt", b"a new a\n");
        let err = undo(&id, &s.backups()).unwrap_err();
        assert!(err.contains("a.txt exists again"), "{err}");
        assert_eq!((s.read("a.txt"), s.read("b.txt")), (b"a new a\n".to_vec(), b"one\n".to_vec()));
        std::fs::remove_file(ws.root().join("a.txt")).unwrap();
        undo(&id, &s.backups()).unwrap();
        assert_eq!(s.read("a.txt"), b"one\n");
    }

    #[test]
    fn a_move_refuses_a_folder_a_missing_file_and_the_wrong_places() {
        let s = Sandbox::new("mv-refuse");
        let ws = s.ws();
        s.write("src/a.rs", b"x\n");
        assert!(plan_move(&ws, "src", "elsewhere").unwrap_err().contains("folder"));
        assert!(plan_move(&ws, "nope.rs", "x.rs").unwrap_err().contains("does not exist"));
        assert!(plan_move(&ws, "src/a.rs", "../out.rs").unwrap_err().contains("outside"));
        assert!(plan_move(&ws, "src/a.rs", ".git/a.rs").unwrap_err().contains(".git"));
        assert!(plan_move(&ws, "src/a.rs", "con.rs").unwrap_err().contains("Windows"));
        assert!(plan_move(&ws, ".git/config", "x").is_err());
        assert!(plan_move(&ws, "src/a.rs", "src/a.rs/inside.rs").unwrap_err().contains("not a folder"));
    }

    // ── H9: a turn of deletes and moves ─────────────────────────────────────

    #[test]
    fn a_create_then_a_delete_of_it_is_undone_as_nothing_having_happened() {
        let s = Sandbox::new("t-create-delete");
        let ws = s.ws();
        let made = create(&s, &ws, "x/new.txt", "hi\n");
        let gone = delete(&s, &ws, "x/new.txt");
        let out = undo_turn(&[made, gone], &s.backups()).unwrap();
        assert_eq!(out.restored.len(), 2);
        assert!(!ws.root().join("x").exists());
        assert!(backup_files(&s).is_empty());
    }

    #[test]
    fn a_delete_then_a_create_of_the_same_path_puts_the_original_back() {
        let s = Sandbox::new("t-delete-create");
        let ws = s.ws();
        s.write("a.txt", b"original\r\n");
        let gone = delete(&s, &ws, "a.txt");
        let made = create(&s, &ws, "a.txt", "replacement\n");
        undo_turn(&[gone, made], &s.backups()).unwrap();
        assert_eq!(s.read("a.txt"), b"original\r\n");
    }

    #[test]
    fn a_move_then_an_edit_of_the_moved_file_is_put_back() {
        let s = Sandbox::new("t-move-edit");
        let ws = s.ws();
        s.write("a.txt", b"one two\n");
        let moved = mv(&s, &ws, "a.txt", "sub/b.txt");
        let edited = replace(&s, &ws, "sub/b.txt", "two", "2");
        undo_turn(&[moved, edited], &s.backups()).unwrap();
        assert_eq!(s.read("a.txt"), b"one two\n");
        assert!(!ws.root().join("sub").exists());
    }

    #[test]
    fn an_edit_then_a_move_is_put_back() {
        let s = Sandbox::new("t-edit-move");
        let ws = s.ws();
        s.write("a.txt", b"one two\n");
        let edited = replace(&s, &ws, "a.txt", "one", "1");
        let moved = mv(&s, &ws, "a.txt", "b.txt");
        undo_turn(&[edited, moved], &s.backups()).unwrap();
        assert_eq!(s.read("a.txt"), b"one two\n");
        assert!(!ws.root().join("b.txt").exists());
    }

    #[test]
    fn a_move_of_a_move_is_walked_back() {
        let s = Sandbox::new("t-move-move");
        let ws = s.ws();
        s.write("a.txt", b"x\n");
        let one = mv(&s, &ws, "a.txt", "b.txt");
        let two = mv(&s, &ws, "b.txt", "deep/er/c.txt");
        undo_turn(&[two, one], &s.backups()).unwrap();
        assert_eq!(s.read("a.txt"), b"x\n");
        assert!(!ws.root().join("b.txt").exists() && !ws.root().join("deep").exists());
    }

    #[test]
    fn a_delete_and_then_a_move_onto_that_path_is_put_back() {
        let s = Sandbox::new("t-delete-move");
        let ws = s.ws();
        s.write("old.rs", b"the old one\n");
        s.write("new.rs", b"the new one\n");
        let gone = delete(&s, &ws, "old.rs");
        let moved = mv(&s, &ws, "new.rs", "old.rs");
        undo_turn(&[gone, moved], &s.backups()).unwrap();
        assert_eq!((s.read("old.rs"), s.read("new.rs")), (b"the old one\n".to_vec(), b"the new one\n".to_vec()));
    }

    #[test]
    fn something_put_on_the_moves_source_since_refuses_the_whole_turn() {
        let s = Sandbox::new("t-on-top");
        let ws = s.ws();
        s.write("a.txt", b"one\n");
        s.write("c.txt", b"c\n");
        let edited = replace(&s, &ws, "c.txt", "c", "C");
        let moved = mv(&s, &ws, "a.txt", "b.txt");
        s.write("a.txt", b"somebody else's\n");
        let err = undo_turn(&[edited, moved], &s.backups()).unwrap_err();
        assert!(err.starts_with("Nothing was undone:") && err.contains("a.txt exists again"), "{err}");
        assert_eq!((s.read("c.txt"), s.read("a.txt"), s.read("b.txt")), (b"C\n".to_vec(), b"somebody else's\n".to_vec(), b"one\n".to_vec()));
    }

    #[test]
    fn two_spellings_of_one_path_are_one_file_in_the_simulation() {
        // Delete `a.txt`, then create `A.txt`: NTFS has one file, the records have
        // two spellings, and the walk must see one.
        let s = Sandbox::new("t-case");
        let ws = s.ws();
        s.write("a.txt", b"original\n");
        let gone = delete(&s, &ws, "a.txt");
        let made = create(&s, &ws, "A.txt", "replacement\n");
        undo_turn(&[gone, made], &s.backups()).unwrap();
        assert_eq!(s.read("a.txt"), b"original\n");
        assert!(backup_files(&s).is_empty());
    }

    #[test]
    fn one_hand_change_is_one_problem_however_many_changes_sit_on_it() {
        let s = Sandbox::new("t-one-problem");
        let ws = s.ws();
        s.write("a.txt", b"one two three\n");
        let first = replace(&s, &ws, "a.txt", "one", "1");
        let second = replace(&s, &ws, "a.txt", "two", "2");
        let third = mv(&s, &ws, "a.txt", "b.txt");
        s.write("b.txt", b"mine\n");
        let err = undo_turn(&[first, second, third], &s.backups()).unwrap_err();
        assert_eq!(err.matches("b.txt").count(), 1, "{err}");
        assert!(!err.contains("a.txt"), "{err}");
    }

    #[test]
    fn a_backup_gone_missing_stops_a_turn_with_a_delete() {
        let s = Sandbox::new("t-delete-nobak");
        let ws = s.ws();
        s.write("a.txt", b"x\n");
        let gone = delete(&s, &ws, "a.txt");
        std::fs::remove_file(s.backups().join(format!("{gone}.bak"))).unwrap();
        let err = undo_turn(&[gone], &s.backups()).unwrap_err();
        assert!(err.contains("backup of a.txt is missing"), "{err}");
    }

    #[test]
    fn a_turn_that_fails_the_simulation_touches_nothing_even_with_a_move_in_it() {
        let s = Sandbox::new("t-untouched");
        let ws = s.ws();
        s.write("a.txt", b"a\n");
        s.write("b.txt", b"b\n");
        let moved = mv(&s, &ws, "a.txt", "sub/a.txt");
        let edited = replace(&s, &ws, "b.txt", "b", "B");
        s.write("b.txt", b"mine\n");
        let before = backup_files(&s);
        assert!(undo_turn(&[moved, edited], &s.backups()).is_err());
        assert!(ws.root().join("sub/a.txt").exists() && !ws.root().join("a.txt").exists());
        let mut after = backup_files(&s);
        let mut before = before;
        before.sort();
        after.sort();
        assert_eq!(before, after);
    }

    #[test]
    fn records_written_before_h9_still_load_and_undo() {
        let s = Sandbox::new("t-old-records");
        let ws = s.ws();
        let p = s.write("new.txt", b"hi\n");
        let id = new_id();
        std::fs::create_dir_all(s.backups()).unwrap();
        let old = serde_json::json!({
            "path": p, "display": "new.txt", "create": true, "after": fingerprint(b"hi\n")
        });
        std::fs::write(s.backups().join(format!("{id}.json")), old.to_string()).unwrap();
        let r: Record = serde_json::from_slice(&std::fs::read(s.backups().join(format!("{id}.json"))).unwrap()).unwrap();
        assert_eq!((r.op(), r.to, r.made_dirs.len()), (Op::Create, None, 0));
        undo(&id, &s.backups()).unwrap();
        assert!(!ws.root().join("new.txt").exists());

        let id = new_id();
        s.write("a.txt", b"changed\n");
        std::fs::write(s.backups().join(format!("{id}.bak")), b"original\n").unwrap();
        let old = serde_json::json!({
            "path": ws.root().join("a.txt"), "display": "a.txt", "create": false, "after": fingerprint(b"changed\n")
        });
        std::fs::write(s.backups().join(format!("{id}.json")), old.to_string()).unwrap();
        undo_turn(std::slice::from_ref(&id), &s.backups()).unwrap();
        assert_eq!(s.read("a.txt"), b"original\n");
    }

    #[test]
    fn an_old_build_cannot_misread_a_delete_or_a_move_record() {
        // What H8's undo does with these records: it reads `path`, `create` and
        // `after` only. Neither has `create: true`, and `path` is not where the
        // file now is, so it stops at "no longer exists".
        let s = Sandbox::new("t-forward");
        let ws = s.ws();
        s.write("a.txt", b"x\n");
        s.write("m.txt", b"y\n");
        let gone = delete(&s, &ws, "a.txt");
        let moved = mv(&s, &ws, "m.txt", "n.txt");
        for id in [gone, moved] {
            let v: serde_json::Value =
                serde_json::from_slice(&std::fs::read(s.backups().join(format!("{id}.json"))).unwrap()).unwrap();
            assert_eq!(v["create"], false);
            assert!(!Path::new(v["path"].as_str().unwrap()).exists());
        }
    }

    // ── review round 2: R-12 to R-16 ────────────────────────────────────────

    #[test]
    fn single_undo_refuses_a_backup_that_is_not_the_original() {
        // R-12. Undo all compares the backup with `before`; the single undo used to
        // write whatever `.bak` held — a truncated or swapped one — over the file.
        let s = Sandbox::new("r12");
        let ws = s.ws();
        s.write("a.txt", b"one two\n");
        let id = replace(&s, &ws, "a.txt", "one", "1");
        let edited = s.read("a.txt");
        std::fs::write(s.backups().join(format!("{id}.bak")), b"").unwrap();
        let err = undo(&id, &s.backups()).unwrap_err();
        assert!(err.contains("not what the file was before"), "{err}");
        assert_eq!(s.read("a.txt"), edited);
        assert!(s.backups().join(format!("{id}.json")).exists(), "the record is kept");
    }

    #[cfg(windows)]
    #[test]
    fn a_record_that_cannot_be_read_is_not_reported_as_already_undone() {
        // R-13. Only a record that is not there means "already undone"; one held open
        // by another process is a change still on disk, and the card must not flip.
        use std::os::windows::fs::OpenOptionsExt;
        let s = Sandbox::new("r13");
        let ws = s.ws();
        s.write("a.txt", b"one\n");
        let id = replace(&s, &ws, "a.txt", "one", "two");
        let json = s.backups().join(format!("{id}.json"));
        let lock = std::fs::OpenOptions::new().read(true).share_mode(0).open(&json).unwrap();
        let single = undo(&id, &s.backups()).unwrap_err();
        let turn = undo_turn(std::slice::from_ref(&id), &s.backups()).unwrap_err();
        drop(lock);
        assert!(!single.contains("already been undone") && single.contains(&id), "{single}");
        assert!(turn.contains(&id), "{turn}");
        assert_eq!(s.read("a.txt"), b"two\n");
        assert!(json.exists());
        // and a record that really is gone still is "already undone"
        let gone = undo_turn(&["abc123".to_string()], &s.backups()).unwrap();
        assert_eq!(gone.already, vec!["abc123".to_string()]);
        assert!(undo("abc123", &s.backups()).unwrap_err().contains("already been undone"));
    }

    /// Saves `path` the way an editor does as soon as a file named by `pred` shows up
    /// in `watch`; true if the save was made.
    fn editor_save_on(
        path: PathBuf,
        watch: PathBuf,
        pred: fn(&str) -> bool,
        done: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> std::thread::JoinHandle<bool> {
        use std::sync::atomic::Ordering;
        std::thread::spawn(move || {
            let t0 = std::time::Instant::now();
            while !done.load(Ordering::SeqCst) && t0.elapsed().as_secs() < 5 {
                let hit = std::fs::read_dir(&watch)
                    .map(|rd| rd.flatten().any(|e| pred(&e.file_name().to_string_lossy())))
                    .unwrap_or(false);
                if hit {
                    return std::fs::write(&path, b"one\nSAVED BY AN EDITOR AFTER THE CHECK\n").is_ok();
                }
            }
            false
        })
    }

    #[test]
    fn an_edit_never_replaces_a_save_that_landed_after_its_staleness_check() {
        // R-14. The save lands as soon as the record is written — after the check at the
        // start of `apply`, before the replacing rename. The old code lost it about a
        // quarter of the time; now it is refused, or the save fails, never replaced.
        let mut lost = 0;
        for _ in 0..30 {
            let s = Sandbox::new("r14-edit");
            let ws = s.ws();
            let path = s.write("a.txt", b"one\n");
            let plan = plan_replace(&ws, "a.txt", "one", "two").unwrap();
            std::fs::create_dir_all(s.backups()).unwrap();
            let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let h = editor_save_on(path, s.backups(), |n| n.ends_with(".json"), done.clone());
            let r = apply(&plan, &s.backups());
            done.store(true, std::sync::atomic::Ordering::SeqCst);
            let saved = h.join().unwrap();
            if r.is_ok() && saved && s.read("a.txt") == plan.after {
                lost += 1;
            }
        }
        assert_eq!(lost, 0, "an edit replaced a save that landed after its check, {lost} of 30 times");
    }

    #[test]
    fn the_undo_of_an_edit_never_replaces_a_save_that_landed_after_its_check() {
        // R-14, undo's side: the save lands when the temporary file appears.
        let mut lost = 0;
        for _ in 0..30 {
            let s = Sandbox::new("r14-undo");
            let ws = s.ws();
            let path = s.write("a.txt", b"one\n");
            let id = replace(&s, &ws, "a.txt", "one", "two");
            let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let h = editor_save_on(path, ws.root().to_path_buf(), |n| n.ends_with(".odin-tmp"), done.clone());
            let r = undo(&id, &s.backups());
            done.store(true, std::sync::atomic::Ordering::SeqCst);
            let saved = h.join().unwrap();
            if r.is_ok() && saved && s.read("a.txt") == b"one\n" {
                lost += 1;
            }
        }
        assert_eq!(lost, 0, "an undo replaced a save that landed after its check, {lost} of 30 times");
    }

    #[test]
    fn a_file_that_changed_before_the_replacing_rename_is_left_alone_and_no_temp_remains() {
        // R-14, the check itself, with no timing: `write_replacing` given a hash that
        // the file does not have.
        let s = Sandbox::new("r14-unit");
        let p = s.write("a.txt", b"theirs\n");
        let err = write_replacing(&p, b"ours\n", fingerprint(b"one\n")).unwrap_err();
        assert!(err.contains("changed just now"), "{err}");
        assert_eq!(s.read("a.txt"), b"theirs\n");
        assert!(!s.dir.join("project").join(".a.txt.odin-tmp").exists());
        write_replacing(&p, b"ours\n", fingerprint(b"theirs\n")).unwrap();
        assert_eq!(s.read("a.txt"), b"ours\n");
    }

    #[cfg(windows)]
    fn hold_open_for_writing(path: &Path, for_ms: u64) -> std::thread::JoinHandle<()> {
        use std::os::windows::fs::OpenOptionsExt;
        let (tx, rx) = std::sync::mpsc::channel();
        let path = path.to_path_buf();
        let h = std::thread::spawn(move || {
            // Write access, sharing read and write: what a log tailer or syncer holds.
            let f = std::fs::OpenOptions::new().read(true).write(true).share_mode(1 | 2).open(&path).unwrap();
            tx.send(()).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(for_ms));
            drop(f);
        });
        rx.recv().unwrap();
        h
    }

    #[cfg(windows)]
    #[test]
    fn a_brief_hold_on_the_file_by_another_program_is_waited_out() {
        let s = Sandbox::new("held-brief");
        let p = s.write("a.txt", b"one
");
        let h = hold_open_for_writing(&p, 120);
        write_replacing(&p, b"two
", fingerprint(b"one
")).unwrap();
        h.join().unwrap();
        assert_eq!(s.read("a.txt"), b"two
");
    }

    #[cfg(windows)]
    #[test]
    fn a_file_held_open_for_writing_throughout_is_reported_plainly_and_left_alone() {
        let s = Sandbox::new("held-long");
        let p = s.write("a.txt", b"one
");
        let h = hold_open_for_writing(&p, 800);
        let err = write_replacing(&p, b"two
", fingerprint(b"one
")).unwrap_err();
        h.join().unwrap();
        assert!(err.contains("is open for writing in another program; nothing was changed"), "{err}");
        assert!(!err.contains("reading"), "{err}");
        assert_eq!(s.read("a.txt"), b"one
");
        assert!(!s.dir.join("project").join(".a.txt.odin-tmp").exists());
    }

    #[test]
    fn applied_folders_names_only_the_folders_this_apply_made() {
        // R-15. `n/` appeared after the card was shown; only `n/m/` is this create's.
        let s = Sandbox::new("r15");
        let ws = s.ws();
        let plan = plan_create(&ws, "n/m/x.txt", "x\n").unwrap();
        assert_eq!(plan.folders, ["n/", "n/m/"]);
        std::fs::create_dir(ws.root().join("n")).unwrap();
        let done = apply(&plan, &s.backups()).unwrap();
        assert_eq!(done.folders, ["n/m/"]);
        undo(&done.id, &s.backups()).unwrap();
        assert!(ws.root().join("n").is_dir() && !ws.root().join("n/m").exists());
    }

    mod tracking {
        //! Counts the bytes the current thread holds at once, so a test can say how
        //! much memory one call needed while other tests run beside it.
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
                let _ = PEAK.try_with(|p| p.set(p.get().max(n)));
            });
        }

        fn shrink(by: usize) {
            let _ = CUR.try_with(|c| c.set(c.get().saturating_sub(by)));
        }

        // SAFETY: every call is forwarded to `System` unchanged; only counters change.
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

        /// Run `f`; the most bytes this thread held beyond what it held before.
        pub fn peak_of<T>(f: impl FnOnce() -> T) -> (T, usize) {
            let base = CUR.with(|c| c.get());
            PEAK.with(|p| p.set(base));
            let out = f();
            (out, PEAK.with(|p| p.get()).saturating_sub(base))
        }
    }

    #[global_allocator]
    static ALLOC: tracking::Track = tracking::Track;

    #[test]
    fn applying_a_move_does_not_hold_the_file_in_memory() {
        // R-16. Moves have no size limit; the last-moment re-check used to read the
        // whole file. A sparse 64 MiB file costs nothing on disk.
        let s = Sandbox::new("r16");
        let ws = s.ws();
        let f = std::fs::File::create(ws.root().join("big.bin")).unwrap();
        f.set_len(64 * 1024 * 1024).unwrap();
        drop(f);
        let plan = plan_move(&ws, "big.bin", "sub/big.bin").unwrap();
        let (done, peak) = tracking::peak_of(|| apply(&plan, &s.backups()).unwrap());
        assert!(ws.root().join("sub/big.bin").exists() && !ws.root().join("big.bin").exists());
        undo(&done.id, &s.backups()).unwrap();
        assert!(peak < 4 << 20, "apply held {} MiB for a move of 64 MiB", peak >> 20);
    }
}
