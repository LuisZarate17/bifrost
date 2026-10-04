//! The local tools the model may ask for, and the rule about what they may touch.
//!
//! **Every tool is code the model chose to run.** That is the whole reason this
//! module is small and boring. G0 measured that the model picks tools well
//! (`qwen3-vl:8b` 10/10, including four cases where calling one would have been
//! wrong) — but "picks well" is not "may be trusted with a filesystem".
//!
//! **What runs unasked is decided by one function, [`approval`].** Until H5
//! (2026-10-02) nothing ran without a per-call approval, and G6 argued for it:
//! every convenience removes the moment where you look. H5 reverses that in part,
//! on evidence — a third of the asks in 48 saved runs were reads and searches
//! inside the folder the user chose, and everything here is local. So:
//!
//! - **Runs at once:** a read of a file inside the project folder that
//!   `list_files` would show and whose name does not look like a secret, and any
//!   listing or search (which cannot leave the folder, and skip secret-named
//!   files' contents).
//! - **Still asks, every call:** a read outside the folder, of a file the
//!   `.gitignore` hides, of a secret-named file, or through a link that points
//!   out; every edit, every create, every `run_check`. There is still no
//!   allowlist and no "always allow".
//! - **Never worth asking:** a read of a folder, or of a file that is not there —
//!   they go straight back to the model with the reason.
//! - **The switch:** `"ask_every_call": true` in `config.json` brings G6 back
//!   whole. A value that cannot be read counts as `true`.
//!
//! **Eight tools, and only the first is always offered.** `read_file` can read
//! anywhere — inside the folder without asking, elsewhere one approved file at a
//! time. `list_files`, `search`, `edit_file`, `create_file`, `delete_file`
//! and `move_file` exist only once a project folder has been chosen, and
//! cannot reach outside it — see [`crate::search`] and [`crate::edit`] for how
//! that is enforced. `run_check` exists only once that folder also has a check
//! command chosen for it, and runs exactly that command — see [`crate::check`].
//!
//! **A read can take part of a file (H8).** `read_file` has `start_line` and
//! `end_line`, 1-based and inclusive, because a read over 64 KB was refused whole
//! and five of Mímir's own source files are over that — `edit_file` can change a
//! file up to 1 MiB, but it needs text only a read gives. The range is stated in
//! one header line, `[lines 120–180 of 2140]`, and the lines are **not** numbered:
//! a small model copies numbers it is shown into the text it then edits. A file
//! too big to read whole answers with its size and line count and says to read in
//! parts; search already gives line numbers. See [`read_text`], and [`Lines`] for
//! what the ticket and the card carry.
//!
//! The descriptions and the system prompt live here, not in the app, so the G7
//! probe measures the exact words the app ships with. A probe that tested
//! different wording would be measuring a different product.

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::chat::{Message, Tool, ToolCall};
use std::path::{Path, PathBuf};

use crate::edit::{self, Applied, Located, Workspace};
use crate::search;

/// How much of a file is worth reading into a conversation.
///
/// A 32k context is about 128 KB of text at four characters a token, and the
/// history and the answer need room too. Refused rather than truncated: half a
/// file answered confidently is worse than being told it was too big.
pub const MAX_READ_BYTES: u64 = 64 * 1024;

/// The most a read will take from a file, whole or in parts. The same 1 MiB
/// [`search`] and [`edit`] stop at, so a file the model can find a line in and
/// edit is a file it can read the part of.
pub const MAX_RANGED_SOURCE_BYTES: u64 = 1024 * 1024;

/// How many tool calls one answer may make before the loop stops it.
///
/// A bound, not a timeout — without one, a model that keeps asking loops
/// forever. G6 used 4, which was right for reads and **too few for edits**: the
/// G7 probe measured a two-file change taking read, read, edit, edit and then
/// an answer — five rounds — so the old bound cut it off one short.
///
/// **H7 keeps 10.** Across three full Qwen3-Coder taskprobe runs, 192 runs in
/// all (H4a's first and its run of record in `evals/out/H4a-2026-10-01/`, H5's
/// in `evals/out/H5-2026-10-02/`), no task passed after round 10 under the
/// probe's bound of 20 — each summary reads "0 only later" — so a higher bound
/// would only let a lost model run longer.
pub const MAX_ROUNDS: usize = 10;

/// The sentence in `read_file`'s description that offers ranges. A constant so
/// that the probes can offer the old read — the taskprobe's `--read whole` takes
/// exactly this out, and says so if it is not there.
pub const READ_RANGE_SENTENCE: &str =
    "A file over 64 KB is read in parts with start_line and end_line; search shows line numbers.";

fn read_tool() -> Tool {
    Tool::function(
        "read_file",
        format!(
            "Read a text file and return its contents. Paths inside the project folder may be \
             relative to it. {READ_RANGE_SENTENCE} The user may be asked to approve a read."
        ),
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file" },
                "start_line": { "type": "integer", "description": "First line to read, counting from 1" },
                "end_line": { "type": "integer", "description": "Last line to read, inclusive" }
            },
            "required": ["path"]
        }),
    )
}

/// Which files there are. A glob, because that is what a model already knows
/// how to write; a bare name matches file names anywhere, the way `.gitignore`
/// and ripgrep read one.
fn list_tool() -> Tool {
    Tool::function(
        "list_files",
        "List the files in the project folder whose path matches a glob pattern, such as \
         src/**/*.rs or *.toml. A pattern without a slash matches file names in any folder; a \
         folder's name lists everything in it; no pattern lists the whole project. Build output, \
         dependencies and .git are left out.",
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern, relative to the project folder" }
            }
        }),
    )
}

/// Where some text is. **Literal, not a regular expression**: a small model
/// writes a malformed pattern far more easily than a malformed string, and the
/// question it is asking is almost always "where is this name".
fn search_tool() -> Tool {
    Tool::function(
        "search",
        "Find every line in the project folder's files that contains some text. The text is \
         matched exactly as written, not as a regular expression, and ignores case unless it has \
         capital letters. Optionally only in files matching a glob pattern, such as src/**/*.rs. \
         Returns path:line: text for each match.",
        json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "The text to look for, on one line" },
                "pattern": { "type": "string", "description": "Optional glob pattern for which files to search" }
            },
            "required": ["text"]
        }),
    )
}

fn edit_tool() -> Tool {
    Tool::function(
        "edit_file",
        "Change a file in the project folder by replacing one exact piece of text. Read the \
         file first. old_text must be copied exactly from the file, including indentation, \
         and must occur exactly once — include a neighbouring line if it could match more \
         than one place. new_text replaces it; use an empty new_text to delete. Make one \
         change per call. The user sees a diff and approves each edit.",
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path, relative to the project folder" },
                "old_text": { "type": "string", "description": "Exact text currently in the file" },
                "new_text": { "type": "string", "description": "Text to put in its place" }
            },
            "required": ["path", "old_text", "new_text"]
        }),
    )
}

/// What `create_file` says about folders. It used to say the folder must already
/// exist; since H9 a missing one is made, and listed on the card.
pub const CREATE_FOLDERS: &str = "Folders in the path that do not exist yet are created.";

fn create_tool() -> Tool {
    Tool::function(
        "create_file",
        format!(
            "Create a new file in the project folder. Fails if the file already exists — use \
             edit_file to change an existing file. {CREATE_FOLDERS}"
        ),
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path, relative to the project folder" },
                "content": { "type": "string", "description": "The whole contents of the new file" }
            },
            "required": ["path", "content"]
        }),
    )
}

/// **Not a way to rewrite a file.** The description says so because a model that
/// wants to change a file can delete it and create it again, which skips the diff
/// the user would have approved; taskprobe counts that.
fn delete_tool() -> Tool {
    Tool::function(
        "delete_file",
        "Delete a file in the project folder. Only files can be deleted, not folders. This is \
         not a way to rewrite a file — use edit_file to change what is in one, and move_file to \
         rename it. The user approves each delete and can undo it.",
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path, relative to the project folder" }
            },
            "required": ["path"]
        }),
    )
}

fn move_tool() -> Tool {
    Tool::function(
        "move_file",
        "Move or rename a file in the project folder. The destination must not already exist, \
         and folders in it that do not exist yet are created. Only files can be moved, not \
         folders. References to the file are not updated — edit them with edit_file. The user \
         approves each move and can undo it.",
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Current file path, relative to the project folder" },
                "to": { "type": "string", "description": "New file path, including the file name, relative to the project folder" }
            },
            "required": ["path", "to"]
        }),
    )
}

/// The folder's check command, named in the description so the model knows what
/// it will run. **No arguments**: the command was chosen by a person, and the
/// model's only decision is when.
fn check_tool(label: &str) -> Tool {
    Tool::function(
        "run_check",
        format!(
            "Run the project's check command, `{label}`, in the project folder. Returns whether \
             it passed and its output. Run it after changing code, and fix what it reports. \
             Takes no arguments. The user approves each run."
        ),
        json!({ "type": "object", "properties": {} }),
    )
}

/// Whole-file rewrite. **Never offered by the app** — only the G7 probe uses it,
/// to measure search-and-replace against the obvious alternative.
pub fn rewrite_tool() -> Tool {
    Tool::function(
        "write_file",
        "Replace the entire contents of an existing file in the project folder. Read the file \
         first, then send back the whole file with your change made.",
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path, relative to the project folder" },
                "content": { "type": "string", "description": "The complete new contents of the file" }
            },
            "required": ["path", "content"]
        }),
    )
}

