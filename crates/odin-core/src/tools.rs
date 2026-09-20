//! The one local tool, and the rule about what it may touch.
//!
//! **Every tool is code the model chose to run.** That is the whole reason this
//! module is small and boring: one function, one argument, and a check that
//! cannot be skipped. G0 measured that the model picks tools well
//! (`qwen3-vl:8b` 10/10, including four cases where calling one would have been
//! wrong) — but "picks well" is not "may be trusted with a filesystem".
//!
//! **Nothing runs without an explicit, per-call approval from the UI.** There is
//! no allowlist in a config file, no remembered folder, no "always allow". The
//! app shows the exact path and waits. That is deliberately the most annoying
//! design that is still usable, because the alternative is a model reading
//! whatever it names and nobody noticing.
//!
//! MCP is a transport over this loop, not a replacement for it — the packet is
//! explicit that a loop which already works is far easier to debug than one
//! arriving with a protocol attached. So: one tool, hardcoded, no MCP.

use serde_json::json;

use crate::chat::{Tool, ToolCall};

/// How much of a file is worth reading into a conversation.
///
/// A 32k context is about 128 KB of text at four characters a token, and the
/// history and the answer need room too. Refused rather than truncated: half a
/// file answered confidently is worse than being told it was too big.
pub const MAX_READ_BYTES: u64 = 64 * 1024;

/// The tools offered to the model. Exactly one.
pub fn available() -> Vec<Tool> {
    vec![Tool::function(
        "read_file",
        "Read a text file from this machine and return its contents. \
         Only use this when the user refers to a specific file. \
         The user must approve each read before it happens.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute path to the file to read"
                }
            },
            "required": ["path"]
        }),
    )]
}

/// What the model asked for, in a form the UI can show before anything happens.
#[derive(Debug, Clone, PartialEq)]
pub struct Requested {
    pub name: String,
    /// The single argument that matters, extracted for display. Showing raw
    /// JSON to someone deciding whether to approve is how approval becomes a
    /// reflex rather than a decision.
    pub path: String,
}

/// Read a call, or say why it cannot be honoured.
///
/// **An unknown tool is refused here, not at execution.** The model can name
/// anything it likes; only this list is real.
pub fn interpret(call: &ToolCall) -> Result<Requested, String> {
    let name = call.function.name.clone();
    if name != "read_file" {
        return Err(format!("no such tool: {name}"));
    }
    let path = call.function.arguments["path"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    if path.is_empty() {
        return Err("read_file was called without a path".to_string());
    }
    Ok(Requested { name, path })
}

/// Run an **already approved** call.
///
/// The approval is the caller's to obtain and cannot be inferred from anything
/// here — which is why this takes a plain path and does no policy of its own
/// beyond refusing what it cannot sensibly return.
///
/// Errors come back as text for the model rather than as a failure, because a
/// tool that cannot answer is a normal event in a conversation: the model should
/// be told "that file does not exist" and continue, not have the turn collapse.
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
        // Lossy on purpose. A file with one stray byte should still be readable
        // rather than refused, and the replacement character is visible in the
        // answer if it matters.
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) => format!("error: cannot read {path}: {e}"),
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
    fn only_the_tool_we_offer_is_accepted() {
        // The model can name anything; only this list is real.
        assert!(interpret(&call("rm_rf", json!({ "path": "/" }))).is_err());
        assert!(interpret(&call("read_file", json!({ "path": "a.txt" }))).is_ok());
    }

    #[test]
    fn a_call_without_a_path_is_refused() {
        assert!(interpret(&call("read_file", json!({}))).is_err());
        assert!(interpret(&call("read_file", json!({ "path": "  " }))).is_err());
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
    fn a_real_file_comes_back() {
        let mut p = std::env::temp_dir();
        p.push("odin-tools-test.txt");
        std::fs::write(&p, "hello from a tool").unwrap();
        assert_eq!(run_read_file(p.to_str().unwrap()), "hello from a tool");
        let _ = std::fs::remove_file(p);
    }
}
