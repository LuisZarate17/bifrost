# I0 — The rename: Bifrost, Vidar, Mimir

ODIN is now **Bifrost**, Heimdall is **Vidar** and Mímir is **Mimir**, everywhere a name is typed,
shown or stored. The data folder moved from `%APPDATA%\odin` to `%APPDATA%\bifrost`, once and
whole. Nothing else changed. **Built 2026-10-12**, in four commits on `stage-i`.

## Why

Stage I builds Hugin, a terminal agent, on an engine Mimir shares, so nothing new should be built
under names that are about to change ([STAGE-I.md](../stages/STAGE-I.md)). The names are Luis's
own set, chosen 2026-10-03 for how they feel ([PLAN.md](../PLAN.md) §0b).

## What changed

| Commit | What |
|---|---|
| **A** `b4f6259` — structure | `apps/heimdall` → `apps/vidar`, `crates/odin-core` → `crates/bifrost-core`, `crates/odin-cli` → `crates/bifrost-cli`, moved with history. Package, crate, library and binary names (`vidar_lib`, `bifrost.exe`). Tauri identifiers `com.bifrost.vidar` and `com.bifrost.mimir`, window titles. npm: `@bifrost/ui`, `app:vidar` beside `app:mimir`, and the bare `app` / `dev` / `build` are gone. Vidar now declares `@bifrost/ui`, which it had imported without declaring. Otherwise the lockfile changed by renames only |
| **B** `5cad123` — names inside the apps | Events `vidar://…`, the `[vidar]` log in `%TEMP%\vidar.log`, the tray, `VIDAR_*` switches and `BIFROST_NVML_PATH`, every visible "Mímir", the temp suffixes an edit writes (`.bifrost-tmp`, `.bifrost-new`), the bench's scratch folders, the CLI's help |
| **C** `eec7ac0` — the data folder | `bifrost_core::paths`, which replaces the five places that built `%APPDATA%\odin` themselves; who moves the folder; the end-to-end scripts' guard |
| **D** — tools and docs | Shortcuts written for the new names, and the old ones removed. The icon tool's keys. Live docs, tools and evals renamed, with history left as it was. `tools/move-repo.ps1` for Step 2. This write-up, and [I-testing.md](../I-testing.md) |

## The data folder

**Where it is.** `paths::data_dir()` decides on every call and never caches:
- `%APPDATA%\bifrost` if it exists;
- else `%APPDATA%\odin` if it exists and holds no `MOVED.txt`;
- else `%APPDATA%\bifrost`.

So every app keeps working on the old folder until the move happens, and on the new one after.

**How it moves.** `paths::migrate()` makes one rename of the whole folder, then writes
`odin\MOVED.txt` saying where everything went:
- **Never a copy,** so there is no half-copied state. **Never an overwrite.**
- **`odin` still holds files, but `bifrost` already exists:** nothing moves, `bifrost` is used, and
  a warning is printed.
- **Something holds a file:** the rename is retried for two seconds (40 × 50 ms). If it still fails,
  the app stays on `odin` and tries again at its next start.
- **Two apps starting at once:** the second finds the folder already moved, and counts that as
  nothing to do.
- **An old folder with no files in it is not live.** That is what is left if the note is deleted
  without the folder, or could not be written. It gives no warning, and the end-to-end scripts don't
  refuse over it.

**Who moves it:**
- **Mimir and Vidar,** right after their log redirect and before Tauri starts, so nothing has read
  the config yet and what happened is written to the log.
- **The CLI,** before it reads the config. It prints only if something moved or failed.
- **Never the probes.**
- **Never the end-to-end scripts,** which refuse to run (exit 2) while `odin` is still live. Their
  first `mkdir` would otherwise make an empty `bifrost`, which `data_dir()` would then prefer,
  stranding the real data.

**The refuse-our-own-folder rule** (`edit.rs` `app_data()`, also skipped by search) now protects
whichever folder is live, so a tool call can no more edit `%APPDATA%\bifrost` than it could edit
`odin`. The refusal now reads *…inside Bifrost's own settings folder, which is never edited*, since
the folder belongs to every app.