/// The tools offered to the model. Finding and editing tools only when there is
/// a folder they are confined to; `run_check` only when that folder has a check,
/// named here by its label.
pub fn available(editing: bool, check: Option<&str>) -> Vec<Tool> {
    let mut tools = vec![read_tool()];
    if editing {
        tools.push(list_tool());
        tools.push(search_tool());
        tools.push(edit_tool());
        tools.push(create_tool());
        tools.push(delete_tool());
        tools.push(move_tool());
        if let Some(label) = check {
            tools.push(check_tool(label));
        }
    }
    tools
}

/// What the model is told about the folder, when editing is on.
///
/// Kept short. The tool descriptions carry the mechanics; this carries the one
/// thing only the app knows — where the folder is — and the habits that make
/// the tools work at all: read before you edit, and check before you say done.
///
/// **The editprobe's Rewrite format replaces the read-then-edit sentence word for
/// word**, so that sentence must not change without changing the probe.
pub fn system_prompt(ws: &Workspace, check: bool) -> String {
    let mut s = format!(
        "You can read and change files in the user's project folder: {}\n\
         Use paths relative to that folder. To find where something is, use search or list_files \
         rather than guessing a path. To answer a question about a file, read it — never \
         guess what it says. To change a file, read it with read_file first, then use edit_file \
         with text copied exactly from what you read. Only change what the user asked for. When \
         you are done, say briefly what you changed.",
        ws.root_display()
    );
    if check {
        s.push_str(
            " After changing code, run run_check. If it fails, read the error, fix the code, and \
             run it again before you say you are done.",
        );
    }
    s
}

/// What the model asked for, in a form the UI can show before anything happens.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Read { path: String, range: Option<Lines> },
    Edit { path: String, old_text: String, new_text: String },
    Create { path: String, content: String },
    /// Delete one file. Backed up first; undoable.
    Delete { path: String },
    /// Move or rename one file to a path that does not exist yet.
    Move { path: String, to: String },
    /// Which files match a glob — all of them, with none.
    List { pattern: Option<String> },
    /// Which lines contain some text, optionally only in files matching a glob.
    Search { text: String, pattern: Option<String> },
    /// Run the folder's check command. Carries nothing: what runs was chosen by
    /// a person, not by the model.
    Check,
    /// Probe only — see [`rewrite_tool`].
    Rewrite { path: String, content: String },
}

impl Request {
    pub fn name(&self) -> &'static str {
        match self {
            Request::Read { .. } => "read_file",
            Request::Edit { .. } => "edit_file",
            Request::Create { .. } => "create_file",
            Request::Delete { .. } => "delete_file",
            Request::Move { .. } => "move_file",
            Request::List { .. } => "list_files",
            Request::Search { .. } => "search",
            Request::Check => "run_check",
            Request::Rewrite { .. } => "write_file",
        }
    }

    /// The file a request is about, or `""` for one that is not about a file.
    pub fn path(&self) -> &str {
        match self {
            Request::Read { path, .. }
            | Request::Edit { path, .. }
            | Request::Create { path, .. }
            | Request::Delete { path }
            | Request::Move { path, .. }
            | Request::Rewrite { path, .. } => path,
            Request::List { .. } | Request::Search { .. } | Request::Check => "",
        }
    }
}

/// An optional argument, where a blank one is the same as none.
fn optional(call: &ToolCall, key: &str) -> Option<String> {
    arg(call, key).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn arg(call: &ToolCall, key: &str) -> Option<String> {
    call.function.arguments.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

/// The part of a file a read asked for: 1-based, inclusive, and already
/// normalised — a start of 0 is 1, and no end means the rest of the file.
///
/// What the ticket holds and what the card shows, so the webview cannot ask for
/// one range and be shown another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lines {
    pub start: usize,
    pub end: Option<usize>,
}

/// A line number the model sent, in whichever of its spellings: `120`, `120.0`,
/// `"120"`, `" 120 "`. Absent, `null` and blank are none; anything else that is
/// not a whole number of 0 or more is an error the model is told about.
///
/// **Strings are accepted because a small model sends them** — a number typed into
/// a JSON string is the commonest slip in a tool call, and refusing it costs a
/// round for no safety. Shared by [`interpret`] and by `compact`, so two spellings
/// of one range are one range there too.
pub fn line_arg(args: &serde_json::Value, key: &str) -> Result<Option<usize>, String> {
    let bad = || format!("{key} must be a line number, counting from 1");
    let whole = |n: f64| (n.is_finite() && n >= 0.0 && n.fract() == 0.0 && n < 1e12).then_some(n as usize);
    let value = match args.get(key) {
        None | Some(serde_json::Value::Null) => return Ok(None),
        Some(v) => v,
    };
    match value {
        serde_json::Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                return Ok(Some(u as usize));
            }
            n.as_f64().and_then(whole).map(Some).ok_or_else(bad)
        }
        serde_json::Value::String(s) => {
            let s = s.trim();
            if s.is_empty() {
                return Ok(None);
            }
            if let Ok(n) = s.parse::<u64>() {
                return Ok(Some(n as usize));
            }
            s.parse::<f64>().ok().and_then(whole).map(Some).ok_or_else(bad)
        }
        _ => Err(bad()),
    }
}

/// The range a `read_file` call asked for, or why it cannot be honoured.
///
/// - Neither argument: no range, a whole read.
/// - `start_line` 0 is 1; `end_line` alone is `1..=end`.
/// - An end before the start is refused rather than swapped: it usually means the
///   model sent a count, and a count read as a line would read the wrong lines.
pub fn read_range(args: &serde_json::Value) -> Result<Option<Lines>, String> {
    let start = line_arg(args, "start_line")?;
    let end = line_arg(args, "end_line")?;
    if start.is_none() && end.is_none() {
        return Ok(None);
    }
    let start = start.unwrap_or(1).max(1);
    if end == Some(0) {
        return Err("end_line must be a line number, counting from 1".to_string());
    }
    if end.is_some_and(|e| e < start) {
        return Err("end_line is the last line to read, not a count".to_string());
    }
    Ok(Some(Lines { start, end }))
}

/// Read a call, or say why it cannot be honoured.
///
/// **An unknown tool is refused here, not at execution.** The model can name
/// anything it likes; only these are real.
pub fn interpret(call: &ToolCall) -> Result<Request, String> {
    let name = call.function.name.as_str();
    let path = arg(call, "path").unwrap_or_default().trim().to_string();
    if path.is_empty() && matches!(name, "read_file" | "edit_file" | "create_file" | "write_file" | "delete_file" | "move_file") {
        return Err(format!("{name} was called without a path"));
    }
    match name {
        "read_file" => Ok(Request::Read { path, range: read_range(&call.function.arguments)? }),
        "edit_file" => Ok(Request::Edit {
            path,
            old_text: arg(call, "old_text").ok_or("edit_file was called without old_text")?,
            // Missing new_text is a deletion, which the description invites.
            new_text: arg(call, "new_text").unwrap_or_default(),
        }),
        "create_file" => Ok(Request::Create {
            path,
            content: arg(call, "content").ok_or("create_file was called without content")?,
        }),
        "delete_file" => Ok(Request::Delete { path }),
        "move_file" => Ok(Request::Move {
            path,
            to: arg(call, "to")
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .ok_or("move_file was called without a destination (to)")?,
        }),
        "write_file" => Ok(Request::Rewrite {
            path,
            content: arg(call, "content").ok_or("write_file was called without content")?,
        }),
        "list_files" => Ok(Request::List { pattern: optional(call, "pattern") }),
        "search" => Ok(Request::Search {
            // Not trimmed: leading spaces can be exactly what is being looked
            // for. Only an empty one is refused.
            text: arg(call, "text").filter(|t| !t.is_empty()).ok_or("search was called without text")?,
            pattern: optional(call, "pattern"),
        }),
        // Any arguments the model sends are ignored: there is nothing it may
        // choose about what runs.
        "run_check" => Ok(Request::Check),
        other => Err(format!("no such tool: {other}")),
    }
}

/// What a call needs before it runs.
#[derive(Debug, Clone, PartialEq)]
pub enum Approval {
    /// Run it now, without a card.
    Auto(Auto),
    /// Show a card and wait for the user.
    Ask,
    /// It could never succeed: say why to the model, ask nobody.
    Reject(String),
}

/// A call that runs unasked, in the form the app runs it.
#[derive(Debug, Clone, PartialEq)]
pub enum Auto {
    /// The canonical path to read, inside the project folder, and which lines.
    Read { real: PathBuf, range: Option<Lines> },
    List { pattern: Option<String> },
    Search { text: String, pattern: Option<String> },
}

