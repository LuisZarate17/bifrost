# H6 — Undo a whole turn

One action puts back every edit the model made in a turn. **Built 2026-10-02.**

Until H6, each edit had its own Undo, and a turn that changed three files took three clicks, in
the right order: one file edited twice has to be undone newest first, or the second Undo refuses
because the file no longer matches.

## What it does

- When a turn made **two or more edits that can still be undone**, a line appears at the end of
  the turn: **"3 edits in this turn"** and an **Undo all** button shaped like the cards' own Undo.
  It sits after the answer, or after the last card when the turn ended with Stop or an error — the
  user chose the end of the turn over a button on the answer, which a stopped turn does not have.
  Always visible, not only on hover.
- **Click twice.** The first click turns the button into **"Undo 3 edits?"**; a second click
  within four seconds does it, and after four seconds it goes back. A second click sooner than
  0.4 s after the first is ignored, so a habitual double-click does not skip the question.
- Every card in the turn reads **Undone**, the line goes, and the model is told on each card, as
  with a single Undo.

## Why it is safe

Each applied edit already kept the file as it was before the edit (`{id}.bak`) and the fingerprint
of what it wrote (`{id}.json`). Two edits of one file therefore link: the newer edit's backup is
exactly what the older edit wrote. `edit::undo_turn` uses that:

1. **Check every file's whole chain first, and write nothing if anything is off:** the file still
   matches the turn's last edit of it; each edit's backup matches what the edit before it wrote;
   the oldest backup matches the file as it was before the turn (records now keep that hash too);
   a file the turn created is the oldest link in its chain. A file changed since — by hand, or by a
   later turn — refuses the whole turn, and the message names every file that is off.
2. **Then put the edits back newest first, across all files**, each through the single Undo, which
   checks once more right before it writes.
3. **It cannot be atomic across files.** If a write fails partway — the disk, a file held open by
   an editor — it stops and says exactly which edits it put back. Because it goes newest first, what
   is left is a state that really existed during the turn, and **Undo all** again finishes the job.

**One lock now covers every edit, Undo and Undo all** in the app, so an edit being applied and an
undo, or two undos, can no longer write the same file at the same moment.

**A bug fixed on the way:** the single Undo marked its card by its position in the thread and saved
to the conversation open when it was clicked. Switching conversations while it ran could mark a card
in the other conversation and save it over that conversation's file. Both buttons now share one
helper that marks cards by their undo handle and saves to the conversation the undo came from.

## Checked

| | |
|---|---|
| Unit tests (`edit.rs`) | 19 new: two files; one file edited twice; ids in any order; create then edit; a file changed since refuses and touches nothing; every problem file named; a turn partly undone by hand finishes the rest; a hand edit between two of the turn's edits refuses; an older turn under a newer edit refuses until the newer goes first; a created file deleted by hand, or created again, refuses; a missing backup refuses; already undone is fine; **a write failing partway leaves the rest intact and retryable**; bad, empty and repeated ids; nothing left behind; old records still load |
| Unit tests (`lib.rs`) | `undo_turn` refuses more than 64 ids; the lock |
| All tests | odin-core 227, mimir 25, bench 15 · clippy clean |
| `tools/e2e/mimir-edit.mjs` §5 (new) | **The real model** changed two CRLF files in one turn: the line counts them; the first click only asks and nothing on disk changes; the question lapses after four seconds; a file edited by hand afterwards makes **Undo all** refuse and write nothing; once the hand edit is reverted, both files are back **byte for byte, CRLF kept**; every card reads Undone; reopened, it still does and the line is gone. 25/25 |
| Other e2e | mimir-check 26/26 · mimir-policy 44/44 · mimir-switch 14/14 |
| `?preview=undo` | a three-edit turn ending in an answer and a stopped two-edit turn show the line; a turn with one edit already undone, and one with a card still waiting, do not |

The e2e removes the backup records it made — only those of its own project — and the user's
backups folder was checked unchanged after each run.

## Reviews

A checker read the diff for data loss, the chain check, the lock and wrong-thread saves, and found
none. Fixed from it: the e2e could reopen the conversation before the undo was saved (it now waits
for the save); Ctrl+C left the run's backup records behind; the cleanup could have removed records
of a Mímir session running alongside (now only this run's project); and three cosmetic slips.

The first screenshots showed **Undo all** as grey text in a borderless line, which read as a
caption rather than something to press. The count is now quiet text and the action a button like
the cards' Undo.

## Found on the way

`mimir-switch.mjs` failed twice in this round: it clicked "clear folder" and a recent folder in the
same instant. Both commands read, change and save the config, and when the clear's save landed last
the folder was empty, so no offer could appear. No person clicks that fast; the script now waits
for the clear. The read-change-save race between two config commands is real, though, and worth a
lock of its own if a later chunk makes such commands slower.

## Accepted

- **Not atomic across files** (above): a partial undo leaves a real in-between state and can be
  finished with a second **Undo all**.
- **Undo ids are wall-clock times.** If the clock steps back during a turn, the order check refuses
  — safe, and the edits can still be undone one at a time.
- **Backups are never pruned.** Not part of Stage H.

## Files

`crates/odin-core/src/edit.rs` (`undo_turn`, `TurnUndo`, `Record.before`) ·
`apps/mimir/src-tauri/src/lib.rs` (the lock, `undo_turn`) · `apps/mimir/src/App.tsx` (`turnsOf`,
the line, click twice, the shared undo helper) · `ToolCard.tsx`, `App.css`, `preview.ts` ·
`tools/e2e/mimir-edit.mjs` (§5), `mimir-switch.mjs` (the wait) · `tools/mimir-shots.mjs`
