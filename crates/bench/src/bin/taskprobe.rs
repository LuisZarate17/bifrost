//! H3 — can the model do real coding work on odin's own code?
//!
//!   cargo run -p bench --bin taskprobe -- --selfcheck
//!   cargo run -p bench --bin taskprobe
//!   cargo run -p bench --bin taskprobe -- --runs 1 --only b02 --model qwen3-vl:8b-instruct-q4_K_M
//!   cargo run -p bench --bin taskprobe -- --model gpt-oss:20b --think high
//!
//! H1 and H2 measured tools in isolation, on fixtures built to exercise them.
//! This measures the thing those tools exist for: implementing a function or a
//! small module from a written task, scored by odin's own hidden tests — not by
//! a naive edit that happens to break the build.
//!
//! Only 8 of the repo's commits add tests, so this mixes two kinds of task:
//!
//! The tasks name `odin-core` and `odin_core` on purpose: the pinned commits come from before the
//! project was renamed Bifrost, and the code the model works on still uses those names.
//!
//! - **Real commits** (kind A): a detached worktree at `<commit>^`, the task is
//!   to make the change that commit made to one odin-core file, scored by that
//!   commit's own test. Four of them, one per odin-core file touched with tests:
//!   `LoadedModel::pinned` (b38bc86), `tools.rs` itself (e3b43eb), `attachment_path`
//!   (871f297), `config.rs` itself (27f6faf).
//! - **Function removals** (kind B): a detached worktree at the pinned commit
//!   `6653c27`, one function's body replaced with `todo!()`, its own tests
//!   hidden. Twelve of them, across `edit.rs`, `toolcall.rs`, `tools.rs` and
//!   `store.rs`.
//!
//! Each run: hide the scored file's tests (cut at its own `#[cfg(test)]`, and for
//! a removal, `todo!()` the named function first — sanity-checked against the
//! line before and after, so a wrong range fails loudly rather than scoring the
//! wrong code); run the shared probe loop with every Phase 1 tool and
//! `cargo check -p odin-core` offered as the check; then put the reference tests
//! back (the model's file, cut at *its own* `#[cfg(test)]`, plus the solution
//! commit's test tail) and run `cargo test -p odin-core --lib -- <module>::tests::`.
//!
//! It builds in the repo's own `target/` (`CARGO_TARGET_DIR` is `<cwd>/target`),
//! so no other cargo command may run beside it: a wait on cargo's lock counts
//! against the timeouts. A scoring run that times out, is stopped or ends with
//! no exit code is an ERROR, never the model's FAIL (R-10); so is a run that
//! would fail after its own `run_check` timed out.
//!
//! **A fresh `CARGO_TARGET_DIR` rebuilds proc-macros from nothing**, and this
//! machine has refused a freshly linked one before (Smart App Control, PLAN.md
//! §3) — so every run shares the repo's own target directory, and `os error
//! 4551` is retried after touching a source file, up to three times, before
//! being called ERROR rather than FAIL: that result measured Windows, not the
//! model.
//!
//! `--selfcheck` runs no model at all: for every task it checks that the
//! solution state passes scoring and the stripped state does not, which is what
//! proves a task is actually testing what it claims to.
//!
//! **Propose mode (I5, `--mode propose`)** runs a task as `hugin -p` runs one. The task
//! folder is the worktree with the scored file stripped and left uncommitted, as a person's
//! folder would hold unfinished work; the model works in a scratch copy of it
//! (`bifrost_core::scratch`), unattended (`agent::unattended`); and what it did comes back as
//! a patch, which is applied to the task folder and scored there like an interactive run.
//! A run is also booked **NOAPPLY** when its patch does not apply, and **touched** when the
//! task folder — its worktree files and the worktree's own git files, its index among
//! them, down to the time each was last written — is different before the patch is applied
//! than it was when the run began.
//! `--mode both` runs every task both ways and reports the paired difference.
//! `--selfcheck --mode propose` runs no model either: it puts each task's solution through
//! the same copy, patch and apply, and scores the result, which proves that round trip on
//! every task.

#[path = "../probe.rs"]
mod probe;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use bifrost_core::agent::unattended;
use bifrost_core::chat::{Message, Options, Tool};
use bifrost_core::config::Think;
use bifrost_core::edit::Workspace;
use bifrost_core::scratch::{self, Git, Scratch};
use bifrost_core::{check, config, ollama, tools};

use probe::{Check, Setup, Transport};

/// Past the app's own bound: a real task can need more rounds than the app
/// allows, and how many it really needs is exactly what this measures.
const PROBE_ROUNDS: usize = 20;

/// **H8's on/off switch**: the read as it was before ranges — no `start_line` or
/// `end_line` in the schema and no sentence about them in the description — so
/// that one build measures both, which a comparison across builds could not.
/// Taking the sentence out asserts it was there, as findprobe does for its own:
/// a description that changed would otherwise leave the "old" read half new.
fn whole_reads(mut offered: Vec<Tool>) -> Vec<Tool> {
    let read = offered.iter_mut().find(|t| t.function.name == "read_file").expect("read_file is always offered");
    let sentence = format!(" {}", tools::READ_RANGE_SENTENCE);
    assert!(
        read.function.description.contains(&sentence),
        "the range sentence changed; update tools::READ_RANGE_SENTENCE or the way it is stripped"
    );
    read.function.description = read.function.description.replace(&sentence, "");
    if let Some(props) = read.function.parameters.get_mut("properties").and_then(|p| p.as_object_mut()) {
        props.remove("start_line");
        props.remove("end_line");
    }
    offered
}

/// The tools as they were at H8: no `delete_file`, no `move_file`, and `create_file`
/// saying what it said then. `--ops off`, to measure H9 on and off in one build.
///
/// The description matters as much as the list: H9 changed what `create_file` tells
/// the model about folders, and a tool list that still said "folders are made" would
/// not be the one H8 was measured with. (The code underneath still makes them; the
/// model is only not told.)
fn without_ops(mut offered: Vec<Tool>) -> Vec<Tool> {
    offered.retain(|t| !matches!(t.function.name.as_str(), "delete_file" | "move_file"));
    let create = offered.iter_mut().find(|t| t.function.name == "create_file").expect("create_file is offered");
    assert!(
        create.function.description.contains(tools::CREATE_FOLDERS),
        "the folder sentence changed; update tools::CREATE_FOLDERS or the way it is swapped back"
    );
    create.function.description = create
        .function
        .description
        .replace(tools::CREATE_FOLDERS, "The folder it goes in must already exist.");
    offered
}

/// The tools without H10's `update_plan`. `--plan off`, to measure the task list on
/// and off in one build — and what `--ops off` implies, since H8's tool list had
/// no task list either. Left, the list is H9's exactly.
fn without_plan(mut offered: Vec<Tool>) -> Vec<Tool> {
    offered.retain(|t| t.function.name != "update_plan");
    offered
}

/// The pinned commit every removal task starts from.
const PIN: &str = "6653c27";

/// Where the shared crate lives inside a worktree of the pinned commits. The
/// tasks are taken from a time it was still `odin-core`, so this stays the old
/// path; the repo itself now calls it `crates/bifrost-core`.
const CRATE: &str = "crates/odin-core";

// ── the two kinds of task ───────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    RealCommit,
    Removal,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::RealCommit => "real commit",
            Kind::Removal => "removal",
        }
    }
}

/// A function's body, named by the 1-based line range of its **interior** —
/// the lines strictly between the opening `{` and the matching closing `}` —
/// in the file as it stands at [`PIN`]. `indent` is the function's own
/// indentation, in spaces, which the closing brace must match.
struct Removal {
    start: usize,
    end: usize,
    indent: usize,
}

struct Task {
    id: &'static str,
    kind: Kind,
    /// Relative to `crates/odin-core/`.
    file: &'static str,
    /// For the test filter: `cargo test ... -- <module>::tests::`.
    module: &'static str,
    prompt: &'static str,
    /// Kind A: the commit whose parent is the starting worktree, and whose own
    /// tree holds the reference tests. Kind B: [`PIN`] for both.
    commit: &'static str,
    removal: Option<Removal>,
}

