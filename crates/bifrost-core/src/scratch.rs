//! A scratch copy of a git folder, for work that must leave the real folder alone (I5).
//!
//! `hugin -p` hands a task to the local model with nobody there to approve anything, so
//! the model works in a **copy**: the repository's HEAD plus the user's uncommitted files,
//! in a folder of its own under `%TEMP%`. What it did comes back as a patch the caller can
//! review and apply. The user's files and their `.git` are never written, and nothing here
//! runs in the user's repository but commands that only read.
//!
//! **A plain copy, not a `git worktree`.** `git worktree add` writes into the user's
//! `.git`, and a run that is killed (Claude Code kills a command at its timeout) leaves its
//! entry there. **Nor does the copy borrow the repository's objects** (an
//! `objects/info/alternates` file): git "freshens" an object it is about to write when the
//! store it reads already holds it, which sets the *time* of the user's loose object, or of
//! the whole pack, whenever the copy hashes a file the repository already has. So the copy
//! is a clone of one commit, `git clone --no-checkout --depth 1 --no-local --template=`
//! through a `file://` URL, which reads the repository through git's own transport and
//! keeps every object it writes to itself. HEAD is then checked out with `git read-tree -u
//! --reset`, which runs no hook. The copy has a real HEAD, so a check that asks git for the
//! commit (a build script stamping a version) finds one, and no remote: the one the clone
//! made is removed, so nothing run in the copy can push to the user's repository.
//!
//! The uncommitted files are then copied as exact bytes, CRLF and binary included, from
//! `git status`; a file the user deleted is deleted in the copy. **Nothing that is a link
//! or a junction is followed**, in the user's folder or in the copy: git for Windows reads
//! a junction as a folder, and what lies beyond one is not the repository's. `core.autocrlf`
//! and `core.eol` are mirrored and `info/exclude` is copied, so the copy treats every file as
//! the repository does.
//!
//! **The patch is made from trees, not from the working folder.** `git add -A` and
//! `git write-tree` in the copy give the tree it started with and the tree it ended with
//! (the first when [`Scratch::of`] returns, the second from [`Scratch::snapshot`]), and
//! `git diff-tree` between the two is the patch. Plumbing takes no part of the user's git
//! settings (a `diff.noprefix`, a colour, an external diff tool), and the patch holds only
//! what the run changed, not the uncommitted work it started from.
//!
//! **Git is run by [`Git`], not by [`crate::check::run`].** The check's runner merges
//! stdout and stderr into one pipe, so git's line-ending warnings would end up inside the
//! patch. [`Git`] keeps the two apart and reads raw bytes, kills git if it hangs, never
//! prompts, never asks LFS for a download, and runs with the settings in [`Git::run`].
//!
//! Blocking, like the rest of this crate.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant, SystemTime};

use crate::{check, edit, store};

/// A file bigger than this is not copied from the user's folder. The copy has the committed
/// version of it, and the run says so.
pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// The most uncommitted file bytes one copy takes; past it the rest are named and not copied.
pub const MAX_COPY_BYTES: u64 = 256 * 1024 * 1024;

/// How long one git command may run before it is killed.
const GIT_TIMEOUT: Duration = Duration::from_secs(180);

/// The least a command is given when a [`Git::until`] deadline has all but passed: enough to
/// finish the patch, which a run that is out of time still owes its caller.
const MIN_LIMIT: Duration = Duration::from_secs(5);

/// How often the wait looks at git.
const POLL: Duration = Duration::from_millis(5);

/// How long to wait for the last of a command's output once it has ended, in case something
/// it started still holds the pipe.
const DRAIN: Duration = Duration::from_secs(5);

/// A run's folder is a leftover after this long, and the next run removes it.
pub const RUN_AGE: Duration = Duration::from_secs(6 * 60 * 60);

/// A patch and its record are kept this long.
pub const PATCH_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// At most this many names of files that were not carried are listed; the rest are counted.
const MAX_LISTED: usize = 20;

/// `%TEMP%\hugin-p`: every run's folder, the patches that were kept, and the lock.
pub fn work_dir() -> PathBuf {
    std::env::temp_dir().join("hugin-p")
}

// ── running git ─────────────────────────────────────────────────────────────

/// What a git command printed, as bytes, with the two streams apart.
#[derive(Debug)]
pub struct Out {
    /// `None` when it was ended by a signal.
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Out {
    fn ok(&self) -> bool {
        self.code == Some(0)
    }

    /// What git said on stderr, or its exit code when it said nothing.
    fn complaint(&self) -> String {
        let said = String::from_utf8_lossy(&self.stderr);
        let said = said.trim();
        if said.is_empty() {
            format!("exit code {}", self.code.map_or("none".to_string(), |c| c.to_string()))
        } else {
            said.to_string()
        }
    }
}

/// Environment variables that point git somewhere other than the folder it is run in. Git
/// sets them for its own hooks and aliases, and a run started from one would inherit them.
const CLEARED: [&str; 7] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_PREFIX",
];

/// A folder that does not exist, named as `core.hooksPath` so that no hook can be found.
const NO_HOOKS: &str = "hugin-no-hooks-here";

/// How git is run. A value of its own so that a test can add environment to every command —
/// a hostile `GIT_CONFIG_GLOBAL` — and shorten the time one may take, without changing the
/// process's or the constant, and so that a run can bound every command by the time it has left.
#[derive(Debug, Clone, Default)]
pub struct Git {
    env: Vec<(String, String)>,
    /// How long one command may run; [`GIT_TIMEOUT`] when `None`.
    timeout: Option<Duration>,
    /// When the run it belongs to is over, if it has a time limit.
    deadline: Option<Instant>,
}

impl Git {
    /// Git as the process finds it.
    pub fn new() -> Self {
        Self::default()
    }

    /// Git with `env` added to every command's environment.
    pub fn with_env(env: Vec<(String, String)>) -> Self {
        Self { env, ..Self::default() }
    }

    /// This git, with no command given longer than there is until `deadline` — but never less
    /// than [`MIN_LIMIT`], so that a run that has used its time can still make its patch.
    pub fn until(&self, deadline: Instant) -> Self {
        Self { deadline: Some(deadline), ..self.clone() }
    }

    /// How long a command may run now: [`GIT_TIMEOUT`], or the time left if that is less.
    fn limit(&self) -> Duration {
        let base = self.timeout.unwrap_or(GIT_TIMEOUT);
        match self.deadline {
            Some(deadline) => base.min(deadline.saturating_duration_since(Instant::now()).max(MIN_LIMIT)),
            None => base,
        }
    }

    /// Run `git <args>` in `dir`. **`Err` only when git could not be started or did not
    /// finish**; a command that ran and failed is an [`Out`] with its exit code.
    ///
    /// Every command runs with `--no-optional-locks` (so `git status` does not refresh the
    /// user's index), `core.safecrlf=false` (so a mixed file does not stop `git add`),
    /// `core.fsmonitor=false` (so no daemon is started), `core.longpaths=true`, and a hooks
    /// path that holds nothing. `GIT_TERMINAL_PROMPT=0` and `GIT_LFS_SKIP_SMUDGE=1` keep it
    /// from asking or downloading.
    fn run(&self, dir: &Path, args: &[&str]) -> Result<Out, String> {
        let hooks = format!("core.hooksPath={NO_HOOKS}");
        let mut all = vec!["--no-optional-locks", "-c", "core.safecrlf=false", "-c", "core.fsmonitor=false"];
        all.extend(["-c", "core.longpaths=true", "-c", &hooks]);
        all.extend(args);
        self.exec(dir, &all)
    }

    /// [`Git::run`], and `Err` with git's own complaint when it exits with anything but 0.
    fn ok(&self, dir: &Path, args: &[&str]) -> Result<Out, String> {
        let out = self.run(dir, args)?;
        if out.ok() { Ok(out) } else { Err(format!("git {}: {}", args.first().unwrap_or(&""), out.complaint())) }
    }

    /// The command exactly as given, with only the environment of [`Git`] around it.
    fn exec(&self, dir: &Path, args: &[&str]) -> Result<Out, String> {
        let mut cmd = Command::new("git");
        cmd.args(args).current_dir(dir).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        for name in CLEARED {
            cmd.env_remove(name);
        }
        cmd.env("GIT_TERMINAL_PROMPT", "0").env("GIT_LFS_SKIP_SMUDGE", "1");
        for (name, value) in &self.env {
            cmd.env(name, value);
        }
        check::no_window(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| format!("could not run git: {e} (is git installed?)"))?;
        let (stdout, stderr) = (drain(child.stdout.take()), drain(child.stderr.take()));
        let (started, limit) = (Instant::now(), self.limit());
        let code = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.code(),
                Ok(None) if started.elapsed() >= limit => {
                    kill_tree(&mut child);
                    let name = args.iter().find(|a| !a.starts_with('-') && !a.contains('=')).unwrap_or(&"");
                    return Err(format!("git {name} did not finish within {:.0} seconds", limit.as_secs_f64()));
                }
                Ok(None) => std::thread::sleep(POLL),
                Err(e) => return Err(format!("could not wait for git: {e}")),
            }
        };
        Ok(Out {
            code,
            stdout: stdout.recv_timeout(DRAIN).unwrap_or_default(),
            stderr: stderr.recv_timeout(DRAIN).unwrap_or_default(),
        })
    }

    /// Apply the patch in `patch` to the working folder of the repository at `root`, the
    /// way a person would: `git -C <root> apply --whitespace=nowarn [--check] <patch>`,
    /// with nothing added. `check_only` says whether it would apply, and writes nothing.
    pub fn apply(&self, root: &Path, patch: &Path, check_only: bool) -> Result<(), String> {
        let patch = patch.to_string_lossy();
        let mut args = vec!["apply", "--whitespace=nowarn"];
        if check_only {
            args.push("--check");
        }
        args.push(&patch);
        let out = self.exec(root, &args)?;
        if out.ok() { Ok(()) } else { Err(out.complaint()) }
    }
}

/// [`Git::apply`] with git as the process finds it.
pub fn apply(root: &Path, patch: &Path, check_only: bool) -> Result<(), String> {
    Git::new().apply(root, patch, check_only)
}

/// Read a pipe to its end on a thread of its own, so a command that fills one cannot stall.
fn drain(pipe: Option<impl Read + Send + 'static>) -> Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        let _ = tx.send(bytes);
    });
    rx
}

