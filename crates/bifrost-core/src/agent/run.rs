//! Running a call: a read, a listing or a search, whether the card was approved
//! or the call needed no approval. What a run produced is a [`Ran`].
//!
//! [`run_approved`] runs a call somebody approved, by spending its ticket
//! ([`run_approved_in`] does it in another [`Env`]); the other functions are what it
//! and the automatic calls share.

use serde::Serialize;

use super::env::Env;
use super::stop::stopped;
use super::ticket::{Find, Payload, plan_name, redeem};
use crate::{check, edit, search, tools};

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
    /// For a check that ran: how it ended, and what it wrote. Kept for a probe's
    /// counts — a check that timed out told the model nothing about its code — and for
    /// the turn loop, which tells a check that failed from one that never judged the code
    /// (`turn::Frontend::insist_on_checks`). **Never serialized**: Mimir does not read it,
    /// and the webview is not sent it.
    #[serde(skip)]
    pub check: Option<check::Outcome>,
}

impl Ran {
    /// A result worded by a tool. **Whether it worked is read from its first word,
    /// which is sound because of the contract in `tools`'s header (R-25): a result
    /// starts `error:` if and only if the call failed**, and a read of a file that
    /// itself begins that way is given a header so it does not.
    pub fn text(result: String) -> Self {
        let ok = !result.trim_start().starts_with("error:");
        Self {
            result,
            ok,
            stopped: false,
            undo: None,
            check: None,
        }
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
        Ok(text) => Ran {
            stopped: stopped(stop),
            ..Ran::text(text)
        },
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
            Some((
                "list_files",
                shown,
                None,
                run_find(&ws.root_display(), &Find::List { pattern }, stop),
            ))
        }
        tools::Auto::Search { text, pattern } => {
            let shown = search::describe_search(&text, pattern.as_deref());
            Some((
                "search",
                shown,
                None,
                run_find(&ws.root_display(), &Find::Search { text, pattern }, stop),
            ))
        }
        // Answered by the caller before it gets here: it needs no folder.
        tools::Auto::Plan { steps } => Some((
            "update_plan",
            String::new(),
            None,
            Ran::text(tools::plan_message(&steps)),
        )),
    }
}

/// Run a call somebody has just approved.
///
/// Redeems the ticket and runs **what the ticket holds** — for a read, the file
/// the card showed; for an edit, the plan computed when the diff was shown; for a
/// check, the folder and argv the card showed. The caller names the call it
/// approved; it never supplies what to write or what to run.
///
/// **A check can be stopped.** Its ticket holds a number from the same counter as
/// a streamed answer, so the Stop button that ends an answer ends a check too —
/// including one stopped before it had started.
///
/// Blocking: an edit waits for [`edits_lock`], and a check runs to its end.
pub fn run_approved(name: &str, path: &str, token: &str) -> Result<Ran, String> {
    run_approved_in(name, path, token, &Env::default())
}

