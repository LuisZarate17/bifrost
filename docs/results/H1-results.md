# H1 — The model runs your check, and fixes what it says

`run_check`: the one command you chose for the project folder, run when the model asks, approved
each time. **Built 2026-09-29.**

G7 made edits byte-exact. It could not tell the model that a byte-exact edit had broken the build —
rename a function, miss its call site, and every edit "worked". H1 gives the model a way to find out.

## The result

On `qwen3-vl:8b-instruct-q4_K_M`, eight tasks written so that **the obvious edit breaks the build**,
scored on "`cargo check` passes **and** the change is there":

| | App sampling (3 × 8) | Temperature 0 (1 × 8) |
|---|---|---|
| **With `run_check`** | **19/24 (79%)** · 1.7 checks a run | **7/8 (88%)** |
| Without | 8/24 (33%) | 3/8 (38%) |

`cargo run -p bench --bin fixprobe` · saved in `evals/out/H1-fix-1790707453/`

Per task, the difference is where the check can see what the edit could not:

| Task | With | Without | What it takes |
|---|---|---|---|
| f02 new parameter | 3/3 | 0/3 | the call site needs the new argument |
| f03 PORT u16 → u32 | 3/3 | 0/3 | `port()` returns it and must change too |
| f05 "it doesn't build — fix it" | 3/3 | 1/3 | the error message *is* the task |
| f07 port() → String | 3/3 | 3/3 | the obvious edit is already right |
| f04 rename across files | 2/3 | 0/3 | the call site is in the *other* file |
| f06 remove a constant | 2/3 | 2/3 | |
| f08 add a function, use it | 2/3 | 1/3 | |
| f01 rename in one file | 1/3 | 1/3 | the edit itself fails to match — see below |

No run passed only after the app's ten rounds — the headline is what the app would have shown.

End to end, the real app driven through its own buttons: **18/18** (`tools/e2e/mimir-check.mjs`),
and G7's `mimir-edit.mjs` still **16/16**.

## The design, and why each part is there