fn tasks() -> Vec<Task> {
    vec![
        // ── kind A: real commits ────────────────────────────────────────────
        Task {
            id: "a01-pinned",
            kind: Kind::RealCommit,
            file: "src/ollama.rs",
            module: "ollama",
            commit: "b38bc86",
            removal: None,
            prompt: "In crates/odin-core/src/ollama.rs, add a method `pub fn pinned(&self) -> bool` \
                     to `impl LoadedModel` (the struct that already exists there, with fields \
                     `name: Option<String>`, `model: Option<String>`, `size_vram: Option<u64>` and \
                     `expires_at: Option<String>`).\n\n\
                     Ollama's `/api/ps` reports `expires_at` as an ISO-8601 timestamp. A model loaded \
                     with `keep_alive: -1` (pinned) is reported roughly 290 years in the future; an \
                     ordinary request is reported only minutes out. `pinned()` should read the first \
                     4 characters of `expires_at` as a year, and return true if that year parses as a \
                     number 2200 or greater. Return false when `expires_at` is `None`, empty, or does \
                     not start with a 4-digit year.",
        },
        Task {
            id: "a02-tools",
            kind: Kind::RealCommit,
            file: "src/tools.rs",
            module: "tools",
            commit: "e3b43eb",
            removal: None,
            prompt: "Create crates/odin-core/src/tools.rs — the one local tool the model may ask \
                     for, with an explicit per-call approval by the caller (nothing here decides \
                     that; it only interprets a call and runs an already-approved one). Also add \
                     `pub mod tools;` to crates/odin-core/src/lib.rs.\n\n\
                     In tools.rs:\n\n\
                     - `pub const MAX_READ_BYTES: u64 = 64 * 1024;` — a read over this many bytes is \
                       refused rather than truncated.\n\
                     - `pub fn available() -> Vec<odin_core::chat::Tool>` returning a `Vec` with one \
                       tool, built with `odin_core::chat::Tool::function(name, description, \
                       parameters)`: name `\"read_file\"`, a description saying it reads a text file \
                       from this machine and returns its contents, that it should only be used when \
                       the user refers to a specific file, and that the user must approve each read; \
                       parameters a JSON Schema object with one required string property `\"path\"` \
                       described as the absolute path to the file to read.\n\
                     - `pub struct Requested { pub name: String, pub path: String }`.\n\
                     - `pub fn interpret(call: &odin_core::chat::ToolCall) -> Result<Requested, \
                       String>`: if `call.function.name` is not `\"read_file\"`, return \
                       `Err(format!(\"no such tool: {name}\"))`. Otherwise read the call's `\"path\"` \
                       argument as a string, trim it, and if it is empty return \
                       `Err(\"read_file was called without a path\".to_string())`; otherwise return \
                       `Ok(Requested { name, path })`.\n\
                     - `pub fn run_read_file(path: &str) -> String`: reads the file at `path`. If it \
                       cannot be read, return `format!(\"error: cannot read {path}: {e}\")` with the \
                       underlying `std::io::Error`. If it is a directory, return \
                       `format!(\"error: {path} is a directory, not a file\")`. If it is bigger than \
                       `MAX_READ_BYTES`, return an error saying its size in bytes and the limit. \
                       Otherwise return its contents read lossily as UTF-8 (replacement characters \
                       for anything invalid, not a refusal).",
        },
        Task {
            id: "a03-attachment",
            kind: Kind::RealCommit,
            file: "src/store.rs",
            module: "store",
            commit: "871f297",
            removal: None,
            prompt: "In crates/odin-core/src/store.rs, add:\n\n\
                     - `pub fn attachments_dir() -> Option<PathBuf>` — \
                       `%APPDATA%\\odin\\attachments`, or `None` if `APPDATA` is not set (the same \
                       shape as this file's existing folder helpers).\n\
                     - `fn attachment_path(name: &str) -> Option<PathBuf>` — `None` if `name` is \
                       empty, contains `/`, `\\` or `:`, starts with `.`, or does not have exactly \
                       one `.` in it (attachment names are generated here, never taken from outside, \
                       so a name shaped any other way is refused rather than trusted). Otherwise \
                       `Some(attachments_dir()?.join(name))`.",
        },
        Task {
            id: "a04-config",
            kind: Kind::RealCommit,
            file: "src/config.rs",
            module: "config",
            commit: "27f6faf",
            removal: None,
            prompt: "Create crates/odin-core/src/config.rs — the settings the odin binaries share, \
                     at `%APPDATA%\\odin\\config.json`. Also add `pub mod config;` to \
                     crates/odin-core/src/lib.rs.\n\n\
                     In config.rs:\n\n\
                     - `pub const DEFAULT_MODEL: &str = \"qwen3-vl:8b-instruct-q4_K_M\";`\n\
                     - `pub const DEFAULT_PROMPT: &str = \"What am I looking at? If there's an error \
                       or something broken, explain it and the fix. Otherwise explain what matters \
                       here.\";`\n\
                     - `pub const DEFAULT_NUM_CTX: u32 = 32768;`\n\
                     - `#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, \
                       serde::Deserialize)]` and `#[serde(default)]` on `pub struct Config { pub \
                       model: String, pub prompt: String, pub num_ctx: u32 }` — the `#[serde(default)]` \
                       on the struct matters: a JSON object missing a field should fill that field \
                       from `Config`'s `Default` rather than failing to parse.\n\
                     - `impl Default for Config`, using the three constants above.\n\
                     - `pub fn path() -> Option<PathBuf>` — the config file's path, or `None` if \
                       `APPDATA` is not set. It should return the path whether or not anything is \
                       there yet.\n\
                     - `pub fn load() -> Config` — **cannot fail.** No `APPDATA`, no file, an \
                       unreadable file or malformed JSON should all fall back to `Config::default()`; \
                       only a file that parses returns what it says.\n\
                     - `pub fn save(config: &Config) -> Result<(), String>` — creates the parent \
                       folder if needed, then writes `config` as pretty-printed JSON. A save is \
                       something the user asked for, so unlike `load` this one does return `Result`.",
        },
        // ── kind B: function removals at 6653c27 ────────────────────────────
        Task {
            id: "b01-tidy-deletion",
            kind: Kind::Removal,
            file: "src/edit.rs",
            module: "edit",
            commit: PIN,
            removal: Some(Removal { start: 529, end: 547, indent: 0 }),
            prompt: "In crates/odin-core/src/edit.rs, the function `fn tidy_deletion(before: &str, \
                     after: &str, ended_with_newline: bool) -> String` currently has a body of \
                     `todo!()`. Implement it.\n\n\
                     It joins what is left around a deletion without leaving a gap. `before` and \
                     `after` are the file's text immediately before and after the text that was \
                     removed. If, once leading newlines are stripped, `after` is empty (the deletion \
                     reached the end of the file), return `before` with its trailing newlines \
                     stripped, then re-add exactly one trailing `\\n` if `ended_with_newline` is true \
                     and the result is non-empty — otherwise return it with no trailing newline. \
                     Otherwise, count the newlines trimmed from the end of `before` and from the \
                     start of `after`; if together there are more than 2, that is a run of blank \
                     lines left behind by the deletion, so collapse it to exactly one blank line \
                     between the trimmed `before` and the trimmed `after` (i.e. \
                     `format!(\"{}\\n\\n{}\", before.trim_end_matches('\\n'), \
                     after.trim_start_matches('\\n'))`). Otherwise, the join was already clean: \
                     return `before` and `after` concatenated exactly as given.",
        },
        Task {
            id: "b02-text-parse",
            kind: Kind::Removal,
            file: "src/edit.rs",
            module: "edit",
            commit: PIN,
            removal: Some(Removal { start: 262, end: 290, indent: 4 }),
            prompt: "In crates/odin-core/src/edit.rs, inside `impl Text`, the method `fn \
                     parse(bytes: &[u8]) -> Result<Self, String>` currently has a body of `todo!()`. \
                     Implement it. (`Text { body: String, crlf: bool, bom: bool }`, `const BOM: \
                     &[u8] = b\"\\xEF\\xBB\\xBF\"` and `pub const MAX_EDIT_BYTES: usize = 1024 * \
                     1024` already exist above it in the file.)\n\n\
                     It takes a file apart into what a model sees and what has to be put back. If \
                     `bytes.len()` is over `MAX_EDIT_BYTES`, return an error saying the file's size \
                     and the limit. Strip a leading UTF-8 BOM if present, remembering whether there \
                     was one. If what remains contains a `0` byte, return an error saying it looks \
                     like a binary file. If it is not valid UTF-8, return an error saying so. Count \
                     `\\r\\n` occurrences and plain `\\n` occurrences: if there is at least one \
                     `\\r\\n` and the two counts differ, the file mixes line endings that could not \
                     be preserved faithfully, so return an error saying that. Otherwise return \
                     `Ok(Self { body, crlf, bom })` with `body` being the text with every `\\r\\n` \
                     replaced by `\\n`, `crlf` true if there was at least one `\\r\\n`, and `bom` \
                     true if a BOM was stripped.",
        },
        Task {
            id: "b03-contain",
            kind: Kind::Removal,
            file: "src/edit.rs",
            module: "edit",
            commit: PIN,
            removal: Some(Removal { start: 150, end: 196, indent: 4 }),
            prompt: "In crates/odin-core/src/edit.rs, inside `impl Workspace`, the method `fn \
                     contain_with(&self, real: &Path, requested: &str, app_data: Option<&Path>) -> \
                     Result<(), String>` currently has a body of `todo!()`. Implement it. (`real` is \
                     already canonicalised, so `..`, symlinks and junctions have all been resolved to \
                     where they actually point; helper functions `clean(requested: &str) -> String` \
                     and `fn decides_what_the_check_runs(inside: &Path) -> bool` already exist in \
                     this file, and `self.root_display()` and `self.root: PathBuf`/`self.root()` are \
                     available.)\n\n\
                     Refuse (each with a clear message naming `clean(requested)`, built with \
                     `format!`) when:\n\n\
                     - `real` does not start with `self.root()` — outside the project folder. \
                       Message: `\"{} is outside the project folder ({}). Only files inside it can be \
                       changed.\"` with `clean(requested)` and `self.root_display()`.\n\
                     - any path component inside the workspace (i.e. of `real.strip_prefix(&self.root)`, \
                       falling back to `real` if that fails) case-insensitively equals `.git`. \
                       Message: `\"{} is inside .git, which is never edited\"`.\n\
                     - any such component's name contains a `:` (an NTFS alternate data stream). \
                       Message: `\"{} names a file stream, which is never edited\"`.\n\
                     - `decides_what_the_check_runs` of that inside path is true. Message: `\"{} \
                       decides what the check command runs, so it is never edited\"`.\n\
                     - `app_data` is `Some(d)` and `real` starts with `d`. Message: `\"{} is inside \
                       Mímir's own settings folder, which is never edited\"`.\n\n\
                     Otherwise return `Ok(())`.",
        },
        Task {
            id: "b04-undo",
            kind: Kind::Removal,
            file: "src/edit.rs",
            module: "edit",
            commit: PIN,
            removal: Some(Removal { start: 705, end: 737, indent: 0 }),
            prompt: "In crates/odin-core/src/edit.rs, the function `pub fn undo(id: &str, backups: \
                     &Path) -> Result<String, String>` currently has a body of `todo!()`. Implement \
                     it. (A private `struct Record { path: PathBuf, display: String, create: bool, \
                     after: u64 }` (Serialize/Deserialize), `fn fingerprint(bytes: &[u8]) -> u64` and \
                     `fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String>` already exist \
                     in this file.)\n\n\
                     Puts a file back the way it was before an edit whose id is `id`. If `id` is \
                     empty or has any non-hex-digit character, return `Err(format!(\"not an edit id: \
                     {id}\"))`. Read `backups.join(format!(\"{id}.json\"))` as a `Record`; if it \
                     cannot be read, return `Err(\"that edit has already been undone, or was never \
                     made\".to_string())`. Read the file at `record.path`. If it cannot be read at \
                     all, return an error saying the file no longer exists so there is nothing to \
                     undo. If it can be read but its `fingerprint` does not equal `record.after`, \
                     return an error saying it has changed since the edit, so undoing it would lose \
                     those changes, and that it was left as is — do not touch the file. Otherwise: if \
                     `record.create` is true, delete `record.path`; otherwise read \
                     `backups.join(format!(\"{id}.bak\"))` and `write_atomic` it back to \
                     `record.path`. Either way, then remove the `.json` and `.bak` backup files \
                     (best effort — ignore errors removing them) and return \
                     `Ok(record.display)`.",
        },
        Task {
            id: "b05-render-chatml",
            kind: Kind::Removal,
            file: "src/toolcall.rs",
            module: "toolcall",
            commit: PIN,
            removal: Some(Removal { start: 89, end: 139, indent: 0 }),
            prompt: "In crates/odin-core/src/toolcall.rs, the function `pub fn \
                     render_chatml(messages: &[odin_core::chat::Message], tools: \
                     &[odin_core::chat::Tool]) -> String` currently has a body of `todo!()`. \
                     Implement it. (`pub fn tools_section(tools: &[Tool]) -> String` and `pub fn \
                     spaced(v: &serde_json::Value) -> String` already exist in this file and are what \
                     you should use for the tool section and for rendering a tool call's arguments.)\n\n\
                     Renders a conversation as the raw prompt Qwen's chat template produces, ending \
                     with the assistant's turn open for the model to write:\n\n\
                     - For the first `\"system\"` message only, write \
                       `<|im_start|>system\\n{content}\\n\\n{tools_section}<|im_end|>\\n` — if no \
                       message has role `\"system\"`, insert `<|im_start|>system\\n{tools_section}<|im_end|>\\n` \
                       at the very start instead.\n\
                     - For an `\"assistant\"` message: start with its trimmed content; for each of \
                       its `tool_calls` (if any), append (with a `\\n` first if there is already \
                       text) `<tool_call>\\n{\"name\": \"<name>\", \"arguments\": <args>}\\n</tool_call>` \
                       where `<args>` is the call's arguments rendered with `spaced` if they are not \
                       already a JSON string, or the string itself if they are. Wrap the whole thing \
                       as `<|im_start|>assistant\\n{text}<|im_end|>\\n`.\n\
                     - For a `\"tool\"` message: `<|im_start|>user\\n<tool_response>\\n{content with \
                       trailing newlines trimmed}\\n</tool_response><|im_end|>\\n`.\n\
                     - For any other role (including a second `\"system\"` message): if the message \
                       has images, prepend a line `[{n} image(s) attached here, not shown in this \
                       retry]\\n` before its content (images cannot ride along in raw mode); a \
                       `\"system\"` role here is written as `\"user\"`. Wrap as \
                       `<|im_start|>{role}\\n{note}{content}<|im_end|>\\n`.\n\n\
                     Finally append `<|im_start|>assistant\\n`.",
        },
        Task {
            id: "b06-extract-calls",
            kind: Kind::Removal,
            file: "src/toolcall.rs",
            module: "toolcall",
            commit: PIN,
            removal: Some(Removal { start: 148, end: 171, indent: 0 }),
            prompt: "In crates/odin-core/src/toolcall.rs, the function `pub fn \
                     extract_calls(content: &str, tools: &[odin_core::chat::Tool]) -> (String, \
                     Vec<odin_core::chat::ToolCall>, Vec<String>)` currently has a body of `todo!()`. \
                     Implement it. (`pub fn parse_call(block: &str, tools: &[Tool]) -> \
                     Result<ToolCall, String>` already exists in this file.)\n\n\
                     Finds every `<tool_call>...</tool_call>` block in `content`. Returns three \
                     things: the text with every such block removed (trimmed), the calls that parsed \
                     successfully (via `parse_call`, passed each block's inner text and `tools`), and \
                     a description of every block that could not be parsed (`parse_call`'s `Err`), in \
                     the order they appeared. A block with no closing `</tool_call>` tag still counts \
                     — take everything after the opening tag to the end of `content` as its body.",
        },
        Task {
            id: "b07-repair",
            kind: Kind::Removal,
            file: "src/toolcall.rs",
            module: "toolcall",
            commit: PIN,
            removal: Some(Removal { start: 276, end: 344, indent: 0 }),
            prompt: "In crates/odin-core/src/toolcall.rs, the function `fn repair(block: &str, tools: \
                     &[odin_core::chat::Tool]) -> Result<odin_core::chat::ToolCall, String>` currently \
                     has a body of `todo!()`. Implement it. (Helper functions `fn skip_ws(s: &[u8], i: \
                     usize) -> usize`, `fn find_key(s: &str, key: &str, from: usize) -> Option<(usize, \
                     usize)>` (returns where a `\"key\"` starts and where its value's content starts, \
                     for an occurrence where the key can actually be — preceded, ignoring whitespace, \
                     by `{` or `,`, followed by `:` then a `\"`), `fn arg_names<'a>(tools: &'a [Tool], \
                     name: &str) -> Option<Vec<&'a str>>` (a tool's argument names from its JSON \
                     schema) and `fn unescape_tolerant(raw: &str) -> String` already exist in this \
                     file.)\n\n\
                     This recovers a tool call from JSON Ollama's own parser rejected — most often \
                     because the model left a `\"` inside a string value un-escaped. Find the `name` \
                     key with `find_key(block, \"name\", 0)`; read the name as the text up to the \
                     next `\"`. Look up its argument names with `arg_names`; if the tool is unknown, \
                     fail with a message naming it. Find the `\"arguments\"` key's `{` to know where \
                     the arguments object starts. For each of the tool's argument names, find it \
                     with `find_key` starting from that `{`; sort what is found by where the key \
                     starts, so arguments are read in the order they appear in the text — if none are \
                     found, fail. Then, for each found argument in order, its value runs from its \
                     value-start to just before the closing quote that precedes either the next \
                     argument's key (preceded by a `,`, skipping whitespace) or, for the last \
                     argument, the call's closing `}}`  (two closing braces, skipping whitespace \
                     before each) — fail with a clear reason if that shape is not there. Unescape the \
                     raw text between those quotes with `unescape_tolerant` and store it as a string \
                     under that argument's key. Every failure message should be built through a \
                     closure like `|why: &str| format!(\"the tool call could not be read ({why}): \
                     {preview}\", preview = block.chars().take(160).collect::<String>())`. Return \
                     `Ok(ToolCall { id: None, function: FunctionCall { name, arguments: \
                     Value::Object(args) } })`.",
        },
        Task {
            id: "b08-unescape",
            kind: Kind::Removal,
            file: "src/toolcall.rs",
            module: "toolcall",
            commit: PIN,
            removal: Some(Removal { start: 239, end: 272, indent: 0 }),
            prompt: "In crates/odin-core/src/toolcall.rs, the function `fn \
                     unescape_tolerant(raw: &str) -> String` currently has a body of `todo!()`. \
                     Implement it.\n\n\
                     Undoes JSON string escapes, leaving everything else — including a bare, \
                     un-escaped `\"` — exactly as it is (that is the whole point: the text being read \
                     is not valid JSON). Walk `raw` character by character. A character that is not \
                     `\\` is copied through unchanged. A `\\` followed by `\"`, `\\`, `/`, `n`, `t`, \
                     `r`, `b` or `f` becomes the character it escapes (`\\n` → a newline, and so on — \
                     `\\b` is backspace `\\u{8}` and `\\f` is form feed `\\u{c}`). A `\\u` followed by \
                     4 hex digits becomes the Unicode character they encode, or, if they do not parse \
                     as a valid code point, is copied through literally as `\\u` plus those four \
                     characters. A `\\` followed by anything else is copied through as both \
                     characters unchanged. A `\\` at the very end of the string (nothing follows it) \
                     is copied through alone.",
        },
        Task {
            id: "b09-prior-failures",
            kind: Kind::Removal,
            file: "src/tools.rs",
            module: "tools",
            commit: PIN,
            removal: Some(Removal { start: 373, end: 390, indent: 0 }),
            prompt: "In crates/odin-core/src/tools.rs, the function `pub fn \
                     prior_failures(history: &[odin_core::chat::Message], call: \
                     &odin_core::chat::ToolCall) -> usize` currently has a body of `todo!()`. \
                     Implement it.\n\n\
                     Counts how many times, since the last message with role `\"user\"` in \
                     `history` (or from the start, if there is none), an assistant message asked for \
                     exactly this call (same `function.name` and same `function.arguments`) and the \
                     very next message was a `\"tool\"` result whose content, trimmed at the start, \
                     begins with `\"error:\"`. Only look within that slice from after the last user \
                     message onward.",
        },
        Task {
            id: "b10-interpret",
            kind: Kind::Removal,
            file: "src/tools.rs",
            module: "tools",
            commit: PIN,
            removal: Some(Removal { start: 267, end: 299, indent: 0 }),
            prompt: "In crates/odin-core/src/tools.rs, the function `pub fn \
                     interpret(call: &odin_core::chat::ToolCall) -> Result<Request, String>` \
                     currently has a body of `todo!()`. Implement it. (The `Request` enum already \
                     exists in this file with variants `Read { path: String }`, `Edit { path: \
                     String, old_text: String, new_text: String }`, `Create { path: String, content: \
                     String }`, `List { pattern: Option<String> }`, `Search { text: String, pattern: \
                     Option<String> }`, `Check`, and `Rewrite { path: String, content: String }`. \
                     Helper functions `fn arg(call: &ToolCall, key: &str) -> Option<String>` and `fn \
                     optional(call: &ToolCall, key: &str) -> Option<String>` (like `arg`, but an \
                     empty or blank trimmed value is treated as `None`) already exist.)\n\n\
                     Read `call.function.name`. Read its `\"path\"` argument, trimmed (empty if \
                     absent); if that is empty and the tool name is `read_file`, `edit_file`, \
                     `create_file` or `write_file`, fail with `format!(\"{name} was called without a \
                     path\")`. Then, by name: `\"read_file\"` → `Request::Read { path }`. \
                     `\"edit_file\"` → `Request::Edit { path, old_text: arg(call, \"old_text\").ok_or(\"edit_file \
                     was called without old_text\")?, new_text: arg(call, \"new_text\").unwrap_or_default() }` \
                     (a missing `new_text` means a deletion, not an error). `\"create_file\"` → \
                     `Request::Create { path, content: arg(call, \"content\").ok_or(\"create_file \
                     was called without content\")? }`. `\"write_file\"` → `Request::Rewrite { path, \
                     content: arg(call, \"content\").ok_or(\"write_file was called without \
                     content\")? }`. `\"list_files\"` → `Request::List { pattern: optional(call, \
                     \"pattern\") }`. `\"search\"` → `Request::Search { text: arg(call, \"text\").filter(|t| \
                     !t.is_empty()).ok_or(\"search was called without text\")?, pattern: \
                     optional(call, \"pattern\") }` (note: the text itself is not trimmed — only \
                     checked for being non-empty — since leading spaces can be exactly what is being \
                     searched for). `\"run_check\"` → `Request::Check` (any arguments the model sent \
                     are ignored). Anything else → `Err(format!(\"no such tool: {other}\"))`.",
        },
        Task {
            id: "b11-build-files",
            kind: Kind::Removal,
            file: "src/tools.rs",
            module: "tools",
            commit: PIN,
            removal: Some(Removal { start: 402, end: 429, indent: 0 }),
            prompt: "In crates/odin-core/src/tools.rs, the function `pub fn \
                     build_files_changed(history: &[odin_core::chat::Message]) -> Vec<String>` \
                     currently has a body of `todo!()`. Implement it. (`fn arg(call: &ToolCall, key: \
                     &str) -> Option<String>` already exists in this file.)\n\n\
                     Looks, since the last `\"user\"` message in `history` (or from the start if \
                     there is none), for every `\"assistant\"` message whose first tool call is \
                     `edit_file` or `create_file`, where the very next message is a `\"tool\"` result \
                     whose content starts with `\"Edited \"` or `\"Created \"` (proof that something \
                     was actually written — not a denial or a refusal). For each such call, read its \
                     `\"path\"` argument, trim it, trim matching `\"`, `'` or `` ` `` quotes from around \
                     it and trim again, then take the part after the last `/` or `\\` and lower-case \
                     it. If that lower-cased file name is exactly `build.rs`, `cargo.toml` or \
                     `package.json`, and the original (untrimmed-of-quotes... i.e. the cleaned, \
                     trimmed) path is not already in the result, push it. Return the paths in the \
                     order their edits happened, each listed once.",
        },
        Task {
            id: "b12-display-title",
            kind: Kind::Removal,
            file: "src/store.rs",
            module: "store",
            commit: PIN,
            removal: Some(Removal { start: 130, end: 147, indent: 4 }),
            prompt: "In crates/odin-core/src/store.rs, inside `impl Conversation`, the method `pub \
                     fn display_title(&self) -> String` currently has a body of `todo!()`. Implement \
                     it. (`Conversation` has a `title: String` field and a `messages: Vec<Message>` \
                     field, where each `Message` has a `role: String` and a `content: String`.)\n\n\
                     If `self.title` is not empty, return a clone of it. Otherwise, find the first \
                     message whose role is `\"user\"` and take its content, trimmed (empty string if \
                     there is no such message). If that is empty, return `\"New conversation\"`. \
                     Otherwise take its first line, up to 60 characters; if the line has more than \
                     60 characters, append `…` after those 60.",
        },
    ]
}

