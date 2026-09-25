//! The local tools the model may ask for, and the rule about what they may touch.
//!
//! **Every tool is code the model chose to run.** That is the whole reason this
//! module is small and boring. G0 measured that the model picks tools well
//! (`qwen3-vl:8b` 10/10, including four cases where calling one would have been
//! wrong) — but "picks well" is not "may be trusted with a filesystem".
//!
//! **Nothing runs without an explicit, per-call approval from the UI.** There is
//! no allowlist, no "always allow". The app shows exactly what will happen —
//! the path for a read, the diff for an edit — and waits.
//!
//! **Three tools, and only the first is always offered.** `read_file` can read
//! anywhere, one approved file at a time. `edit_file` and `create_file` exist
//! only once a project folder has been chosen, and cannot reach outside it —
//! see [`crate::edit`] for how that is enforced.
//!
//! The descriptions and the system prompt live here, not in the app, so the G7
//! probe measures the exact words the app ships with. A probe that tested
//! different wording would be measuring a different product.

use serde_json::json;

use crate::chat::{Message, Tool, ToolCall};
use crate::edit::{self, Applied, Workspace};

/// How much of a file is worth reading into a conversation.
///
/// A 32k context is about 128 KB of text at four characters a token, and the
/// history and the answer need room too. Refused rather than truncated: half a
/// file answered confidently is worse than being told it was too big.
pub const MAX_READ_BYTES: u64 = 64 * 1024;

/// How many tool calls one answer may make before the loop stops it.
///
/// A bound, not a timeout — without one, a model that keeps asking loops
/// forever. G6 used 4, which was right for reads and **too few for edits**: the
/// G7 probe measured a two-file change taking read, read, edit, edit and then
/// an answer — five rounds — so the old bound cut it off one short.
pub const MAX_ROUNDS: usize = 10;