| | |
|---|---|
| **Picked, never typed** | Rust reads the folder's manifests — `Cargo.toml` gives `cargo check` and `cargo test`, `package.json` gives its real `test` and `build` scripts — and the chip offers only those. The webview chooses **by position**, and `set_check` re-detects and checks the label, so a position that has come to mean another command is refused. No command string crosses IPC. Anything else goes in `config.json` by hand |
| **No arguments** | The model decides *when*, never *what*. Arguments it sends are ignored |
| **The ticket holds the command** | Folder and argv live in the approval ticket, and `run_tool` runs those — not the config re-read, which could have changed while the card was on screen |
| **Stop kills the tree** | The check's ticket holds a number from the same counter as a streamed answer — taken when the card is shown, so a Stop pressed between Allow and the start reaches it too. Everything the command starts runs in one Windows **job object**: Stop, a timeout, a check that leaves a process behind, and Mímir exiting mid-check all end the whole tree, with `taskkill /T` as a backstop. `cargo` alone would die and leave `rustc` compiling. Measured: the card says *Stopped* in ~400 ms and no child is left |
| **Output capped, never lost silently** | One pipe for both streams, so an error sits next to its line. The first 6 KB and last 2 KB are kept — errors lead, test summaries trail — with a count of what was cut. A grandchild holding the pipe open cannot hang the app: after 2 s the output so far is what the model gets, and it is told it may be incomplete |
| **A failing build is not a failed call** | What the model is told never starts `error:` unless the command could not start. `prior_failures` counts `error:` results, and three identical ones end the turn — a model re-running the check after each fix would otherwise be stopped for doing exactly the right thing |
| **A blocked build is not a broken build** | Smart App Control refuses fresh proc-macro DLLs on this machine (PLAN.md §3). On a *failed* run only, `os error 4551` — and `E0463` for a crate the project's `Cargo.lock` lists, so one it really depends on — are reported as *"not a problem in the code — do not change any files"*. The same error for a crate the lockfile does not list is the model's mistake, and is left as one |
| **Files that decide what a check runs are never edited** | `.cargo/`, `rust-toolchain(.toml)`, `.npmrc` are refused like `.git`: each can point cargo or npm at any program. `build.rs`, `Cargo.toml` and `package.json` are the project's own files, so they are allowed — and a Run-check card names any of them an edit changed earlier in the turn |
| **Mímir's own folder is never edited** | Only reachable if the chosen folder contains `%APPDATA%` — your home folder, say. Editing `config.json` there would let the model pick its own check. Nor is any path with a `:` in a name: `.npmrc::$DATA` *is* `.npmrc` on NTFS |
| **No window, no UNC** | `CREATE_NO_WINDOW` on the command and on `taskkill`; the working directory is the plain path, because cmd.exe (under `npm.cmd`) refuses `\\?\` and falls back to `C:\Windows` — tested. A network share becomes `\\server\share`, not the relative `UNC\server\share` stripping the prefix would leave |
| **A typo costs one check** | `checks` is the one config field people type by hand, so it is read leniently: an entry that is not a list of strings is dropped. Before, it failed the whole file, every setting fell back to its default, and the next save wrote that over it |

## Groundwork the new tools needed

- **Replay what the model sent.** Every past call used to go back to the model as `{path}`. A
  search would come back without its text, and the model copies what it sees. The real arguments
  are now saved and replayed — except an edit's, whose text can be the size of the file.
- **`ok` comes from Rust.** A failed build is a successful check. The card no longer guesses
  success from whether the text starts with `error:`.
- **Cards update by position**, not as "the last entry" — and only while their own thread is on
  screen. A result that lands after Stop is saved behind Stop's own save, so a written edit's undo
  handle is not lost and two saves cannot race into two files; if another conversation was opened
  meanwhile, that one is left alone and the result goes to the stopped thread's own file.
- **One probe loop.** `crates/bench/src/probe.rs`, taken out of editprobe and shared by every probe
  from here on. It records rounds past the app's bound, which H7 will need.

## What the probe found

### 1. The remaining failures are mostly the edit, not the check

Three of the five failures with the check never got as far as a build: the model's `old_text` did
not match the file, the same way twice, and loop-breaking ended the turn. On `f01` the model sends
the whole function with a **semicolon after `format!(…)` that the file does not have** — G7's
finding #3, and the reason the G-testing worksheet stopped using `greet.rs` for its first edit.
Six more runs of `f01` each way: with the check 3/6, without 1/6, and the mismatch in both. The
check cannot help an edit that never lands.

The "closest text" hint shows the right lines; the model re-sends its own. **Worth a chunk of its
own:** say *which character differs* — "line 2 of your old_text ends with `;`, the file's does not".

### 2. The model reads the error, but not always where it points

On `f04` (rename `address`, called from `main.rs`) the check said *cannot find function `address`
in module `config`* — an error that can only be in `main.rs`, and cargo says where. Twice the model
kept editing `config.rs` — the file it had already fixed — until loop-breaking stopped it. It never
opened `main.rs`. **This is what H2
is for:** a model that can search for `address(` does not have to infer the file from an error.

### 3. A partial-line `old_text` compounds

On one `f06` run the model matched half a line and replaced it with a whole statement, leaving the
rest of the line behind — then did it again after each failing check. Every edit applied; the line
grew. Byte-exact matching guarantees *what was asked for* was written, not that it was sensible.
The check did its job: it said the build was broken, four times.

## No regression from moving editprobe onto the shared loop

The model's input is byte-identical by construction — same tools, same wording (`system_prompt`
without a check is the G7 prompt, word for word), same loop. Measured anyway, because a probe that
drifts measures a different product:

| | `t02` rename | `t09` read-only |
|---|---|---|
| Before H1, 8 runs | 5/8 | 7/8 |
| After H1, 8 runs | 6/8 | 7/8 |

Across all ten tasks after H1: 27/30 at app sampling, 10/10 at temperature 0. **G7's 98/100 was on
the lucky side** — two-step renames sit nearer 70% than 80% on today's runs, before and after.

## Tests

| | |
|---|---|
| `odin-core` | **129** — 22 new for `check` (including Stop killing a tree, a check's leftover process ended with it, and a passing run never called blocked), 7 for `tools`, 4 for `config`, 4 for `edit` |
| Mímir | **10** — two new: a check ticket redeems exactly its command and stop number, and cannot be redeemed as a read |
| Probe | `cargo run -p bench --bin fixprobe` — `--runs`, `--only`, `--variant with|without`. Headline pass counts only runs within the app's ten rounds; later passes are shown apart |
| End to end | `node tools/e2e/mimir-check.mjs` — 18 checks in six scenarios, `--only N` for one |

## What is still weak

- **Edits that never match** (finding 1) are now the main thing between the model and a passing
  build.
- **Errors in another file** (finding 2) — H2.
- **Nothing limits a long fix loop's context.** Each check result can be ~8 KB; H7.

## What the review found

An independent review of the chunk, by an agent that had not written it, against the guarantees
above. **No way was found around the approval boundary**: no command string reaches Rust, a check
ticket runs exactly its argv, and editprobe's input is unchanged. It found four real bugs and a
handful of smaller ones, all fixed:

| | What it was | Fix |
|---|---|---|
| 1 | **A result landing after Stop could overwrite another conversation.** Stop frees the sidebar; open another thread within the second the killed check takes to report, and its card at the same position was patched and saved — dropping it from that file | Cards change only while their own thread is on screen. A late result for a thread no longer open is saved to that thread's file, from what Stop saved. e2e scenario 5 does exactly this and checks the other thread is unchanged, on screen and on disk |
| 2 | **Stop right after Allow could miss the check** — it took its stop number only on arriving in Rust, and could then run 600 s with no Stop button | The number is taken when the card is shown; `run_tool` refuses to start a check already stopped; the loop checks `live()` before calling it |
| 3 | **Block detection was wrong both ways.** G7's real block (`E0463` for `tauri`) has no macro-like name, and `extern crate serde_derive;` without serde would have been blamed on Windows. A passing run could also read as blocked | `E0463` is a block only for a crate in `Cargo.lock`, and only when the run failed |
| 4 | **A typo in a hand-typed check reset the whole config** | `checks` read leniently (above) |
| 5 | The build-files warning named declined edits and missed quoted paths | Only `Edited`/`Created` results count; paths cleaned as `edit` cleans them |
| 6 | fixprobe's headline could count passes past the app's round bound | Counted apart — and none had |
| 7 | A process a check left behind ran on, and checks outlived Mímir | The job object |
| 8 | A network-share folder became a relative path | `\\?\UNC\` → `\\` |

Smaller: `name:stream` paths refused deliberately rather than by accident; reads replay their full
path again, so a thread reopened with another folder does not show one folder's contents under
another's relative path; a `run_tool` that throws marks its card failed instead of leaving it
spinning; a `.github` path no longer reads as `.git`.

**Left as found:** `npm test` puts `node_modules/.bin` on `PATH`, so an edited `vitest.cmd` there
changes what the check runs, and the card does not say so. `node_modules/` is not something the
model is asked to edit, and H2's search skips it — but it is not refused. And the probes still use
the real `%APPDATA%` for anything that writes there; nothing in them does yet.