// ── stripping and restoring ─────────────────────────────────────────────────

/// Everything from the first line whose trim is exactly `#[cfg(test)]` to the
/// end, if there is one.
fn test_tail(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let i = lines.iter().position(|l| l.trim() == "#[cfg(test)]")?;
    let mut tail = lines[i..].join("\n");
    tail.push('\n');
    Some(tail)
}

/// `text` with its test module (if any) cut off.
fn cut_tests(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    match lines.iter().position(|l| l.trim() == "#[cfg(test)]") {
        Some(0) => String::new(),
        Some(i) => {
            let mut kept = lines[..i].join("\n");
            kept.push('\n');
            kept
        }
        None => {
            if text.is_empty() || text.ends_with('\n') { text.to_string() } else { format!("{text}\n") }
        }
    }
}

/// Replace a function's body with `todo!()`, sanity-checked against the line
/// before and after the named range: a wrong range must fail loudly, not
/// silently score the wrong code.
fn apply_removal(text: &str, r: &Removal) -> Result<String, String> {
    let lines: Vec<&str> = text.lines().collect();
    let before = *lines.get(r.start.checked_sub(2).ok_or("removal range starts too early")?).ok_or("removal start out of range")?;
    let after = *lines.get(r.end).ok_or("removal end out of range")?;
    if !before.trim_end().ends_with('{') {
        return Err(format!(
            "removal range is wrong: line {} does not end with `{{`: {before:?}",
            r.start - 1
        ));
    }
    let want_close = format!("{}}}", " ".repeat(r.indent));
    if after != want_close {
        return Err(format!(
            "removal range is wrong: line {} is not `{want_close}`: {after:?}",
            r.end + 1
        ));
    }
    let mut out: Vec<String> = lines[..r.start - 1].iter().map(|s| s.to_string()).collect();
    out.push(format!("{}todo!()", " ".repeat(r.indent + 4)));
    out.extend(lines[r.end..].iter().map(|s| s.to_string()));
    let mut joined = out.join("\n");
    joined.push('\n');
    Ok(joined)
}

/// What a run starts from: the scored file, tests hidden and (for a removal)
/// its function replaced with `todo!()`.
fn strip(original: &str, removal: &Option<Removal>) -> Result<String, String> {
    let body = match removal {
        Some(r) => apply_removal(original, r)?,
        None => original.to_string(),
    };
    Ok(cut_tests(&body))
}

/// What is scored: the model's own file (or nothing, if it never wrote one),
/// cut at *its own* `#[cfg(test)]`, with the solution's reference tests
/// appended.
fn restore(model_text: Option<&str>, solution_text: &str) -> String {
    let base = cut_tests(model_text.unwrap_or(""));
    let tail = test_tail(solution_text).unwrap_or_default();
    format!("{base}{tail}")
}

