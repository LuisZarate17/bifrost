//! The approval boundary. A ticket is issued for one call and spent on that call
//! only, and what runs afterwards is what the ticket holds — never anything the
//! frontend sends back.

use serde::Serialize;

use crate::{edit, tools};

/// What the model asked to run, for the UI to show before anything happens.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingCall {
    pub name: String,
    /// For a read, the exact path that will be read. For an edit, the file
    /// relative to the project folder.
    pub path: String,
    /// A single-use ticket for exactly this call.
    ///
    /// **Without it `run_approved` would be a filesystem operation that takes
    /// arguments.** The approval would live entirely in the frontend, and
    /// `tools.rs` would be promising a guarantee Rust could not enforce. With
    /// it, the only thing the webview can do is redeem what it was shown.
    pub token: String,
    /// The diff, for an edit. Computed by the same plan [`edit::apply`] writes,
    /// so what is approved and what is written cannot disagree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<DiffView>,
    /// For a read of part of a file: the lines, as Rust normalised them. What the
    /// card shows is what the ticket holds — see [`Payload::Read`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<tools::Lines>,
    /// For a check: build files an edit changed earlier in this turn. A check
    /// runs them, so the card names them before you approve it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changed_build_files: Vec<String>,
    /// For a move: where the file goes, relative to the project folder, as the
    /// plan resolved it. The webview sends back only `(name, path, token)`; the
    /// destination is in the ticket's plan, so what is approved is what is moved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// For a create or a move: the folders it will make, shallowest first, each
    /// with a trailing slash.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub folders: Vec<String>,
    /// For a delete or a move: the size of the file, in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffView {
    pub hunks: Vec<edit::Hunk>,
    pub added: usize,
    pub removed: usize,
    pub create: bool,
}

/// A call that is waiting for a decision.
///
/// **For an edit, the whole plan lives here** — resolved path, exact bytes to
/// write, the hash of the file it was planned against. `run_approved` redeems the
/// ticket and applies *this*. Nothing about the edit is taken back from the
/// webview, so a compromised or buggy page cannot approve one thing and write
/// another. **For a check, the folder and the exact argv live here**, for the
/// same reason: what runs is what the card showed, even if the config changes
/// while it is on screen.
struct Armed {
    token: String,
    name: String,
    path: String,
    payload: Option<Payload>,
}

/// The tool name a plan belongs to. A ticket for one kind of change is never
/// redeemed as another: a delete's plan has no bytes to write, and a move's has
/// a destination an edit's does not.
pub fn plan_name(p: &edit::Plan) -> &'static str {
    match p.op {
        edit::Op::Edit => "edit_file",
        edit::Op::Create => "create_file",
        edit::Op::Delete => "delete_file",
        edit::Op::Move => "move_file",
    }
}

/// What an approval carries beyond its name and path.
#[derive(Debug, Clone, PartialEq)]
pub enum Payload {
    Edit(edit::Plan),
    /// A read that asked: the file the card showed, as `read_target` resolved
    /// it, and the lines the card showed. `run_approved` opens **this**, never a path
    /// or a range from the webview — so an approved `src/a.rs` cannot become a
    /// read of something else, nor lines 1–50 a read of the whole file.
    Read {
        real: std::path::PathBuf,
        range: Option<tools::Lines>,
    },
    /// `stop` is this check's number on the Stop counter, taken **when the card
    /// is shown** rather than when the command starts: a Stop pressed between
    /// Allow and the start — while the approval is still on its way — must
    /// reach it too, or it would run with nothing left to stop it.
    Check {
        root: String,
        argv: Vec<String>,
        stop: u64,
    },
    /// A search or a listing: the folder it was approved for and exactly what
    /// the card showed, with a place on the Stop counter — a walk of a large
    /// folder can take a while, and Stop must reach it.
    Find {
        root: String,
        find: Find,
        stop: u64,
    },
    /// A shell command, Hugin's: the folder, the shell and the exact text the card
    /// showed — which is also the ticket's path, so a ticket for one command cannot
    /// be redeemed for another.
    Command {
        root: String,
        shell: crate::check::Shell,
        command: String,
        stop: u64,
    },
}

/// What a finding call looks for.
#[derive(Debug, Clone, PartialEq)]
pub enum Find {
    List { pattern: Option<String> },
    Search { text: String, pattern: Option<String> },
}