/// **The approval rule** — one function, so the app and the probe cannot
/// disagree about what asks.
///
/// - No project folder, or `ask_every_call`: ask for everything. So does a
///   [`broad_root`] — a folder that holds the user's whole profile or a whole drive.
/// - A read inside the folder of a file that [`search::visible`] says the
///   listing would show, whose name is not [`search::secret_name`]: automatic.
///   Inside but a folder, or nothing there: rejected, because it could never
///   succeed. Everything else — outside, hidden by a `.gitignore`, secret-named,
///   through a link: ask.
/// - A listing or a search: automatic. Callers have already run
///   [`search::validate`], and the walk cannot leave the folder.
/// - An edit, a create, a delete, a move, a check (and the probe's rewrite): ask.
///
/// A path the check resolved is checked again by the app just before it opens
/// the file — see `run_read` in the app.
pub fn approval(ws: Option<&Workspace>, req: &Request, ask_every_call: bool) -> Approval {
    let Some(ws) = ws else { return Approval::Ask };
    if ask_every_call {
        return Approval::Ask;
    }
    // Choosing `C:\Users\you` as the folder would otherwise let the model read
    // `AppData`, `.gitconfig`, `known_hosts`… unasked: none of them is secret-named.
    // Edits ask already; reads, listings and searches are what this holds back.
    if broad_root(ws.root(), std::env::var_os("USERPROFILE").map(PathBuf::from).as_deref()) {
        return Approval::Ask;
    }
    match req {
        Request::Read { path, range } => match ws.locate(path) {
            Located::Inside(real) if real.is_dir() => Approval::Reject(format!(
                "{} is a folder; use list_files to see what is in it",
                edit::clean(path)
            )),
            Located::Inside(real) if search::visible(ws, &real) && !search::secret_name(&real) => {
                Approval::Auto(Auto::Read { real, range: *range })
            }
            Located::MissingInside => {
                Approval::Reject(format!("{} does not exist in the project folder", edit::clean(path)))
            }
            Located::Inside(_) | Located::Outside(_) => Approval::Ask,
        },
        Request::List { pattern } => Approval::Auto(Auto::List { pattern: pattern.clone() }),
        Request::Search { text, pattern } => {
            Approval::Auto(Auto::Search { text: text.clone(), pattern: pattern.clone() })
        }
        Request::Edit { .. }
        | Request::Create { .. }
        | Request::Delete { .. }
        | Request::Move { .. }
        | Request::Check
        | Request::Rewrite { .. } => Approval::Ask,
    }
}

/// A folder too broad to read from unasked: a drive root, or the user's profile
/// folder or one of its ancestors (`C:\Users`). Compared in plain form,
/// case-insensitively, so `\?\C:\Users\You` equals `c:\users\you`.
pub fn broad_root(root: &Path, profile: Option<&Path>) -> bool {
    let lower = |p: &Path| PathBuf::from(edit::plain(p).to_lowercase());
    let root = lower(root);
    if root.parent().is_none() {
        return true;
    }
    // The profile is canonicalised like the root, so a junctioned profile still matches.
    profile.is_some_and(|p| {
        let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        lower(&p).starts_with(&root)
    })
}

/// Where a read will actually look, and how to show it: `(open, shown)`.
///
/// Relative paths are relative to the project folder when there is one — the
/// same rule edits use, so "read src/main.rs then edit src/main.rs" names one
/// file, not two. `..` is resolved and a file that exists is canonicalised, so
/// the card shows what will really be opened, and the ticket holds that — not a
/// string from the webview. `shown` goes through [`edit::plain`], which also
/// turns `\\?\UNC\server\share` into `\\server\share`.
pub fn read_target(path: &str, ws: Option<&Workspace>) -> (PathBuf, String) {
    let cleaned = edit::clean(path);
    let p = Path::new(&cleaned);
    let joined = match ws {
        Some(ws) if !p.is_absolute() => ws.root().join(p),
        _ => p.to_path_buf(),
    };
    // A network or device path is shown as given and never resolved here: this
    // runs before the card, and resolving it would already contact the host.
    // The read opens it only after Allow.
    if edit::remote_or_device(&joined, ws.map_or(Path::new(""), Workspace::root)) {
        let shown = edit::plain(&joined);
        return (joined, shown);
    }
    let open = std::path::absolute(&joined).unwrap_or(joined);
    let open = std::fs::canonicalize(&open).unwrap_or(open);
    let shown = edit::plain(&open);
    (open, shown)
}

/// What a read answers with, from a file's bytes: the whole text, or the lines
/// asked for under a one-line header. Pure, so every path that reads — the card,
/// the automatic read and the probe — gives the same answer for the same bytes.
/// `shown` is the path as a person reads it, for the errors.
///
/// 1. **A NUL in the first 8 KB is a binary (or UTF-16) file**, refused as text —
///    [`search`]'s own rule, which until now a read did not apply: a UTF-16 file
///    came back as `l\0i\0n\0e\0` and the model tried to edit that. It runs before
///    anything about size.
/// 2. The text is [`edit::for_model`]'s — no BOM, `\n` endings — and a line is what
///    ends in `\n`, which is how `search` numbers `path:line:` and how an edit
///    finds text, so a number from a search is the number here.
/// 3. **No range** and 64 KB or less on disk: the text, exactly as before, with no
///    header. Over that, an error that gives the size and the line count and says
///    to read in parts. It stays an error, so a model that keeps asking for the
///    whole file is stopped by the repeat rule rather than fed a prefix it would
///    take for the file. The numbers carry no separators a model could copy.
/// 4. **A range** reads whole lines, up to 64 KB of them, under
///    `[lines A–B of L]` — or `…; cut at 65536 bytes — read on from line B+1]`
///    when the limit stopped it before the end asked for. An end past the last line
///    is clamped, and the header says so by saying "of L". **The header is one line,
///    and the lines are not numbered**: a small model copies numbers it is shown
///    into the `old_text` of an edit, and the header is refused there too — see
///    [`is_read_header`]. The path is not in it; a canonical Windows path copied
///    into JSON loses its backslashes.
pub fn read_text(shown: &str, bytes: &[u8], range: Option<Lines>) -> String {
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return format!("error: {shown} looks like a binary (or UTF-16) file, so it is not read as text");
    }
    let text = edit::for_model(bytes);
    let Some(range) = range else {
        if bytes.len() as u64 <= MAX_READ_BYTES {
            return text;
        }
        return format!(
            "error: {shown} is {} bytes and {} lines, over the {MAX_READ_BYTES}-byte limit for one \
             read. Read it in parts with start_line and end_line; search gives the line numbers.",
            bytes.len(),
            text.split_inclusive('\n').count()
        );
    };

    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let total = lines.len();
    if total == 0 {
        return format!("error: {shown} is empty, so it has no lines to read");
    }
    if range.start > total {
        return format!(
            "error: {shown} has {total} line{}; start_line {} is past the end",
            if total == 1 { "" } else { "s" },
            range.start
        );
    }
    let last = range.end.map_or(total, |e| e.min(total));
    let mut body = String::new();
    let mut taken = 0;
    for line in &lines[range.start - 1..last] {
        if body.len() + line.len() > MAX_READ_BYTES as usize {
            break;
        }
        body.push_str(line);
        taken += 1;
    }
    if taken == 0 {
        return format!(
            "error: line {} of {shown} is {} bytes, over the {MAX_READ_BYTES}-byte limit for one read",
            range.start,
            lines[range.start - 1].len()
        );
    }
    let to = range.start + taken - 1;
    let cut = if to < last {
        format!("; cut at {MAX_READ_BYTES} bytes — read on from line {}", to + 1)
    } else {
        String::new()
    };
    format!("[lines {}–{to} of {total}{cut}]\n{body}", range.start)
}

/// Whether a line is the header a ranged read puts first — which is never part
/// of the file. Checked by `edit` on an `old_text` that matched nothing.
pub fn is_read_header(line: &str) -> bool {
    let line = line.trim();
    line.starts_with("[lines ") && line.ends_with(']') && line.contains(" of ")
}

/// A file too big to read even in parts. Said before anything is read.
fn over_source(shown: &str, len: u64) -> String {
    format!(
        "error: {shown} is {len} bytes, over the {MAX_RANGED_SOURCE_BYTES}-byte limit for reading any part of a file"
    )
}

/// Run an **already approved** read.
///
/// Errors come back as text for the model rather than as a failure, because a
/// tool that cannot answer is a normal event in a conversation: the model should
/// be told "that file does not exist" and continue, not have the turn collapse.
///
/// The contents are normalised the way edits match them — `\n` endings, no
/// BOM — so text the model copies out of a read is text an edit can find. What is
/// answered, and when, is [`read_text`]'s.
pub fn run_read_file(p: &Path, range: Option<Lines>) -> String {
    use std::io::Read;

    let path = edit::plain(p);

    let meta = match std::fs::metadata(p) {
        Ok(m) => m,
        Err(e) => return format!("error: cannot read {path}: {e}"),
    };
    if meta.is_dir() {
        return format!("error: {path} is a directory, not a file");
    }
    if meta.len() > MAX_RANGED_SOURCE_BYTES {
        return over_source(&path, meta.len());
    }

    let mut bytes = Vec::new();
    match std::fs::File::open(p).and_then(|f| f.take(MAX_RANGED_SOURCE_BYTES + 1).read_to_end(&mut bytes)) {
        Ok(_) if bytes.len() as u64 > MAX_RANGED_SOURCE_BYTES => over_source(&path, bytes.len() as u64),
        Ok(_) => read_text(&path, &bytes, range),
        Err(e) => format!("error: cannot read {path}: {e}"),
    }
}