**Tests.** Nine, on temp folders, never the real `%APPDATA%`:
- a fresh machine;
- only the old folder, in use until it moves;
- the move keeps config, conversations, attachments and backups, and marks the old folder;
- only the new folder;
- both live, left alone;
- a marked old folder is not live;
- an empty old folder is not live;
- one file deep in the old folder keeps it live;
- a file held open blocks the move, nothing is lost, and the move succeeds once it is released.

The last two came from the review, below, after the recorded move.

## The recorded move

The first launch of anything built from commit C, 2026-10-12 18:05. Until then nothing built from
C had run: `%APPDATA%` held only `odin` at the end of every check.

1. **Before.** `%APPDATA%\odin` held 45 files: `config.json`, 35 conversations, 1 attachment and 4
   undo records (8 files). Each was hashed, and the folder was copied to
   `%APPDATA%\odin-before-bifrost`, identical by hash.
2. **The new Mimir started, and moved it.** `odin\MOVED.txt` is stamped 18:05:15, and
   `mimir.log` says `[mimir] moved %APPDATA%\odin to %APPDATA%\bifrost`.
3. **After.**
   - `%APPDATA%\bifrost` holds the same 45 files, every one identical by SHA-256 to the record.
   - `odin` holds only `MOVED.txt`.
   - `bifrost status` reads `qwen3-coder:30b`, the model the config named before.

The copy in `%APPDATA%\odin-before-bifrost` stays until a few days of use pass without surprises.

## End to end, against the moved folder

All nine scripts ran right after the move, against the running Mimir over its debug port. They used
the configured model, `qwen3-coder:30b`; `mimir-switch` also loads the vision model and switches
back.

| Script | What it drives | Result |
|---|---|---|
| `mimir-edit` | editing files, and undo | 25/25 |
| `mimir-check` | the check command, finding code | 26/26 |
| `mimir-switch` | the coding-model offer, and switching back | 14/14 |
| `mimir-policy` | what runs unasked and what asks (H5) | 47/47 |
| `mimir-context` | the context budget inside a turn (H7) | 8/8 |
| `mimir-bigfile` | partial reads of a big file (H8) | 17/17 |
| `mimir-ops` | new folders, delete, move (H9) | 32/33, then 31/31 with 1 skipped, then **33/33** |
| `mimir-notes` | project notes and the task list (H10) | 17/17 |
| `mimir-acceptance` | Stage H's finish line | 3/3 runs, 21/21 checks |

**The one miss was the model, not the rename.**
- **Run 1.** After its delete of `../outside.txt` was refused, the model asked to *read* that file.
  A read outside the folder asks (H5), so it waited for approval, and both "nothing was touched" and
  "no file appeared outside the folder" passed. The script still counts any card aimed at a refused
  place as a failure, by design.
- **Run 2.** The model answered without making the call, so that scenario was skipped.
- **Run 3.** Everything passed.

Nothing the model reads in that scenario changed in I0. Against `main`, the system prompt, the tool
descriptions and the refusal texts are the same, apart from the settings-folder refusal below. The
project folder's random name is part of the prompt, so the model's choices move from run to run.
The H re-check on 2026-10-11 had 33/33.

## What reaches the model differently

- **The bench's scratch folders** are now `bifrost-*probe-<pid>`, and that folder name appears in
  the system prompt the model sees. It was not re-measured: it is one word in a temp path, and the
  same path already carried a different process id on every run. If a later benchmark moves, this
  is a difference to keep in mind.
- **The settings-folder refusal** reads *Bifrost's* rather than *Mímir's*.
- **The temp files an edit writes** beside its target are `.bifrost-tmp` and `.bifrost-new`. No code
  looks for leftovers by their suffix, since each is removed where it was made, so there was no old
  suffix to keep accepting.

## Kept old on purpose

- **History.** `stages/`, `docs/results/`, the H review and `evals/out/` keep the names they were
  written with. BUILD.md, a dated log, got a note at the top instead, which is a change from the
  packet. So did the done items in G-testing.md and PENDING-CHECKS.md.