/// End git and whatever it started: `taskkill /T` walks the tree, which `Child::kill` does not.
fn kill_tree(child: &mut std::process::Child) {
    #[cfg(windows)]
    {
        let mut kill = Command::new("taskkill");
        kill.args(["/T", "/F", "/PID", &child.id().to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        check::no_window(&mut kill);
        let _ = kill.status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

// ── the copy ────────────────────────────────────────────────────────────────

/// Where the user's folder is: its repository, and the commit the copy starts from.
struct Found {
    /// The repository's top folder, where a patch applies.
    root: PathBuf,
    /// The repository's own git folder; for a linked worktree, the one it shares with the rest.
    git_dir: PathBuf,
    /// The folder's place inside the repository: empty at the top, else `sub/dir/`.
    prefix: String,
    /// The commit HEAD names.
    base: String,
}

impl Found {
    fn of(git: &Git, folder: &Path) -> Result<Self, String> {
        let not_git = || format!("{} is not in a git repository; hugin -p works on git folders only", folder.display());
        let out = git.run(folder, &["rev-parse", "--show-toplevel", "--git-common-dir", "--show-prefix"])?;
        if !out.ok() {
            // Git has more to say than "no" when the folder is a repository it will not open.
            let said = out.complaint();
            return Err(if said.contains("not a git repository") || said.contains("work tree") {
                not_git()
            } else {
                format!("git cannot use {} as a repository: {said}", folder.display())
            });
        }
        // Three lines, and the last is empty at the top of the repository: split, never trim.
        let text = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
        let mut lines = text.split('\n').map(str::trim);
        let (top, common, prefix) = (lines.next().unwrap_or(""), lines.next().unwrap_or(""), lines.next().unwrap_or(""));
        if top.is_empty() || common.is_empty() {
            return Err(not_git());
        }
        // Printed relative to the folder it was run in, or absolute for a linked worktree.
        let common = PathBuf::from(common);
        let common = if common.is_absolute() { common } else { folder.join(common) };
        let git_dir = std::fs::canonicalize(&common)
            .map(|p| PathBuf::from(edit::plain(&p)))
            .map_err(|e| format!("cannot find the repository's git folder {}: {e}", common.display()))?;
        // Git prints forward slashes; the path is shown to people and handed to cargo as Windows writes it.
        let root = PathBuf::from(if cfg!(windows) { top.replace('/', "\\") } else { top.to_string() });

        let head = git.run(&root, &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"])?;
        if !head.ok() {
            return Err("the repository has no commits yet; hugin -p starts from the last commit".to_string());
        }
        let base = String::from_utf8_lossy(&head.stdout).trim().to_string();

        if !prefix.is_empty() {
            let inside = prefix.trim_end_matches('/');
            // Exit 0 is "ignored"; 1 is not; anything else (128) is git unable to say.
            if git.run(&root, &["check-ignore", "--quiet", "--", inside])?.code == Some(0) {
                return Err(format!(
                    "{} is ignored by git, so a copy of it would hold none of its files",
                    folder.display()
                ));
            }
        }
        Ok(Self { root, git_dir, prefix: prefix.to_string(), base })
    }
}

/// What a copy of the user's folder is, and the tree it started with. **Dropping it removes
/// its folder.**
#[derive(Debug)]
pub struct Scratch {
    /// The repository's top folder, where the patch applies.
    pub repo_root: PathBuf,
    /// The repository's git folder (the shared one, for a linked worktree).
    pub git_dir: PathBuf,
    /// The commit the copy started from.
    pub base: String,
    /// Where the user's folder is inside the repository: empty at the top, else `sub/dir/`.
    pub prefix: String,
    /// What was not carried into the copy, each with why: a file over [`MAX_FILE_BYTES`], a
    /// submodule, an LFS pointer, a link. The copy holds the committed version, or nothing.
    pub skipped: Vec<String>,
    git: Git,
    run: PathBuf,
    copy: PathBuf,
    start: String,
}

/// Makes the name of each run's folder different within one process.
static RUNS: AtomicU32 = AtomicU32::new(0);

/// The time as a name can hold it: `20261021T153045Z`.
pub fn file_stamp() -> String {
    store::stamp().replace(['-', ':'], "")
}

impl Scratch {
    /// Copy the folder `folder` is in, as git sees it: see the module. `Err` says why not —
    /// not a git folder, no commits, a folder git ignores, git missing or failing.
    pub fn of(folder: &Path) -> Result<Scratch, String> {
        Self::of_in(&Git::new(), &work_dir(), folder)
    }

    /// [`Scratch::of`] with the git to run and the folder the copy's own folder is made in.
    pub fn of_in(git: &Git, work: &Path, folder: &Path) -> Result<Scratch, String> {
        let folder = std::fs::canonicalize(folder)
            .map(|p| PathBuf::from(edit::plain(&p)))
            .map_err(|e| format!("cannot use {} as the folder: {e}", folder.display()))?;
        if !folder.is_dir() {
            return Err(format!("{} is not a folder", folder.display()));
        }
        Self::make(git, work, Found::of(git, &folder)?)
    }

    /// The copy of the repository `found` describes, in a new folder under `work`.
    fn make(git: &Git, work: &Path, found: Found) -> Result<Scratch, String> {
        let run = work.join(format!("run-{}-{}-{}", file_stamp(), std::process::id(), RUNS.fetch_add(1, Ordering::Relaxed)));
        let copy = run.join("copy");
        std::fs::create_dir_all(&copy).map_err(|e| format!("could not make {}: {e}", copy.display()))?;
        // From here the folder is removed if anything fails, by the drop.
        let mut scratch = Scratch {
            repo_root: found.root.clone(),
            git_dir: found.git_dir.clone(),
            base: found.base.clone(),
            prefix: found.prefix.clone(),
            skipped: Vec::new(),
            git: git.clone(),
            run,
            copy,
            start: String::new(),
        };
        scratch.fill(&found)?;
        Ok(scratch)
    }

    /// The folder the model works in: the user's folder, in the copy.
    pub fn folder(&self) -> PathBuf {
        self.copy.join(&self.prefix)
    }

    /// The copy's top folder, where its `.git` is.
    pub fn copy_root(&self) -> &Path {
        &self.copy
    }

    /// The folder the whole run lives in, which holds the copy; a run keeps its other files here.
    pub fn run_dir(&self) -> &Path {
        &self.run
    }

    /// The tree the copy started with: HEAD with the user's uncommitted files.
    pub fn start_tree(&self) -> &str {
        &self.start
    }

    fn fill(&mut self, found: &Found) -> Result<(), String> {
        let (git, copy) = (self.git.clone(), self.copy.clone());

        // The commit HEAD names, as the repository's own transport hands it over: the copy
        // holds every object it needs and borrows none (see the module). The remote the clone
        // makes is taken away at once, so that nothing run in the copy can reach the original.
        let (url, into) = (file_url(&found.root), copy.to_string_lossy().into_owned());
        let clone = ["clone", "--quiet", "--no-checkout", "--depth", "1", "--no-tags", "--no-local", "--template=", "--origin", "source"];
        git.ok(&self.run, &[&clone[..], &[&url, &into]].concat())?;
        git.ok(&copy, &["remote", "remove", "source"])?;
        let head = git.ok(&copy, &["rev-parse", "--verify", "HEAD^{commit}"])?;
        self.base = String::from_utf8_lossy(&head.stdout).trim().to_string();

        // How the repository turns line endings, as it is in force there (any scope), made
        // local to the copy so no setting of the machine's changes what the copy does.
        for key in ["core.autocrlf", "core.eol"] {
            let value = git.run(&found.root, &["config", "--get", key])?;
            if value.ok() {
                let value = String::from_utf8_lossy(&value.stdout).trim().to_string();
                git.ok(&copy, &["config", "--local", key, &value])?;
            }
        }
        // What the repository keeps to itself, so `git add -A` leaves out what the user's git does.
        for name in ["exclude", "attributes"] {
            let from = found.git_dir.join("info").join(name);
            if let Ok(bytes) = std::fs::read(&from) {
                let to = copy.join(".git").join("info").join(name);
                std::fs::create_dir_all(to.parent().unwrap_or(&copy)).map_err(|e| format!("{e}"))?;
                std::fs::write(&to, bytes).map_err(|e| format!("could not copy {}: {e}", from.display()))?;
            }
        }

        git.ok(&copy, &["read-tree", "-u", "--reset", &self.base])?;
        self.carry(found)?;
        self.note_unfetched(found)?;
        // The uncommitted files were read after the commit was: they belong to it only if HEAD
        // is still where it was.
        let now = git.run(&found.root, &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"])?;
        if String::from_utf8_lossy(&now.stdout).trim() != self.base {
            return Err("the repository's HEAD moved while the copy was being made; run it again".to_string());
        }
        std::fs::create_dir_all(self.folder()).map_err(|e| format!("could not make the folder in the copy: {e}"))?;

        git.ok(&copy, &["add", "-A"])?;
        self.start = tree(&git, &copy)?;
        Ok(())
    }

    /// Copy what is uncommitted in the user's folder, byte for byte: whatever `git status`
    /// lists, as the folder holds it now. A path that is no longer there is a deletion.
    fn carry(&mut self, found: &Found) -> Result<(), String> {
        let status = self.git.ok(
            &found.root,
            &["status", "--porcelain=v1", "-z", "--no-renames", "--untracked-files=all"],
        )?;
        let (mut total, mut seen) = (0u64, HashSet::new());
        let (mut unnamed, mut links, mut named) = (0usize, HashMap::new(), HashSet::new());
        for entry in status.stdout.split(|b| *b == 0).filter(|e| e.len() > 3) {
            // `XY path`, and nothing else once renames are off.
            let Ok(rel) = std::str::from_utf8(&entry[3..]) else {
                unnamed += 1;
                continue;
            };
            let rel = rel.trim_end_matches('/');
            let outside = Path::new(rel).components().any(|c| !matches!(c, Component::Normal(_)));
            if rel.is_empty() || outside || !seen.insert(rel.to_string()) {
                continue;
            }
            // **Any part of the path may be a link or a junction**, not only the file: git reads
            // a junction as a folder, and lists what is beyond it as if it were here. Named once.
            if let Some(link) = link_in(&found.root, rel, &mut links) {
                if named.insert(link.clone()) {
                    self.skipped.push(format!("{link} (a link or junction: what it leads to is not carried)"));
                }
                continue;
            }
            let (from, to) = (found.root.join(rel), self.copy.join(rel));
            match std::fs::symlink_metadata(&from) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => remove_file_and_empty_parents(&to, &self.copy),
                Err(e) => self.skipped.push(format!("{rel} (could not be read: {e})")),
                Ok(m) if m.is_dir() => self.skipped.push(format!("{rel} (a nested repository or submodule)")),
                Ok(m) if m.len() > MAX_FILE_BYTES => {
                    self.skipped.push(format!("{rel} (over {} MB)", MAX_FILE_BYTES / 1024 / 1024));
                }
                Ok(m) if total + m.len() > MAX_COPY_BYTES => {
                    self.skipped.push(format!("{rel} (over the {} MB limit for uncommitted files)", MAX_COPY_BYTES / 1024 / 1024));
                }
                Ok(m) => {
                    total += m.len();
                    // Bytes only: a read-only file in the folder must not be read-only in the copy.
                    let copied = std::fs::read(&from).and_then(|bytes| {
                        if let Some(parent) = to.parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        std::fs::write(&to, bytes)
                    });
                    if let Err(e) = copied {
                        self.skipped.push(format!("{rel} (could not be copied: {e})"));
                    }
                }
            }
        }
        if unnamed > 0 {
            self.skipped.push(format!("{unnamed} file(s) whose names are not valid UTF-8"));
        }
        Ok(())
    }

    /// Say which submodules and Git LFS pointers the folder has: the copy holds an empty
    /// folder for the first and the pointer text for the second, never the real files.
    fn note_unfetched(&mut self, found: &Found) -> Result<(), String> {
        let base = self.base.clone();
        let mut args = vec!["ls-tree", "-r", "-l", "-z", base.as_str()];
        let inside = found.prefix.trim_end_matches('/').to_string();
        if !inside.is_empty() {
            args.extend(["--", &inside]);
        }
        let listing = self.git.ok(&self.copy, &args)?;
        let mut checked = 0;
        for entry in listing.stdout.split(|b| *b == 0).filter(|e| !e.is_empty()) {
            // `<mode> <type> <object> <size>\t<path>`
            let text = String::from_utf8_lossy(entry);
            let Some((meta, path)) = text.split_once('\t') else { continue };
            let mut parts = meta.split_whitespace();
            let (mode, _kind, _object, size) = (parts.next(), parts.next(), parts.next(), parts.next());
            if mode == Some("160000") {
                // A submodule with changes was named already, as the folder it is.
                if !self.skipped.iter().any(|s| s.starts_with(&format!("{path} "))) {
                    self.skipped.push(format!("{path} (a submodule: its files are not in the copy)"));
                }
                continue;
            }
            // A pointer is a short file: `version …`, `oid sha256:…`, `size …`.
            let small = size.and_then(|s| s.parse::<u64>().ok()).is_some_and(|n| (100..=300).contains(&n));
            if small && checked < 5000 {
                checked += 1;
                let mut head = [0u8; 64];
                let read = std::fs::File::open(self.copy.join(path)).and_then(|mut f| f.read(&mut head)).unwrap_or(0);
                if head[..read].starts_with(b"version https://git-lfs.github.com/spec/") {
                    self.skipped.push(format!("{path} (a Git LFS pointer: the real file is not in the copy)"));
                }
            }
        }
        Ok(())
    }

    /// Everything the copy holds now, as a tree: `git add -A` and `git write-tree`. The
    /// copy's own index is written; nothing else is.
    ///
    /// **Links made since the copy began are left out first.** A check runs code the model
    /// wrote, which can make a junction to a folder outside the copy, and git for Windows adds
    /// what lies beyond a junction as if it were in the copy. So the copy is walked for links
    /// and junctions, without following any, and each one git does not track is put in the
    /// copy's `info/exclude` and named in [`Scratch::skipped`].
    pub fn snapshot(&mut self) -> Result<String, String> {
        self.exclude_new_links()?;
        self.git.ok(&self.copy, &["add", "-A"])?;
        tree(&self.git, &self.copy)
    }

    /// [`Scratch::snapshot`] and then [`Scratch::patch`] on what it gave.
    pub fn finish(&mut self) -> Result<Patch, String> {
        let end = self.snapshot()?;
        self.patch(&end)
    }

    fn exclude_new_links(&mut self) -> Result<(), String> {
        let mut found: Vec<String> = Vec::new();
        let mut stack = vec![(self.copy.clone(), String::new())];
        while let Some((dir, prefix)) = stack.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let Ok(kind) = entry.file_type() else { continue };
                if prefix.is_empty() && name == ".git" {
                    continue;
                }
                if kind.is_symlink() {
                    found.push(format!("{prefix}{name}"));
                } else if kind.is_dir() {
                    stack.push((entry.path(), format!("{prefix}{name}/")));
                }
            }
        }
        if found.is_empty() {
            return Ok(());
        }
        // A link the repository itself tracks is its own, and git adds it as a link, not as what it leads to.
        let tracked: HashSet<String> = self
            .git
            .ok(&self.copy, &["ls-files", "-z"])?
            .stdout
            .split(|b| *b == 0)
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .collect();
        found.retain(|p| !tracked.contains(p));
        // Named and excluded already, by an earlier snapshot.
        found.retain(|p| !self.skipped.iter().any(|s| s.starts_with(&format!("{p} (a link or junction made during the run"))));
        found.sort();
        if found.is_empty() {
            return Ok(());
        }
        let exclude = self.copy.join(".git").join("info").join("exclude");
        std::fs::create_dir_all(exclude.parent().unwrap_or(&self.copy)).map_err(|e| format!("{e}"))?;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&exclude)
            .map_err(|e| format!("could not write {}: {e}", exclude.display()))?;
        for link in &found {
            // Anchored to the copy's top, with what a pattern reads as more than a name escaped.
            let escaped: String = link.chars().flat_map(|c| if "*?[]\\".contains(c) { vec!['\\', c] } else { vec![c] }).collect();
            writeln!(file, "\n/{escaped}").map_err(|e| format!("could not write {}: {e}", exclude.display()))?;
            self.skipped.push(format!("{link} (a link or junction made during the run: not in the patch)"));
        }
        Ok(())
    }

    /// What changed between the copy as it started and `end` (from [`Scratch::snapshot`]):
    /// the patch, byte for byte, and the files it touches. The patch's paths are relative to
    /// the repository's top folder, so it applies there with `git apply`.
    ///
    /// Made with `git diff-tree`, which takes no part of the user's settings, and with the
    /// prefixes, colour and tools spelled out all the same.
    pub fn patch(&self, end: &str) -> Result<Patch, String> {
        if end == self.start {
            return Ok(Patch::default());
        }
        let show = ["diff-tree", "-r", "-M", "--no-color", "--no-ext-diff", "--no-textconv"];
        let (start, end) = (self.start.as_str(), end);
        let mut args = show.to_vec();
        args.extend(["-p", "--binary", "--full-index", "--src-prefix=a/", "--dst-prefix=b/", start, end]);
        let bytes = self.git.ok(&self.copy, &args)?.stdout;

        // How many lines each file gained and lost. `-` for a binary file, read as no count.
        let mut args = show.to_vec();
        args.extend(["-z", "--numstat", start, end]);
        let stat = self.git.ok(&self.copy, &args)?.stdout;
        let mut counts: HashMap<String, (Option<u64>, Option<u64>)> = HashMap::new();
        let mut tokens = stat.split(|b| *b == 0).filter(|t| !t.is_empty()).map(|t| String::from_utf8_lossy(t).into_owned());
        while let Some(token) = tokens.next() {
            let mut parts = token.splitn(3, '\t');
            let (Some(added), Some(deleted), Some(path)) = (parts.next(), parts.next(), parts.next()) else { continue };
            // A rename has no path on its own line: the old name and the new one follow.
            let path = if path.is_empty() { tokens.nth(1).unwrap_or_default() } else { path.to_string() };
            counts.insert(path, (added.parse().ok(), deleted.parse().ok()));
        }

        let mut args = show.to_vec();
        args.extend(["-z", "--name-status", start, end]);
        let listing = self.git.ok(&self.copy, &args)?.stdout;
        let mut names = listing.split(|b| *b == 0).filter(|n| !n.is_empty()).map(|n| String::from_utf8_lossy(n).into_owned());
        let mut files = Vec::new();
        while let Some(code) = names.next() {
            let Some(path) = names.next() else { break };
            let (path, from, status) = match code.chars().next() {
                Some('A') => (path, None, "added"),
                Some('D') => (path, None, "deleted"),
                // A rename or copy names the old path first and the new one second.
                Some('R' | 'C') => match names.next() {
                    Some(new) => (new, Some(path), "renamed"),
                    None => break,
                },
                _ => (path, None, "modified"),
            };
            let (added, deleted) = counts.get(&path).copied().unwrap_or_default();
            files.push(Change { path, from, status, added, deleted });
        }
        Ok(Patch { bytes, files })
    }

    /// How [`Scratch::skipped`] reads: at most [`MAX_LISTED`] lines, the rest counted.
    pub fn skipped_lines(&self) -> Vec<String> {
        let mut lines: Vec<String> = self.skipped.iter().take(MAX_LISTED).cloned().collect();
        if self.skipped.len() > MAX_LISTED {
            lines.push(format!("… and {} more", self.skipped.len() - MAX_LISTED));
        }
        lines
    }
}

impl Drop for Scratch {
    /// The run's folder goes. Tried for a couple of seconds, since a process that is a moment
    /// from ending can still hold something in it.
    fn drop(&mut self) {
        remove_dir_patiently(&self.run);
    }
}

/// `git write-tree` in `copy`, trimmed.
fn tree(git: &Git, copy: &Path) -> Result<String, String> {
    let out = git.ok(copy, &["write-tree"])?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `path` as a `file://` URL git can clone from: forward slashes, and every byte that is not
/// plain (a space, `%`, `#`, a letter with an accent) written as `%XX`, which git reads back.
fn file_url(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut url = String::from(if text.starts_with("//") { "file:" } else if text.starts_with('/') { "file://" } else { "file:///" });
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/:".contains(&byte) {
            url.push(byte as char);
        } else {
            url.push_str(&format!("%{byte:02X}"));
        }
    }
    url
}

/// The first part of `rel`, a path inside `root` with forward slashes, that is a link or a
/// junction, as the path up to and including it. Each folder is looked at once: `known` is
/// what has been seen.
fn link_in(root: &Path, rel: &str, known: &mut HashMap<PathBuf, bool>) -> Option<String> {
    let (mut at, mut shown) = (root.to_path_buf(), String::new());
    for part in rel.split('/') {
        at.push(part);
        if !shown.is_empty() {
            shown.push('/');
        }
        shown.push_str(part);
        let link = *known
            .entry(at.clone())
            .or_insert_with(|| std::fs::symlink_metadata(&at).is_ok_and(|m| m.file_type().is_symlink()));
        if link {
            return Some(shown);
        }
    }
    None
}

fn remove_file_and_empty_parents(file: &Path, stop: &Path) {
    let _ = std::fs::remove_file(file);
    let mut dir = file.parent();
    while let Some(d) = dir.filter(|d| *d != stop && d.starts_with(stop)) {
        if std::fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
}

fn remove_dir_patiently(dir: &Path) {
    for _ in 0..20 {
        if std::fs::remove_dir_all(dir).is_ok() || !dir.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// What a run changed, as `git diff-tree` has it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Patch {
    /// The patch itself, byte for byte. Empty when nothing changed.
    pub bytes: Vec<u8>,
    pub files: Vec<Change>,
}

/// One file a patch touches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// Relative to the repository's top folder, with forward slashes. For a rename, the new name.
    pub path: String,
    /// For a rename, the old name.
    pub from: Option<String>,
    /// `added`, `modified`, `deleted` or `renamed`.
    pub status: &'static str,
    /// The lines the file gained and lost; `None` for a file that is not text.
    pub added: Option<u64>,
    pub deleted: Option<u64>,
}

// ── what a run leaves behind ────────────────────────────────────────────────

/// Remove what earlier runs left in `work`: a run's folder older than [`RUN_AGE`], a patch or
/// its record older than [`PATCH_AGE`]. How many were removed. Nothing else is touched.
pub fn sweep(work: &Path) -> usize {
    sweep_at(work, SystemTime::now())
}

fn sweep_at(work: &Path, now: SystemTime) -> usize {
    let older = |path: &Path, than: Duration| {
        let modified = std::fs::metadata(path).and_then(|m| m.modified());
        modified.is_ok_and(|then| now.duration_since(then).is_ok_and(|age| age > than))
    };
    let mut removed = 0;
    for entry in std::fs::read_dir(work).into_iter().flatten().flatten() {
        let path = entry.path();
        let is_run = entry.file_name().to_string_lossy().starts_with("run-");
        if is_run && path.is_dir() && older(&path, RUN_AGE) {
            remove_dir_patiently(&path);
            removed += usize::from(!path.exists());
        }
    }
    for entry in std::fs::read_dir(work.join("patches")).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_file() && older(&path, PATCH_AGE) && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Held for the length of a run: **one `hugin -p` at a time**, since two would wait on each
/// other's cargo lock and ask the model for twice the memory. The file is open without
/// sharing, so Windows ends the hold with the process however it ends, and nothing is left
/// to go stale.
#[derive(Debug)]
pub struct Lock {
    _file: Option<std::fs::File>,
}

impl Lock {
    /// Take the lock in `work`, which is made if it is not there. `Err` says another run holds it.
    pub fn take(work: &Path) -> Result<Lock, String> {
        std::fs::create_dir_all(work).map_err(|e| format!("could not make {}: {e}", work.display()))?;
        let path = work.join("lock");
        match open_exclusive(&path) {
            Ok(file) => Ok(Lock { _file: file }),
            Err(e) if e.raw_os_error() == Some(32) => {
                Err("another hugin -p is already running; wait for it to finish".to_string())
            }
            Err(e) => Err(format!("could not take the lock {}: {e}", path.display())),
        }
    }
}

/// Open with `share_mode(0)`: no other handle to the file can be made while this one is open.
#[cfg(windows)]
fn open_exclusive(path: &Path) -> std::io::Result<Option<std::fs::File>> {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new().write(true).create(true).truncate(false).share_mode(0).open(path).map(Some)
}

/// Elsewhere there is no such hold, and runs are not kept apart.
#[cfg(not(windows))]
fn open_exclusive(_: &Path) -> std::io::Result<Option<std::fs::File>> {
    Ok(None)
}

/// What one entry of a folder was, as [`fingerprints`] sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Print {
    /// A fingerprint of a file's contents and length; 0 for a folder.
    pub contents: u64,
    /// When it was last written, in nanoseconds since 1970: a folder's changes when something is
    /// made or removed in it, a file's when it is written, **or touched**.
    pub modified: u128,
}

/// What every entry under `dir` was, by path: its contents and the time it was last written,
/// and a folder as itself. Links are not followed. For showing that a run left a folder, its
/// `.git` included, exactly as it found it — **down to the times**, since a program that only
/// "touches" a file (git does, to an object it finds it already has) has written to it.
pub fn fingerprints(dir: &Path) -> std::io::Result<BTreeMap<String, Print>> {
    fn walk(dir: &Path, prefix: &str, into: &mut BTreeMap<String, Print>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
            // Looked at through the entry itself, not the listing: NTFS keeps a copy of a
            // folder's times in the listing that is only brought up to date when something opens it.
            let meta = std::fs::symlink_metadata(entry.path())?;
            let kind = meta.file_type();
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            if kind.is_dir() {
                into.insert(format!("{name}/"), Print { contents: 0, modified });
                walk(&entry.path(), &format!("{name}/"), into)?;
            } else if kind.is_file() {
                let contents = edit::fingerprint_file(&entry.path())? ^ meta.len().rotate_left(32);
                into.insert(name, Print { contents, modified });
            } else {
                into.insert(name, Print { contents: 1, modified });
            }
        }
        Ok(())
    }
    let mut into = BTreeMap::new();
    walk(dir, "", &mut into)?;
    Ok(into)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway place for repositories, with a git configuration of its own so that
    /// nothing a test checks depends on the machine's: no system file, and a global one
    /// the test writes.
    struct World {
        dir: PathBuf,
        git: Git,
    }

    impl World {
        fn new(tag: &str) -> Self {
            Self::with_global(tag, "[user]\n\tname = t\n\temail = t@t\n")
        }

        fn with_global(tag: &str, global: &str) -> Self {
            let n = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            let dir = std::env::temp_dir().join(format!("bifrost-scratch-{tag}-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let config = dir.join("global.gitconfig");
            std::fs::write(&config, global).unwrap();
            let env = vec![
                ("GIT_CONFIG_GLOBAL".to_string(), config.to_string_lossy().into_owned()),
                ("GIT_CONFIG_NOSYSTEM".to_string(), "1".to_string()),
            ];
            World { dir, git: Git::with_env(env) }
        }

        /// A world whose global configuration is written to get in the way: a diff with no
        /// prefixes and an external tool, colour always, a safecrlf that refuses mixed files, a
        /// status that hides untracked files, a whitespace check on apply, and a hooks folder in
        /// which each of `hooks` writes a marker. The second value says where a marker is.
        fn hostile(tag: &str, hooks: &[&str]) -> (Self, impl Fn(&str) -> PathBuf) {
            let world = Self::with_global(tag, "");
            let dir = world.dir.clone();
            let marker = move |name: &str| dir.join(format!("marker-{name}"));
            let slash = |p: &Path| p.to_string_lossy().replace('\\', "/");
            let folder = world.dir.join("hooks");
            std::fs::create_dir_all(&folder).unwrap();
            for hook in hooks {
                let body = format!("#!/bin/sh\necho ran > \"{}\"\nexit 0\n", slash(&marker(hook)));
                std::fs::write(folder.join(hook), body).unwrap();
            }
            let external = world.dir.join("ext.sh");
            std::fs::write(&external, format!("#!/bin/sh\necho ran > \"{}\"\n", slash(&marker("external")))).unwrap();
            let global = format!(
                "[user]\n\tname = t\n\temail = t@t\n\
                 [diff]\n\tnoprefix = true\n\tmnemonicPrefix = true\n\texternal = {}\n\trenames = false\n\
                 [color]\n\tui = always\n\tdiff = always\n\
                 [core]\n\tsafecrlf = true\n\thooksPath = {}\n\tquotePath = false\n\
                 [status]\n\tshowUntrackedFiles = no\n\tshort = true\n\
                 [apply]\n\twhitespace = error\n",
                slash(&external),
                slash(&folder)
            );
            std::fs::write(world.dir.join("global.gitconfig"), global).unwrap();
            (world, marker)
        }

        /// A repository with a local `core.autocrlf=true`, as the machine's own repositories have.
        fn repo(&self, name: &str) -> Repo {
            let root = self.dir.join(name);
            std::fs::create_dir_all(&root).unwrap();
            let repo = Repo { root, env: self.git.env.clone() };
            repo.git(&["init", "-q", "-b", "main"]);
            repo.git(&["config", "core.autocrlf", "true"]);
            repo
        }

        fn work(&self) -> PathBuf {
            self.dir.join("work")
        }

        fn scratch(&self, folder: &Path) -> Result<Scratch, String> {
            Scratch::of_in(&self.git, &self.work(), folder)
        }
    }

    impl Drop for World {
        fn drop(&mut self) {
            remove_dir_patiently(&self.dir);
        }
    }

    struct Repo {
        root: PathBuf,
        env: Vec<(String, String)>,
    }

    impl Repo {
        /// Plain git, as a person types it, in this repository.
        fn git(&self, args: &[&str]) -> String {
            let mut cmd = Command::new("git");
            cmd.args(args).current_dir(&self.root);
            for (k, v) in &self.env {
                cmd.env(k, v);
            }
            let out = cmd.output().expect("git runs");
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).into_owned()
        }

        fn write(&self, rel: &str, bytes: &[u8]) {
            let p = self.root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, bytes).unwrap();
        }

        fn read(&self, rel: &str) -> Vec<u8> {
            std::fs::read(self.root.join(rel)).unwrap()
        }

        fn commit_all(&self) {
            self.git(&["add", "-A"]);
            self.git(&["commit", "-q", "-m", "x"]);
        }
    }

    fn have_git() -> bool {
        Command::new("git").arg("--version").output().is_ok()
    }

    /// `dir` holds exactly what `before` says, times included, and a failure names what differs
    /// and how.
    fn unchanged(dir: &Path, before: &BTreeMap<String, Print>, when: &str) {
        let now = fingerprints(dir).unwrap();
        let mut differ: Vec<String> = Vec::new();
        for key in before.keys().chain(now.keys()).collect::<std::collections::BTreeSet<_>>() {
            let (was, is) = (before.get(key), now.get(key));
            if was != is {
                let how = match (was, is) {
                    (Some(a), Some(b)) if a.contents == b.contents => "only its time".to_string(),
                    (Some(_), Some(_)) => "its contents".to_string(),
                    (Some(_), None) => "removed".to_string(),
                    _ => "added".to_string(),
                };
                differ.push(format!("{key} ({how})"));
            }
        }
        differ.truncate(10);
        assert!(differ.is_empty(), "{when}: these changed: {differ:?}");
    }

    /// Set the time every file under `dir` was last written to `by` ago, read-only ones
    /// (git's objects are) included, so that a program that touches one shows. Windows only
    /// in what it clears: on Unix `set_readonly(false)` would make a file world-writable.
    #[allow(clippy::permissions_set_readonly_false)]
    fn age(dir: &Path, by: Duration) {
        let old = SystemTime::now() - by;
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                age(&path, by);
                continue;
            }
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            let was_read_only = permissions.readonly();
            permissions.set_readonly(false);
            std::fs::set_permissions(&path, permissions.clone()).unwrap();
            std::fs::OpenOptions::new().write(true).open(&path).unwrap().set_modified(old).unwrap();
            permissions.set_readonly(was_read_only);
            std::fs::set_permissions(&path, permissions).unwrap();
        }
    }

    /// Whether anything named `name` is anywhere under `dir`.
    fn has_entry(dir: &Path, name: &str) -> bool {
        std::fs::read_dir(dir).into_iter().flatten().flatten().any(|e| {
            e.file_name().to_string_lossy() == name || (e.file_type().is_ok_and(|k| k.is_dir()) && has_entry(&e.path(), name))
        })
    }

    /// A junction `link` to the folder `target`: what `mklink /J` makes, and needs no privilege.
    #[cfg(windows)]
    fn junction(link: &Path, target: &Path) {
        let made = Command::new("cmd").arg("/C").arg("mklink").arg("/J").arg(link).arg(target).output().unwrap();
        assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stdout));
    }

    /// A patch written where `git apply` can read it.
    fn patch_file(world: &World, patch: &Patch) -> PathBuf {
        let p = world.dir.join(format!("p-{}.patch", patch.bytes.len()));
        std::fs::write(&p, &patch.bytes).unwrap();
        p
    }

    // ── what is refused ─────────────────────────────────────────────────────

    #[test]
    fn a_folder_outside_git_a_repository_with_no_commits_and_an_ignored_folder_are_refused() {
        if !have_git() {
            return;
        }
        let w = World::new("refuse");
        let plain = w.dir.join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let e = w.scratch(&plain).unwrap_err();
        assert!(e.contains("not in a git repository"), "{e}");

        let empty = w.repo("empty");
        let e = w.scratch(&empty.root).unwrap_err();
        assert!(e.contains("no commits"), "{e}");

        let repo = w.repo("ignores");
        repo.write(".gitignore", b"build/\n");
        repo.write("a.txt", b"a\n");
        repo.commit_all();
        std::fs::create_dir_all(repo.root.join("build/deep")).unwrap();
        for folder in ["build", "build/deep"] {
            let e = w.scratch(&repo.root.join(folder)).unwrap_err();
            assert!(e.contains("is ignored by git"), "{folder}: {e}");
        }
        assert!(w.scratch(&repo.root).is_ok(), "the repository itself is fine");
        assert!(w.scratch(&w.dir.join("no-such-folder")).is_err());
    }

    // ── what is carried ─────────────────────────────────────────────────────

    #[test]
    fn uncommitted_work_is_carried_byte_for_byte_and_the_rest_is_left() {
        if !have_git() {
            return;
        }
        let w = World::new("carry");
        let repo = w.repo("r");
        repo.write("crlf.txt", b"one\r\ntwo\r\n");
        repo.write("lf.txt", b"x\ny\n");
        repo.write("staged.txt", b"s\n");
        repo.write("gone.txt", b"bye\n");
        repo.write("sub/deep/gone-too.txt", b"bye\n");
        repo.write("moved.txt", b"m\n");
        repo.write("same.txt", b"same\n");
        repo.write("data.bin", &[0, 1, 2, 255, 254, 0, 13, 10]);
        repo.write(".gitignore", b"ignored.txt\n");
        repo.commit_all();

        repo.write("crlf.txt", b"one\r\nTWO\r\n");
        repo.write("lf.txt", b"x\nY\n");
        repo.write("staged.txt", b"staged change\n");
        repo.git(&["add", "staged.txt"]);
        std::fs::remove_file(repo.root.join("gone.txt")).unwrap();
        std::fs::remove_dir_all(repo.root.join("sub")).unwrap();
        std::fs::rename(repo.root.join("moved.txt"), repo.root.join("elsewhere.txt")).unwrap();
        repo.write("untracked/new.txt", b"new\r\n");
        repo.write("data.bin", &[9, 0, 9, 0, 13, 10, 255]);
        repo.write("ignored.txt", b"never\n");
        std::fs::write(repo.root.join(".git/info/exclude"), "excluded.txt\n").unwrap();
        repo.write("excluded.txt", b"never\n");

        let mut s = w.scratch(&repo.root).unwrap();
        let copy = |rel: &str| std::fs::read(s.folder().join(rel));
        assert_eq!(copy("crlf.txt").unwrap(), b"one\r\nTWO\r\n", "CRLF as the folder has it");
        assert_eq!(copy("lf.txt").unwrap(), b"x\nY\n", "LF in a CRLF checkout");
        assert_eq!(copy("staged.txt").unwrap(), b"staged change\n");
        assert_eq!(copy("untracked/new.txt").unwrap(), b"new\r\n");
        assert_eq!(copy("data.bin").unwrap(), [9, 0, 9, 0, 13, 10, 255], "binary bytes");
        assert_eq!(copy("elsewhere.txt").unwrap(), b"m\n");
        assert_eq!(copy("same.txt").unwrap(), b"same\r\n", "a file the user did not touch is HEAD's, checked out as the repository does");
        for gone in ["gone.txt", "moved.txt", "sub/deep/gone-too.txt", "ignored.txt", "excluded.txt"] {
            assert!(!s.folder().join(gone).exists(), "{gone}");
        }
        assert!(!s.folder().join("sub").exists(), "a folder emptied by deletions goes too");
        assert!(s.skipped.is_empty(), "{:?}", s.skipped);
        assert_eq!(s.base, repo.git(&["rev-parse", "HEAD"]).trim());

        // The copy as it started is the starting tree, and a snapshot with no work done is the same tree.
        assert_eq!(s.snapshot().unwrap(), s.start_tree());
        assert_eq!(s.patch(s.start_tree()).unwrap(), Patch::default());
    }

    #[test]
    fn a_big_file_a_link_a_pointer_and_a_nested_repository_are_named_not_carried() {
        if !have_git() {
            return;
        }
        let w = World::new("skipped");
        let repo = w.repo("r");
        let pointer = b"version https://git-lfs.github.com/spec/v1\noid sha256:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393\nsize 12345\n";
        repo.write("lfs.bin", pointer);
        repo.write("big.txt", b"small\n");
        repo.commit_all();
        repo.write("big.txt", &vec![b'x'; MAX_FILE_BYTES as usize + 1]);
        repo.write("untracked-big.txt", &vec![b'y'; MAX_FILE_BYTES as usize + 5]);
        // A nested repository shows up in `git status` as one untracked folder.
        let nested = Repo { root: repo.root.join("inner"), env: repo.env.clone() };
        std::fs::create_dir_all(&nested.root).unwrap();
        nested.git(&["init", "-q"]);
        nested.write("x.txt", b"x\n");

        let s = w.scratch(&repo.root).unwrap();
        let said = s.skipped.join("\n");
        assert!(said.contains("big.txt (over 10 MB)"), "{said}");
        assert!(said.contains("untracked-big.txt (over 10 MB)"), "{said}");
        assert!(said.contains("inner (a nested repository or submodule)"), "{said}");
        assert!(said.contains("lfs.bin (a Git LFS pointer"), "{said}");
        // The copy holds the committed version of the big file, and nothing of the untracked one.
        assert_eq!(std::fs::read(s.folder().join("big.txt")).unwrap(), b"small\r\n");
        assert!(!s.folder().join("untracked-big.txt").exists());
        assert!(!s.folder().join("inner").exists());
    }

    /// A submodule's files are not in the copy, and it is named once, whether or not its
    /// contents have changed in the user's folder.
    #[test]
    fn a_submodule_is_named_once_and_its_folder_is_empty_in_the_copy() {
        if !have_git() {
            return;
        }
        let w = World::new("submodule");
        let lib = w.repo("lib");
        lib.write("lib.txt", b"lib\n");
        lib.commit_all();
        let repo = w.repo("main");
        repo.write("a.txt", b"a\n");
        repo.commit_all();
        let lib_path = lib.root.to_string_lossy().into_owned();
        repo.git(&["-c", "protocol.file.allow=always", "submodule", "add", "-q", &lib_path, "sub"]);
        repo.commit_all();

        let s = w.scratch(&repo.root).unwrap();
        assert_eq!(s.skipped.len(), 1, "{:?}", s.skipped);
        assert!(s.skipped[0].starts_with("sub (a submodule"), "{:?}", s.skipped);
        assert!(s.folder().join("sub").is_dir() && std::fs::read_dir(s.folder().join("sub")).unwrap().next().is_none());
        drop(s);

        // Changed inside, the folder shows up in `git status`: still one line for it.
        std::fs::write(repo.root.join("sub/lib.txt"), b"changed\n").unwrap();
        let s = w.scratch(&repo.root).unwrap();
        assert_eq!(s.skipped.len(), 1, "{:?}", s.skipped);
        assert!(s.skipped[0].starts_with("sub (a nested repository or submodule)"), "{:?}", s.skipped);
    }

    #[test]
    fn a_long_list_of_skipped_files_is_cut_and_counted() {
        let s = Scratch {
            repo_root: PathBuf::new(),
            git_dir: PathBuf::new(),
            base: String::new(),
            prefix: String::new(),
            skipped: (0..25).map(|n| format!("f{n}")).collect(),
            git: Git::new(),
            run: std::env::temp_dir().join("bifrost-scratch-never-made"),
            copy: PathBuf::new(),
            start: String::new(),
        };
        let lines = s.skipped_lines();
        assert_eq!(lines.len(), MAX_LISTED + 1);
        assert_eq!(lines.last().unwrap(), "… and 5 more");
    }

    // ── the patch ───────────────────────────────────────────────────────────

    /// **The round trip.** The copy is changed the way a run changes it — an edit, a create, a
    /// delete, a move — and the patch applies to the user's folder, which still holds its own
    /// uncommitted work, as it would after the run.
    #[test]
    fn the_patch_is_what_the_run_changed_and_applies_to_the_users_folder() {
        if !have_git() {
            return;
        }
        let w = World::new("patch");
        let repo = w.repo("r");
        repo.write("a.txt", b"one\r\ntwo\r\nthree\r\n");
        repo.write("lf.txt", b"x\ny\n");
        repo.write("gone.txt", b"bye\n");
        repo.write("old.txt", b"same\nlines\nin\nthe\nfile\nso\nit\nis\na\nrename\n");
        repo.commit_all();
        repo.write("a.txt", b"one\r\nTWO\r\nthree\r\n");
        repo.write("untracked.txt", b"u\n");

        let mut s = w.scratch(&repo.root).unwrap();
        let in_copy = s.folder();
        std::fs::write(in_copy.join("a.txt"), b"one\r\nTWO\r\nTHREE\r\n").unwrap();
        std::fs::write(in_copy.join("lf.txt"), b"x\ny\nz\n").unwrap();
        std::fs::create_dir_all(in_copy.join("created")).unwrap();
        std::fs::write(in_copy.join("created/new.txt"), b"fresh\n").unwrap();
        std::fs::remove_file(in_copy.join("gone.txt")).unwrap();
        std::fs::rename(in_copy.join("old.txt"), in_copy.join("new.txt")).unwrap();
        std::fs::write(in_copy.join("untracked.txt"), b"u\nmore\n").unwrap();

        let before = fingerprints(&repo.root).unwrap();
        let end = s.snapshot().unwrap();
        let patch = s.patch(&end).unwrap();
        let text = String::from_utf8(patch.bytes.clone()).unwrap();
        assert!(text.starts_with("diff --git a/"), "{text}");
        assert!(!text.contains('\u{1b}'), "no colour");
        assert!(text.contains("--- a/a.txt\n+++ b/a.txt\n") && text.contains("-three\n+THREE\n"), "{text}");
        assert!(text.contains("--- /dev/null\n+++ b/created/new.txt\n"), "{text}");
        assert!(text.contains("+++ /dev/null\n"), "{text}");
        assert!(text.contains("rename from old.txt\nrename to new.txt"), "{text}");
        // The user's uncommitted work started the copy, so it is not in the patch; the run's change to it is.
        assert!(text.contains("+more\n") && !text.contains("+u\n"), "{text}");
        let listed: Vec<(&str, &str)> = patch.files.iter().map(|c| (c.path.as_str(), c.status)).collect();
        assert_eq!(
            listed,
            [
                ("a.txt", "modified"),
                ("created/new.txt", "added"),
                ("gone.txt", "deleted"),
                ("lf.txt", "modified"),
                ("new.txt", "renamed"),
                ("untracked.txt", "modified"),
            ]
        );
        assert_eq!(patch.files[4].from.as_deref(), Some("old.txt"));
        // How many lines each gained and lost; a pure rename gained and lost none.
        let counts: Vec<(Option<u64>, Option<u64>)> = patch.files.iter().map(|c| (c.added, c.deleted)).collect();
        assert_eq!(
            counts,
            [(Some(1), Some(1)), (Some(1), Some(0)), (Some(0), Some(1)), (Some(1), Some(0)), (Some(0), Some(0)), (Some(1), Some(0))]
        );

        // Nothing has touched the user's folder yet, its .git/index included.
        unchanged(&repo.root, &before, "");

        let file = patch_file(&w, &patch);
        w.git.apply(&repo.root, &file, true).unwrap();
        unchanged(&repo.root, &before, "--check writes nothing");
        w.git.apply(&repo.root, &file, false).unwrap();
        // Git turns line endings as the repository's settings say: the CRLF file stays CRLF.
        assert_eq!(repo.read("a.txt"), b"one\r\nTWO\r\nTHREE\r\n");
        assert_eq!(String::from_utf8_lossy(&repo.read("lf.txt")).replace("\r\n", "\n"), "x\ny\nz\n");
        assert_eq!(String::from_utf8_lossy(&repo.read("created/new.txt")).replace("\r\n", "\n"), "fresh\n");
        assert!(!repo.root.join("gone.txt").exists() && !repo.root.join("old.txt").exists());
        assert!(repo.root.join("new.txt").exists());
        assert_eq!(String::from_utf8_lossy(&repo.read("untracked.txt")).replace("\r\n", "\n"), "u\nmore\n");
    }

    #[test]
    fn a_binary_file_travels_in_the_patch() {
        if !have_git() {
            return;
        }
        let w = World::new("binary");
        let repo = w.repo("r");
        repo.write("data.bin", &[0, 1, 2, 3, 4, 5, 6, 7, 0, 0, 255]);
        repo.commit_all();
        let mut s = w.scratch(&repo.root).unwrap();
        std::fs::write(s.folder().join("data.bin"), [0, 9, 9, 9, 4, 5, 6, 7, 0, 0, 255, 13, 10, 0]).unwrap();
        std::fs::write(s.folder().join("added.bin"), [0, 1, 0, 1, 0]).unwrap();
        let patch = s.finish().unwrap();
        assert!(String::from_utf8_lossy(&patch.bytes).contains("GIT binary patch"));
        assert!(patch.files.iter().all(|c| c.added.is_none() && c.deleted.is_none()), "no line counts for a file that is not text: {:?}", patch.files);
        let file = patch_file(&w, &patch);
        w.git.apply(&repo.root, &file, false).unwrap();
        assert_eq!(repo.read("data.bin"), [0, 9, 9, 9, 4, 5, 6, 7, 0, 0, 255, 13, 10, 0]);
        assert_eq!(repo.read("added.bin"), [0, 1, 0, 1, 0]);
    }

    #[test]
    fn a_stale_patch_does_not_apply_and_says_so() {
        if !have_git() {
            return;
        }
        let w = World::new("stale");
        let repo = w.repo("r");
        repo.write("a.txt", b"one\ntwo\nthree\n");
        repo.commit_all();
        let mut s = w.scratch(&repo.root).unwrap();
        std::fs::write(s.folder().join("a.txt"), b"one\ntwo\nTHREE\n").unwrap();
        let patch = s.finish().unwrap();
        repo.write("a.txt", b"one\ntwo\nthree, changed meanwhile\n");
        let e = w.git.apply(&repo.root, &patch_file(&w, &patch), true).unwrap_err();
        assert!(e.contains("a.txt"), "{e}");
    }

    // ── the user's folder ───────────────────────────────────────────────────

    /// **The user's folder is byte-identical afterwards, its `.git/index` included** —
    /// after the copy is made, after the run's work is snapshotted, and after the copy is gone.
    #[test]
    fn the_users_folder_and_git_folder_are_not_written() {
        if !have_git() {
            return;
        }
        let w = World::new("untouched");
        let repo = w.repo("r");
        repo.write("a.txt", b"one\r\ntwo\r\n");
        repo.write("sub/b.txt", b"b\n");
        repo.commit_all();
        repo.write("a.txt", b"one\r\nchanged\r\n");
        repo.write("sub/new.txt", b"n\n");
        // A file whose time no longer matches the index makes `git status` want to refresh the
        // index, which it writes unless told not to: the case this is about.
        let old = SystemTime::now() - Duration::from_secs(3600);
        let stale = std::fs::OpenOptions::new().write(true).open(repo.root.join("sub/b.txt")).unwrap();
        stale.set_modified(old).unwrap();
        drop(stale);
        // Packed, with its refs, and every file of the git folder made old: its objects, packs,
        // index, refs and config all show a touch.
        repo.git(&["gc", "-q"]);
        age(&repo.root.join(".git"), Duration::from_secs(3 * 3600));
        let before = fingerprints(&repo.root).unwrap();
        for kept in [".git/index", ".git/config", ".git/packed-refs", ".git/HEAD"] {
            assert!(before.contains_key(kept), "{kept} is part of what is compared");
        }
        assert!(before.keys().any(|k| k.starts_with(".git/objects/pack/pack-") && k.ends_with(".pack")));

        let run;
        {
            let mut s = w.scratch(&repo.root).unwrap();
            run = s.run_dir().to_path_buf();
            unchanged(&repo.root, &before, "after the copy is made");
            std::fs::write(s.folder().join("a.txt"), b"one\r\nedited\r\n").unwrap();
            let end = s.snapshot().unwrap();
            let _ = s.patch(&end).unwrap();
            unchanged(&repo.root, &before, "after the snapshot and the patch");
            assert!(run.exists());
        }
        unchanged(&repo.root, &before, "after the copy is removed");
        assert!(!run.exists(), "dropping the copy removes its folder");
        assert!(!repo.root.join(".git/worktrees").exists(), "no worktree entry was made");
    }

    #[test]
    fn a_subfolder_is_the_folder_the_model_works_in_and_the_whole_repository_is_the_copy() {
        if !have_git() {
            return;
        }
        let w = World::new("sub");
        let repo = w.repo("r");
        repo.write("top.txt", b"top\n");
        repo.write("crates/x/src/lib.rs", b"pub fn x() {}\n");
        repo.write("crates/y/src/lib.rs", b"pub fn y() {}\n");
        repo.commit_all();
        repo.write("crates/y/src/lib.rs", b"pub fn y() { /* edited */ }\n");
        let sub = repo.root.join("crates").join("x");

        let mut s = w.scratch(&sub).unwrap();
        assert_eq!(s.prefix, "crates/x/");
        assert_eq!(s.repo_root, PathBuf::from(edit::plain(&std::fs::canonicalize(&repo.root).unwrap())));
        assert_eq!(s.folder(), s.copy_root().join("crates/x/"));
        assert!(s.folder().join("src/lib.rs").is_file());
        // The copy is the whole repository, uncommitted work included, so a check run from here sees it.
        assert!(s.copy_root().join("top.txt").is_file());
        assert_eq!(
            std::fs::read(s.copy_root().join("crates/y/src/lib.rs")).unwrap(),
            b"pub fn y() { /* edited */ }\n"
        );
        // A change in the folder is in the patch under the repository's path.
        std::fs::write(s.folder().join("src/lib.rs"), b"pub fn x() { 1; }\n").unwrap();
        let patch = s.finish().unwrap();
        assert_eq!(patch.files.len(), 1);
        assert_eq!(patch.files[0].path, "crates/x/src/lib.rs");
        w.git.apply(&repo.root, &patch_file(&w, &patch), false).unwrap();
        assert_eq!(String::from_utf8_lossy(&repo.read("crates/x/src/lib.rs")).replace("\r\n", "\n"), "pub fn x() { 1; }\n");
    }

    #[test]
    fn a_linked_worktree_can_be_the_source() {
        if !have_git() {
            return;
        }
        let w = World::new("linked");
        let repo = w.repo("main");
        repo.write("a.txt", b"one\ntwo\n");
        repo.commit_all();
        repo.write("later.txt", b"only in the main repository's later commit\n");
        repo.commit_all();
        // The linked worktree is on the earlier commit: its HEAD is its own.
        let linked = w.dir.join("linked");
        repo.git(&["worktree", "add", "-q", "--detach", &linked.to_string_lossy(), "HEAD~1"]);
        let tree = Repo { root: linked.clone(), env: repo.env.clone() };
        tree.write("a.txt", b"one\ntwo\nuncommitted\n");
        tree.write("extra.txt", b"extra\n");
        age(&repo.root.join(".git"), Duration::from_secs(3600));
        let before = fingerprints(&linked).unwrap();
        let admin_before = fingerprints(&repo.root.join(".git")).unwrap();

        let mut s = w.scratch(&linked).unwrap();
        assert!(s.git_dir.ends_with(".git") && s.git_dir.join("worktrees").is_dir(), "{:?}", s.git_dir);
        let head = tree.git(&["rev-parse", "HEAD"]).trim().to_string();
        assert_eq!(s.base, head, "the worktree's commit, not the main repository's");
        assert_ne!(s.base, repo.git(&["rev-parse", "HEAD"]).trim());
        assert!(!s.folder().join("later.txt").exists());
        let in_copy = Repo { root: s.copy_root().to_path_buf(), env: repo.env.clone() };
        assert_eq!(in_copy.git(&["rev-parse", "HEAD"]).trim(), head, "the copy has a real HEAD");
        assert_eq!(std::fs::read(s.folder().join("extra.txt")).unwrap(), b"extra\n");
        std::fs::write(s.folder().join("a.txt"), b"one\ntwo\nuncommitted\nmore\n").unwrap();
        let patch = s.finish().unwrap();
        assert_eq!(patch.files.len(), 1, "{:?}", patch.files);
        unchanged(&linked, &before, "");
        unchanged(&repo.root.join(".git"), &admin_before, "the main repository's .git is untouched");
        w.git.apply(&linked, &patch_file(&w, &patch), false).unwrap();
        assert!(String::from_utf8_lossy(&tree.read("a.txt")).replace("\r\n", "\n").ends_with("uncommitted\nmore\n"));
    }

    // ── the repository is only read ─────────────────────────────────────────

    /// **Not even touched.** A copy that borrowed the repository's objects would "freshen" any
    /// object it was about to write and found already there — setting the time of the user's
    /// loose object, or of the pack that holds it. Here the copy hashes a staged file (a loose
    /// blob the repository has), an untracked file whose text a packed blob has, and a tree, and
    /// every file under the user's `.git` keeps its time, as does every folder.
    #[test]
    fn the_users_objects_keep_their_times_when_the_copy_holds_what_the_repository_already_has() {
        if !have_git() {
            return;
        }
        let w = World::new("freshen");
        let repo = w.repo("r");
        repo.write("a.txt", b"one\r\ntwo\r\n");
        repo.write("b.txt", b"same text\n");
        repo.commit_all();
        repo.git(&["gc", "-q"]);
        // A loose blob the repository holds because the user staged it, and a file the pack holds.
        repo.write("a.txt", b"one\r\ntwo\r\nthree\r\n");
        repo.git(&["add", "a.txt"]);
        repo.write("copy-of-b.txt", b"same text\n");
        assert!(std::fs::read_dir(repo.root.join(".git/objects/pack")).unwrap().flatten().count() >= 2, "packed");
        age(&repo.root.join(".git"), Duration::from_secs(3 * 3600));
        let before = fingerprints(&repo.root).unwrap();

        let mut s = w.scratch(&repo.root).unwrap();
        std::fs::write(s.folder().join("a.txt"), b"one\r\ntwo\r\nTHREE\r\n").unwrap();
        let patch = s.finish().unwrap();
        assert_eq!(patch.files.len(), 1, "{:?}", patch.files);
        unchanged(&repo.root, &before, "after the copy, the snapshot and the patch");
        drop(s);
        unchanged(&repo.root, &before, "after the copy is gone");

        // Nothing at all to hash, either: the copy of a clean repository writes the tree HEAD already is.
        let clean = w.repo("clean");
        clean.write("c.txt", b"c\n");
        clean.commit_all();
        clean.git(&["gc", "-q"]);
        age(&clean.root.join(".git"), Duration::from_secs(3 * 3600));
        let before = fingerprints(&clean.root).unwrap();
        let mut s = w.scratch(&clean.root).unwrap();
        assert_eq!(s.snapshot().unwrap(), s.start_tree());
        unchanged(&clean.root, &before, "a copy that holds exactly HEAD's tree");
    }

    /// The commit the copy starts from is the one HEAD names, a branch or not, and **the copy
    /// has it as its own HEAD** — a check that asks git for the commit finds one — with no
    /// remote to reach the repository it came from.
    #[test]
    fn the_copy_has_the_commit_head_names_as_its_own_head_and_no_remote() {
        if !have_git() {
            return;
        }
        let w = World::new("head");
        let repo = w.repo("r");
        repo.write("a.txt", b"one\n");
        repo.commit_all();
        let first = repo.git(&["rev-parse", "HEAD"]).trim().to_string();
        repo.write("a.txt", b"one\ntwo\n");
        repo.commit_all();
        let second = repo.git(&["rev-parse", "HEAD"]).trim().to_string();

        let check = |s: &Scratch, want: &str, text: &str| {
            let copy = Repo { root: s.copy_root().to_path_buf(), env: repo.env.clone() };
            assert_eq!(s.base, want);
            assert_eq!(copy.git(&["rev-parse", "HEAD"]).trim(), want, "the copy's HEAD");
            assert!(copy.git(&["remote"]).trim().is_empty(), "no remote");
            assert_eq!(String::from_utf8_lossy(&std::fs::read(s.folder().join("a.txt")).unwrap()).replace("\r\n", "\n"), text);
            // The copy's working tree is clean against its own HEAD apart from what was carried.
            assert!(copy.git(&["status", "--porcelain"]).trim().is_empty(), "{}", copy.git(&["status", "--porcelain"]));
        };
        check(&w.scratch(&repo.root).unwrap(), &second, "one\ntwo\n");

        // Detached on the earlier commit.
        repo.git(&["checkout", "-q", "--detach", "HEAD~1"]);
        check(&w.scratch(&repo.root).unwrap(), &first, "one\n");
        // And back on the branch, in a subfolder of it.
        repo.git(&["checkout", "-q", "main"]);
        repo.write("sub/deep/x.txt", b"x\n");
        repo.commit_all();
        let third = repo.git(&["rev-parse", "HEAD"]).trim().to_string();
        let s = w.scratch(&repo.root.join("sub").join("deep")).unwrap();
        assert_eq!(s.base, third);
        assert!(s.folder().join("x.txt").is_file());
    }

    /// Git for Windows reads a junction as a folder, so `git status` lists what lies beyond one
    /// as if it were in the repository. **None of it is carried**, and the junction is named once.
    #[cfg(windows)]
    #[test]
    fn a_junction_in_the_users_folder_is_not_followed_and_is_named_once() {
        if !have_git() {
            return;
        }
        let w = World::new("junction");
        let outside = w.dir.join("outside");
        std::fs::create_dir_all(outside.join("deeper")).unwrap();
        std::fs::write(outside.join("secret.txt"), "not the repository's\n").unwrap();
        std::fs::write(outside.join("deeper").join("more.txt"), "nor this\n").unwrap();
        let repo = w.repo("r");
        repo.write("a.txt", b"a\n");
        repo.commit_all();
        let link = repo.root.join("link");
        junction(&link, &outside);
        // The premise: git does list what is beyond the junction.
        let listed = repo.git(&["status", "--porcelain", "--untracked-files=all"]);
        assert!(listed.contains("link/secret.txt") || listed.contains("link/"), "{listed}");

        let result = w.scratch(&repo.root);
        let s = result.as_ref().unwrap();
        assert!(!s.copy_root().join("link").exists(), "nothing of the junction is in the copy");
        assert!(!has_entry(s.copy_root(), "secret.txt") && !has_entry(s.copy_root(), "more.txt"));
        assert_eq!(s.skipped.len(), 1, "{:?}", s.skipped);
        assert!(s.skipped[0].starts_with("link (a link or junction"), "{:?}", s.skipped);
        drop(result);
        std::fs::remove_dir(&link).unwrap();
    }

    /// A check runs code the model wrote, and that can make a junction in the copy. **What lies
    /// beyond it stays out of the patch**, and the junction is named.
    #[cfg(windows)]
    #[test]
    fn a_junction_made_during_the_run_stays_out_of_the_patch() {
        if !have_git() {
            return;
        }
        let w = World::new("junction-run");
        let outside = w.dir.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "not the repository's\n").unwrap();
        let repo = w.repo("r");
        repo.write("a.txt", b"a\n");
        repo.commit_all();

        let mut s = w.scratch(&repo.root).unwrap();
        std::fs::write(s.folder().join("a.txt"), b"a\nedited\n").unwrap();
        let made = s.folder().join("made[1]");
        junction(&made, &outside);
        // The premise: left to itself, git adds what is beyond the junction.
        let in_copy = Repo { root: s.copy_root().to_path_buf(), env: repo.env.clone() };
        assert!(in_copy.git(&["add", "-A", "-n"]).contains("secret.txt"), "git does not follow the junction");

        let patch = s.finish().unwrap();
        let files: Vec<&str> = patch.files.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(files, ["a.txt"]);
        assert!(!String::from_utf8_lossy(&patch.bytes).contains("secret"));
        assert_eq!(s.skipped.len(), 1, "{:?}", s.skipped);
        assert!(s.skipped[0].starts_with("made[1] (a link or junction made during the run"), "{:?}", s.skipped);
        drop(s);
        let _ = std::fs::remove_dir(&made);
        assert!(outside.join("secret.txt").exists(), "what the junction led to is still there");
    }

    /// A copy that was never given a link names none.
    #[test]
    fn a_clean_copy_names_no_links() {
        if !have_git() {
            return;
        }
        let w = World::new("nolinks");
        let repo = w.repo("r");
        repo.write("a.txt", b"a\n");
        repo.commit_all();
        let mut s = w.scratch(&repo.root).unwrap();
        s.finish().unwrap();
        assert!(s.skipped.is_empty(), "{:?}", s.skipped);
    }

    // ── what a run is given ─────────────────────────────────────────────────

    #[test]
    fn a_command_is_given_no_more_than_the_time_that_is_left_and_no_less_than_a_floor() {
        assert_eq!(Git::new().limit(), GIT_TIMEOUT);
        assert_eq!(Git::new().until(Instant::now() + Duration::from_secs(3600)).limit(), GIT_TIMEOUT);
        let soon = Git::new().until(Instant::now() + Duration::from_secs(30)).limit();
        assert!(soon <= Duration::from_secs(30) && soon > Duration::from_secs(25), "{soon:?}");
        // Out of time: still enough to finish a patch.
        assert_eq!(Git::new().until(Instant::now()).limit(), MIN_LIMIT);
        assert_eq!(Git::new().until(Instant::now() - Duration::from_secs(5)).limit(), MIN_LIMIT);
        // A shorter limit of its own wins.
        let short = Git { timeout: Some(Duration::from_secs(1)), ..Git::new() };
        assert_eq!(short.until(Instant::now() + Duration::from_secs(60)).limit(), Duration::from_secs(1));
    }

    #[test]
    fn a_file_url_is_made_of_plain_bytes_git_reads_back() {
        assert_eq!(file_url(Path::new(r"C:\Users\you\repo")), "file:///C:/Users/you/repo");
        assert_eq!(file_url(Path::new(r"C:\a b\c#d%e\é")), "file:///C:/a%20b/c%23d%25e/%C3%A9");
        assert_eq!(file_url(Path::new(r"\\server\share\x y")), "file://server/share/x%20y");
        assert_eq!(file_url(Path::new("/home/me/repo")), "file:///home/me/repo");
    }

    // ── a hostile configuration ─────────────────────────────────────────────

    /// A global configuration written to get in the way: a diff with no prefixes, colour
    /// always, an external diff tool, a safecrlf that refuses mixed files, a status that
    /// hides untracked files, and a hooks folder whose every hook writes a marker. The copy,
    /// the patch and the apply all come out as they do without it, and no hook or tool runs.
    #[test]
    fn a_hostile_global_configuration_changes_nothing() {
        if !have_git() {
            return;
        }
        let hooks = ["pre-commit", "post-checkout", "post-merge", "post-index-change", "reference-transaction"];
        let (w, marker) = World::hostile("hostile", &hooks);

        // The configuration is hostile: ordinary git runs the hooks and the external diff, and colours.
        let probe = w.repo("probe");
        // CRLF, so that `safecrlf` has nothing to refuse in an ordinary `git add`.
        probe.write("p.txt", b"p\r\n");
        probe.git(&["add", "-A"]);
        probe.git(&["commit", "-q", "-m", "x"]);
        probe.write("p.txt", b"q\r\n");
        assert!(probe.git(&["diff", "--no-ext-diff"]).contains('\u{1b}'), "colour is on for ordinary git");
        probe.git(&["diff"]);
        probe.git(&["add", "-A"]);
        for live in ["pre-commit", "post-index-change", "external"] {
            assert!(marker(live).exists(), "{live} should be live for ordinary git");
        }

        let repo = w.repo("r");
        repo.write("a.txt", b"one\r\ntwo\r\n");
        repo.write("mixed.txt", b"lf\ncrlf\r\n");
        repo.git(&["-c", "core.safecrlf=false", "-c", "core.hooksPath=none", "add", "-A"]);
        repo.git(&["-c", "core.hooksPath=none", "commit", "-q", "-m", "x"]);
        repo.write("a.txt", b"one\r\nchanged\r\n");
        repo.write("brand-new.txt", b"fresh\n");
        repo.write("mixed.txt", b"lf\ncrlf\r\nmore\n");
        let before = fingerprints(&repo.root).unwrap();
        for name in hooks.iter().chain(&["external"]) {
            let _ = std::fs::remove_file(marker(name));
        }

        let mut s = w.scratch(&repo.root).unwrap();
        assert_eq!(
            std::fs::read(s.folder().join("brand-new.txt")).unwrap(),
            b"fresh\n",
            "carried although status is told to hide untracked files"
        );
        std::fs::write(s.folder().join("a.txt"), b"one\r\nedited\r\n").unwrap();
        std::fs::write(s.folder().join("mixed.txt"), b"lf\ncrlf\r\nmore\nand more\n").unwrap();
        let patch = s.finish().unwrap();
        let text = String::from_utf8(patch.bytes.clone()).unwrap();
        assert!(text.starts_with("diff --git a/a.txt b/a.txt\n"), "{text}");
        assert!(text.contains("--- a/a.txt\n+++ b/a.txt\n"), "prefixes as asked: {text}");
        assert!(!text.contains('\u{1b}'), "no colour: {text:?}");
        assert_eq!(patch.files.len(), 2, "{:?}", patch.files);
        unchanged(&repo.root, &before, "");
        w.git.apply(&repo.root, &patch_file(&w, &patch), false).unwrap();
        assert_eq!(repo.read("a.txt"), b"one\r\nedited\r\n");

        for name in hooks.iter().chain(&["external"]) {
            assert!(!marker(name).exists(), "{name} ran");
        }
    }

    // ── what a run leaves ───────────────────────────────────────────────────

    #[test]
    fn the_sweep_removes_old_runs_and_old_patches_and_nothing_else() {
        let w = World::new("sweep");
        let work = w.work();
        std::fs::create_dir_all(work.join("run-old/copy")).unwrap();
        std::fs::write(work.join("run-old/copy/f.txt"), "x").unwrap();
        std::fs::create_dir_all(work.join("patches")).unwrap();
        std::fs::write(work.join("patches/a.patch"), "p").unwrap();
        std::fs::write(work.join("patches/a.json"), "{}").unwrap();
        std::fs::write(work.join("lock"), "").unwrap();
        std::fs::create_dir_all(work.join("target-keep")).unwrap();
        std::fs::write(work.join("not-a-run.txt"), "keep").unwrap();

        let now = SystemTime::now();
        assert_eq!(sweep_at(&work, now), 0, "nothing is old yet");
        assert!(work.join("run-old").exists());

        let hours = |h: u64| now + Duration::from_secs(h * 3600);
        assert_eq!(sweep_at(&work, hours(5)), 0, "a run is a leftover only after six hours");
        assert_eq!(sweep_at(&work, hours(7)), 1);
        assert!(!work.join("run-old").exists());
        assert!(work.join("patches/a.patch").exists(), "a patch is kept for days");
        assert_eq!(sweep_at(&work, hours(24 * 8)), 2, "the patch and its record");
        assert!(!work.join("patches/a.patch").exists() && !work.join("patches/a.json").exists());
        for kept in ["lock", "target-keep", "not-a-run.txt"] {
            assert!(work.join(kept).exists(), "{kept}");
        }
        assert_eq!(sweep(&w.dir.join("no-such-work-folder")), 0);
    }

    #[test]
    fn one_run_holds_the_lock_at_a_time() {
        if !cfg!(windows) {
            return;
        }
        let w = World::new("lock");
        let first = Lock::take(&w.work()).unwrap();
        let e = Lock::take(&w.work()).unwrap_err();
        assert!(e.contains("already running"), "{e}");
        drop(first);
        assert!(Lock::take(&w.work()).is_ok(), "released with the run");
    }

    /// A copy that fails part-way — here, a repository that is not there to clone — takes its folder with it.
    #[test]
    fn a_failed_copy_leaves_no_folder_behind() {
        if !have_git() {
            return;
        }
        let w = World::new("failed");
        let repo = w.repo("r");
        repo.write("a.txt", b"a\n");
        repo.commit_all();
        let found = Found {
            root: w.dir.join("no-such-repository"),
            git_dir: repo.root.join(".git"),
            prefix: String::new(),
            base: "0".repeat(40),
        };
        let e = Scratch::make(&w.git, &w.work(), found).unwrap_err();
        assert!(e.contains("git clone"), "{e}");
        assert!(w.work().exists(), "the run's folder was made, and is gone");
        let left: Vec<_> = std::fs::read_dir(w.work()).unwrap().flatten().collect();
        assert!(left.is_empty(), "{left:?}");
    }

    /// A git that does not finish is killed with whatever it started, and the wait ends.
    #[cfg(windows)]
    #[test]
    fn a_hung_git_is_killed_when_its_time_is_up() {
        if !have_git() {
            return;
        }
        let git = Git { timeout: Some(Duration::from_millis(400)), ..Git::new() };
        let started = Instant::now();
        // An alias that is a shell command, which runs `ping` for half a minute.
        let e = git.exec(&std::env::temp_dir(), &["-c", "alias.slow=!ping -n 30 127.0.0.1", "slow"]).unwrap_err();
        assert!(e.contains("slow did not finish"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(10), "took {:?}", started.elapsed());
    }

    #[test]
    fn git_that_cannot_be_found_is_said_in_words() {
        let nowhere = std::env::temp_dir().join("bifrost-scratch-no-git-here");
        let git = Git::with_env(vec![("PATH".to_string(), nowhere.to_string_lossy().into_owned())]);
        // Where Windows would still find a git.exe through its own folders, there is nothing to say.
        if let Err(e) = git.run(&std::env::temp_dir(), &["--version"]) {
            assert!(e.contains("could not run git"), "{e}");
        }
    }

    #[test]
    fn output_and_errors_are_kept_apart_as_bytes() {
        if !have_git() {
            return;
        }
        let git = Git::new();
        // The system's temporary folder is in no repository.
        let out = git.run(&std::env::temp_dir(), &["rev-parse", "--git-dir"]).unwrap();
        assert!(!out.ok() && !out.stderr.is_empty() && out.stdout.is_empty());
        let ok = git.run(&std::env::temp_dir(), &["--version"]).unwrap();
        assert!(ok.ok() && ok.stdout.starts_with(b"git version") && ok.stderr.is_empty());
    }
}