/// The one call currently awaiting approval.
///
/// One slot, not a set: the UI shows a single approval card and waits, so a
/// second pending call would mean a bug rather than a queue. Issuing a new
/// ticket invalidates the previous one.
static PENDING: std::sync::OnceLock<std::sync::Mutex<Option<Armed>>> = std::sync::OnceLock::new();

fn pending() -> &'static std::sync::Mutex<Option<Armed>> {
    PENDING.get_or_init(|| std::sync::Mutex::new(None))
}

/// Issue a ticket for a call, replacing any outstanding one.
pub fn arm(name: &str, path: &str, payload: Option<Payload>) -> String {
    let token = format!(
        "{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    );
    if let Ok(mut slot) = pending().lock() {
        *slot = Some(Armed {
            token: token.clone(),
            name: name.into(),
            path: path.into(),
            payload,
        });
    }
    token
}

/// The decision, without the global slot — so it is testable without one.
///
/// **The slot is taken either way.** A mismatched attempt burns the ticket
/// rather than leaving it for a second try, so a caller cannot guess repeatedly
/// against a standing approval.
fn redeem_in(slot: &mut Option<Armed>, token: &str, name: &str, path: &str) -> Result<Option<Payload>, String> {
    match slot.take() {
        Some(a) if a.token == token && a.name == name && a.path == path => Ok(a.payload),
        _ => Err("this call was not approved".to_string()),
    }
}

/// Spend a ticket. Fails unless it matches the exact call that was approved.
pub fn redeem(token: &str, name: &str, path: &str) -> Result<Option<Payload>, String> {
    let mut slot = pending().lock().map_err(|_| "approval state poisoned")?;
    redeem_in(&mut slot, token, name, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The approval boundary. These were the only tests in Mimir on purpose:
    /// everything else there is a thin `spawn_blocking` wrapper over `bifrost-core`,
    /// which has its own. This is the one place Mimir enforced a rule of its
    /// own, and a rule that is not tested is a comment.
    ///
    /// They drive `redeem_in` with a local slot rather than the process-wide
    /// one, because tests run in parallel and a shared static would make them
    /// interfere with each other instead of with the thing under test.
    fn armed(name: &str, path: &str) -> (Option<Armed>, String) {
        let token = "ticket-1".to_string();
        (
            Some(Armed {
                token: token.clone(),
                name: name.into(),
                path: path.into(),
                payload: None,
            }),
            token,
        )
    }

    #[test]
    fn a_ticket_only_works_for_the_call_it_was_issued_for() {
        let (mut slot, t) = armed("read_file", "C:/a.txt");
        assert!(redeem_in(&mut slot, &t, "read_file", "C:/somewhere-else.txt").is_err());
    }

    #[test]
    fn a_ticket_is_rejected_for_another_tool() {
        let (mut slot, t) = armed("read_file", "C:/a.txt");
        assert!(redeem_in(&mut slot, &t, "delete_file", "C:/a.txt").is_err());
    }

    #[test]
    fn a_matching_ticket_is_accepted_once() {
        let (mut slot, t) = armed("read_file", "C:/a.txt");
        assert!(redeem_in(&mut slot, &t, "read_file", "C:/a.txt").is_ok());
        // A replay must not read the file a second time on one approval.
        assert!(redeem_in(&mut slot, &t, "read_file", "C:/a.txt").is_err());
    }

    #[test]
    fn a_wrong_ticket_burns_the_real_one() {
        let (mut slot, t) = armed("read_file", "C:/a.txt");
        assert!(redeem_in(&mut slot, "guessed", "read_file", "C:/a.txt").is_err());
        assert!(redeem_in(&mut slot, &t, "read_file", "C:/a.txt").is_err());
    }

    #[test]
    fn nothing_is_approved_by_default() {
        let mut slot = None;
        assert!(redeem_in(&mut slot, "anything", "read_file", "C:/a.txt").is_err());
    }

    #[test]
    fn arming_issues_a_ticket_for_exactly_that_call() {
        let token = arm("read_file", "C:/from-arm.txt", None);
        assert!(!token.is_empty());
        // Whatever was outstanding is replaced, so only the newest call stands.
        let slot = pending().lock().unwrap();
        let a = slot.as_ref().expect("a call should be armed");
        assert_eq!(
            (a.token.as_str(), a.name.as_str(), a.path.as_str()),
            (token.as_str(), "read_file", "C:/from-arm.txt")
        );
    }

    /// **The edit that runs is the one that was planned** - the ticket hands back
    /// the stored plan, and nothing about the edit comes from the caller.
    #[test]
    fn an_edit_ticket_redeems_the_plan_it_was_armed_with() {
        let dir = std::env::temp_dir().join(format!("mimir-plan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.txt"),
            "one
",
        )
        .unwrap();
        let ws = edit::Workspace::open(&dir).unwrap();
        let plan = edit::plan_replace(&ws, "a.txt", "one", "two").unwrap();
        let mut slot = Some(Armed {
            token: "t".into(),
            name: "edit_file".into(),
            path: "a.txt".into(),
            payload: Some(Payload::Edit(plan.clone())),
        });
        let got = redeem_in(&mut slot, "t", "edit_file", "a.txt").unwrap();
        assert_eq!(got, Some(Payload::Edit(plan)));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// **A move's destination is in the ticket**, not in what the webview sends
    /// back: redeeming it hands over the plan with `to`, and a different path
    /// does not redeem at all.
    #[test]
    fn a_move_ticket_redeems_its_destination() {
        let dir = std::env::temp_dir().join(format!("mimir-move-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        let ws = edit::Workspace::open(&dir).unwrap();
        let plan = edit::plan_move(&ws, "a.txt", "sub/b.txt").unwrap();
        assert_eq!(plan_name(&plan), "move_file");
        let armed = |plan: &edit::Plan| {
            Some(Armed {
                token: "t".into(),
                name: "move_file".into(),
                path: "a.txt".into(),
                payload: Some(Payload::Edit(plan.clone())),
            })
        };
        let mut slot = armed(&plan);
        let Some(Payload::Edit(got)) = redeem_in(&mut slot, "t", "move_file", "a.txt").unwrap() else {
            panic!("no plan")
        };
        assert_eq!(got.to_display.as_deref(), Some("sub/b.txt"));
        assert_eq!(got.folders, ["sub/"]);
        let mut slot = armed(&plan);
        assert!(redeem_in(&mut slot, "t", "move_file", "other.txt").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A delete's approval cannot be spent as an edit or a create — the name is
    /// part of the ticket, and the plan's own kind is checked again on redeeming.
    #[test]
    fn a_delete_ticket_cannot_be_redeemed_as_an_edit() {
        let dir = std::env::temp_dir().join(format!("mimir-delete-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        let ws = edit::Workspace::open(&dir).unwrap();
        let plan = edit::plan_delete(&ws, "a.txt").unwrap();
        assert_eq!(plan_name(&plan), "delete_file");
        for as_name in ["edit_file", "create_file", "move_file"] {
            let mut slot = Some(Armed {
                token: "t".into(),
                name: "delete_file".into(),
                path: "a.txt".into(),
                payload: Some(Payload::Edit(plan.clone())),
            });
            assert!(redeem_in(&mut slot, "t", as_name, "a.txt").is_err(), "{as_name}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_edit_ticket_cannot_be_redeemed_as_a_read() {
        // Same token and path, different tool: a read must not get the plan, and
        // must not be allowed to run on an edit's approval.
        let mut slot = Some(Armed {
            token: "t".into(),
            name: "edit_file".into(),
            path: "a.txt".into(),
            payload: None,
        });
        assert!(redeem_in(&mut slot, "t", "read_file", "a.txt").is_err());
    }

    fn check_ticket() -> Option<Armed> {
        Some(Armed {
            token: "t".into(),
            name: "run_check".into(),
            path: "cargo check".into(),
            payload: Some(Payload::Check {
                root: r"C:\work\demo".into(),
                argv: vec!["cargo".into(), "check".into()],
                stop: 7,
            }),
        })
    }

    /// **The command that runs is the one the card showed** — folder, argv and
    /// its place on the Stop counter come back from the ticket, and nothing
    /// about them comes from the caller.
    #[test]
    fn a_check_ticket_redeems_the_command_it_was_armed_with() {
        let mut slot = check_ticket();
        let got = redeem_in(&mut slot, "t", "run_check", "cargo check").unwrap();
        assert_eq!(
            got,
            Some(Payload::Check {
                root: r"C:\work\demo".into(),
                argv: vec!["cargo".into(), "check".into()],
                stop: 7
            })
        );
    }

    /// What is searched for comes back from the ticket, as the card showed it —
    /// never from the caller.
    #[test]
    fn a_search_ticket_redeems_what_the_card_showed() {
        let shown = "\"greet(\" in src/**/*.rs";
        let find = Find::Search {
            text: "greet(".into(),
            pattern: Some("src/**/*.rs".into()),
        };
        let payload = Payload::Find {
            root: r"C:\work\demo".into(),
            find,
            stop: 3,
        };
        let armed = || {
            Some(Armed {
                token: "t".into(),
                name: "search".into(),
                path: shown.into(),
                payload: Some(payload.clone()),
            })
        };
        // Not as a read of a file that happens to have that name, nor as a listing.
        assert!(redeem_in(&mut armed(), "t", "read_file", shown).is_err());
        assert!(redeem_in(&mut armed(), "t", "list_files", shown).is_err());
        assert_eq!(
            redeem_in(&mut armed(), "t", "search", shown).unwrap(),
            Some(payload.clone())
        );
    }

    /// **The file that is read is the one that was resolved when the card was
    /// shown** — the ticket hands back that path, and nothing about it comes from
    /// the caller.
    #[test]
    fn a_read_ticket_redeems_the_file_it_was_armed_with() {
        let open = std::path::PathBuf::from(r"C:\work\demo\src\a.rs");
        let shown = r"C:\work\demo\src\a.rs";
        let mut slot = Some(Armed {
            token: "t".into(),
            name: "read_file".into(),
            path: shown.into(),
            payload: Some(Payload::Read {
                real: open.clone(),
                range: None,
            }),
        });
        assert_eq!(
            redeem_in(&mut slot, "t", "read_file", shown).unwrap(),
            Some(Payload::Read {
                real: open,
                range: None
            })
        );
    }

    /// **The lines are the ticket's too**: the webview names the call it approved
    /// and gets back the range the card showed, which it cannot change.
    #[test]
    fn a_read_ticket_redeems_with_its_range() {
        let open = std::path::PathBuf::from(r"C:\work\demo\src\a.rs");
        let shown = r"C:\work\demo\src\a.rs";
        let range = Some(tools::Lines {
            start: 120,
            end: Some(180),
        });
        let payload = Payload::Read { real: open, range };
        let mut slot = Some(Armed {
            token: "t".into(),
            name: "read_file".into(),
            path: shown.into(),
            payload: Some(payload.clone()),
        });
        assert_eq!(redeem_in(&mut slot, "t", "read_file", shown).unwrap(), Some(payload));
    }

    #[test]
    fn a_read_ticket_cannot_be_redeemed_as_a_search() {
        let shown = r"C:\work\demo\src\a.rs";
        let armed = || {
            Some(Armed {
                token: "t".into(),
                name: "read_file".into(),
                path: shown.into(),
                payload: Some(Payload::Read {
                    real: shown.into(),
                    range: None,
                }),
            })
        };
        assert!(redeem_in(&mut armed(), "t", "search", shown).is_err());
        assert!(redeem_in(&mut armed(), "t", "list_files", shown).is_err());
    }

    /// **The command that runs is the exact string that was approved** - folder,
    /// shell and text come back from the ticket, and one differing byte redeems
    /// nothing.
    #[test]
    fn a_command_ticket_is_redeemed_only_for_its_exact_command() {
        let command = "git log --oneline\n  git status ";
        let payload = Payload::Command {
            root: r"C:\work\demo".into(),
            shell: crate::check::Shell::Cmd,
            command: command.into(),
            stop: 9,
        };
        let armed = || {
            Some(Armed {
                token: "t".into(),
                name: "run_command".into(),
                path: command.into(),
                payload: Some(payload.clone()),
            })
        };
        assert_eq!(
            redeem_in(&mut armed(), "t", "run_command", command).unwrap(),
            Some(payload.clone())
        );
        for other in [
            "git log --oneline\n  git status",
            "git log --oneline",
            " git log --oneline\n  git status ",
        ] {
            assert!(redeem_in(&mut armed(), "t", "run_command", other).is_err(), "{other:?}");
        }
        // Not as a check, nor as a read of a file with that name.
        assert!(redeem_in(&mut armed(), "t", "run_check", command).is_err());
        assert!(redeem_in(&mut armed(), "t", "read_file", command).is_err());
    }

    #[test]
    fn a_check_ticket_cannot_be_redeemed_as_a_read() {
        // A read of a file called "cargo check" must not run on a check's approval.
        let mut slot = check_ticket();
        assert!(redeem_in(&mut slot, "t", "read_file", "cargo check").is_err());
    }
}