// ── the worktree ─────────────────────────────────────────────────────────────

/// A detached worktree, removed on drop unless `keep` is set.
struct Worktree {
    path: PathBuf,
    keep: bool,
}

impl Worktree {
    fn add(at: &Path, treeish: &str, keep: bool) -> Result<Self, String> {
        let _ = Command::new("git").args(["worktree", "prune"]).status();
        let _ = std::fs::remove_dir_all(at);
        if let Some(parent) = at.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let out = Command::new("git")
            .args(["worktree", "add", "--detach"])
            .arg(at)
            .arg(treeish)
            .output()
            .map_err(|e| format!("could not run git: {e}"))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).into_owned());
        }
        Ok(Self { path: at.to_path_buf(), keep })
    }
}

impl Drop for Worktree {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        let _ = Command::new("git").args(["worktree", "remove", "--force"]).arg(&self.path).status();
        let _ = Command::new("git").args(["worktree", "prune"]).status();
    }
}

/// `git show <treeish>:<path>` from the repo's own git, not the worktree's —
/// reading history does not need the worktree to exist.
fn show(treeish: &str, path: &str) -> Result<String, String> {
    let out = Command::new("git")
        .args(["show", &format!("{treeish}:{path}")])
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    Ok(String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"))
}

/// Touch a source file so the next build sees it as changed — the fix for a
/// Smart App Control block on a freshly linked binary.
fn touch(worktree: &Path) {
    let p = worktree.join(CRATE).join("src/lib.rs");
    if let Ok(bytes) = std::fs::read(&p) {
        let _ = std::fs::write(&p, bytes);
    }
}

// ── scoring ──────────────────────────────────────────────────────────────────

enum TestOutcome {
    Pass(usize),
    NoCompile(String),
    Failed(String),
    ZeroRan,
}

fn judge_tests(output: &str) -> TestOutcome {
    if let Some(at) = output.find("test result: ok. ") {
        let rest = &output[at + "test result: ok. ".len()..];
        let n: usize = rest.split(' ').next().and_then(|s| s.parse().ok()).unwrap_or(0);
        return if n > 0 { TestOutcome::Pass(n) } else { TestOutcome::ZeroRan };
    }
    if output.contains("test result: FAILED") {
        let names: Vec<&str> = output
            .lines()
            .filter(|l| l.trim_start().starts_with("test ") && l.trim_end().ends_with("FAILED"))
            .collect();
        return TestOutcome::Failed(if names.is_empty() {
            "some tests failed".to_string()
        } else {
            names.join(", ")
        });
    }
    let first = output.lines().find(|l| l.trim_start().starts_with("error")).unwrap_or("no `test result:` line");
    TestOutcome::NoCompile(first.to_string())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Pass,
    Fail,
    Error,
}

impl Verdict {
    fn label(self) -> &'static str {
        match self {
            Verdict::Pass => "PASS",
            Verdict::Fail => "FAIL",
            Verdict::Error => "ERROR",
        }
    }
}

/// Why a scoring run says nothing about the model's code, if it does not: it
/// hit its timeout, was stopped, or ended with no exit code (R-10). Waiting on
/// cargo's lock for the shared `target/`, a locked output file or a full disk
/// all look like this, and none of them is the model's doing. `None` means the
/// run finished by itself and its output can be judged.
fn unfinished(o: &check::Outcome) -> Option<String> {
    match &o.ended {
        check::Ended::TimedOut => Some(format!("the scoring test run timed out after {:.0}s — environment", o.elapsed.as_secs_f64())),
        check::Ended::Stopped => Some("the scoring test run was stopped — environment".to_string()),
        check::Ended::DiskFull => Some("the scoring test run was stopped for a nearly full disk — environment".to_string()),
        check::Ended::Exited(None) => Some("the scoring test run ended with no exit code — environment".to_string()),
        check::Ended::Exited(Some(_)) => None,
    }
}

/// A run that would be a FAIL, but whose own `run_check` never finished, is an
/// ERROR: the model was working with a check that told it nothing, and that is
/// the environment's failure. A PASS stays a PASS — it got there anyway.
fn book_with_check_timeouts(verdict: Verdict, why: String, check_timeouts: usize) -> (Verdict, String) {
    if verdict == Verdict::Fail && check_timeouts > 0 {
        (Verdict::Error, format!("the model's check timed out — environment ({why})"))
    } else {
        (verdict, why)
    }
}

/// Run the scoring test command in `worktree`, retrying past a Smart App
/// Control block up to twice more by touching a source file.
fn score(worktree: &Path, module: &str, target_dir: &Path, appdata: &Path) -> (Verdict, String) {
    let argv = vec!["cargo".to_string(), "test".to_string(), "-p".to_string(), "odin-core".to_string(), "--lib".to_string(), "--".to_string(), format!("{module}::tests::")];
    let env = vec![
        ("CARGO_TARGET_DIR".to_string(), target_dir.to_string_lossy().into_owned()),
        ("CARGO_NET_OFFLINE".to_string(), "true".to_string()),
        ("APPDATA".to_string(), appdata.to_string_lossy().into_owned()),
        ("CARGO_TERM_COLOR".to_string(), "never".to_string()),
    ];
    for attempt in 0..3 {
        match check::run(worktree, &argv, check::TEST_TIMEOUT, &env, || false) {
            Err(e) => return (Verdict::Error, e),
            Ok(o) if o.blocked.is_some() => {
                if attempt < 2 {
                    touch(worktree);
                    continue;
                }
                return (Verdict::Error, "blocked by Smart App Control".to_string());
            }
            Ok(o) => {
                if let Some(why) = unfinished(&o) {
                    return (Verdict::Error, why);
                }
                return match judge_tests(&o.output) {
                    TestOutcome::Pass(n) => (Verdict::Pass, format!("{n} passed")),
                    TestOutcome::ZeroRan => (Verdict::Fail, "zero tests ran".to_string()),
                    TestOutcome::Failed(names) => (Verdict::Fail, format!("failed: {names}")),
                    TestOutcome::NoCompile(first) => (Verdict::Fail, format!("does not compile: {first}")),
                };
            }
        }
    }
    (Verdict::Error, "blocked by Smart App Control".to_string())
}

// ── selfcheck ────────────────────────────────────────────────────────────────

fn selfcheck(tasks: &[Task], scratch_root: &Path, target_dir: &Path, keep: bool) -> bool {
    println!("H3 — task probe selfcheck (no model)\n");
    println!("| task | kind | solution passes? | stripped fails? | test count |");
    println!("|---|---|---|---|---|");
    let mut all_ok = true;
    for task in tasks {
        let appdata = scratch_root.join(format!("appdata-{}", task.id));
        let path = format!("{CRATE}/{}", task.file);

        // (a) the solution: the tree at the commit that made this change, tests
        // as they really are.
        let sol_dir = scratch_root.join(format!("sol-{}", task.id));
        let (sol_ok, sol_note) = match Worktree::add(&sol_dir, task.commit, keep) {
            Err(e) => (false, format!("could not check out: {e}")),
            Ok(_tree) => {
                let (v, why) = score(&sol_dir, task.module, target_dir, &appdata);
                (v == Verdict::Pass, why)
            }
        };

        // (b) stripped: the run's own starting point, with the model's part left
        // out entirely.
        let start = match task.kind {
            Kind::RealCommit => format!("{}^", task.commit),
            Kind::Removal => task.commit.to_string(),
        };
        let strip_dir = scratch_root.join(format!("strip-{}", task.id));
        let (strip_fails, strip_note) = match Worktree::add(&strip_dir, &start, keep) {
            Err(e) => (false, format!("could not check out: {e}")),
            Ok(tree) => {
                let original = std::fs::read_to_string(tree.path.join(&path)).unwrap_or_default();
                match strip(&original, &task.removal) {
                    Err(e) => (false, e),
                    Ok(stripped) => {
                        let solution_text = match show(task.commit, &path) {
                            Ok(t) => t,
                            Err(e) => {
                                all_ok = false;
                                println!(
                                    "| {} | {} | — | — | could not read solution text: {e} |",
                                    task.id,
                                    task.kind.name()
                                );
                                continue;
                            }
                        };
                        let scored = restore(Some(&stripped), &solution_text);
                        let _ = std::fs::write(tree.path.join(&path), &scored);
                        let (v, why) = score(&tree.path, task.module, target_dir, &appdata);
                        // Only a real FAIL counts (R-35): an ERROR (blocked build,
                        // timeout, no exit code) says nothing about the stripped
                        // state, and "not a pass" used to accept it as proof.
                        (v == Verdict::Fail, why)
                    }
                }
            }
        };

        let ok = sol_ok && strip_fails;
        all_ok &= ok;
        println!(
            "| {} | {} | {} ({}) | {} ({}) | — |",
            task.id,
            task.kind.name(),
            if sol_ok { "yes" } else { "NO" },
            sol_note,
            if strip_fails { "yes" } else { "NO" },
            strip_note,
        );
    }
    println!();
    if all_ok {
        println!("every kept task is OK.");
    } else {
        println!("BROKEN TASKS ABOVE — fix or drop them before running the probe.");
    }
    all_ok
}

// ── running one task ─────────────────────────────────────────────────────────

struct Outcome {
    verdict: Verdict,
    why: String,
    rounds: usize,
    late: bool,
    checks: usize,
    /// Calls that would have shown a card under the app's policy, and under
    /// the one before H5 (every call).
    asks: usize,
    asks_every_call: usize,
    calls: Vec<String>,
    files: Vec<String>,
    secs: f64,
    log: Vec<String>,
    /// Reads that named a line range. Every file here is under 64 KB, so each is
    /// a file the model chose to page rather than read whole.
    ranged_reads: usize,
    /// Ollama's prompt token count for each request, the largest of them, how
    /// many things were set aside (H7), and the rounds where the prompt shrank
    /// with nothing set aside — Ollama cutting it to fit `num_ctx`.
    prompt_tokens: Vec<u32>,
    max_prompt: u32,
    set_aside: usize,
    truncated: usize,
    /// H9: delete_file and move_file calls, and creates of a path the run deleted.
    deletes: usize,
    moves: usize,
    recreated: usize,
    /// H10: `update_plan` calls the loop accepted.
    plans: usize,
    /// R-03: automatic reads, listings and searches that succeeded and cost no round.
    autos: usize,
    /// I5, propose mode: the patch did not apply to the task folder.
    noapply: bool,
    /// I5, propose mode: the task folder was different when the run ended, before the patch
    /// was applied, than when it began.
    touched: bool,
}

impl Outcome {
    /// A run that failed before the model was asked anything: an ERROR with its reason, and
    /// nothing counted.
    fn early(started: Instant, why: String) -> Self {
        Outcome {
            verdict: Verdict::Error,
            why,
            rounds: 0,
            late: false,
            checks: 0,
            asks: 0,
            asks_every_call: 0,
            calls: Vec::new(),
            files: Vec::new(),
            secs: started.elapsed().as_secs_f64(),
            log: Vec::new(),
            ranged_reads: 0,
            prompt_tokens: Vec::new(),
            max_prompt: 0,
            set_aside: 0,
            truncated: 0,
            deletes: 0,
            moves: 0,
            recreated: 0,
            plans: 0,
            autos: 0,
            noapply: false,
            touched: false,
        }
    }
}