- **The pinned benchmarks.** taskprobe's tasks, its `CRATE = "crates/odin-core"`, and findprobe's
  questions describe commits from before the rename, and run on worktrees of them. Against `main`,
  nothing inside `tasks()` or `questions()` changed: only imports, scratch-folder names and two
  comments. `taskprobe --selfcheck`: *every kept task is OK.*
- **Code that has to name the old folder:** the move, its messages, and the end-to-end guard.
- **The repo folder,** `…\repos\odin`, with its path in G-testing.md, PLAN.md and Mimir's preview
  fixtures. These change with the folder in Step 2.
- **Two lines of test data** in `chat.rs` that say *Mímir – naïve café*. They test non-ASCII text,
  not names.

## Checks

- **Tests.** `cargo test`:
  - bifrost-core: 416 passed, 2 ignored (407 before C, plus the 9 above);
  - bench: 48 (4, 6, 7, 8, 23);
  - mimir: 31.
- **Lint and types.** `cargo clippy --all-targets` on all five crates, with no warnings.
  `tsc --noEmit` is clean in both apps.
- **Builds.** `npm run app:vidar`, `npm run app:mimir` and the CLI. They were built again after the
  review's fixes, and `mimir-edit` passed 25/25 against that build. Its log showed no move and no
  warning, as it should once the move is done.
- **Shortcuts.** `node tools/shortcuts.mjs` wrote `Vidar.lnk` and `Mimir.lnk` on the Desktop and in
  the Start Menu, pointing at the new exes. It removed `Heimdall.lnk` and `Mímir.lnk` from both;
  `--check` agrees. Taskbar pins can't be changed by a script; that is a hand check.
- **The grep audit.** No old name is left outside the list above.
- **By hand** ([I-testing.md](../I-testing.md) §0). Vidar's and Mimir's checks passed on
  2026-10-14: the hotkey, the tray, the moved config, every
  conversation, the theme starting over, and an old Undo finding its record.
- **Housekeeping.** The old build outputs (`heimdall`, `odin`, `warden`, `citadel`) were deleted
  from `target\debug`, along with a leftover temp folder from a test.
- **Independent review** of the whole diff against `main`, 2026-10-14. Nothing high or critical:
  the pinned text is untouched, and no path was found by which the move could lose, half-copy or
  overwrite data. Ten low findings. Fixed:
  - **H-testing.md** still had the old names. Its 50 boxes were never ticked, since the Stage H run
    was recorded in PENDING-CHECKS.md, so it is a live worksheet. It now has the new names and a
    note.
  - **An empty old folder** counted as live. This gave a false warning at every start, and the
    end-to-end scripts refused for good. It also made the second of two apps starting at once
    report a false "could not move". The rule is now *holds files*, in paths.rs and in the
    scripts' guard alike. The warning now says what it means: `odin` still holds data, and
    `bifrost` already exists.
  - **`move-repo.ps1`** didn't rebuild the CLI and probes after `cargo clean`, and didn't say to
    start it from outside the repo. It now does both, and sets its own process's folder too.
    Separately, a test showed that a running script can rename the folder it lives in.
  - **Smaller fixes:**
    - a preview fixture named the removed `npm run app`;
    - the retired shortcuts are removed only when they point at one of this repo's exes;
    - wording here.

  Left as they are, on purpose:
  - **An empty `bifrost` made before the move would hide a live `odin`.** It can't happen any more
    on the one machine this runs on, since the move is done, and handling it would add a removal
    step with its own race.
  - **The refusal protects only the live folder, not both names.** The old folder holds nothing an
    app reads.
  - **The bench's scratch names** stay as they are, discussed above.

## Different from the packet

- **BUILD.md** got a top note rather than new names, because it is a dated log.
- **Names in `tools/` and `evals/`** were renamed in D rather than B.
- **Mimir's theme and accent start over once.** The new identifier comes with a fresh WebView2
  profile. This was expected.

## Next

I0b, the colors and badges, starting with a board of badge options.

Step 2 waits for Luis's go-ahead, each part at its moment:
1. The GitHub repo becomes `bifrost`.
2. `tools/move-repo.ps1` moves `…\repos\odin` to `…\repos\bifrost`, with Claude Code's memory folder
   and the paths in the config.

The old Undo has already been checked, so the move can come at any time.