/// An **automatic** read, done carefully — `None` means "do not read this
/// unasked; ask the user instead".
///
/// [`approval`] decided on a path, and a path is not a file: between that
/// decision and the open, a link can be swapped in, and a hard link makes a file
/// inside the folder *be* a file outside it (`mklink /H` needs no admin rights),
/// which no check of its name can see. So, on Windows, after the decision:
///
/// 1. canonicalise `real` again and require it is still the same path;
/// 2. open it, and ask the **open handle** where it really is
///    (`GetFinalPathNameByHandleW`) and require that is `real` — so what is read
///    is the file that was approved, not whatever the name points at by now;
/// 3. ask the handle how many names the file has (`GetFileInformationByHandle`)
///    and require exactly one — a second name is a hard link, possibly from
///    outside the folder.
///
/// Those are [`edit::open_checked`]'s checks, shared with the search. The bytes
/// come from that same handle, so nothing can change between the
/// checks and the read. Reads past the limit or that fail are `Some("error: …")`,
/// as for [`run_read_file`]: the model is told, and nobody is asked about a read
/// that cannot work. Where this is not Windows nothing is checkable, so nothing
/// is automatic.
#[cfg(windows)]
pub fn read_unasked(real: &Path, range: Option<Lines>) -> Option<String> {
    use std::io::Read;

    let mut file = edit::open_checked(real)?;
    let shown = edit::plain(real);
    let len = match file.metadata() {
        Ok(m) => m.len(),
        Err(e) => return Some(format!("error: cannot read {shown}: {e}")),
    };
    if len > MAX_RANGED_SOURCE_BYTES {
        return Some(over_source(&shown, len));
    }
    let mut bytes = Vec::new();
    // One past the limit, so a file that grew since `metadata` is still refused.
    if let Err(e) = (&mut file).take(MAX_RANGED_SOURCE_BYTES + 1).read_to_end(&mut bytes) {
        return Some(format!("error: cannot read {shown}: {e}"));
    }
    if bytes.len() as u64 > MAX_RANGED_SOURCE_BYTES {
        return Some(over_source(&shown, bytes.len() as u64));
    }
    Some(read_text(&shown, &bytes, range))
}

#[cfg(not(windows))]
pub fn read_unasked(_real: &Path, _range: Option<Lines>) -> Option<String> {
    None
}

/// Said to the model when it sends a call identical to one that already failed.
pub const REPEATED_NOTE: &str = " You already sent exactly this call and it failed the same way. \
     Change it before trying again — copy the text from the file.";

/// The same, for a call that has nothing in it to change. Telling the model to
/// copy text from a file would send it looking for a mistake it did not make.
const REPEATED_CHECK_NOTE: &str = " You already sent this call and it failed the same way. Do not \
     call it again — tell the user what happened.";

/// The same, for a read. There is no text to copy from a file when the call
/// was the read: what can change is which lines, or whether to search first.
const REPEATED_READ_NOTE: &str = " You already sent exactly this call and it failed the same way. \
     Change it before trying again — a different range, or search first.";

/// The same, for a delete or a move. What can change is the path, not any text.
const REPEATED_PATH_NOTE: &str = " You already sent exactly this call and it failed the same way. \
     Change it before trying again — check the path with list_files.";

/// Which repeat note fits this tool.
pub fn repeated_note(name: &str) -> &'static str {
    match name {
        "run_check" => REPEATED_CHECK_NOTE,
        "read_file" => REPEATED_READ_NOTE,
        "delete_file" | "move_file" => REPEATED_PATH_NOTE,
        _ => REPEATED_NOTE,
    }
}

/// The third identical failure stops the loop.
///
/// **Measured at G7:** the model re-sent the same impossible edit — once eight
/// times running — until the round bound ended the turn. Each repeat costs a
/// round and changes nothing, since the file has not changed either.
pub const MAX_IDENTICAL_FAILURES: usize = 3;

/// How many times this exact call has already failed since the user's last
/// message.
///
/// Reads in between do not reset the count — the loop the probe caught was
/// read, fail, read, fail — and a failure is a tool result starting `error:`,
/// which is how every tool here reports one.
pub fn prior_failures(history: &[Message], call: &ToolCall) -> usize {
    let since = history.iter().rposition(|m| m.role == "user").map_or(0, |i| i + 1);
    let turns = &history[since..];
    let mut count = 0;
    for (i, m) in turns.iter().enumerate() {
        let same = m.role == "assistant"
            && m.tool_calls.as_deref().is_some_and(|calls| {
                calls.iter().any(|c| {
                    c.function.name == call.function.name && c.function.arguments == call.function.arguments
                })
            });
        let failed = turns
            .get(i + 1)
            .is_some_and(|r| r.role == "tool" && r.content.trim_start().starts_with("error:"));
        if same && failed {
            count += 1;
        }
    }
    count
}

/// Build files changed by an edit since the user's last message: `build.rs`,
/// `Cargo.toml`, `package.json` — written, deleted, or moved from or to.
///
/// **A check runs them.** `build.rs` is compiled and executed; the manifests say
/// what else runs. The model may have good reason to change them, so they are
/// not refused like `.cargo/` — but the card asking you to run the check names
/// them, because approving a check after one changed is approving whatever the
/// check now does.
pub fn build_files_changed(history: &[Message]) -> Vec<String> {
    const BUILD_FILES: [&str; 3] = ["build.rs", "cargo.toml", "package.json"];
    let since = history.iter().rposition(|m| m.role == "user").map_or(0, |i| i + 1);
    let turns = &history[since..];
    let mut out: Vec<String> = Vec::new();
    for (i, m) in turns.iter().enumerate() {
        let Some(call) = m.tool_calls.as_deref().and_then(|c| c.first()) else { continue };
        if m.role != "assistant"
            || !matches!(call.function.name.as_str(), "edit_file" | "create_file" | "delete_file" | "move_file")
        {
            continue;
        }
        // Only what was actually written: `applied_message` is the only result
        // that says so. A refusal, a denial or a rejection says something else.
        let written = turns.get(i + 1).is_some_and(|r| {
            r.role == "tool"
                && ["Edited ", "Created ", "Deleted ", "Moved "].iter().any(|p| r.content.starts_with(p))
        });
        // A move changes two paths: the build file it took away and the one it
        // put there. Cleaned the way `edit` cleans it before writing, so a path
        // the model wrapped in quotes or backticks is still recognised.
        for key in ["path", "to"] {
            let path = edit::clean(&arg(call, key).unwrap_or_default());
            let file = path.rsplit(['/', '\\']).next().unwrap_or_default().to_ascii_lowercase();
            if written && BUILD_FILES.contains(&file.as_str()) && !out.contains(&path) {
                out.push(path);
            }
        }
    }
    out
}