fn run(
    model: &str,
    task: &Task,
    opts: Options,
    scratch: &Path,
    target_dir: &Path,
    keep: bool,
    // `--compact` and `--read`: whether results are set aside (H7) and whether
    // the read offers ranges (H8).
    // `--ops`: whether delete_file and move_file are offered (H9).
    // `--plan`: whether update_plan is (H10).
    (compact, parts, ops, plan): (bool, bool, bool, bool),
) -> Outcome {
    let started = Instant::now();
    let start = match task.kind {
        Kind::RealCommit => format!("{}^", task.commit),
        Kind::Removal => task.commit.to_string(),
    };
    let wt_path = scratch.join("wt");
    let backups = scratch.join("backups");
    let appdata = scratch.join("appdata");
    let path = format!("{CRATE}/{}", task.file);

    let tree = match Worktree::add(&wt_path, &start, keep) {
        Ok(t) => t,
        Err(e) => return Outcome::early(started, format!("could not check out {start}: {e}")),
    };
    let ws = match Workspace::open(&tree.path) {
        Ok(w) => w,
        Err(e) => return Outcome::early(started, e),
    };

    let full_path = tree.path.join(&path);
    let original = std::fs::read_to_string(&full_path).unwrap_or_default();
    let stripped = match strip(&original, &task.removal) {
        Ok(s) => s,
        Err(e) => return Outcome::early(started, e),
    };
    // a02 and a04 create a file that does not exist at the parent commit —
    // leave it absent so the model has to use create_file, not edit_file.
    if !original.is_empty() || full_path.exists() {
        if let Some(parent) = full_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&full_path, &stripped);
    }

    let argv = vec!["cargo".to_string(), "check".to_string(), "-p".to_string(), "odin-core".to_string()];
    let cargo = Check {
        argv,
        env: vec![
            ("CARGO_TARGET_DIR".to_string(), target_dir.to_string_lossy().into_owned()),
            ("CARGO_NET_OFFLINE".to_string(), "true".to_string()),
        ],
    };
    let offered = tools::available(true, Some(&check::label(&cargo.argv)));
    let offered = if parts { offered } else { whole_reads(offered) };
    let offered = if ops { offered } else { without_ops(offered) };
    // `--ops off` is H8's list, which had no task list.
    let offered = if plan && ops { offered } else { without_plan(offered) };
    let mut messages =
        vec![Message::system(tools::system_prompt(&ws, true)), Message::user(task.prompt)];
    // Reads stay in the worktree. It sits on the same disk as the repo it came
    // from, whose files hold every reference solution — and its `.git` file
    // names where that repo is.
    //
    // A task that stubs out a safety function (b03's `contain_with`) stubs the
    // worktree's copy only: the tools the model calls run on the bifrost-core this
    // probe was built with, whose rules are intact.
    let setup = Setup { ws: &ws, backups: &backups, check: Some(&cargo), confine_reads: true, compact, commands: None, unattended: false };
    let r = probe::run_loop(model, &mut messages, &offered, opts, Transport::Recover, PROBE_ROUNDS, &setup);

    let files = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&tree.path)
        .output()
        .ok()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let check_timeouts = r.check_timeouts;
    let mut out = Outcome {
        verdict: Verdict::Fail,
        why: String::new(),
        rounds: r.rounds,
        late: r.rounds > tools::MAX_ROUNDS,
        checks: r.checks.len(),
        asks: r.asks,
        asks_every_call: r.asks_every_call,
        calls: r.calls,
        files,
        secs: 0.0,
        log: r.log,
        ranged_reads: r.ranged_reads,
        max_prompt: r.prompt_tokens.iter().copied().max().unwrap_or(0),
        prompt_tokens: r.prompt_tokens,
        set_aside: r.set_aside,
        truncated: r.truncated,
        deletes: r.delete_calls,
        moves: r.move_calls,
        recreated: r.delete_then_create,
        plans: r.plan_updates,
        autos: r.auto_calls,
        noapply: false,
        touched: false,
    };

    if r.runaway {
        // The model's failure, not Ollama's: it wrote until the request's
        // five-minute limit cut it off. Scored, not excused.
        out.why = "the model wrote for five minutes without finishing a reply".to_string();
    } else if let Some(e) = r.error {
        out.verdict = Verdict::Error;
        out.why = e;
    } else if r.blocked {
        out.verdict = Verdict::Error;
        out.why = "Smart App Control blocked the model's own check".to_string();
    } else if r.hit_bound {
        out.why = format!("still calling tools after {PROBE_ROUNDS} rounds");
    } else {
        let model_text = std::fs::read_to_string(&full_path).ok();
        let solution_text = match show(task.commit, &path) {
            Ok(t) => t,
            Err(e) => {
                out.verdict = Verdict::Error;
                out.why = format!("could not read the reference tests: {e}");
                out.secs = started.elapsed().as_secs_f64();
                return out;
            }
        };
        let scored = restore(model_text.as_deref(), &solution_text);
        let _ = std::fs::write(&full_path, &scored);
        let (v, why) = score(&tree.path, task.module, target_dir, &appdata);
        out.verdict = v;
        out.why = why;
    }
    (out.verdict, out.why) = book_with_check_timeouts(out.verdict, std::mem::take(&mut out.why), check_timeouts);
    if r.stopped && out.verdict != Verdict::Pass {
        out.why = format!("stopped after repeating a failing call — {}", out.why);
    }
    out.secs = started.elapsed().as_secs_f64();
    out
}

// ── propose mode (I5) ───────────────────────────────────────────────────────

/// Which way a task is run.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// The model works in the worktree itself and every card is approved: H3's run.
    Interactive,
    /// The model works in a scratch copy of the worktree, unattended, and its patch is applied.
    Propose,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Interactive => "interactive",
            Mode::Propose => "propose",
        }
    }
}

/// The commit a task's worktree is made at: the parent of the solution commit for a real
/// commit, the pin for a removal.
fn start_commit(task: &Task) -> String {
    match task.kind {
        Kind::RealCommit => format!("{}^", task.commit),
        Kind::Removal => task.commit.to_string(),
    }
}

/// Put a task's starting state in its worktree: the scored file with its tests hidden and,
/// for a removal, its function replaced with `todo!()`. Left uncommitted, as a person's
/// unfinished work would be.
fn start_state(tree: &Path, task: &Task) -> Result<(), String> {
    let full_path = tree.join(format!("{CRATE}/{}", task.file));
    let original = std::fs::read_to_string(&full_path).unwrap_or_default();
    let stripped = strip(&original, &task.removal)?;
    // a02 and a04 create a file that does not exist at the parent commit —
    // leave it absent so the model has to use create_file, not edit_file.
    if !original.is_empty() || full_path.exists() {
        if let Some(parent) = full_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&full_path, &stripped).map_err(|e| format!("could not write {}: {e}", full_path.display()))?;
    }
    Ok(())
}

/// What a task folder holds: every file of the worktree, and every file of the worktree's
/// own git folder — its index among them — which lives beside the repository's, found by
/// the `.git` file the worktree has. For showing that a run left the folder alone.
fn folder_state(tree: &Path) -> Result<BTreeMap<String, scratch::Print>, String> {
    let read = |dir: &Path| scratch::fingerprints(dir).map_err(|e| format!("could not read {}: {e}", dir.display()));
    let mut state = read(tree)?;
    let link = std::fs::read_to_string(tree.join(".git")).unwrap_or_default();
    if let Some(admin) = link.trim().strip_prefix("gitdir:") {
        for (name, print) in read(Path::new(admin.trim()))? {
            state.insert(format!("<worktree's git folder>/{name}"), print);
        }
    }
    Ok(state)
}

/// How a patch's files read in a run's list: `M path`, as `git status --porcelain` has them.
fn file_lines(patch: &scratch::Patch) -> Vec<String> {
    let letter = |status: &str| match status {
        "added" => "A",
        "deleted" => "D",
        "renamed" => "R",
        _ => "M",
    };
    patch.files.iter().map(|c| format!("{} {}", letter(c.status), c.path)).collect()
}

/// A task run the way `hugin -p` runs one. The worktree is the task folder, with the starting
/// state left uncommitted in it; the model works in a scratch copy of it, with nobody to ask
/// ([`unattended`]); what it did comes back as a patch, which is applied to the task folder,
/// and then the reference tests go back in and the folder is scored as an interactive run is.
///
/// **NOAPPLY**, a patch that does not apply, and **touched**, a task folder that is different
/// before the patch than it was at the start, are the two ways the copy can fail the folder it
/// copies. Both are ERRORs — the model did nothing wrong — and both are counted apart.
fn run_propose(
    model: &str,
    task: &Task,
    opts: Options,
    scratch_dir: &Path,
    target_dir: &Path,
    keep: bool,
    (compact, parts, ops, plan): (bool, bool, bool, bool),
) -> Outcome {
    let started = Instant::now();
    let start = start_commit(task);
    let (wt_path, backups, appdata) = (scratch_dir.join("wt"), scratch_dir.join("backups"), scratch_dir.join("appdata"));
    let path = format!("{CRATE}/{}", task.file);

    let tree = match Worktree::add(&wt_path, &start, keep) {
        Ok(t) => t,
        Err(e) => return Outcome::early(started, format!("could not check out {start}: {e}")),
    };
    if let Err(e) = start_state(&tree.path, task) {
        return Outcome::early(started, e);
    }
    let before = match folder_state(&tree.path) {
        Ok(b) => b,
        Err(e) => return Outcome::early(started, e),
    };
    let mut copy = match Scratch::of_in(&Git::new(), &scratch_dir.join("work"), &tree.path) {
        Ok(c) => c,
        Err(e) => return Outcome::early(started, format!("could not copy the task folder: {e}")),
    };
    let ws = match Workspace::open(copy.folder()) {
        Ok(w) => w,
        Err(e) => return Outcome::early(started, e),
    };

    let argv = vec!["cargo".to_string(), "check".to_string(), "-p".to_string(), "odin-core".to_string()];
    let cargo = Check {
        argv,
        env: vec![
            ("CARGO_TARGET_DIR".to_string(), target_dir.to_string_lossy().into_owned()),
            ("CARGO_NET_OFFLINE".to_string(), "true".to_string()),
        ],
    };
    let offered = tools::available(true, Some(&check::label(&cargo.argv)));
    let offered = if parts { offered } else { whole_reads(offered) };
    let offered = if ops { offered } else { without_ops(offered) };
    let offered = if plan && ops { offered } else { without_plan(offered) };
    let mut messages =
        vec![Message::system(tools::system_prompt(&ws, true)), Message::user(task.prompt)];
    // Unattended, as `hugin -p` is: the rule answers each card, and a read outside the copy is
    // turned away by it (`confine_reads` is the interactive run's way of saying the same).
    let setup = Setup {
        ws: &ws,
        backups: &backups,
        check: Some(&cargo),
        confine_reads: true,
        compact,
        commands: None,
        unattended: true,
    };
    let r = probe::run_loop(model, &mut messages, &offered, opts, Transport::Recover, unattended::ROUNDS, &setup);

    // What the run did, and whether the task folder is still as it was.
    let patch = copy.finish();
    let touched = folder_state(&tree.path).map_or(true, |now| now != before);

    let check_timeouts = r.check_timeouts;
    let mut out = Outcome {
        verdict: Verdict::Fail,
        why: String::new(),
        rounds: r.rounds,
        late: r.rounds > tools::MAX_ROUNDS,
        checks: r.checks.len(),
        asks: r.asks,
        asks_every_call: r.asks_every_call,
        calls: r.calls,
        files: patch.as_ref().map(file_lines).unwrap_or_default(),
        secs: 0.0,
        log: r.log,
        ranged_reads: r.ranged_reads,
        max_prompt: r.prompt_tokens.iter().copied().max().unwrap_or(0),
        prompt_tokens: r.prompt_tokens,
        set_aside: r.set_aside,
        truncated: r.truncated,
        deletes: r.delete_calls,
        moves: r.move_calls,
        recreated: r.delete_then_create,
        plans: r.plan_updates,
        autos: r.auto_calls,
        noapply: false,
        touched,
    };

    if touched {
        out.verdict = Verdict::Error;
        out.why = "the run changed the task folder before its patch was applied".to_string();
    } else if r.runaway {
        out.why = "the model wrote for five minutes without finishing a reply".to_string();
    } else if let Some(e) = r.error {
        out.verdict = Verdict::Error;
        out.why = e;
    } else if r.blocked {
        out.verdict = Verdict::Error;
        out.why = "Smart App Control blocked the model's own check".to_string();
    } else if r.hit_bound {
        out.why = format!("still calling tools after {} rounds", unattended::ROUNDS);
    } else {
        match patch {
            Err(e) => {
                out.verdict = Verdict::Error;
                out.why = format!("could not make the patch: {e}");
            }
            Ok(patch) => {
                let applied = if patch.bytes.is_empty() {
                    Ok(())
                } else {
                    let file = scratch_dir.join("run.patch");
                    std::fs::write(&file, &patch.bytes)
                        .map_err(|e| format!("could not write the patch: {e}"))
                        .and_then(|_| scratch::apply(&tree.path, &file, false))
                };
                match applied {
                    Err(e) => {
                        out.noapply = true;
                        out.verdict = Verdict::Error;
                        out.why = format!("NOAPPLY — the patch did not apply: {e}");
                    }
                    Ok(()) => {
                        let full_path = tree.path.join(&path);
                        let model_text = std::fs::read_to_string(&full_path).ok();
                        match show(task.commit, &path) {
                            Err(e) => {
                                out.verdict = Verdict::Error;
                                out.why = format!("could not read the reference tests: {e}");
                            }
                            Ok(solution_text) => {
                                let scored = restore(model_text.as_deref(), &solution_text);
                                let _ = std::fs::write(&full_path, &scored);
                                (out.verdict, out.why) = score(&tree.path, task.module, target_dir, &appdata);
                            }
                        }
                    }
                }
            }
        }
    }
    (out.verdict, out.why) = book_with_check_timeouts(out.verdict, std::mem::take(&mut out.why), check_timeouts);
    if r.stopped && out.verdict != Verdict::Pass {
        out.why = format!("stopped after repeating a failing call — {}", out.why);
    }
    if keep {
        std::mem::forget(copy);
    }
    out.secs = started.elapsed().as_secs_f64();
    out
}

