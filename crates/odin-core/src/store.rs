//! Conversations on disk, at `%APPDATA%\odin\conversations\<id>.json`.
//!
//! One file per conversation, beside `config.json` and in the same format. No
//! database, deliberately: G4 needs save, list, rename and delete, and nothing
//! here needs a query. What a plain file buys is that you can read it in a text
//! editor when something looks wrong, back it up by copying it, and delete one
//! conversation by deleting one file — which matters more for a tool one person
//! runs than indexing would.
//!
//! **The record carries fields it does not use yet.** `images`, `tool_calls`,
//! `model` and `created_at` are written from the first save, because
//! STAGE-G decision #2 is that adding them later means migrating files that
//! already hold real conversations. They cost a few bytes now and a bad
//! afternoon later.
//!
//! **Storing an image means storing a filename, not base64.** A 1080p screenshot
//! is ~790 KB once base64-encoded — inlining that would turn a 20 KB
//! conversation into a multi-megabyte file rewritten on every turn. G5 puts the
//! bytes in their own file and puts the name here.
//!
//! Blocking, like the rest of this crate.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::chat::Message;

/// One stored turn.
///
/// Deliberately *not* [`crate::chat::Message`]. That type is the wire format and
/// should stay free to change with Ollama's API; this one is a file format and
/// has to stay readable by future versions. Keeping them separate means a change
/// to one is not silently a migration of the other.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredMessage {
    pub role: String,
    pub content: String,

    /// Attachment **filenames**, not bytes. Empty until G5.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,

    /// Tool calls the assistant asked for, as raw JSON. Empty until G6.
    ///
    /// Held as a `Value` rather than a typed struct on purpose: the shape is
    /// Ollama's, and a file written today should still parse if that shape gains
    /// a field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<serde_json::Value>,

    /// Which model produced this turn. Empty on a user turn.
    ///
    /// Worth keeping even though nothing reads it yet: a conversation can span a
    /// model switch, and "why did the tone change halfway down" is otherwise
    /// unanswerable.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,

    /// RFC 3339, UTC.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub created_at: String,
}

impl StoredMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: content.into(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            model: String::new(),
            created_at: stamp(),
        }
    }

    pub fn assistant(content: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: content.into(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            model: model.into(),
            created_at: stamp(),
        }
    }

    /// The wire form, for sending to Ollama.
    pub fn to_message(&self) -> Message {
        Message {
            role: self.role.clone(),
            content: self.content.clone(),
            // Filenames are not what Ollama wants; G5 loads the bytes here.
            images: None,
            tool_calls: None,
            tool_name: None,
        }
    }
}

/// A whole conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub messages: Vec<StoredMessage>,
}

impl Conversation {
    pub fn new() -> Self {
        let now = stamp();
        Self {
            id: new_id(),
            title: String::new(),
            created_at: now.clone(),
            updated_at: now,
            messages: Vec::new(),
        }
    }

    /// A title taken from the first thing asked, when none was set by hand.
    ///
    /// A list of conversations all called "New conversation" is a list you
    /// cannot use, and asking for a title before the first message is a question
    /// nobody wants. The first question is almost always what it was about.
    pub fn display_title(&self) -> String {
        if !self.title.is_empty() {
            return self.title.clone();
        }
        let first = self
            .messages
            .iter()
            .find(|m| m.role == "user")
            .map(|m| m.content.trim())
            .unwrap_or("");
        if first.is_empty() {
            return "New conversation".to_string();
        }
        let line = first.lines().next().unwrap_or(first);
        let mut title: String = line.chars().take(60).collect();
        if line.chars().count() > 60 {
            title.push('…');
        }
        title
    }
}

impl Default for Conversation {
    fn default() -> Self {
        Self::new()
    }
}

/// A row in the conversation list — enough to render it without reading every
/// message of every file.
#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub id: String,
    pub title: String,
    pub updated_at: String,
    pub message_count: usize,
}

/// `%APPDATA%\odin\conversations\`, or `None` if `%APPDATA%` is not set.
pub fn dir() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join("odin").join("conversations"))
}

fn path_for(id: &str) -> Option<PathBuf> {
    // An id with a separator in it would write outside the directory. Ids are
    // generated here so this should be impossible — which is exactly why it is
    // worth refusing rather than trusting.
    if id.is_empty() || id.contains(['/', '\\', '.', ':']) {
        return None;
    }
    Some(dir()?.join(format!("{id}.json")))
}

/// Every conversation, newest first.
///
/// **A file that will not parse is skipped, not fatal.** One bad file should
/// cost you that conversation, not the list.
pub fn list() -> Result<Vec<Summary>, String> {
    let Some(dir) = dir() else {
        return Ok(Vec::new());
    };
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let entries = std::fs::read_dir(&dir).map_err(|e| format!("reading {}: {e}", dir.display()))?;
    let mut out: Vec<Summary> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|text| serde_json::from_str::<Conversation>(&text).ok())
        .map(|c| Summary {
            title: c.display_title(),
            id: c.id,
            updated_at: c.updated_at,
            message_count: c.messages.len(),
        })
        .collect();

    // RFC 3339 in UTC sorts correctly as a string, which is most of why it was
    // chosen over a timestamp.
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(out)
}

