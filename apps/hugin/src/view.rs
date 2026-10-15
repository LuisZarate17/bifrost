//! A tool card, put into words: the one line it ends as, and what must be read before
//! it is approved.
//!
//! Kept apart from the [`Frontend`](bifrost_core::agent::turn::Frontend) that tracks
//! the cards, so what a line says can be tested on a card alone.

use bifrost_core::agent::ticket::PendingCall;
use bifrost_core::agent::turn::{Card, CardState};
use bifrost_core::tools::Lines;

use crate::ui::{Approval, ToolLine};

/// The longest outcome shown, in characters. A result can be pages; the line is a
/// reminder of what happened, and the model has the rest.
const OUTCOME_CHARS: usize = 80;

/// The most diff lines an approval shows before saying how many it left out.
const DIFF_LINES: usize = 40;

/// What the call is about: a read's path and lines, an edit's path with what it adds
/// and removes, a move's `from → to`; for a search, a listing and a check, what the
/// card already says.
pub fn target(card: &Card) -> String {
    match card.name.as_str() {
        "read_file" => match card.range {
            Some(range) => format!("{}:{}", card.path, span(range)),
            None => card.path.clone(),
        },
        "edit_file" | "create_file" => match &card.diff {
            Some(d) => format!("{} +{} −{}", card.path, d.added, d.removed),
            None => card.path.clone(),
        },
        "move_file" => match &card.to {
            Some(to) => format!("{} → {to}", card.path),
            None => card.path.clone(),
        },
        _ => card.path.clone(),
    }
}

/// `10-20`, or `10-` for the rest of the file.
fn span(range: Lines) -> String {
    match range.end {
        Some(end) => format!("{}-{end}", range.start),
        None => format!("{}-", range.start),
    }
}

/// How it ended. **The card's state wins** over its text: a call the person declined
/// or Stop cut short has no result worth quoting.
pub fn outcome(card: &Card) -> String {
    match card.state {
        CardState::Denied => "declined".into(),
        CardState::Stopped => "stopped".into(),
        CardState::Failed => {
            let why = card.error.as_deref().or(card.result.as_deref()).unwrap_or("");
            format!("failed: {}", first_line(why))
        }
        CardState::Rejected => first_line(card.error.as_deref().unwrap_or("")),
        CardState::Pending | CardState::Running | CardState::Ran => {
            let text = card.result.as_deref().unwrap_or("");
            if card.name == "read_file" && card.ok == Some(true) {
                lines_read(text)
            } else {
                first_line(text)
            }
        }
    }
}

/// `12 lines`: the lines a read returned, not counting the `[lines A–B of L]` header
/// a ranged read starts with.
fn lines_read(text: &str) -> String {
    let all = text.lines().count();
    let n = if text.starts_with("[lines ") { all - 1 } else { all };
    format!("{n} line{}", if n == 1 { "" } else { "s" })
}

/// The first line of a result, cut to [`OUTCOME_CHARS`].
fn first_line(text: &str) -> String {
    let line = text.trim_start().lines().next().unwrap_or("").trim_end();
    if line.chars().count() <= OUTCOME_CHARS {
        return line.to_string();
    }
    let cut: String = line.chars().take(OUTCOME_CHARS - 1).collect();
    format!("{cut}…")
}

/// The card as the line it ends as.
pub fn tool_line(card: &Card) -> ToolLine {
    ToolLine { name: card.name.clone(), target: target(card), outcome: outcome(card) }
}