/// `git show <treeish>:<path>` as the bytes it holds, for a file that is not text.
fn show_bytes(treeish: &str, path: &str) -> Result<Vec<u8>, String> {
    let out = Command::new("git")
        .args(["show", &format!("{treeish}:{path}")])
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    Ok(out.stdout)
}

/// What a commit changed from its parent: `(status letter, path)` for each file.
fn changed_by(commit: &str) -> Result<Vec<(char, String)>, String> {
    let out = Command::new("git")
        .args(["diff-tree", "-r", "-z", "--no-renames", "--name-status", &format!("{commit}^"), commit])
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut names = text.split('\0').filter(|n| !n.is_empty());
    let mut changed = Vec::new();
    while let (Some(status), Some(path)) = (names.next(), names.next()) {
        changed.push((status.chars().next().unwrap_or('M'), path.to_string()));
    }
    Ok(changed)
}

/// What the propose self-check found for one task.
struct ProposeCheck {
    untouched: bool,
    applies: bool,
    verdict: Verdict,
    note: String,
}

/// One task's solution, put through the copy, the patch and the apply, with no model: the
/// task folder as a run starts it, a copy of it with the solution written in, the patch between
/// the two applied to the folder, the reference tests put back, and the folder scored.
fn propose_self_check(task: &Task, scratch_root: &Path, target_dir: &Path, keep: bool) -> Result<ProposeCheck, String> {
    let path = format!("{CRATE}/{}", task.file);
    let appdata = scratch_root.join(format!("appdata-{}", task.id));
    let tree = Worktree::add(&scratch_root.join(format!("prop-{}", task.id)), &start_commit(task), keep)
        .map_err(|e| format!("could not check out: {e}"))?;
    start_state(&tree.path, task)?;
    let before = folder_state(&tree.path)?;
    let mut copy = Scratch::of_in(&Git::new(), &scratch_root.join(format!("work-{}", task.id)), &tree.path)?;

    // The solution, as a model that did the whole job would leave the copy: every file the
    // solution commit changed as it made it, but the scored file without its tests.
    let solution_text = show(task.commit, &path)?;
    let in_copy = copy.copy_root();
    let mut wrote_scored_file = false;
    if task.kind == Kind::RealCommit {
        for (status, name) in changed_by(task.commit)? {
            let to = in_copy.join(&name);
            if status == 'D' {
                let _ = std::fs::remove_file(&to);
                continue;
            }
            let bytes = if name == path { cut_tests(&solution_text).into_bytes() } else { show_bytes(task.commit, &name)? };
            wrote_scored_file |= name == path;
            if let Some(parent) = to.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::write(&to, bytes).map_err(|e| format!("could not write {name} in the copy: {e}"))?;
        }
    }
    if !wrote_scored_file {
        std::fs::write(in_copy.join(&path), cut_tests(&solution_text)).map_err(|e| format!("could not write the scored file: {e}"))?;
    }

    let patch = copy.finish()?;
    let untouched = folder_state(&tree.path)? == before;
    let file = scratch_root.join(format!("{}.patch", task.id));
    std::fs::write(&file, &patch.bytes).map_err(|e| format!("could not write the patch: {e}"))?;
    let applied = if patch.bytes.is_empty() {
        Err("the solution made no patch at all".to_string())
    } else {
        scratch::apply(&tree.path, &file, true).and_then(|_| scratch::apply(&tree.path, &file, false))
    };
    let mut check = ProposeCheck { untouched, applies: applied.is_ok(), verdict: Verdict::Error, note: String::new() };
    match applied {
        Err(e) => check.note = format!("did not apply: {e}"),
        Ok(()) => {
            let full_path = tree.path.join(&path);
            let model_text = std::fs::read_to_string(&full_path).ok();
            let scored = restore(model_text.as_deref(), &solution_text);
            std::fs::write(&full_path, &scored).map_err(|e| format!("could not write the scored file: {e}"))?;
            let (verdict, why) = score(&tree.path, task.module, target_dir, &appdata);
            check.verdict = verdict;
            check.note = format!("{why}, {} file(s) in the patch", patch.files.len());
        }
    }
    if keep {
        std::mem::forget(copy);
    }
    Ok(check)
}

/// `--selfcheck --mode propose`: for every task, the solution through the copy, the patch and
/// the apply must leave the task folder untouched until the apply, apply cleanly, and pass the
/// task's scoring.
fn selfcheck_propose(tasks: &[Task], scratch_root: &Path, target_dir: &Path, keep: bool) -> bool {
    println!("I5 — task probe selfcheck, propose mode (no model)\n");
    println!("| task | kind | folder untouched before the apply? | patch applies? | solution scores |");
    println!("|---|---|---|---|---|");
    let (mut good, mut all_ok) = (0, true);
    for task in tasks {
        let yes = |b: bool| if b { "yes" } else { "NO" };
        match propose_self_check(task, scratch_root, target_dir, keep) {
            Err(e) => {
                all_ok = false;
                println!("| {} | {} | — | — | could not run: {e} |", task.id, task.kind.name());
            }
            Ok(c) => {
                let ok = c.untouched && c.applies && c.verdict == Verdict::Pass;
                good += usize::from(ok);
                all_ok &= ok;
                println!(
                    "| {} | {} | {} | {} | {} ({}) |",
                    task.id,
                    task.kind.name(),
                    yes(c.untouched),
                    yes(c.applies),
                    c.verdict.label(),
                    c.note
                );
            }
        }
    }
    println!();
    println!("{good}/{} tasks round-trip through the copy, the patch and the apply.", tasks.len());
    if all_ok {
        println!("every task is OK in propose mode.");
    } else {
        println!("BROKEN ABOVE — the copy, the patch or the apply loses something.");
    }
    all_ok
}

/// One finished run, kept for the paired comparison of the modes.
struct Paired {
    setting: String,
    task: &'static str,
    run: usize,
    mode: Mode,
    /// Passed within the app's own rounds: what the headline counts.
    pass: bool,
}

/// How the propose runs did against the interactive ones, run for run: the same setting, the
/// same task, the same run number. One line a setting, and a closing line for all of them.
fn paired_report(runs: &[Paired]) -> Vec<String> {
    let mut settings: Vec<&str> = Vec::new();
    for r in runs {
        if !settings.contains(&r.setting.as_str()) {
            settings.push(&r.setting);
        }
    }
    let mut lines = Vec::new();
    let (mut total, mut sums) = (0usize, [0usize; 4]);
    for setting in settings {
        // [both pass, only interactive, only propose, neither]
        let mut counts = [0usize; 4];
        let (mut i_pass, mut p_pass) = (0, 0);
        for i in runs.iter().filter(|r| r.setting == setting && r.mode == Mode::Interactive) {
            let Some(p) = runs.iter().find(|p| p.mode == Mode::Propose && p.setting == setting && p.task == i.task && p.run == i.run)
            else {
                continue;
            };
            counts[match (i.pass, p.pass) {
                (true, true) => 0,
                (true, false) => 1,
                (false, true) => 2,
                (false, false) => 3,
            }] += 1;
            i_pass += usize::from(i.pass);
            p_pass += usize::from(p.pass);
        }
        let n: usize = counts.iter().sum();
        total += n;
        for (sum, c) in sums.iter_mut().zip(counts) {
            *sum += c;
        }
        lines.push(format!(
            "  {setting}: interactive {i_pass}/{n} · propose {p_pass}/{n} · propose − interactive {:+} (both pass {} · only interactive {} · only propose {} · neither {})",
            counts[2] as i64 - counts[1] as i64,
            counts[0],
            counts[1],
            counts[2],
            counts[3]
        ));
    }
    lines.push(format!(
        "  all: {total} pairs · propose − interactive {:+} (both pass {} · only interactive {} · only propose {} · neither {})",
        sums[2] as i64 - sums[1] as i64,
        sums[0],
        sums[1],
        sums[2],
        sums[3]
    ));
    lines
}

// ── main ────────────────────────────────────────────────────────────────────