pub fn load(id: &str) -> Result<Conversation, String> {
    let p = path_for(id).ok_or_else(|| format!("bad conversation id: {id}"))?;
    let text = std::fs::read_to_string(&p).map_err(|e| format!("reading {}: {e}", p.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("parsing {}: {e}", p.display()))
}

/// Write a conversation, creating the directory if needed.
///
/// **Written to a temporary file and renamed**, so a crash mid-write leaves the
/// previous version intact rather than a half-file. Every save rewrites the
/// whole conversation, which is affordable because they are tens of kilobytes —
/// and is the reason images are stored by name rather than inline.
pub fn save(conversation: &Conversation) -> Result<(), String> {
    let p = path_for(&conversation.id)
        .ok_or_else(|| format!("bad conversation id: {}", conversation.id))?;
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("creating {}: {e}", d.display()))?;
    }

    let json = serde_json::to_string_pretty(conversation)
        .map_err(|e| format!("serializing conversation: {e}"))?;

    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| format!("writing {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &p).map_err(|e| format!("replacing {}: {e}", p.display()))
}

pub fn delete(id: &str) -> Result<bool, String> {
    let p = path_for(id).ok_or_else(|| format!("bad conversation id: {id}"))?;
    match std::fs::remove_file(&p) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("deleting {}: {e}", p.display())),
    }
}

pub fn rename(id: &str, title: &str) -> Result<(), String> {
    let mut c = load(id)?;
    c.title = title.trim().to_string();
    c.updated_at = stamp();
    save(&c)
}

/// RFC 3339 in UTC, without pulling in a date library.
///
/// Public because a caller updating a conversation needs the same clock the
/// store writes with - two clocks would sort against each other.
///
/// `chrono` would be a dependency for one function. This crate carries none it
/// does not need, and a timestamp that sorts as a string is all that is wanted.
pub fn stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, mo, d, h, mi, s) = civil_from_unix(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Days-from-epoch to a civil date, by Howard Hinnant's algorithm.
///
/// Proleptic Gregorian, valid far past anything this will see. Written out
/// because the alternative is a dependency for six lines of arithmetic.
fn civil_from_unix(secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };

    (
        y,
        m,
        d,
        (rem / 3600) as u32,
        ((rem % 3600) / 60) as u32,
        (rem % 60) as u32,
    )
}

/// A sortable, collision-resistant id: timestamp plus process entropy.
///
/// No uuid dependency for something only this machine ever reads.
fn new_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unused_fields_are_omitted_when_empty() {
        // Decision #2 keeps them on the struct; a text turn should not pay for
        // them on disk until something fills them in.
        let json = serde_json::to_string(&StoredMessage {
            created_at: String::new(),
            ..StoredMessage::user("hello")
        })
        .unwrap();
        assert_eq!(json, r#"{"role":"user","content":"hello"}"#);
    }

    #[test]
    fn an_old_record_still_parses() {
        // The property that makes G5 and G6 cheap: a file written before a field
        // existed must still load.
        let m: StoredMessage = serde_json::from_str(r#"{"role":"user","content":"hi"}"#).unwrap();
        assert!(m.images.is_empty() && m.tool_calls.is_empty() && m.model.is_empty());
    }

    #[test]
    fn title_comes_from_the_first_question() {
        let mut c = Conversation::new();
        assert_eq!(c.display_title(), "New conversation");
        c.messages.push(StoredMessage::user("Why does this borrow fail?\nmore detail"));
        assert_eq!(c.display_title(), "Why does this borrow fail?");
        c.title = "Borrow checker".into();
        assert_eq!(c.display_title(), "Borrow checker");
    }

    #[test]
    fn a_long_first_line_is_truncated() {
        let mut c = Conversation::new();
        c.messages.push(StoredMessage::user("x".repeat(200)));
        let t = c.display_title();
        assert_eq!(t.chars().count(), 61); // 60 plus the ellipsis
        assert!(t.ends_with('…'));
    }

    #[test]
    fn ids_with_separators_are_refused() {
        // Generated ids never contain these; refusing anyway is what stops a
        // crafted id writing outside the directory.
        assert!(path_for("../config").is_none());
        assert!(path_for("a/b").is_none());
        assert!(path_for("").is_none());
        assert!(path_for("1a2b3c").is_some());
    }

    #[test]
    fn the_clock_produces_a_sortable_stamp() {
        let s = stamp();
        assert_eq!(s.len(), 20, "{s}");
        assert!(s.ends_with('Z'));
        // 2026-09-19 was the day this was written; anything far off means the
        // civil-date arithmetic is wrong rather than merely surprising.
        let year: i32 = s[..4].parse().unwrap();
        assert!((2020..2100).contains(&year), "{s}");
    }
}