/// [`run_approved`] in another setting: an edit's backups go where `env` says, and a
/// check runs with `env`'s additions to its environment. What is run, and the ticket
/// that must allow it, are the same.
pub fn run_approved_in(name: &str, path: &str, token: &str, env: &Env) -> Result<Ran, String> {
    // Before anything touches the disk. The ticket was issued when the UI was
    // shown this exact call, and is spent here - so what runs is what was
    // approved, and it runs once.
    let payload = redeem(token, name, path)?;
    match (name, payload) {
        ("read_file", Some(Payload::Read { real, range })) => Ok(run_read(&real, range)),
        ("edit_file" | "create_file" | "delete_file" | "move_file", Some(Payload::Edit(plan))) => {
            if plan_name(&plan) != name {
                return Err(format!("this approval was for {}, not {name}", plan_name(&plan)));
            }
            let backups = env
                .backups
                .clone()
                .or_else(edit::backups_dir)
                .ok_or("APPDATA is not set")?;
            let _edits = edits_lock();
            // A file changed since the diff was shown is refused, not written:
            // the model is told, and can read it again.
            Ok(match edit::apply(&plan, &backups) {
                Ok(done) => {
                    let said = tools::applied_message(&done);
                    Ran {
                        undo: Some(done.id),
                        ..Ran::text(said)
                    }
                }
                Err(e) => Ran::text(format!("error: {e}")),
            })
        }
        ("run_check", Some(Payload::Check { root, argv, stop })) => {
            if stopped(stop) {
                return Ok(Ran {
                    result: format!("`{}` was stopped by the user before it ran.", check::label(&argv)),
                    ok: false,
                    stopped: true,
                    undo: None,
                    check: None,
                });
            }
            let root = std::path::PathBuf::from(root);
            Ok(
                match check::run(&root, &argv, check::timeout_for(&argv), &env.check_env, || {
                    stopped(stop)
                }) {
                    Ok(o) => Ran {
                        result: check::for_model(&argv, &o),
                        ok: o.passed(),
                        stopped: o.ended == check::Ended::Stopped,
                        undo: None,
                        check: Some(o),
                    },
                    Err(e) => Ran::text(format!("error: {e}")),
                },
            )
        }
        (
            "run_command",
            Some(Payload::Command {
                root,
                shell,
                command,
                stop,
            }),
        ) => {
            if stopped(stop) {
                return Ok(Ran {
                    result: format!("`{command}` was stopped by the user before it ran."),
                    ok: false,
                    stopped: true,
                    undo: None,
                    check: None,
                });
            }
            let root = std::path::PathBuf::from(root);
            Ok(
                match check::run_shell(&root, shell, &command, env.command_timeout, &env.check_env, || {
                    stopped(stop)
                }) {
                    Ok(o) => Ran {
                        result: check::for_command(shell, &command, &o),
                        ok: o.passed(),
                        stopped: o.ended == check::Ended::Stopped,
                        undo: None,
                        check: Some(o),
                    },
                    Err(e) => Ran::text(format!("error: {e}")),
                },
            )
        }
        ("list_files" | "search", Some(Payload::Find { root, find, stop })) => Ok(run_find(&root, &find, stop)),
        (other, _) => Err(format!("no such tool: {other}")),
    }
}

/// Every write to a project file — an edit, an undo, a turn's undo — takes this
/// first, so two of them never touch the same file at once. Two undos from two
/// clicks, or an undo while the next edit is being applied, would otherwise each
/// pass their own "is the file still what I expect" check and then write over
/// one another. It lives with the code that applies an approved edit rather than
/// in `edit.rs` because the tests and probes there apply edits in parallel on
/// purpose.
static EDITS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A panic while holding the lock leaves nothing half-written that the next
/// holder's own checks would not catch, so a poisoned lock is simply taken.
pub fn edits_lock() -> std::sync::MutexGuard<'static, ()> {
    EDITS.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::stop::{STREAMS_STOPPED, next_stop};
    use std::sync::atomic::Ordering;

    /// The lock is what keeps two writers off one file, so a second taker must
    /// wait while the first holds it, and a panic in a holder must not wedge it.
    #[test]
    fn the_edits_lock_is_exclusive_and_survives_a_panicking_holder() {
        {
            let _held = edits_lock();
            assert!(
                EDITS.try_lock().is_err(),
                "a second writer got in while the lock was held"
            );
        }
        let _ = std::thread::spawn(|| {
            let _held = edits_lock();
            panic!("a writer died holding the lock");
        })
        .join();
        drop(edits_lock());
    }

    /// Run as `run_approved` runs it, the ticket's range is what is read.
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
        let ran = run_find(
            &root,
            &Find::Search {
                text: "needle".into(),
                pattern: None,
            },
            stop,
        );
        assert!(ran.stopped);
        assert!(!ran.result.contains("a.txt:1"), "{}", ran.result);
        assert!(ran.result.contains("stopped"), "{}", ran.result);
        // And a number taken after the Stop is not caught by it.
        let later = run_find(
            &root,
            &Find::Search {
                text: "needle".into(),
                pattern: None,
            },
            next_stop(),
        );
        assert!(!later.stopped && later.result.contains("a.txt:1"), "{}", later.result);
        let _ = std::fs::remove_dir_all(dir);
    }
}
