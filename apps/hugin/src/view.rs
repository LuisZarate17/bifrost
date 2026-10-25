//! A tool card, put into words: the one line it ends as, and what must be read before
//! it is approved.
//!
//! Kept apart from the [`Frontend`](bifrost_core::agent::turn::Frontend) that tracks
//! the cards, so what a line says can be tested on a card alone.

use bifrost_core::agent::ticket::PendingCall;
use bifrost_core::agent::turn::{Card, CardState};
use bifrost_core::check::Shell;
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
        // The approval showed all of it; the line is a reminder.
        "run_command" => visible(&first_line(&card.path)),
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
            let worked = card.ok == Some(true);
            match card.name.as_str() {
                "read_file" if worked => lines_read(text),
                "edit_file" if worked => "applied".into(),
                "create_file" if worked => "created".into(),
                "delete_file" if worked => "deleted".into(),
                "move_file" if worked => "moved".into(),
                "list_files" if worked => counted(text, "file", "files").unwrap_or_else(|| first_line(text)),
                "search" if worked => hits(text).unwrap_or_else(|| first_line(text)),
                "run_check" => check_outcome(text).unwrap_or_else(|| first_line(text)),
                "run_command" => command_outcome(&card.path, text).unwrap_or_else(|| first_line(text)),
                _ => first_line(text),
            }
        }
    }
}

/// The verb a tool goes by in a line: `read`, `edit`, `check`.
pub fn verb(name: &str) -> String {
    match name {
        "read_file" => "read",
        "list_files" => "list",
        "search" => "search",
        "edit_file" => "edit",
        "create_file" => "create",
        "delete_file" => "delete",
        "move_file" => "move",
        "run_check" => "check",
        "run_command" => "command",
        "update_plan" => "plan",
        other => other,
    }
    .to_string()
}

/// `12 files`, from a first line that starts `12 files match …` (or `1 file matches …`).
fn counted(text: &str, one: &str, many: &str) -> Option<String> {
    let mut words = text.split_whitespace();
    let n: usize = words.next()?.parse().ok()?;
    words.next().filter(|w| w.starts_with(one))?;
    Some(format!("{n} {}", if n == 1 { one } else { many }))
}

/// `6 hits` from `6 matches for … in 2 files:`, `no match` from `No matches for …`.
fn hits(text: &str) -> Option<String> {
    if text.starts_with("No matches") {
        return Some("no match".into());
    }
    counted(text, "match", "matches").map(|c| {
        let n = c.split(' ').next().unwrap_or("0");
        format!("{n} hit{}", if n == "1" { "" } else { "s" })
    })
}

/// `passed`, `failed (exit 2)` or `stopped`, from the sentence a check's result starts with.
fn check_outcome(text: &str) -> Option<String> {
    let head = text.lines().next()?;
    if head.contains("` passed (") {
        Some("passed".into())
    } else if let Some((_, rest)) = head.split_once("` failed (exit ") {
        let code = rest.split([',', ')']).next()?;
        Some(format!("failed (exit {code})"))
    } else if head.contains("` was stopped by the user") {
        Some("stopped".into())
    } else {
        None
    }
}