fn arg(args: &[String], key: &str) -> Option<String> {
    args.iter().position(|a| a == key).and_then(|i| args.get(i + 1)).cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = config::load();
    let model = arg(&args, "--model").unwrap_or(cfg.model.clone());
    let runs: usize = arg(&args, "--runs").and_then(|s| s.parse().ok()).unwrap_or(3);
    let only = arg(&args, "--only");
    let keep = args.iter().any(|a| a == "--keep");
    let do_selfcheck = args.iter().any(|a| a == "--selfcheck");
    // On, like the app: a probe that sent the whole conversation would measure
    // the loop as it was before H7. `--compact off` is how that is measured.
    let compact = match arg(&args, "--compact").as_deref() {
        None | Some("on") => true,
        Some("off") => false,
        Some(other) => {
            eprintln!("--compact takes on or off, not {other}");
            std::process::exit(2);
        }
    };

    // Unset, like the app with no `model_settings` entry: the model's own. `--think high` is
    // the model's setting for the whole run (J0d); a probe reads no config entry for it.
    let think = match arg(&args, "--think") {
        None => None,
        Some(word) => match Think::parse(&word) {
            Some(think) => Some(think),
            None => {
                eprintln!("--think takes off, low, medium or high, not {word}");
                std::process::exit(2);
            }
        },
    };

    // Parts, like the app: the read offers `start_line` and `end_line`. `--read
    // whole` offers the read as it was, to measure H8 on and off in one build.
    let parts = match arg(&args, "--read").as_deref() {
        None | Some("parts") => true,
        Some("whole") => false,
        Some(other) => {
            eprintln!("--read takes whole or parts, not {other}");
            std::process::exit(2);
        }
    };

    // On, like the app. `--plan off` offers H9's tools exactly (see `without_plan`).
    let plan = match arg(&args, "--plan").as_deref() {
        None | Some("on") => true,
        Some("off") => false,
        Some(other) => {
            eprintln!("--plan takes on or off, not {other}");
            std::process::exit(2);
        }
    };

    // On, like the app. `--ops off` offers H8's tools exactly (see `without_ops`).
    let ops = match arg(&args, "--ops").as_deref() {
        None | Some("on") => true,
        Some("off") => false,
        Some(other) => {
            eprintln!("--ops takes on or off, not {other}");
            std::process::exit(2);
        }
    };

    // Interactive, as H3 ran it. `--mode propose` runs each task the way `hugin -p` does (see
    // `run_propose`), and `--mode both` runs every task both ways and reports the paired
    // difference.
    let modes: Vec<Mode> = match arg(&args, "--mode").as_deref() {
        None | Some("interactive") => vec![Mode::Interactive],
        Some("propose") => vec![Mode::Propose],
        Some("both") => vec![Mode::Interactive, Mode::Propose],
        Some(other) => {
            eprintln!("--mode takes interactive, propose or both, not {other}");
            std::process::exit(2);
        }
    };

    let tasks: Vec<Task> =
        tasks().into_iter().filter(|t| only.as_deref().is_none_or(|o| t.id.starts_with(o))).collect();

    let scratch_root = std::env::temp_dir().join(format!("bifrost-taskprobe-{}", std::process::id()));
    // The repo's own target dir: warming a fresh one would rebuild every
    // proc-macro from nothing, and a fresh link is what Smart App Control has
    // refused on this machine before.
    let target_dir = std::env::current_dir().unwrap_or_default().join("target");

    if do_selfcheck {
        let mut ok = true;
        for mode in &modes {
            ok &= match mode {
                Mode::Interactive => selfcheck(&tasks, &scratch_root, &target_dir, keep),
                Mode::Propose => selfcheck_propose(&tasks, &scratch_root, &target_dir, keep),
            };
        }
        let _ = std::fs::remove_dir_all(&scratch_root);
        std::process::exit(if ok { 0 } else { 1 });
    }

    println!("H3 — task probe");
    println!("model:  {model}");
    println!("think:  {}", think.map_or("unset (the model's own)", Think::name));
    println!("mode:   {}", modes.iter().map(|m| m.name()).collect::<Vec<_>>().join(" and "));
    println!("ctx:    {}", cfg.num_ctx);
    println!("compact: {}", if compact { "on" } else { "off" });
    println!(
        "plan:   {}",
        if plan && ops { "on (update_plan offered)" } else { "off (no task list)" }
    );
    println!("ops:    {}", if ops { "on (delete_file and move_file offered)" } else { "off (the tools as at H8)" });
    println!("read:   {}", if parts { "parts (start_line and end_line offered)" } else { "whole (the read before H8)" });
    println!(
        "tasks:  {} · {} real commits · {} removals",
        tasks.len(),
        tasks.iter().filter(|t| t.kind == Kind::RealCommit).count(),
        tasks.iter().filter(|t| t.kind == Kind::Removal).count()
    );
    println!("runs:   {runs} at the app's own settings, plus 1 at temperature 0");
    println!("bound:  {PROBE_ROUNDS} rounds (the app stops at {})\n", tools::MAX_ROUNDS);

    // Started if it is not running, as the apps and the CLI do: an hour-long
    // run should not fail at the first request because nothing had needed
    // Ollama since the last reboot.
    if let Err(e) = ollama::ensure_running() {
        eprintln!("could not start Ollama: {e}");
        std::process::exit(1);
    }
    match ollama::load(&model, cfg.num_ctx) {
        Ok(d) => println!("warmed in {:.1}s\n", d.as_secs_f64()),
        Err(e) => {
            eprintln!("could not load {model}: {e}");
            std::process::exit(1);
        }
    }

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let settings: Vec<(&str, Options, usize)> = vec![
        ("app", Options::new(cfg.num_ctx).thinking(think), runs),
        ("t=0", Options::measured(cfg.num_ctx).thinking(think), 1),
    ];

    let mut rows = Vec::new();
    let mut json = Vec::new();
    // Every pass is one mode over one setting, the interactive ones first.
    let passes: Vec<(Mode, &str, Options, usize)> =
        modes.iter().flat_map(|m| settings.iter().map(move |(l, o, n)| (*m, *l, *o, *n))).collect();
    let mut paired = Vec::new();
    let (mut noapply_total, mut touched_total) = (0usize, 0usize);

    for (mode, setting, opts, n) in &passes {
        // What a pass is called where it is shown: the setting, and the mode when it is not H3's.
        let label = &if *mode == Mode::Propose { format!("{setting} {}", mode.name()) } else { setting.to_string() };
        let (mut pass, mut late, mut fail, mut error) = (0, 0, 0, 0);
        let (mut kind_a, mut kind_b) = ((0usize, 0usize), (0usize, 0usize));
        let (mut secs, mut rounds, mut checks) = (0.0, 0usize, 0usize);
        let (mut asks, mut asks_every_call) = (0usize, 0usize);
        let (mut max_prompt, mut set_aside, mut cut_runs) = (0u32, 0usize, 0usize);
        let mut ranged_reads = 0usize;
        let (mut deletes, mut moves, mut recreated) = (0usize, 0usize, 0usize);
        let mut plans = 0usize;
        let mut autos = 0usize;
        println!("── {label} ──");
        for task in &tasks {
            let mut line = String::new();
            for i in 0..*n {
                let scratch = scratch_root.join(format!("{}-{}-{}-{i}", mode.name(), setting.replace('=', ""), task.id));
                let flags = (compact, parts, ops, plan);
                let o = match mode {
                    Mode::Interactive => run(&model, task, *opts, &scratch, &target_dir, keep, flags),
                    Mode::Propose => run_propose(&model, task, *opts, &scratch, &target_dir, keep, flags),
                };
                if !keep {
                    let _ = std::fs::remove_dir_all(&scratch);
                }
                noapply_total += usize::from(o.noapply);
                touched_total += usize::from(o.touched);
                paired.push(Paired {
                    setting: setting.to_string(),
                    task: task.id,
                    run: i + 1,
                    mode: *mode,
                    pass: o.verdict == Verdict::Pass && !o.late,
                });
                let counted = match task.kind {
                    Kind::RealCommit => &mut kind_a,
                    Kind::Removal => &mut kind_b,
                };
                match o.verdict {
                    Verdict::Pass if o.late => late += 1,
                    Verdict::Pass => {
                        pass += 1;
                        counted.0 += 1;
                    }
                    Verdict::Fail => fail += 1,
                    Verdict::Error => error += 1,
                }
                counted.1 += 1;
                secs += o.secs;
                rounds += o.rounds;
                checks += o.checks;
                asks += o.asks;
                asks_every_call += o.asks_every_call;
                max_prompt = max_prompt.max(o.max_prompt);
                set_aside += o.set_aside;
                ranged_reads += o.ranged_reads;
                deletes += o.deletes;
                moves += o.moves;
                recreated += o.recreated;
                plans += o.plans;
                autos += o.autos;
                cut_runs += (o.truncated > 0) as usize;
                let shown = if o.verdict == Verdict::Pass && o.late { "LATE" } else { o.verdict.label() };
                line.push_str(&format!("{shown:<5} "));
                if o.verdict != Verdict::Pass {
                    println!("    {} run {}: {} — {}", task.id, i + 1, o.verdict.label(), o.why);
                    for l in &o.log {
                        println!("      {l}");
                    }
                }
                json.push(serde_json::json!({
                    "setting": setting, "mode": mode.name(), "noApply": o.noapply, "touched": o.touched,
                    "task": task.id, "kind": task.kind.name(), "run": i + 1,
                    "verdict": o.verdict.label(), "why": o.why, "rounds": o.rounds, "late": o.late,
                    "checks": o.checks, "asks": o.asks, "asksEveryCall": o.asks_every_call, "calls": o.calls, "filesChanged": o.files, "log": o.log,
                    "promptTokens": o.prompt_tokens, "maxPrompt": o.max_prompt, "setAside": o.set_aside,
                    "truncated": o.truncated, "rangedReads": o.ranged_reads,
                    "deletes": o.deletes, "moves": o.moves, "deleteThenCreate": o.recreated, "planUpdates": o.plans, "autoCalls": o.autos,
                    "secs": (o.secs * 10.0).round() / 10.0,
                }));
            }
            println!("  {:<20} {line}", task.id);
        }
        let total = pass + late + fail + error;
        println!(
            "  → {pass}/{total} pass within the app's {} rounds ({:.0}%) · {late} only later · {fail} fail · \
             {error} error · real commits {}/{} · removals {}/{} · {:.1} rounds/run · {:.1} plans/run · {:.1} checks/run · \
             {:.1} asks/run ({:.1} asking every call) · max prompt {max_prompt} · {:.1} set aside/run · \
             {cut_runs} cut runs · {ranged_reads} ranged reads · {deletes} deletes · {moves} moves · \
             {recreated} delete-then-create · {:.1}s avg\n",
            tools::MAX_ROUNDS,
            100.0 * pass as f64 / total.max(1) as f64,
            kind_a.0,
            kind_a.1,
            kind_b.0,
            kind_b.1,
            rounds as f64 / total.max(1) as f64,
            plans as f64 / total.max(1) as f64,
            checks as f64 / total.max(1) as f64,
            asks as f64 / total.max(1) as f64,
            asks_every_call as f64 / total.max(1) as f64,
            set_aside as f64 / total.max(1) as f64,
            secs / total.max(1) as f64,
        );
        rows.push(format!(
            "| {label} | {pass}/{total} ({:.0}%) | {late} | {fail} | {error} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1}s | {max_prompt} | {cut_runs} | {ranged_reads} | {deletes} | {moves} | {recreated} |",
            100.0 * pass as f64 / total.max(1) as f64,
            rounds as f64 / total.max(1) as f64,
            plans as f64 / total.max(1) as f64,
            autos as f64 / total.max(1) as f64,
            checks as f64 / total.max(1) as f64,
            asks as f64 / total.max(1) as f64,
            secs / total.max(1) as f64,
        ));
    }

    println!("─── {model} ───");
    println!("| setting | pass (≤{} rounds) | pass later | fail | error | rounds/run | plans/run | autos/run | checks/run | asks/run | time/run | max prompt | cut runs | ranged reads | deletes | moves | delete→create |", tools::MAX_ROUNDS);
    println!("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|");
    for r in &rows {
        println!("{r}");
    }
    if modes.contains(&Mode::Propose) {
        println!("\npropose mode: {noapply_total} NOAPPLY (a patch that did not apply) · {touched_total} touched the task folder");
    }
    if modes.len() > 1 {
        println!("\npaired, run for run (passes within the app's {} rounds):", tools::MAX_ROUNDS);
        for line in paired_report(&paired) {
            println!("{line}");
        }
    }

    // H3's own results keep H3's name; a run that includes propose mode is I5's.
    let kind = if modes == [Mode::Interactive] { "H3" } else { "I5" };
    let out = PathBuf::from("evals/out").join(format!("{kind}-task-{stamp}"));
    if std::fs::create_dir_all(&out).is_ok() {
        let _ = std::fs::write(
            out.join("results.json"),
            serde_json::to_string_pretty(&serde_json::json!({ "model": model, "runs": json })).unwrap(),
        );
        println!("\nsaved {}", out.join("results.json").display());
    }
    if !keep {
        let _ = std::fs::remove_dir_all(&scratch_root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ended(how: check::Ended) -> check::Outcome {
        check::Outcome {
            ended: how,
            elapsed: std::time::Duration::from_secs(600),
            output: String::new(),
            bytes: 0,
            complete: true,
            blocked: None,
            unfinished_tests: 0,
        }
    }

    /// R-10: only a run that finished with an exit code may be judged by its output.
    #[test]
    fn a_scoring_run_that_never_finished_is_not_judged() {
        assert!(unfinished(&ended(check::Ended::TimedOut)).unwrap().contains("timed out"));
        assert!(unfinished(&ended(check::Ended::Stopped)).unwrap().contains("stopped"));
        assert!(unfinished(&ended(check::Ended::Exited(None))).unwrap().contains("no exit code"));
        assert!(unfinished(&ended(check::Ended::Exited(Some(0)))).is_none());
        assert!(unfinished(&ended(check::Ended::Exited(Some(101)))).is_none());
    }

    /// R-10: a check that timed out turns a would-be FAIL into an ERROR, and nothing else.
    #[test]
    fn a_check_timeout_excuses_a_fail_but_not_a_pass() {
        let (v, why) = book_with_check_timeouts(Verdict::Fail, "zero tests ran".into(), 1);
        assert!(v == Verdict::Error && why.contains("environment") && why.contains("zero tests ran"));
        assert!(book_with_check_timeouts(Verdict::Fail, "x".into(), 0).0 == Verdict::Fail);
        assert!(book_with_check_timeouts(Verdict::Pass, "3 passed".into(), 2).0 == Verdict::Pass);
        assert!(book_with_check_timeouts(Verdict::Error, "x".into(), 2).0 == Verdict::Error);
    }

    /// `--read whole` is the read as shipped before H8, word for word.
    #[test]
    fn the_whole_read_is_the_old_schema_and_the_old_description() {
        let old = whole_reads(tools::available(true, None));
        let read = old.iter().find(|t| t.function.name == "read_file").unwrap();
        assert_eq!(
            read.function.description,
            "Read a text file and return its contents. Paths inside the project folder may be \
             relative to it. The user may be asked to approve a read."
        );
        assert_eq!(
            read.function.parameters,
            serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string", "description": "Path to the file" } },
                "required": ["path"]
            })
        );
        // The other tools are untouched.
        let new = tools::available(true, None);
        assert_eq!(old.len(), new.len());
        assert_eq!(old[1].function.description, new[1].function.description);
    }

    /// **`--ops off` is H8's tool list**, in names and in what `create_file` says —
    /// not H9's with two tools hidden.
    #[test]
    fn ops_off_is_the_tool_list_as_it_was_at_h8() {
        let on = tools::available(true, Some("cargo check"));
        let names = |v: &[Tool]| v.iter().map(|t| t.function.name.clone()).collect::<Vec<_>>();
        assert!(names(&on).contains(&"delete_file".to_string()) && names(&on).contains(&"move_file".to_string()));
        let off = without_ops(without_plan(on));
        assert_eq!(names(&off), ["read_file", "list_files", "search", "edit_file", "create_file", "run_check"]);
        let create = off.iter().find(|t| t.function.name == "create_file").unwrap();
        assert_eq!(
            create.function.description,
            "Create a new file in the project folder. Fails if the file already exists — use \
             edit_file to change an existing file. The folder it goes in must already exist."
        );
        assert_eq!(
            create.function.parameters,
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path, relative to the project folder" },
                    "content": { "type": "string", "description": "The whole contents of the new file" }
                },
                "required": ["path", "content"]
            })
        );
    }

    /// **`--plan off` is H9's tool list**, word for word: the same tools in the same
    /// order, with only `update_plan` gone and `run_check` still last.
    #[test]
    fn plan_off_is_the_tool_list_as_it_was_at_h9() {
        let on = tools::available(true, Some("cargo check"));
        let names = |v: &[Tool]| v.iter().map(|t| t.function.name.clone()).collect::<Vec<_>>();
        assert!(names(&on).contains(&"update_plan".to_string()));
        let off = without_plan(on.clone());
        assert_eq!(
            names(&off),
            ["read_file", "list_files", "search", "edit_file", "create_file", "delete_file", "move_file", "run_check"]
        );
        // Nothing else was touched: the other tools are the same, byte for byte.
        let kept: Vec<_> = on.iter().filter(|t| t.function.name != "update_plan").collect();
        assert_eq!(serde_json::to_value(&kept).unwrap(), serde_json::to_value(&off).unwrap());
    }

    /// **The model cannot read its way to the answer.** A worktree's `.git`
    /// names the repo it came from, and that repo holds every reference
    /// solution; with reads confined, neither is reachable — inside the folder,
    /// reading works as ever. The shared loop asks the app's rule whether a read
    /// needs a card and the probe's whether to allow it: this is the second.
    #[test]
    fn reads_are_confined_to_the_worktree() {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("bifrost-taskprobe-confine-{}-{n}", std::process::id()));
        let tree = base.join("tree");
        std::fs::create_dir_all(tree.join("src")).unwrap();
        std::fs::write(tree.join("src/lib.rs"), "pub fn inside() {}\n").unwrap();
        std::fs::write(tree.join(".git"), "gitdir: C:/somewhere/.git/worktrees/x\n").unwrap();
        std::fs::write(base.join("answer.rs"), "the reference solution\n").unwrap();

        let ws = Workspace::open(&tree).unwrap();
        assert!(probe::read_allowed(&ws, "src/lib.rs"));
        assert!(!probe::read_allowed(&ws, &base.join("answer.rs").to_string_lossy()));
        assert!(!probe::read_allowed(&ws, "../answer.rs"));
        assert!(!probe::read_allowed(&ws, ".git"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn test_tail_starts_at_the_first_marker() {
        let text = "fn a() {}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn x() {}\n}\n";
        assert_eq!(
            test_tail(text).unwrap(),
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn x() {}\n}\n"
        );
        assert!(test_tail("fn a() {}\n").is_none());
    }

    #[test]
    fn cut_tests_removes_everything_from_the_marker() {
        let text = "fn a() {}\n\n#[cfg(test)]\nmod tests {\n    fn x() {}\n}\n";
        assert_eq!(cut_tests(text), "fn a() {}\n\n");
        assert_eq!(cut_tests("fn a() {}\n"), "fn a() {}\n");
        assert_eq!(cut_tests(""), "");
    }

    #[test]
    fn restore_appends_the_solutions_tests_to_the_models_own_code() {
        let solution = "fn a() {\n    2\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() { assert_eq!(a(), 2); }\n}\n";
        let model = "fn a() {\n    2\n}\n";
        assert_eq!(
            restore(Some(model), solution),
            "fn a() {\n    2\n}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() { assert_eq!(a(), 2); }\n}\n"
        );
    }

    #[test]
    fn a_missing_model_file_still_gets_the_reference_tests() {
        let solution = "fn a() {}\n\n#[cfg(test)]\nmod tests {\n    fn t() {}\n}\n";
        assert_eq!(restore(None, solution), "#[cfg(test)]\nmod tests {\n    fn t() {}\n}\n");
    }

    #[test]
    fn apply_removal_replaces_the_bodys_interior_with_todo() {
        let text = "fn f(x: i32) -> i32 {\n    let y = x + 1;\n    y\n}\n";
        let out = apply_removal(text, &Removal { start: 2, end: 3, indent: 0 }).unwrap();
        assert_eq!(out, "fn f(x: i32) -> i32 {\n    todo!()\n}\n");
    }

    #[test]
    fn apply_removal_respects_indentation_for_a_method() {
        let text = "impl X {\n    fn f(&self) -> i32 {\n        1\n    }\n}\n";
        let out = apply_removal(text, &Removal { start: 3, end: 3, indent: 4 }).unwrap();
        assert_eq!(out, "impl X {\n    fn f(&self) -> i32 {\n        todo!()\n    }\n}\n");
    }

    #[test]
    fn a_wrong_range_is_refused_rather_than_silently_scoring_the_wrong_code() {
        let text = "fn f() -> i32 {\n    1\n}\n";
        // Off by one: the true body is line 2, not line 1.
        assert!(apply_removal(text, &Removal { start: 1, end: 2, indent: 0 }).is_err());
        assert!(apply_removal(text, &Removal { start: 2, end: 2, indent: 4 }).is_err());
    }

    #[test]
    fn judge_reads_a_passing_run() {
        let out = "running 3 tests\n...\ntest result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n";
        assert!(matches!(judge_tests(out), TestOutcome::Pass(3)));
    }

    #[test]
    fn judge_reads_zero_tests_run_as_zero_ran_not_a_pass() {
        let out = "running 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n";
        assert!(matches!(judge_tests(out), TestOutcome::ZeroRan));
    }

    #[test]
    fn judge_reads_a_failing_run() {
        let out = "running 2 tests\ntest edit::tests::a_thing ... FAILED\ntest edit::tests::b_thing ... ok\n\ntest result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n";
        match judge_tests(out) {
            TestOutcome::Failed(names) => assert!(names.contains("a_thing")),
            _ => panic!("expected Failed"),
        }
    }

    #[test]
    fn judge_reads_a_build_that_does_not_compile() {
        let out = "error[E0425]: cannot find function `todo_helper` in this scope\n --> src/edit.rs:10:5\n";
        match judge_tests(out) {
            TestOutcome::NoCompile(first) => assert!(first.contains("E0425")),
            _ => panic!("expected NoCompile"),
        }
    }

    #[test]
    fn every_removal_task_has_a_body_of_at_least_two_lines() {
        for t in tasks() {
            if let Some(r) = &t.removal {
                assert!(r.end >= r.start, "{}: empty range", t.id);
            }
        }
    }

    #[test]
    fn task_ids_are_unique() {
        let ts = tasks();
        for (i, a) in ts.iter().enumerate() {
            for b in &ts[i + 1..] {
                assert_ne!(a.id, b.id);
            }
        }
    }

    // ── propose mode ────────────────────────────────────────────────────────

    fn paired(setting: &str, task: &'static str, run: usize, mode: Mode, pass: bool) -> Paired {
        Paired { setting: setting.into(), task, run, mode, pass }
    }

    /// **The pairs are the same setting, task and run number**, and a run with no partner is
    /// not counted on either side.
    #[test]
    fn the_paired_report_compares_run_for_run() {
        use Mode::{Interactive as I, Propose as P};
        let runs = vec![
            paired("app", "a01", 1, I, true),
            paired("app", "a01", 1, P, true),
            paired("app", "a01", 2, I, true),
            paired("app", "a01", 2, P, false),
            paired("app", "b01", 1, I, false),
            paired("app", "b01", 1, P, true),
            paired("app", "b01", 2, I, false),
            paired("app", "b01", 2, P, false),
            paired("app", "b01", 3, I, true),
            paired("t=0", "a01", 1, I, true),
            paired("t=0", "a01", 1, P, true),
        ];
        let lines = paired_report(&runs);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert_eq!(
            lines[0],
            "  app: interactive 2/4 · propose 2/4 · propose − interactive +0 (both pass 1 · only interactive 1 · only propose 1 · neither 1)"
        );
        assert_eq!(
            lines[1],
            "  t=0: interactive 1/1 · propose 1/1 · propose − interactive +0 (both pass 1 · only interactive 0 · only propose 0 · neither 0)"
        );
        assert_eq!(
            lines[2],
            "  all: 5 pairs · propose − interactive +0 (both pass 2 · only interactive 1 · only propose 1 · neither 1)"
        );
        // A run of one mode alone has no pairs.
        assert!(paired_report(&[paired("app", "a01", 1, I, true)])[0].contains("interactive 0/0"));
    }

    #[test]
    fn a_patchs_files_read_as_git_status_has_them() {
        let change = |path: &str, status: &'static str| scratch::Change { path: path.into(), from: None, status, added: None, deleted: None };
        let patch = scratch::Patch {
            bytes: Vec::new(),
            files: vec![
                change("a.rs", "modified"),
                change("b.rs", "added"),
                change("c.rs", "deleted"),
                change("d.rs", "renamed"),
            ],
        };
        assert_eq!(file_lines(&patch), ["M a.rs", "A b.rs", "D c.rs", "R d.rs"]);
    }

    #[test]
    fn a_tasks_starting_commit_is_its_parent_for_a_real_commit_and_the_pin_for_a_removal() {
        let ts = tasks();
        let real = ts.iter().find(|t| t.kind == Kind::RealCommit).unwrap();
        assert_eq!(start_commit(real), format!("{}^", real.commit));
        let removal = ts.iter().find(|t| t.kind == Kind::Removal).unwrap();
        assert_eq!(start_commit(removal), PIN);
    }

    /// The starting state is left uncommitted in the worktree: the scored file without its
    /// tests, and a removal's function as `todo!()`; a file the parent does not have stays absent.
    #[test]
    fn the_starting_state_strips_the_scored_file_in_place() {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("bifrost-taskprobe-start-{}-{n}", std::process::id()));
        let task = Task {
            id: "t",
            kind: Kind::Removal,
            file: "src/x.rs",
            module: "x",
            prompt: "",
            commit: PIN,
            removal: Some(Removal { start: 2, end: 2, indent: 0 }),
        };
        let file = dir.join(CRATE).join(task.file);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "fn f() -> i32 {\n    1\n}\n\n#[cfg(test)]\nmod tests {}\n").unwrap();
        start_state(&dir, &task).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "fn f() -> i32 {\n    todo!()\n}\n\n");

        let new_file = Task { removal: None, kind: Kind::RealCommit, file: "src/absent.rs", ..task };
        start_state(&dir, &new_file).unwrap();
        assert!(!dir.join(CRATE).join("src/absent.rs").exists(), "a file the parent lacks stays absent");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