/// The card, and the call that asked, as what a person reads before answering.
pub fn approval(card: &Card, call: &PendingCall) -> Approval {
    let mut detail = Vec::new();
    if let Some(diff) = &call.diff {
        let mut lines: Vec<String> = diff
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .map(|l| format!("{}{}", if l.tag.is_empty() { " " } else { l.tag.as_str() }, l.text))
            .collect();
        if lines.len() > DIFF_LINES {
            let more = lines.len() - DIFF_LINES;
            lines.truncate(DIFF_LINES);
            lines.push(format!("… {more} more lines"));
        }
        detail = lines;
    }
    match call.name.as_str() {
        // The whole command, which the line above may have cut.
        "run_check" => {
            detail.push(call.path.clone());
            if !call.changed_build_files.is_empty() {
                detail.push(format!("runs build files changed this turn: {}", call.changed_build_files.join(", ")));
            }
        }
        "read_file" => detail.push(call.path.clone()),
        "move_file" => {
            if let Some(to) = &call.to {
                detail.push(format!("to {to}"));
            }
        }
        _ => {}
    }
    if matches!(call.name.as_str(), "create_file" | "move_file") && !call.folders.is_empty() {
        detail.push(format!("makes folders: {}", call.folders.join(", ")));
    }
    Approval { name: card.name.clone(), target: target(card), detail }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bifrost_core::agent::ticket::DiffView;
    use bifrost_core::edit::{DiffLine, Hunk};

    fn done(name: &str, path: &str, result: &str) -> Card {
        let mut c = Card::new(name, path, CardState::Ran);
        c.ok = Some(!result.starts_with("error:"));
        c.result = Some(result.into());
        c
    }

    fn diff_line(tag: &str, text: &str) -> DiffLine {
        DiffLine { tag: tag.into(), text: text.into(), old: None, new: None }
    }

    fn call(name: &str, path: &str) -> PendingCall {
        PendingCall {
            token: "t".into(),
            name: name.into(),
            path: path.into(),
            diff: None,
            range: None,
            changed_build_files: Vec::new(),
            to: None,
            folders: Vec::new(),
            size: None,
        }
    }

    #[test]
    fn a_read_says_its_lines_and_how_many_came_back() {
        let mut c = done("read_file", "src/a.rs", "[lines 3–4 of 9]\nfn a() {}\n}\n");
        c.range = Some(Lines { start: 3, end: Some(4) });
        let line = tool_line(&c);
        assert_eq!((line.target.as_str(), line.outcome.as_str()), ("src/a.rs:3-4", "2 lines"));
        assert_eq!(outcome(&done("read_file", "a", "one\n")), "1 line");
        let mut open = done("read_file", "a", "x");
        open.range = Some(Lines { start: 5, end: None });
        assert_eq!(target(&open), "a:5-");
    }

    #[test]
    fn a_long_first_line_is_cut_and_a_failed_read_quotes_its_error() {
        let long = format!("{}\nsecond", "x".repeat(200));
        let out = outcome(&done("run_check", "cargo check", &long));
        assert_eq!(out.chars().count(), OUTCOME_CHARS);
        assert!(out.ends_with('…'));
        assert_eq!(outcome(&done("read_file", "a", "error: no such file")), "error: no such file");
    }

    #[test]
    fn the_card_state_decides_declined_stopped_and_failed() {
        let mut c = Card::new("edit_file", "a.rs", CardState::Denied);
        assert_eq!(outcome(&c), "declined");
        c.state = CardState::Stopped;
        assert_eq!(outcome(&c), "stopped");
        c.state = CardState::Failed;
        c.error = Some("error: a.rs changed since the diff was shown\nmore".into());
        assert_eq!(outcome(&c), "failed: error: a.rs changed since the diff was shown");
    }

    #[test]
    fn an_edit_and_a_move_name_what_they_do_in_the_target() {
        let mut e = Card::new("edit_file", "a.rs", CardState::Pending);
        e.diff = Some(DiffView { hunks: Vec::new(), added: 2, removed: 1, create: false });
        assert_eq!(target(&e), "a.rs +2 −1");
        let mut m = Card::new("move_file", "a.rs", CardState::Pending);
        m.to = Some("b/a.rs".into());
        assert_eq!(target(&m), "a.rs → b/a.rs");
    }

    #[test]
    fn an_approval_shows_the_diff_capped_and_a_checks_whole_command() {
        let mut p = call("edit_file", "a.rs");
        let mut lines = vec![diff_line(" ", "keep")];
        lines.extend((0..44).map(|n| diff_line("+", &format!("new {n}"))));
        p.diff = Some(DiffView { hunks: vec![Hunk { lines }], added: 44, removed: 0, create: false });
        let card = Card::new("edit_file", "a.rs", CardState::Pending);
        let a = approval(&card, &p);
        assert_eq!(a.detail.len(), DIFF_LINES + 1);
        assert_eq!((a.detail[0].as_str(), a.detail[1].as_str()), (" keep", "+new 0"));
        assert_eq!(a.detail[DIFF_LINES], "… 5 more lines");

        let mut c = call("run_check", "cargo check --workspace");
        c.changed_build_files = vec!["Cargo.toml".into()];
        let a = approval(&Card::new("run_check", "cargo check --workspace", CardState::Pending), &c);
        assert_eq!(a.detail[0], "cargo check --workspace");
        assert!(a.detail[1].contains("Cargo.toml"));
    }
}
