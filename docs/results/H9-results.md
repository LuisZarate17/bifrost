# H9 — File operations: new folders, delete, move

Mímir can create a file in folders that do not exist yet, delete a file, and move or rename one —
each asked, each undoable, and none able to overwrite anything. **Built 2026-10-03.**

## Why

Until H9 Mímir could edit and create files, but only in folders that already existed (refused on
purpose), and could not delete or move anything. Renaming a module, deleting a dead one or
splitting a file into a sub-module were impossible.

## What it does

- **`create_file` makes missing folders.** The card lists them before you approve ("Makes the
  folders: src/net/, src/net/http/"); undo removes the file and then those folders — only the ones
  this create made, only if empty, never recursively.
- **`delete_file(path)`** — files only. The bytes are backed up first (up to 16 MiB), and undo puts
  them back exactly, CRLF and BOM included.
- **`move_file(path, to)`** — files only, never onto an existing file, making the destination's
  folders. Undo moves it back and removes the folders it made.
- **All three always ask.** Each is undoable on its own card, and with **Undo all** — whose line now
  counts "changes", not "edits".
- **The model is told the limits in the tools' own descriptions:** delete is "not a way to rewrite a
  file — use edit_file", and a move does not update references.

## Nothing overwrites

**Windows' ordinary rename silently replaces an existing destination.** Checking first is not
enough — a file appearing between the check and the rename would be lost with no backup. So every
operation that puts a file somewhere new — a move, moving back on undo, a create, restoring a
delete — goes through one no-replace move (`MoveFileExW` without `REPLACE_EXISTING`, and never a
copy-then-delete fallback). Only edits keep the replacing atomic write, which is what an edit is.

**What is refused, before any card:** anything outside the project, network and device paths (never
even resolved — H5), a new folder named `.git` or `.cargo`, names Windows mishandles (`CON`, a
trailing dot or space), more than 8 new folder levels, a folder as the source, a link or junction as
the source (it would act on the target), a read-only file, and a case-only rename. A move onto `build.rs` is flagged on the next check's card, like an edit of it.

*Corrected 2026-10-09 (review §7): this list used to include a move across drives. That one is not refused before a card. It fails only when applied, after Allow (`ERROR_NOT_SAME_DEVICE`), says so and changes nothing — test `move_across_drives_says_so_and_changes_nothing`.*

**Right before acting,** apply checks again that every folder involved is still where it was planned
(a junction swapped in since would point the action elsewhere) and that a file being deleted or moved
still has the bytes that were approved (an editor saving it in between would otherwise lose its
newest save). Undo checks the folder it restores into the same way.

## Undo all, generalised

H6 checked a chain of edits per file. Deletes and moves break that — "move A to B, then edit B" has
no earlier record on B. Undo all now **replays the turn's changes newest first against a snapshot of
the real files** (every path any of them touched, compared case-insensitively, each absent, a file
with a known hash, or something else), checking each change's after-state and rolling the snapshot
back to its before-state. Edits, creates, deletes and moves alike; if anything does not line up,
nothing is touched and every problem is named. Every H6 test passes unchanged.

## Measured

**opsprobe (new)** — an offline fixture crate, Qwen3-Coder, 3 runs plus 1 at temperature 0:

| Task | App sampling | t=0 |
|---|---|---|
| o01 rename `util.rs` → `helpers.rs`, fix `mod`/`use` | 3/3 | 1/1 |
| o02 delete an unused module and its `mod` line | 3/3 | 1/1 |
| o03 create `src/net/http.rs` in new folders, wired in | 3/3 | 1/1 |
| o04 move `config.rs` into `src/settings/` | 3/3 | 0/1 |
| o05 **trap:** remove an unused *function* — the file must stay | 3/3 | 1/1 |
| **Total** | **15/15** | 4/5 |

**After every run, Undo all over that run's changes gave the fixture back byte for byte with no new
folders: 20 of 20.** (That part is deterministic — a miss would be a bug, not the model.) The trap
task never called `delete_file`; no run deleted and re-created a file. The one miss: at temperature
0 the move worked, but the model wired the module wrong (it needed `settings/mod.rs`) and ran out of
rounds — Rust, not the tool.

**taskprobe, same build, the new tools on and off** — the bar, set before: on ≥ 26/48, no errors:

| | On | Off |
|---|---|---|
| Pass, app sampling | **28/48** | 30/48 |
| Pass, temperature 0 | 10/16 | 8/16 |
| Errors | 0 | 0 |
| Rounds per run | 5.8 | 5.8 |
| Deletes · moves · delete-then-create | **0 · 0 · 0** | — |

It clears the bar; the difference is the run-to-run spread seen since H4 (25–30). **On ordinary coding
tasks the model never touched the new tools** — no stray deletes, no rewrite by delete and re-create.

**findprobe:** 47/54 (H8: 47/54) — named 24/24, vague 16/21, no tool 7/9; 17/18 at temperature 0.
The two no-tool misses were the model reading a file, and once writing its arithmetic into a new
`calculation.txt` — with `create_file`, which asks in the app, the same habit H5 saw with a haiku.

Saved in `evals/out/H9-ops-1791070190/`, `evals/out/H3-task-1791070356/` (on), `…1791072687/` (off),
`evals/out/H2-find-1791075140/`; logs in `evals/out/H9-2026-10-03/`.

## End to end — `tools/e2e/mimir-ops.mjs`, 33/33

1. A file created in two new folders — the card listed them; approved, they existed; undo removed
   the file and both folders, the project exactly as it started.
2. A CRLF file deleted — a Delete card with its size; undo brought back the exact bytes.
3. A file moved into new folders — the card showed `notes.txt → docs/archive/notes.txt` and the
   folders; undo moved it back and took the folders away.
4. A delete of `.git/config` and of `../outside.txt`, a move into `.git` and outside: each refused
   with no card, and nothing touched. (After the refused move into `.git` the model tried creating
   the file elsewhere instead; that asked like any create and was denied.)
5. Undo all over an edit and a move: "2 changes in this turn", asked twice, the project back exactly.

The other six scripts on the final build: mimir-edit 25/25 · mimir-bigfile 18/18 · mimir-context 8/8 ·
mimir-check 26/26 · mimir-policy 44/44 · mimir-switch 14/14.

## Checked

| | |
|---|---|
| Unit tests | odin-core 315 (+45, 118 in `edit`: every resolution refusal; create in new folders and undo, shared folders in and out of order, a junction swapped in, a failed apply leaving nothing; delete restored exactly for CRLF, BOM and binary; moves never overwriting, even a destination appearing after planning; the turn simulation for create→delete, delete→create, move→edit, edit→move, move→move, a later change on top; old records) · mimir 31 · bench 31 · clippy clean |

## Review

A checker read the diff for one question — can anything lose, overwrite or delete data it was not
asked to — and found no such path. It confirmed that an **older** Mímir reading the new records
refuses or re-writes identical bytes rather than destroying anything. Fixed from it, all narrow race
windows: a delete re-checks the file's bytes right before removing it; apply re-checks the source's
folder and the first new folder's parent before acting; undo re-checks the folder it restores into.
Accepted and noted: in one contrived folder sequence Undo all can stop partway (nothing lost, a
retry finishes), and a restored file gets its bytes back but not its timestamps or attributes.

Also fixed on the way: `mimir-check.mjs` left its edits' undo records in the user's backups folder;
it now removes this run's, as the other scripts do.

## Accepted

- **Empty folders can be left behind** by single undos out of order, or after moving or deleting a
  folder's last file.
- **Backups grow** with every delete (16 MiB per file at most). Pruning is not part of Stage H.
- **Editor autosaves, OneDrive placeholders and antivirus locks** surface as refusals or errors,
  never as damage.

## Files

`crates/odin-core/src/edit.rs` (`resolve_target`, `resolve_source`, `move_new`, `write_new`,
`plan_delete`, `plan_move`, records by kind, per-kind undo, the simulated Undo all) · `tools.rs` (the
two tools, descriptions, `build_files_changed`) · `apps/mimir/src-tauri/src/lib.rs` (tickets bind the
destination) · `apps/mimir/src` (`ToolCard` routed by name, the delete/move/create cards,
`isChange`, Undo all's "changes") · `crates/bench` (`opsprobe` new, `taskprobe --ops`) ·
`tools/e2e/mimir-ops.mjs` (new), `mimir-edit.mjs`, `mimir-check.mjs`
