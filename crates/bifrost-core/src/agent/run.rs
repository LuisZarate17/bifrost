//! Running a call: a read, a listing or a search, whether the card was approved
//! or the call needed no approval. What a run produced is a [`Ran`].

use serde::Serialize;

use super::stop::stopped;
use super::ticket::Find;
use crate::{edit, search, tools};

/// What running an approved call produced.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Ran {
    /// Goes back to the model.
    pub result: String,
    /// Whether it worked. **Said here, not inferred from the text:** a check
    /// that fails has a result that does not start with `error:` — it worked,
    /// and what it found is the news — and the card must still show it failed.
    pub ok: bool,
    /// Ended by Stop rather than finishing.
    pub stopped: bool,
    /// For an edit that was written: the handle to undo it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub undo: Option<String>,
}

impl Ran {
    /// A result worded by a tool. **Whether it worked is read from its first word,
    /// which is sound because of the contract in `tools`'s header (R-25): a result
    /// starts `error:` if and only if the call failed**, and a read of a file that
    /// itself begins that way is given a header so it does not.
    pub fn text(result: String) -> Self {
        let ok = !result.trim_start().starts_with("error:");
        Self { result, ok, stopped: false, undo: None }
    }
}

/// Read a file the user approved — the one the ticket holds, the lines it holds.
pub fn run_read(real: &std::path::Path, range: Option<tools::Lines>) -> Ran {
    Ran::text(tools::run_read_file(real, range))
}

/// Run a listing or a search, for an approved card and for an automatic call
/// alike. The folder is opened again rather than trusted: it may have been moved
/// or deleted since it was chosen, or while the card was on screen.
///
/// `stop` is a place on the Stop counter taken before the call began, so a Stop
/// pressed at any point since reaches the walk — and one already pressed ends it
/// at once, having read nothing.
pub fn run_find(root: &str, find: &Find, stop: u64) -> Ran {
    let ws = match edit::Workspace::open(root) {
        Ok(ws) => ws,
        Err(e) => return Ran::text(format!("error: {e}")),
    };
    let found = match find {
        Find::List { pattern } => search::list(&ws, pattern.as_deref(), || stopped(stop)),
        Find::Search { text, pattern } => search::search(&ws, text, pattern.as_deref(), || stopped(stop)),
    };
    match found {
        Ok(text) => Ran { stopped: stopped(stop), ..Ran::text(text) },
        Err(e) => Ran::text(format!("error: {e}")),
    }
}

/// An automatic call, run: `(tool name, what the card would show, the lines of a
/// read, outcome)`.
/// `None` for a read that could not be made safely — see
/// [`tools::read_unasked`] — which the caller then asks about like any other.
pub fn run_auto(
    ws: &edit::Workspace,
    auto: tools::Auto,
    stop: u64,
) -> Option<(&'static str, String, Option<tools::Lines>, Ran)> {
    match auto {
        tools::Auto::Read { real, range } => {
            let text = tools::read_unasked(&real, range)?;
            // Always inside the folder, so shown the way edit cards show a path.
            Some(("read_file", ws.relative(&real), range, Ran::text(text)))
        }
        tools::Auto::List { pattern } => {
            let shown = search::describe_list(pattern.as_deref());
            Some(("list_files", shown, None, run_find(&ws.root_display(), &Find::List { pattern }, stop)))
        }
        tools::Auto::Search { text, pattern } => {
            let shown = search::describe_search(&text, pattern.as_deref());
            Some(("search", shown, None, run_find(&ws.root_display(), &Find::Search { text, pattern }, stop)))
        }
        // Answered by the caller before it gets here: it needs no folder.
        tools::Auto::Plan { steps } => Some(("update_plan", String::new(), None, Ran::text(tools::plan_message(&steps)))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::stop::{next_stop, STREAMS_STOPPED};
    use std::sync::atomic::Ordering;

    /// Run as `run_tool` runs it, the ticket's range is what is read.
    #[test]
    fn run_read_gives_the_ticket_range_and_the_whole_file_with_none() {
        let dir = std::env::temp_dir().join(format!("mimir-range-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, "one\r\ntwo\r\nthree\r\n").unwrap();
        let part = run_read(&file, Some(tools::Lines { start: 2, end: Some(3) }));
        assert!(part.ok);
        assert_eq!(part.result, "[lines 2–3 of 3]\ntwo\nthree\n");
        assert_eq!(run_read(&file, None).result, "one\ntwo\nthree\n");
        let past = run_read(&file, Some(tools::Lines { start: 9, end: None }));
        assert!(!past.ok && past.result.contains("is past the end"), "{}", past.result);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Stop pressed before an automatic walk starts: it ends at once, says so,
    /// and has read no file.
    #[test]
    fn run_find_with_an_already_stopped_number_reports_stopped_and_reads_nothing() {
        let dir = std::env::temp_dir().join(format!("mimir-stopped-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "needle\n").unwrap();
        let root = edit::Workspace::open(&dir).unwrap().root_display();
        let stop = next_stop();
        STREAMS_STOPPED.fetch_max(stop, Ordering::SeqCst);
        let ran = run_find(&root, &Find::Search { text: "needle".into(), pattern: None }, stop);
        assert!(ran.stopped);
        assert!(!ran.result.contains("a.txt:1"), "{}", ran.result);
        assert!(ran.result.contains("stopped"), "{}", ran.result);
        // And a number taken after the Stop is not caught by it.
        let later = run_find(&root, &Find::Search { text: "needle".into(), pattern: None }, next_stop());
        assert!(!later.stopped && later.result.contains("a.txt:1"), "{}", later.result);
        let _ = std::fs::remove_dir_all(dir);
    }
}