/// `exited 0`, `exited 1`, `timed out`, `stopped` or `blocked`, from the sentence a command's
/// result starts with: the command in backticks (all of it, whatever it holds), then how it ended.
fn command_outcome(command: &str, text: &str) -> Option<String> {
    let rest = text.strip_prefix('`')?.strip_prefix(command)?.strip_prefix('`')?.trim_start();
    if let Some(after) = rest.strip_prefix("exited ") {
        let code = after.split_whitespace().next()?;
        Some(format!("exited {code}"))
    } else if rest.starts_with("ran for ") {
        Some("timed out".into())
    } else if rest.starts_with("was stopped by the user") {
        Some("stopped".into())
    } else if rest.starts_with("could not finish") {
        Some("blocked".into())
    } else if rest.starts_with("ended without") {
        Some("no exit code".into())
    } else {
        None
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
/// A command as a person must see it before saying yes: every character that would act
/// on the terminal instead of showing — a carriage return that redraws the line, an
/// escape sequence that recolours or erases it, a tab — and every one that is there
/// but invisible — a direction override, a zero-width space — written out as an escape
/// (`\r`, `\t`, `\u{1b}`, `\u{202e}`), so what is shown is what runs. Lines are
/// split at `\n` before this. Only for showing: the command that runs is never touched.
pub fn visible(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        let hidden = matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{feff}');
        match c {
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            c if c.is_control() || hidden => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

pub fn first_line(text: &str) -> String {
    let line = text.trim_start().lines().next().unwrap_or("").trim_end();
    if line.chars().count() <= OUTCOME_CHARS {
        return line.to_string();
    }
    let cut: String = line.chars().take(OUTCOME_CHARS - 1).collect();
    format!("{cut}…")
}

/// `hugin · <folder> · <branch>, <n> changed · <model> · <shell>`: the first line of a session.
/// Outside a repository the branch and the count are left out; so is an empty shell.
pub fn header(folder: &str, branch: Option<&str>, changed: Option<usize>, model: &str, shell: &str) -> String {
    let git = git_note(branch, changed).map_or(String::new(), |g| format!(" · {g}"));
    let shell = if shell.is_empty() { String::new() } else { format!(" · {shell}") };
    format!("hugin · {}{git} · {model}{shell}", folder_name(folder))
}

/// The last part of the folder's path: the name a person calls it.
pub fn folder_name(folder: &str) -> String {
    std::path::Path::new(folder)
        .file_name()
        .map_or(folder.to_string(), |n| n.to_string_lossy().into_owned())
}

/// `<branch>, <n> changed`; None outside a repository.
pub fn git_note(branch: Option<&str>, changed: Option<usize>) -> Option<String> {
    match (branch, changed) {
        (Some(b), Some(n)) => Some(format!("{b}, {n} changed")),
        (Some(b), None) => Some(b.to_string()),
        _ => None,
    }
}

/// The card as the line it ends as.
pub fn tool_line(card: &Card) -> ToolLine {
    ToolLine { name: verb(&card.name), target: target(card), outcome: outcome(card) }
}

/// What a command allowed for the session shows in place of a question.
pub const ALLOWED_NOTE: &str = "allowed for this session";

/// What a turn says when it ends with the model's last check still failing, whatever the
/// model's own last words were.
pub const NOT_DONE: &str = "the check failed — not done";

/// A command asked about: where it runs, then **the command exactly as it will run, every
/// line of it** — never cut, whatever its length (the interface wraps; this does not).
pub fn command_approval(command: &str, shell: Shell) -> Approval {
    Approval {
        name: "command".into(),
        target: format!("({})", shell.label()),
        detail: command.split('\n').map(visible).collect(),
    }
}

/// The card, and the call that asked, as what a person reads before answering.
pub fn approval(card: &Card, call: &PendingCall) -> Approval {
    let mut detail = Vec::new();
    if let Some(diff) = &call.diff {
        let mut lines: Vec<String> = Vec::new();
        for (n, hunk) in diff.hunks.iter().enumerate() {
            if n > 0 {
                lines.push("⋯".into());
            }
            lines.extend(
                hunk.lines
                    .iter()
                    .map(|l| format!("{}{}", if l.tag.is_empty() { " " } else { l.tag.as_str() }, l.text)),
            );
        }
        if lines.len() > DIFF_LINES {
            let more = lines.len() - DIFF_LINES;
            lines.truncate(DIFF_LINES);
            lines.push(format!("… {more} more lines"));
        }
        detail = lines;
    }
    match call.name.as_str() {
        // The command is the target already; it is repeated only if they differ.
        "run_check" => {
            if call.path != target(card) {
                detail.push(call.path.clone());
            }
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
    Approval { name: verb(&card.name), target: target(card), detail }
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
        // The command is the target, so it is not said twice.
        assert_eq!((a.name.as_str(), a.target.as_str()), ("check", "cargo check --workspace"));
        assert_eq!(a.detail.len(), 1);
        assert!(a.detail[0].contains("Cargo.toml"));
    }

    #[test]
    fn two_hunks_are_parted_by_an_ellipsis_line() {
        let mut p = call("edit_file", "a.rs");
        let hunks = vec![Hunk { lines: vec![diff_line("-", "a")] }, Hunk { lines: vec![diff_line("+", "b")] }];
        p.diff = Some(DiffView { hunks, added: 1, removed: 1, create: false });
        let a = approval(&Card::new("edit_file", "a.rs", CardState::Pending), &p);
        assert_eq!(a.detail, ["-a", "⋯", "+b"]);
    }

    #[test]
    fn tools_go_by_their_verbs() {
        let names = ["read_file", "list_files", "search", "edit_file", "create_file", "delete_file", "move_file", "run_check", "update_plan"];
        let verbs: Vec<_> = names.iter().map(|n| verb(n)).collect();
        assert_eq!(verbs, ["read", "list", "search", "edit", "create", "delete", "move", "check", "plan"]);
        assert_eq!(tool_line(&done("edit_file", "a.rs", "Edited a.rs.")).name, "edit");
    }

    #[test]
    fn a_change_that_worked_says_what_became_of_the_file() {
        for (name, said) in [("edit_file", "applied"), ("create_file", "created"), ("delete_file", "deleted"), ("move_file", "moved")] {
            assert_eq!(outcome(&done(name, "a", "Done a.")), said);
        }
        assert_eq!(outcome(&done("edit_file", "a", "error: a.rs changed since the diff was shown")), "error: a.rs changed since the diff was shown");
    }


    #[test]
    fn a_listing_and_a_search_say_how_many_and_otherwise_their_first_line() {
        assert_eq!(outcome(&done("list_files", "x", "12 files match all files:\na.rs")), "12 files");
        assert_eq!(outcome(&done("list_files", "x", "1 file matches *.rs:\na.rs")), "1 file");
        assert_eq!(outcome(&done("list_files", "x", "No files match *.zz.")), "No files match *.zz.");
        assert_eq!(outcome(&done("search", "x", "6 matches for \"x\" in 2 files:\na.rs:1: x")), "6 hits");
        assert_eq!(outcome(&done("search", "x", "1 match for \"x\" in 1 file:\na.rs:1: x")), "1 hit");
        assert_eq!(outcome(&done("search", "x", "No matches for \"x\".")), "no match");
        assert_eq!(outcome(&done("search", "x", "something else\nmore")), "something else");
    }

    #[test]
    fn a_check_passed_failed_or_was_stopped() {
        assert_eq!(outcome(&done("run_check", "c", "`cargo check` passed (exit 0, 1.2 s).")), "passed");
        assert_eq!(outcome(&done("run_check", "c", "`cargo check` failed (exit 101, 3.0 s). Output:\n\nerror")), "failed (exit 101)");
        assert_eq!(outcome(&done("run_check", "c", "`cargo check` was stopped by the user after 0.4 s.")), "stopped");
        let late = "`cargo check` ran for 600 s, past its time limit, and was stopped.";
        assert_eq!(outcome(&done("run_check", "c", late)), late);
    }

    #[test]
    fn the_header_names_the_folder_and_the_branch_only_inside_a_repository() {
        assert_eq!(header(r"C:\work\demo", Some("main"), Some(2), "m", ""), "hugin · demo · main, 2 changed · m");
        assert_eq!(header(r"C:\work\demo", None, None, "m", ""), "hugin · demo · m");
        assert_eq!(header(r"C:\work\demo", None, None, "m", "bash"), "hugin · demo · m · bash");
    }
}

#[cfg(test)]
mod command_tests {
    use super::*;

    fn ran(command: &str, text: &str) -> Card {
        let mut c = Card::new("run_command", command, CardState::Ran);
        c.ok = Some(text.contains("exited 0"));
        c.result = Some(text.into());
        c
    }

    #[test]
    fn a_command_line_names_its_first_line_and_how_it_ended() {
        let multi = "echo `hi`\nexit 3";
        let line = tool_line(&ran(multi, &format!("`{multi}` exited 3 (0.1 s). Output:\n\nhi")));
        assert_eq!((line.name.as_str(), line.target.as_str(), line.outcome.as_str()), ("command", "echo `hi`", "exited 3"));
        for (how, want) in [
            ("exited 0 (1.2 s).", "exited 0"),
            ("ran for 120 s, past its time limit, and was stopped.", "timed out"),
            ("was stopped by the user after 2.0 s.", "stopped"),
            ("could not finish: Windows Smart App Control blocked a program.", "blocked"),
        ] {
            assert_eq!(tool_line(&ran("make", &format!("`make` {how}"))).outcome, want);
        }
        let long = format!("echo {}", "y".repeat(200));
        assert_eq!(tool_line(&ran(&long, &format!("`{long}` exited 0 (0.1 s)."))).target.chars().count(), 80);
    }

    #[test]
    fn a_commands_approval_holds_every_line_and_names_the_shell() {
        let text = format!("{}\n  indented\n\nlast", "z".repeat(300));
        let a = command_approval(&text, Shell::Bash);
        assert_eq!((a.name.as_str(), a.target.as_str()), ("command", "(Git Bash)"));
        assert_eq!(a.detail.join("\n"), text);
    }

    /// A command's approval shows every character that would act on the terminal or hide
    /// itself, as an escape — so a carriage return cannot redraw `echo ok` over what
    /// really runs (review I3, finding 2). The command itself is untouched.
    #[test]
    fn an_approval_shows_control_and_hidden_characters_as_escapes() {
        let sneaky = "echo ok\rrm -rf build\tx\u{1b}[2K\u{202e}y\u{200b}z\nsecond";
        let a = command_approval(sneaky, Shell::Bash);
        assert_eq!(a.detail, ["echo ok\\rrm -rf build\\tx\\u{1b}[2K\\u{202e}y\\u{200b}z", "second"]);
        assert!(a.detail.iter().all(|l| !l.chars().any(|c| c.is_control())));
        assert_eq!(visible("plain text, é and 中"), "plain text, é and 中");
    }
}