fn read_tool() -> Tool {
    Tool::function(
        "read_file",
        "Read a text file and return its contents. Paths inside the project folder may be \
         relative to it. Only use this when the user refers to a file. The user approves \
         each read.",
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file" }
            },
            "required": ["path"]
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

fn create_tool() -> Tool {
    Tool::function(
        "create_file",
        "Create a new file in the project folder. Fails if the file already exists — use \
         edit_file to change an existing file. The folder it goes in must already exist.",
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

/// The tools offered to the model. Editing tools only when there is a folder
/// they are confined to.
pub fn available(editing: bool) -> Vec<Tool> {
    if editing {
        vec![read_tool(), edit_tool(), create_tool()]
    } else {
        vec![read_tool()]
    }
}

/// What the model is told about the folder, when editing is on.
///
/// Kept short. The tool descriptions carry the mechanics; this carries the one
/// thing only the app knows — where the folder is — and the habit that makes
/// search-and-replace work at all: read before you edit.
pub fn system_prompt(ws: &Workspace) -> String {
    format!(
        "You can read and change files in the user's project folder: {}\n\
         Use paths relative to that folder. To answer a question about a file, read it — never \
         guess what it says. To change a file, read it with read_file first, then use edit_file \
         with text copied exactly from what you read. Only change what the user asked for. When \
         you are done, say briefly what you changed.",
        ws.root_display()
    )
}

/// What the model asked for, in a form the UI can show before anything happens.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Read { path: String },
    Edit { path: String, old_text: String, new_text: String },
    Create { path: String, content: String },
    /// Probe only — see [`rewrite_tool`].
    Rewrite { path: String, content: String },
}

impl Request {
    pub fn name(&self) -> &'static str {
        match self {
            Request::Read { .. } => "read_file",
            Request::Edit { .. } => "edit_file",
            Request::Create { .. } => "create_file",
            Request::Rewrite { .. } => "write_file",
        }
    }

    pub fn path(&self) -> &str {
        match self {
            Request::Read { path }
            | Request::Edit { path, .. }
            | Request::Create { path, .. }
            | Request::Rewrite { path, .. } => path,
        }
    }
}

fn arg(call: &ToolCall, key: &str) -> Option<String> {
    call.function.arguments.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

/// Read a call, or say why it cannot be honoured.
///
/// **An unknown tool is refused here, not at execution.** The model can name
/// anything it likes; only these are real.
pub fn interpret(call: &ToolCall) -> Result<Request, String> {
    let name = call.function.name.as_str();
    let path = arg(call, "path").unwrap_or_default().trim().to_string();
    if path.is_empty() && matches!(name, "read_file" | "edit_file" | "create_file" | "write_file") {
        return Err(format!("{name} was called without a path"));
    }
    match name {
        "read_file" => Ok(Request::Read { path }),
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
        "write_file" => Ok(Request::Rewrite {
            path,
            content: arg(call, "content").ok_or("write_file was called without content")?,
        }),
        other => Err(format!("no such tool: {other}")),
    }
}

/// Where a read will actually look. Relative paths are relative to the project
/// folder when there is one — the same rule edits use, so "read src/main.rs then
/// edit src/main.rs" names one file, not two.
pub fn read_target(path: &str, ws: Option<&Workspace>) -> String {
    let p = std::path::Path::new(path.trim());
    match ws {
        Some(ws) if !p.is_absolute() => ws.root().join(p).to_string_lossy().replace(r"\\?\", ""),
        _ => path.trim().to_string(),
    }
}

/// Run an **already approved** read.
///
/// Errors come back as text for the model rather than as a failure, because a
/// tool that cannot answer is a normal event in a conversation: the model should
/// be told "that file does not exist" and continue, not have the turn collapse.
///
/// The contents are normalised the way edits match them — `\n` endings, no
/// BOM — so text the model copies out of a read is text an edit can find.
pub fn run_read_file(path: &str) -> String {
    let p = std::path::Path::new(path);

    let meta = match std::fs::metadata(p) {
        Ok(m) => m,
        Err(e) => return format!("error: cannot read {path}: {e}"),
    };
    if meta.is_dir() {
        return format!("error: {path} is a directory, not a file");
    }
    if meta.len() > MAX_READ_BYTES {
        return format!(
            "error: {path} is {} bytes, over the {} byte limit for a single read",
            meta.len(),
            MAX_READ_BYTES
        );
    }

    match std::fs::read(p) {
        Ok(bytes) => edit::for_model(&bytes),
        Err(e) => format!("error: cannot read {path}: {e}"),
    }
}

/// Said to the model when it sends a call identical to one that already failed.
pub const REPEATED_NOTE: &str = " You already sent exactly this call and it failed the same way. \
     Change it before trying again — copy the text from the file.";

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

/// What the model is told after an edit. Short, and specific enough that it can
/// tell a one-line fix from a rewrite it did not intend.
pub fn applied_message(a: &Applied) -> String {
    if a.create {
        format!("Created {} ({} lines).", a.display, a.added)
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
        assert!(interpret(&call("delete_file", json!({ "path": "a.txt" }))).is_err());
        assert!(interpret(&call("read_file", json!({ "path": "a.txt" }))).is_ok());
    }

    #[test]
    fn a_call_without_a_path_is_refused() {
        assert!(interpret(&call("read_file", json!({}))).is_err());
        assert!(interpret(&call("read_file", json!({ "path": "  " }))).is_err());
        assert!(interpret(&call("edit_file", json!({ "old_text": "a", "new_text": "b" }))).is_err());
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
        assert_eq!(names(available(false)), ["read_file"]);
        assert_eq!(names(available(true)), ["read_file", "edit_file", "create_file"]);
        // The probe-only rewrite is never offered by the app.
        assert!(!names(available(true)).contains(&"write_file".to_string()));
    }

    #[test]
    fn errors_come_back_as_text_for_the_model() {
        // Not a Result: a tool that cannot answer is a normal conversational
        // event, and the model should be told rather than the turn collapsing.
        let out = run_read_file("C:\\definitely\\not\\here.txt");
        assert!(out.starts_with("error:"), "{out}");
    }

    #[test]
    fn a_directory_is_refused_clearly() {
        let out = run_read_file(".");
        assert!(out.contains("directory"), "{out}");
    }

    #[test]
    fn a_real_file_comes_back_normalised() {
        let mut p = std::env::temp_dir();
        p.push(format!("odin-tools-test-{}.txt", std::process::id()));
        std::fs::write(&p, "hello\r\nfrom a tool\r\n").unwrap();
        assert_eq!(run_read_file(p.to_str().unwrap()), "hello\nfrom a tool\n");
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn relative_reads_resolve_in_the_project_folder() {
        let dir = std::env::temp_dir().join(format!("odin-read-ws-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ws = Workspace::open(&dir).unwrap();
        let t = read_target("src/main.rs", Some(&ws));
        assert!(t.ends_with(r"src/main.rs") || t.ends_with(r"src\main.rs"), "{t}");
        assert!(!t.starts_with(r"\\?\"), "shown to a person, so no verbatim prefix: {t}");
        // Absolute paths are left alone; with no folder, nothing is joined.
        assert_eq!(read_target(r"C:\x\y.txt", Some(&ws)), r"C:\x\y.txt");
        assert_eq!(read_target("src/main.rs", None), "src/main.rs");
        let _ = std::fs::remove_dir_all(dir);
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
}