/// What the model is told after an edit. Short, and specific enough that it can
/// tell a one-line fix from a rewrite it did not intend.
pub fn applied_message(a: &Applied) -> String {
    let made = if a.folders.is_empty() {
        String::new()
    } else {
        format!(" Made the folder{} {}.", if a.folders.len() == 1 { "" } else { "s" }, a.folders.join(", "))
    };
    if a.op == edit::Op::Delete {
        format!("Deleted {}.", a.display)
    } else if a.op == edit::Op::Move {
        format!("Moved {} to {}.{made}", a.display, a.to.as_deref().unwrap_or_default())
    } else if a.create {
        format!("Created {} ({} lines).{made}", a.display, a.added)
    } else {
        format!(
            "Edited {}: {} line{} removed, {} added.",
            a.display,
            a.removed,
            if a.removed == 1 { "" } else { "s" },
            a.added
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::FunctionCall;

    fn call(name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: None,
            function: FunctionCall {
                name: name.to_string(),
                arguments: args,
            },
        }
    }

    #[test]
    fn only_the_tools_we_offer_are_accepted() {
        // The model can name anything; only this list is real.
        assert!(interpret(&call("rm_rf", json!({ "path": "/" }))).is_err());
        assert!(interpret(&call("remove_file", json!({ "path": "a.txt" }))).is_err());
        assert!(interpret(&call("rename_file", json!({ "path": "a.txt", "to": "b.txt" }))).is_err());
        assert!(interpret(&call("read_file", json!({ "path": "a.txt" }))).is_ok());
    }

    #[test]
    fn a_delete_and_a_move_are_read_whole() {
        let d = interpret(&call("delete_file", json!({ "path": " old.rs " }))).unwrap();
        assert_eq!(d, Request::Delete { path: "old.rs".into() });
        assert_eq!((d.name(), d.path()), ("delete_file", "old.rs"));
        let m = interpret(&call("move_file", json!({ "path": "a.rs", "to": " src/b.rs " }))).unwrap();
        assert_eq!(m, Request::Move { path: "a.rs".into(), to: "src/b.rs".into() });
        assert_eq!((m.name(), m.path()), ("move_file", "a.rs"));
        // A move needs somewhere to go.
        assert!(interpret(&call("move_file", json!({ "path": "a.rs" }))).is_err());
        assert!(interpret(&call("move_file", json!({ "path": "a.rs", "to": "  " }))).is_err());
    }

    #[test]
    fn a_call_without_a_path_is_refused() {
        assert!(interpret(&call("read_file", json!({}))).is_err());
        assert!(interpret(&call("read_file", json!({ "path": "  " }))).is_err());
        assert!(interpret(&call("edit_file", json!({ "old_text": "a", "new_text": "b" }))).is_err());
        assert!(interpret(&call("delete_file", json!({}))).is_err());
        assert!(interpret(&call("move_file", json!({ "to": "b.rs" }))).is_err());
    }

    #[test]
    fn edits_are_read_whole() {
        let r = interpret(&call(
            "edit_file",
            json!({ "path": "src/a.rs", "old_text": "x", "new_text": "y" }),
        ))
        .unwrap();
        assert_eq!(
            r,
            Request::Edit { path: "src/a.rs".into(), old_text: "x".into(), new_text: "y".into() }
        );
        assert_eq!(r.name(), "edit_file");
        assert_eq!(r.path(), "src/a.rs");
    }

    #[test]
    fn a_missing_new_text_is_a_deletion() {
        let r = interpret(&call("edit_file", json!({ "path": "a", "old_text": "x" }))).unwrap();
        assert!(matches!(r, Request::Edit { ref new_text, .. } if new_text.is_empty()));
    }

    #[test]
    fn an_edit_needs_old_text_and_a_create_needs_content() {
        assert!(interpret(&call("edit_file", json!({ "path": "a", "new_text": "y" }))).is_err());
        assert!(interpret(&call("create_file", json!({ "path": "a" }))).is_err());
    }

    #[test]
    fn editing_tools_are_offered_only_with_a_folder() {
        let names = |v: Vec<Tool>| v.into_iter().map(|t| t.function.name).collect::<Vec<_>>();
        assert_eq!(names(available(false, None)), ["read_file"]);
        assert_eq!(
            names(available(true, None)),
            ["read_file", "list_files", "search", "edit_file", "create_file", "delete_file", "move_file"]
        );
        // The probe-only rewrite is never offered by the app.
        assert!(!names(available(true, None)).contains(&"write_file".to_string()));
    }

    #[test]
    fn the_new_tools_say_what_they_are_not_for_and_what_they_do_not_do() {
        let tools = available(true, None);
        let desc = |n: &str| tools.iter().find(|t| t.function.name == n).unwrap().function.description.clone();
        assert!(desc("delete_file").contains("not a way to rewrite a file — use edit_file"));
        assert!(desc("move_file").contains("References to the file are not updated — edit them"));
        let create = desc("create_file");
        assert!(create.contains(CREATE_FOLDERS) && !create.contains("must already exist"), "{create}");
        let params = |n: &str| tools.iter().find(|t| t.function.name == n).unwrap().function.parameters.clone();
        assert_eq!(params("move_file")["required"], json!(["path", "to"]));
        assert_eq!(params("delete_file")["required"], json!(["path"]));
    }

    #[test]
    fn the_check_is_offered_only_with_a_folder_that_has_one() {
        let names = |v: Vec<Tool>| v.into_iter().map(|t| t.function.name).collect::<Vec<_>>();
        assert_eq!(
            names(available(true, Some("cargo check"))),
            [
                "read_file",
                "list_files",
                "search",
                "edit_file",
                "create_file",
                "delete_file",
                "move_file",
                "run_check"
            ]
        );
        // A check without a folder has nowhere to run.
        assert_eq!(names(available(false, Some("cargo check"))), ["read_file"]);
        // And the model is told what it would run.
        let tool = available(true, Some("cargo check")).pop().unwrap();
        assert!(tool.function.description.contains("`cargo check`"));
    }

    #[test]
    fn finding_calls_are_read_with_their_optional_pattern() {
        assert_eq!(interpret(&call("list_files", json!({}))).unwrap(), Request::List { pattern: None });
        assert_eq!(
            interpret(&call("list_files", json!({ "pattern": " src/**/*.rs " }))).unwrap(),
            Request::List { pattern: Some("src/**/*.rs".into()) }
        );
        // A blank pattern is no pattern, not a pattern matching nothing.
        assert_eq!(
            interpret(&call("search", json!({ "text": "  greet(", "pattern": "" }))).unwrap(),
            Request::Search { text: "  greet(".into(), pattern: None }
        );
        assert!(interpret(&call("search", json!({ "pattern": "*.rs" }))).is_err());
        assert!(interpret(&call("search", json!({ "text": "" }))).is_err());
    }

    #[test]
    fn a_check_call_carries_nothing_the_model_chose() {
        // Arguments are ignored: an argv smuggled in by the model goes nowhere.
        let r = interpret(&call("run_check", json!({ "command": "del /s C:\\" }))).unwrap();
        assert_eq!(r, Request::Check);
        assert_eq!((r.name(), r.path()), ("run_check", ""));
    }

    #[test]
    fn the_prompt_asks_for_a_check_only_when_there_is_one() {
        let dir = std::env::temp_dir().join(format!("odin-prompt-ws-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ws = Workspace::open(&dir).unwrap();
        assert!(!system_prompt(&ws, false).contains("run_check"));
        assert!(system_prompt(&ws, true).contains("run_check"));
        // editprobe's Rewrite format swaps this sentence out word for word.
        for check in [false, true] {
            assert!(system_prompt(&ws, check)
                .contains("then use edit_file with text copied exactly from what you read"));
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_repeated_check_is_not_told_to_copy_text_from_a_file() {
        assert!(repeated_note("edit_file").contains("copy the text"));
        assert!(!repeated_note("run_check").contains("copy the text"));
    }

    #[test]
    fn a_repeated_read_is_told_to_change_the_range_not_to_copy_text() {
        let note = repeated_note("read_file");
        assert!(note.contains("a different range, or search first"), "{note}");
        assert!(!note.contains("copy the text"), "{note}");
    }

    #[test]
    fn errors_come_back_as_text_for_the_model() {
        // Not a Result: a tool that cannot answer is a normal conversational
        // event, and the model should be told rather than the turn collapsing.
        let out = run_read_file(Path::new("C:\\definitely\\not\\here.txt"), None);
        assert!(out.starts_with("error:"), "{out}");
    }

    #[test]
    fn a_directory_is_refused_clearly() {
        let out = run_read_file(Path::new("."), None);
        assert!(out.contains("directory"), "{out}");
    }

    #[test]
    fn a_real_file_comes_back_normalised() {
        let mut p = std::env::temp_dir();
        p.push(format!("odin-tools-test-{}.txt", std::process::id()));
        std::fs::write(&p, "hello\r\nfrom a tool\r\n").unwrap();
        assert_eq!(run_read_file(&p, None), "hello\nfrom a tool\n");
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn relative_reads_resolve_in_the_project_folder() {
        let s = Sandbox::new("relread");
        let ws = s.ws();
        s.write("src/main.rs", "x");
        let (open, shown) = read_target("src/main.rs", Some(&ws));
        assert_eq!(open, std::fs::canonicalize(ws.root().join("src/main.rs")).unwrap());
        assert!(shown.ends_with(r"src\main.rs"), "{shown}");
        assert!(!shown.starts_with(r"\\?\"), "shown to a person, so no verbatim prefix: {shown}");
        // Absolute paths are left alone; with no folder, nothing is joined.
        assert_eq!(read_target(r"C:\x\y.txt", Some(&ws)).1, r"C:\x\y.txt");
        assert!(read_target("src/main.rs", None).1.ends_with(r"src\main.rs"));
    }

    /// The card must show what will really be opened, not `proj\src\..\..\x`.
    #[test]
    fn a_read_target_resolves_dot_dot_and_shows_no_verbatim_prefix() {
        let s = Sandbox::new("dotdotread");
        let ws = s.ws();
        std::fs::write(s.dir.join("outside.txt"), "x").unwrap();
        let (open, shown) = read_target("src/../../outside.txt", Some(&ws));
        assert_eq!(open, std::fs::canonicalize(s.dir.join("outside.txt")).unwrap());
        assert_eq!(shown, edit::plain(&open));
        assert!(!shown.contains("..") && !shown.starts_with(r"\\?\"), "{shown}");
        // A file that is not there yet is still shown resolved.
        let (_, shown) = read_target("src/../nope.txt", Some(&ws));
        assert!(!shown.contains("..") && shown.ends_with(r"project\nope.txt"), "{shown}");
    }

    /// The `\\?\UNC\…` form of a share, shown the way a person writes it.
    #[test]
    fn a_network_share_is_shown_as_one() {
        assert_eq!(edit::plain(Path::new(r"\\?\UNC\server\share\a.txt")), r"\\server\share\a.txt");
    }

    // ── the approval rule ──

    /// A throwaway project at `dir/project`, removed on drop.
    struct Sandbox {
        dir: PathBuf,
    }

    impl Sandbox {
        fn new(tag: &str) -> Self {
            let n = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            let dir = std::env::temp_dir().join(format!("odin-tools-{tag}-{}-{n}", std::process::id()));
            std::fs::create_dir_all(dir.join("project")).unwrap();
            Self { dir }
        }
        fn ws(&self) -> Workspace {
            Workspace::open(self.dir.join("project")).unwrap()
        }
        fn write(&self, rel: &str, text: &str) {
            let p = self.dir.join("project").join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn read(path: &str) -> Request {
        Request::Read { path: path.to_string(), range: None }
    }

    fn auto_read(s: &Sandbox, rel: &str) -> Approval {
        Approval::Auto(Auto::Read {
            real: std::fs::canonicalize(s.dir.join("project").join(rel)).unwrap(),
            range: None,
        })
    }

    #[test]
    fn a_read_inside_the_folder_runs_without_asking() {
        let s = Sandbox::new("auto");
        let ws = s.ws();
        s.write("src/main.rs", "fn main() {}\n");
        s.write(".github/ci.yml", "x\n");
        assert_eq!(approval(Some(&ws), &read("src/main.rs"), false), auto_read(&s, "src/main.rs"));
        assert_eq!(approval(Some(&ws), &read(".github/ci.yml"), false), auto_read(&s, ".github/ci.yml"));
        // However it is spelled: absolute, with a `..` that stays inside, quoted.
        let abs = ws.root().join("src/main.rs");
        assert_eq!(approval(Some(&ws), &read(&edit::plain(&abs)), false), auto_read(&s, "src/main.rs"));
        assert_eq!(approval(Some(&ws), &read("src/../src/main.rs"), false), auto_read(&s, "src/main.rs"));
        assert_eq!(approval(Some(&ws), &read("`src/main.rs`"), false), auto_read(&s, "src/main.rs"));
    }

    #[test]
    fn a_read_that_leaves_the_folder_or_looks_secret_or_is_hidden_asks() {
        let s = Sandbox::new("ask");
        let ws = s.ws();
        std::fs::write(s.dir.join("outside.txt"), "x").unwrap();
        s.write(".env", "TOKEN=x\n");
        s.write("keys/id_rsa", "x\n");
        s.write("keys/server.pem", "x\n");
        s.write(".gitignore", "local.txt\n");
        s.write("local.txt", "x\n");
        s.write("target/debug/x.rs", "x\n");
        s.write(".git/config", "x\n");
        for path in [
            "../outside.txt",
            r"C:\Windows\win.ini",
            ".env",
            "keys/id_rsa",
            "keys/server.pem",
            "local.txt",
            "target/debug/x.rs",
            ".git/config",
        ] {
            assert_eq!(approval(Some(&ws), &read(path), false), Approval::Ask, "{path}");
        }
    }

    #[test]
    fn a_read_through_a_junction_that_points_out_asks() {
        let s = Sandbox::new("junction");
        let ws = s.ws();
        let outside = s.dir.join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("there.txt"), "x").unwrap();
        let link = ws.root().join("linked");
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(edit::plain(&link))
            .arg(edit::plain(&outside))
            .output();
        if !matches!(made, Ok(ref o) if o.status.success()) {
            eprintln!("could not create a junction here; skipping");
            return;
        }
        assert_eq!(approval(Some(&ws), &read("linked/there.txt"), false), Approval::Ask);
        // Missing, under the same link: still outside, so still a question, not a rejection.
        assert_eq!(approval(Some(&ws), &read("linked/missing.txt"), false), Approval::Ask);
    }

    #[test]
    fn a_read_that_could_never_succeed_is_rejected_not_asked() {
        let s = Sandbox::new("reject");
        let ws = s.ws();
        s.write("src/main.rs", "x");
        match approval(Some(&ws), &read("src"), false) {
            Approval::Reject(why) => assert_eq!(why, "src is a folder; use list_files to see what is in it"),
            other => panic!("{other:?}"),
        }
        match approval(Some(&ws), &read("src/none.rs"), false) {
            Approval::Reject(why) => assert_eq!(why, "src/none.rs does not exist in the project folder"),
            other => panic!("{other:?}"),
        }
    }

    #[cfg(windows)]
    #[test]
    fn an_automatic_read_opens_a_plain_file_and_returns_it_normalised() {
        let s = Sandbox::new("unasked");
        s.write("a.txt", "one\r\ntwo\r\n");
        let real = std::fs::canonicalize(s.dir.join("project/a.txt")).unwrap();
        assert_eq!(read_unasked(&real, None).as_deref(), Some("one\ntwo\n"));
        // Too big is an answer for the model, not a question for the user.
        s.write("big.txt", &"x".repeat(MAX_READ_BYTES as usize + 1));
        let big = std::fs::canonicalize(s.dir.join("project/big.txt")).unwrap();
        assert!(read_unasked(&big, None).unwrap().starts_with("error:"));
    }

    /// **A hard link is a second name for the same file**, so a file inside the
    /// folder can be one outside it. Nothing about its name or path shows that;
    /// the handle does.
    #[cfg(windows)]
    #[test]
    fn an_automatic_read_refuses_a_file_with_a_second_name() {
        let s = Sandbox::new("hardlink");
        s.write("a.txt", "x");
        let a = s.dir.join("project/a.txt");
        if std::fs::hard_link(&a, s.dir.join("project/b.txt")).is_err() {
            eprintln!("could not create a hard link here; skipping");
            return;
        }
        for name in ["a.txt", "b.txt"] {
            let real = std::fs::canonicalize(s.dir.join("project").join(name)).unwrap();
            assert_eq!(read_unasked(&real, None), None, "{name}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn an_automatic_read_refuses_a_path_that_is_not_the_one_it_resolved() {
        let s = Sandbox::new("moved");
        s.write("src/a.txt", "x");
        let via_dots = s.dir.join("project/src/../src/a.txt");
        assert_eq!(read_unasked(&via_dots, None), None);
        assert_eq!(read_unasked(&s.dir.join("project/missing.txt"), None), None);
    }

    #[test]
    fn listing_and_searching_run_without_asking() {
        let s = Sandbox::new("find");
        let ws = s.ws();
        let list = Request::List { pattern: Some("*.rs".into()) };
        let search = Request::Search { text: "greet".into(), pattern: None };
        assert_eq!(
            approval(Some(&ws), &list, false),
            Approval::Auto(Auto::List { pattern: Some("*.rs".into()) })
        );
        assert_eq!(
            approval(Some(&ws), &search, false),
            Approval::Auto(Auto::Search { text: "greet".into(), pattern: None })
        );
    }

    #[test]
    fn edits_creates_deletes_moves_and_checks_always_ask() {
        let s = Sandbox::new("edits");
        let ws = s.ws();
        s.write("a.rs", "x");
        let edit = Request::Edit { path: "a.rs".into(), old_text: "x".into(), new_text: "y".into() };
        let create = Request::Create { path: "b.rs".into(), content: "x".into() };
        let rewrite = Request::Rewrite { path: "a.rs".into(), content: "y".into() };
        let delete = Request::Delete { path: "a.rs".into() };
        let moved = Request::Move { path: "a.rs".into(), to: "b.rs".into() };
        for req in [edit, create, delete, moved, rewrite, Request::Check] {
            assert_eq!(approval(Some(&ws), &req, false), Approval::Ask, "{req:?}");
        }
    }

    #[test]
    fn the_switch_and_having_no_folder_ask_for_everything() {
        let s = Sandbox::new("switch");
        let ws = s.ws();
        s.write("src/main.rs", "x");
        let list = Request::List { pattern: None };
        let search = Request::Search { text: "x".into(), pattern: None };
        for req in [read("src/main.rs"), read("src"), read("nope.rs"), list, search] {
            assert_eq!(approval(Some(&ws), &req, true), Approval::Ask, "on: {req:?}");
            assert_eq!(approval(None, &req, false), Approval::Ask, "no folder: {req:?}");
        }
    }

    fn asked(call: &ToolCall) -> Message {
        Message {
            role: "assistant".into(),
            content: String::new(),
            images: None,
            tool_calls: Some(vec![call.clone()]),
            tool_name: None,
        }
    }

    #[test]
    fn identical_failures_are_counted_across_reads() {
        // read, fail, read, fail — the G7 loop. The reads must not reset it.
        let edit = call("edit_file", json!({ "path": "a", "old_text": "x", "new_text": "y" }));
        let read = call("read_file", json!({ "path": "a" }));
        let history = vec![
            Message::user("change x to y"),
            asked(&read),
            Message::tool_result("read_file", "x"),
            asked(&edit),
            Message::tool_result("edit_file", "error: old_text was not found"),
            asked(&read),
            Message::tool_result("read_file", "x"),
            asked(&edit),
            Message::tool_result("edit_file", "error: old_text was not found"),
        ];
        assert_eq!(prior_failures(&history, &edit), 2);
        assert_eq!(prior_failures(&history, &read), 0, "reads succeeded");
    }

    #[test]
    fn a_different_call_or_a_success_is_not_a_repeat() {
        let a = call("edit_file", json!({ "path": "a", "old_text": "x", "new_text": "y" }));
        let b = call("edit_file", json!({ "path": "a", "old_text": "x ", "new_text": "y" }));
        let history = vec![
            Message::user("go"),
            asked(&a),
            Message::tool_result("edit_file", "Edited a: 1 line removed, 1 added."),
        ];
        assert_eq!(prior_failures(&history, &a), 0, "it worked");
        assert_eq!(prior_failures(&history, &b), 0, "a different call");
    }

    #[test]
    fn build_files_edited_this_turn_are_named() {
        let manifest = call("edit_file", json!({ "path": "Cargo.toml", "old_text": "a", "new_text": "b" }));
        let script = call("create_file", json!({ "path": "crates/x/build.rs", "content": "fn main() {}" }));
        let source = call("edit_file", json!({ "path": "src/main.rs", "old_text": "a", "new_text": "b" }));
        let refused = call("edit_file", json!({ "path": "web/package.json", "old_text": "a", "new_text": "b" }));
        let history = vec![
            Message::user("go"),
            asked(&manifest),
            Message::tool_result("edit_file", "Edited Cargo.toml: 1 line removed, 1 added."),
            asked(&script),
            Message::tool_result("create_file", "Created crates/x/build.rs (1 lines)."),
            asked(&source),
            Message::tool_result("edit_file", "Edited src/main.rs: 1 line removed, 1 added."),
            // Refused, so nothing was written.
            asked(&refused),
            Message::tool_result("edit_file", "error: old_text was not found"),
        ];
        assert_eq!(build_files_changed(&history), ["Cargo.toml", "crates/x/build.rs"]);
    }

    #[test]
    fn a_move_to_build_rs_a_move_from_cargo_toml_and_a_delete_of_package_json_are_named() {
        let to = call("move_file", json!({ "path": "scripts/b.rs", "to": "crates/x/build.rs" }));
        let from = call("move_file", json!({ "path": "Cargo.toml", "to": "old/Cargo.toml.bak" }));
        let gone = call("delete_file", json!({ "path": "web/package.json" }));
        let plain = call("delete_file", json!({ "path": "src/old.rs" }));
        let refused = call("delete_file", json!({ "path": "build.rs" }));
        let history = vec![
            Message::user("go"),
            asked(&to),
            Message::tool_result("move_file", "Moved scripts/b.rs to crates/x/build.rs."),
            asked(&from),
            Message::tool_result("move_file", "Moved Cargo.toml to old/Cargo.toml.bak."),
            asked(&gone),
            Message::tool_result("delete_file", "Deleted web/package.json."),
            asked(&plain),
            Message::tool_result("delete_file", "Deleted src/old.rs."),
            asked(&refused),
            Message::tool_result("delete_file", "error: build.rs is read-only, so it is never deleted"),
        ];
        assert_eq!(
            build_files_changed(&history),
            ["crates/x/build.rs", "Cargo.toml", "web/package.json"]
        );
    }

    #[test]
    fn the_results_of_the_new_tools_read_as_what_was_done() {
        let applied = |op, create, to: Option<&str>, folders: &[&str]| Applied {
            id: "1".into(),
            display: "src/a.rs".into(),
            added: 3,
            removed: 0,
            create,
            op,
            to: to.map(str::to_string),
            folders: folders.iter().map(|f| f.to_string()).collect(),
        };
        assert_eq!(applied_message(&applied(edit::Op::Delete, false, None, &[])), "Deleted src/a.rs.");
        assert_eq!(
            applied_message(&applied(edit::Op::Move, false, Some("src/net/a.rs"), &["src/net/"])),
            "Moved src/a.rs to src/net/a.rs. Made the folder src/net/."
        );
        assert_eq!(
            applied_message(&applied(edit::Op::Create, true, None, &["src/", "src/x/"])),
            "Created src/a.rs (3 lines). Made the folders src/, src/x/."
        );
        assert_eq!(applied_message(&applied(edit::Op::Create, true, None, &[])), "Created src/a.rs (3 lines).");
    }

    #[test]
    fn a_repeated_delete_or_move_is_told_to_check_the_path_not_to_copy_text() {
        for name in ["delete_file", "move_file"] {
            let note = repeated_note(name);
            assert!(note.contains("check the path with list_files"), "{note}");
            assert!(!note.contains("copy the text"), "{note}");
        }
    }

    #[test]
    fn a_declined_build_file_edit_is_not_named_but_a_quoted_one_is() {
        let declined = call("edit_file", json!({ "path": "Cargo.toml", "old_text": "a", "new_text": "b" }));
        let quoted = call("edit_file", json!({ "path": "`build.rs` ", "old_text": "a", "new_text": "b" }));
        let history = vec![
            Message::user("go"),
            asked(&declined),
            Message::tool_result("edit_file", "The user declined this change to Cargo.toml. Nothing was written."),
            asked(&quoted),
            Message::tool_result("edit_file", "Edited build.rs: 1 line removed, 1 added."),
        ];
        assert_eq!(build_files_changed(&history), ["build.rs"]);
    }

    #[test]
    fn build_files_from_an_earlier_request_are_not_named() {
        let manifest = call("edit_file", json!({ "path": "Cargo.toml", "old_text": "a", "new_text": "b" }));
        let history = vec![
            Message::user("first"),
            asked(&manifest),
            Message::tool_result("edit_file", "Edited Cargo.toml: 1 line removed, 1 added."),
            Message::user("now check it"),
        ];
        assert!(build_files_changed(&history).is_empty());
    }

    #[test]
    fn failures_before_the_last_user_message_do_not_count() {
        let a = call("edit_file", json!({ "path": "a", "old_text": "x", "new_text": "y" }));
        let history = vec![
            Message::user("first"),
            asked(&a),
            Message::tool_result("edit_file", "error: nope"),
            Message::user("try again now, I fixed the file"),
        ];
        assert_eq!(prior_failures(&history, &a), 0);
    }

    #[test]
    fn a_drive_root_or_the_whole_profile_is_too_broad_to_read_unasked() {
        // Made-up profile names, so nothing here depends on this machine's folders.
        let profile = Some(Path::new(r"C:\Users\odin-test-user"));
        assert!(broad_root(Path::new(r"C:\"), profile));
        assert!(broad_root(Path::new(r"\\?\D:\"), None));
        assert!(broad_root(Path::new(r"C:\Users"), profile));
        assert!(broad_root(Path::new(r"C:\Users\odin-test-user"), profile));
        assert!(broad_root(Path::new(r"\\?\c:\users\ODIN-TEST-USER"), profile), "case and prefix do not matter");
        assert!(!broad_root(Path::new(r"C:\Users\odin-test-user\source\repos\odin"), profile));
        assert!(!broad_root(Path::new(r"C:\work\proj"), profile));
        assert!(!broad_root(Path::new(r"C:\work\proj"), None));
    }

    #[test]
    fn a_read_on_another_machine_is_shown_as_given_and_not_resolved() {
        for ws in [None, Some(&Sandbox::new("rt-remote").ws())] {
            let (open, shown) = read_target(r"\\odin-test-no-such-host\share\x.txt", ws);
            assert_eq!(open, PathBuf::from(r"\\odin-test-no-such-host\share\x.txt"));
            assert_eq!(shown, r"\\odin-test-no-such-host\share\x.txt");
        }
    }

    // ── reading part of a file ──

    fn lines(start: usize, end: Option<usize>) -> Option<Lines> {
        Some(Lines { start, end })
    }

    /// `n` lines, `line 1` to `line n`.
    fn numbered(n: usize) -> String {
        (1..=n).map(|i| format!("line {i}\n")).collect()
    }

    #[test]
    fn a_line_number_is_a_number_or_a_string_of_one() {
        let get = |v: serde_json::Value| line_arg(&json!({ "start_line": v }), "start_line");
        for (v, want) in [
            (json!(120), Some(120)),
            (json!("120"), Some(120)),
            (json!(" 180 "), Some(180)),
            (json!(120.0), Some(120)),
            (json!(null), None),
            (json!(""), None),
            (json!("   "), None),
        ] {
            assert_eq!(get(v.clone()), Ok(want), "{v}");
        }
        assert_eq!(line_arg(&json!({}), "start_line"), Ok(None));
        for bad in [json!(-3), json!("abc"), json!(12.5), json!("12.5"), json!("-1"), json!(true)] {
            assert_eq!(
                get(bad.clone()),
                Err("start_line must be a line number, counting from 1".to_string()),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_read_carries_the_range_it_asked_for() {
        let r = |args: serde_json::Value| interpret(&call("read_file", args));
        let read = |range| Ok(Request::Read { path: "a.rs".into(), range });
        assert_eq!(r(json!({ "path": "a.rs" })), read(None));
        assert_eq!(r(json!({ "path": "a.rs", "start_line": null, "end_line": "" })), read(None));
        assert_eq!(r(json!({ "path": "a.rs", "start_line": 120, "end_line": 180 })), read(lines(120, Some(180))));
        assert_eq!(
            r(json!({ "path": "a.rs", "start_line": "120", "end_line": " 180 " })),
            read(lines(120, Some(180)))
        );
        assert_eq!(r(json!({ "path": "a.rs", "start_line": 120.0 })), read(lines(120, None)));
        // An end alone is the top of the file down to it; a start of 0 is 1.
        assert_eq!(r(json!({ "path": "a.rs", "end_line": 40 })), read(lines(1, Some(40))));
        assert_eq!(r(json!({ "path": "a.rs", "start_line": 0, "end_line": 5 })), read(lines(1, Some(5))));
        assert_eq!(r(json!({ "path": "a.rs", "start_line": 0 })), read(lines(1, None)));
    }

    #[test]
    fn a_range_that_cannot_be_read_is_refused_with_the_reason() {
        let r = |args: serde_json::Value| interpret(&call("read_file", args)).unwrap_err();
        assert_eq!(
            r(json!({ "path": "a", "start_line": 90, "end_line": 20 })),
            "end_line is the last line to read, not a count"
        );
        assert_eq!(
            r(json!({ "path": "a", "start_line": -1 })),
            "start_line must be a line number, counting from 1"
        );
        assert_eq!(
            r(json!({ "path": "a", "end_line": "abc" })),
            "end_line must be a line number, counting from 1"
        );
        assert_eq!(
            r(json!({ "path": "a", "start_line": 12.5 })),
            "start_line must be a line number, counting from 1"
        );
        assert_eq!(r(json!({ "path": "a", "end_line": 0 })), "end_line must be a line number, counting from 1");
    }

    #[test]
    fn the_read_schema_offers_a_range_and_the_description_says_when() {
        let tool = available(false, None).pop().unwrap();
        let schema = serde_json::to_value(&tool).unwrap();
        let props = &schema["function"]["parameters"]["properties"];
        assert_eq!(props["start_line"]["type"], "integer");
        assert_eq!(props["end_line"]["type"], "integer");
        assert_eq!(schema["function"]["parameters"]["required"], json!(["path"]));
        assert!(tool.function.description.contains(READ_RANGE_SENTENCE));
        assert!(tool.function.description.ends_with("The user may be asked to approve a read."));
    }

    #[test]
    fn a_whole_read_of_a_small_file_is_the_text_and_nothing_else() {
        assert_eq!(read_text("f", b"hello\r\nfrom a tool\r\n", None), "hello\nfrom a tool\n");
        assert_eq!(read_text("f", b"\xEF\xBB\xBFone\r\ntwo", None), "one\ntwo");
        assert_eq!(read_text("f", b"", None), "");
    }

    #[test]
    fn a_whole_read_over_the_limit_gives_the_size_and_the_lines_and_says_to_read_in_parts() {
        let text = "x\n".repeat(40000);
        assert_eq!(
            read_text("big.rs", text.as_bytes(), None),
            "error: big.rs is 80000 bytes and 40000 lines, over the 65536-byte limit for one read. \
             Read it in parts with start_line and end_line; search gives the line numbers."
        );
    }

    #[test]
    fn a_range_comes_back_under_one_header_line_with_the_lines_unnumbered() {
        let text = numbered(100);
        assert_eq!(
            read_text("f", text.as_bytes(), lines(20, Some(22))),
            "[lines 20–22 of 100]\nline 20\nline 21\nline 22\n"
        );
        // No end: to the last line.
        assert_eq!(read_text("f", text.as_bytes(), lines(99, None)), "[lines 99–100 of 100]\nline 99\nline 100\n");
        // A big file is read in parts whatever its size.
        let big = numbered(20000);
        assert!(big.len() > MAX_READ_BYTES as usize);
        let part = read_text("f", big.as_bytes(), lines(15000, Some(15001)));
        assert_eq!(part, "[lines 15000–15001 of 20000]\nline 15000\nline 15001\n");
    }

    #[test]
    fn lines_are_counted_the_way_search_and_edit_count_them() {
        // CRLF and a BOM go; a lone \r is not a line break to either.
        assert_eq!(
            read_text("f", b"\xEF\xBB\xBFone\r\ntwo\r\nthree\r\n", lines(2, Some(2))),
            "[lines 2–2 of 3]\ntwo\n"
        );
        assert_eq!(read_text("f", b"a\rb\nc\n", lines(1, Some(1))), "[lines 1–1 of 2]\na\rb\n");
        // No final newline: the last line is still a line, and is not given one.
        assert_eq!(read_text("f", b"a\nb", lines(2, None)), "[lines 2–2 of 2]\nb");
    }

    #[test]
    fn an_end_past_the_last_line_is_clamped_and_a_start_past_it_is_an_error() {
        let text = numbered(5);
        assert_eq!(
            read_text("f", text.as_bytes(), lines(4, Some(99))),
            "[lines 4–5 of 5]\nline 4\nline 5\n"
        );
        assert_eq!(
            read_text("f", text.as_bytes(), lines(6, None)),
            "error: f has 5 lines; start_line 6 is past the end"
        );
        assert_eq!(read_text("f", b"x\n", lines(2, None)), "error: f has 1 line; start_line 2 is past the end");
        assert_eq!(read_text("f", b"", lines(1, None)), "error: f is empty, so it has no lines to read");
    }

    #[test]
    fn a_range_over_the_limit_is_cut_at_a_line_and_says_where_to_read_on() {
        // 100 lines of 1000 bytes: 65 fit under 65536, a 66th would not.
        let text = format!("{}\n", "x".repeat(999)).repeat(100);
        let out = read_text("f", text.as_bytes(), lines(1, Some(100)));
        let (header, body) = out.split_once('\n').unwrap();
        assert_eq!(header, "[lines 1–65 of 100; cut at 65536 bytes — read on from line 66]");
        assert_eq!(body.len(), 65 * 1000);
        // Starting later, the cut is counted from there.
        let out = read_text("f", text.as_bytes(), lines(50, None));
        assert!(out.starts_with("[lines 50–100 of 100]\n"), "50 lines fit: {}", &out[..40]);
    }

    #[test]
    fn a_single_line_over_the_limit_is_an_error_not_half_a_line() {
        let text = format!("{}\nshort\n", "x".repeat(70000));
        assert_eq!(
            read_text("f", text.as_bytes(), lines(1, Some(2))),
            "error: line 1 of f is 70001 bytes, over the 65536-byte limit for one read"
        );
        assert!(read_text("f", text.as_bytes(), lines(2, Some(2))).ends_with("short\n"));
    }

    #[test]
    fn a_binary_or_utf16_file_is_not_read_as_text_whole_or_in_part() {
        let utf16: Vec<u8> = "fn main() {}\n".encode_utf16().flat_map(u16::to_le_bytes).collect();
        for bytes in [&b"ab\0cd"[..], &utf16[..]] {
            for range in [None, lines(1, Some(1))] {
                assert_eq!(
                    read_text("f", bytes, range),
                    "error: f looks like a binary (or UTF-16) file, so it is not read as text"
                );
            }
        }
    }

    #[test]
    fn a_file_over_a_mebibyte_is_refused_before_it_is_read() {
        let s = Sandbox::new("huge");
        s.write("huge.txt", &"x\n".repeat(MAX_RANGED_SOURCE_BYTES as usize / 2 + 1));
        let real = std::fs::canonicalize(s.dir.join("project/huge.txt")).unwrap();
        for range in [None, lines(1, Some(2))] {
            let out = run_read_file(&real, range);
            assert!(
                out.starts_with("error: ") && out.contains("over the 1048576-byte limit for reading any part"),
                "{out}"
            );
        }
        #[cfg(windows)]
        assert!(read_unasked(&real, lines(1, Some(2))).unwrap().contains("limit for reading any part"));
    }

    #[test]
    fn search_gives_a_line_number_and_reading_that_line_finds_the_text() {
        let s = Sandbox::new("searchread");
        let ws = s.ws();
        let mut text = String::from("\u{feff}");
        for i in 1..=3000 {
            text.push_str(&if i == 2500 {
                "    let marker_x = 1;\r\n".to_string()
            } else {
                format!("    let filler_{i} = {i};\r\n")
            });
        }
        s.write("src/big.rs", &text);
        let found = search::search(&ws, "marker_x", None, || false).unwrap();
        assert!(found.contains("src/big.rs:2500:"), "{found}");
        let real = std::fs::canonicalize(s.dir.join("project/src/big.rs")).unwrap();
        let out = run_read_file(&real, lines(2500, Some(2500)));
        assert_eq!(out, "[lines 2500–2500 of 3000]\n    let marker_x = 1;\n");
    }

    #[cfg(windows)]
    #[test]
    fn the_card_and_the_automatic_read_answer_a_range_identically() {
        let s = Sandbox::new("samerange");
        s.write("a.txt", &numbered(300).replace('\n', "\r\n"));
        let real = std::fs::canonicalize(s.dir.join("project/a.txt")).unwrap();
        for range in [None, lines(10, Some(12)), lines(299, None), lines(500, None)] {
            assert_eq!(read_unasked(&real, range), Some(run_read_file(&real, range)), "{range:?}");
        }
    }

    #[test]
    fn an_approval_carries_the_range_and_a_ranged_read_is_still_automatic_inside_the_folder() {
        let s = Sandbox::new("approvalrange");
        let ws = s.ws();
        s.write("src/main.rs", "fn main() {}\n");
        let range = lines(2, Some(9));
        let req = Request::Read { path: "src/main.rs".into(), range };
        assert_eq!(
            approval(Some(&ws), &req, false),
            Approval::Auto(Auto::Read {
                real: std::fs::canonicalize(s.dir.join("project/src/main.rs")).unwrap(),
                range
            })
        );
        assert_eq!(approval(Some(&ws), &req, true), Approval::Ask);
    }

    #[test]
    fn a_read_header_is_recognised_and_ordinary_text_is_not() {
        assert!(is_read_header("[lines 120–180 of 2140]"));
        assert!(is_read_header("[lines 1–65 of 100; cut at 65536 bytes — read on from line 66]"));
        assert!(is_read_header("  [lines 3–4 of 9]\r"));
        assert!(!is_read_header("let lines = [1, 2];"));
        assert!(!is_read_header("[lines]"));
        assert!(!is_read_header("[lines 3-4]"));
    }
}
