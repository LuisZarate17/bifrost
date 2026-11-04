# H3 — A benchmark of real work, scored by the project's own tests

`taskprobe`: sixteen tasks on odin's own code, each checked out in a git worktree, done by the
model with every Phase 1 tool, and scored by tests it never saw. **Built 2026-09-30.**

The probes before it each measure one move — an edit (G7), a fix after a failed check (H1), a
file found (H2). None of them measures what Stage H is for: *can Mímir do a coding task well enough
to trust the result?* This one does, and H4 uses it to decide whether the model should change.

## The tasks

Only 8 of the repo's commits add tests, so the set mixes two kinds:

| Kind | Tasks | Start | Scored by |
|---|---|---|---|
| **Real commits** | a01 `LoadedModel::pinned` (b38bc86) · a02 create `tools.rs` (e3b43eb) · a03 `store::attachment_path` (871f297) · a04 create `config.rs` (27f6faf) | the commit's parent | the commit's own tests |
| **Function removals** | b01 `tidy_deletion` · b02 `Text::parse` · b03 `Workspace::contain_with` · b04 `undo` · b05 `render_chatml` · b06 `extract_calls` · b07 `repair` · b08 `unescape_tolerant` · b09 `prior_failures` · b10 `interpret` · b11 `build_files_changed` · b12 `display_title` | 6653c27, the body replaced by `todo!()` | that file's tests |

Each task is written the way a person would ask for it, with the **exact signature** of everything
the tests use — one wrong name fails the whole test build — and never the implementation.

## How a run works

1. A detached worktree at the start commit, in `%TEMP%`.
2. The scored file's tests are cut off at its `#[cfg(test)]`; for a removal, the body becomes
   `todo!()` — checked against the line before and after, so a wrong range fails loudly instead
   of scoring the wrong code.
3. The shared probe loop with every tool, `cargo check -p odin-core` as the check, up to twenty
   rounds — a pass after the app's ten is counted apart, as LATE.
4. The reference tests go back: the model's file, cut at its *own* `#[cfg(test)]` if it wrote one,
   plus the solution commit's test tail. Nothing the model wrote in a test module survives.
5. `cargo test -p odin-core --lib -- <module>::tests::`. **PASS only if it reports `N passed; 0
   failed` with N above zero** — zero tests run is a fail, not a pass.
6. The worktree is removed on every exit path, panic included.

`--selfcheck` runs no model: every task's **solution state must pass** and its **stripped state
must not** — which is what proves a task tests what it claims. **All 16 do.**

## What keeps it honest

| | |
|---|---|
| **Reads stay in the worktree** | The app lets a person approve reading anything; a probe approves every call. The worktree sits on the same disk as the repo holding every reference solution, and its `.git` file names where that repo is — so here, `read_file` is confined like an edit is. Tested: a path outside, `../`, and `.git` are all refused |
| **Stubbing a safety function stubs the worktree's copy** | b03 removes `contain_with` — the rule that keeps edits inside the folder. The tools the model calls run on the odin-core the probe was built with, whose rules are intact |
| **Scratch `%APPDATA%`** | a03 and a04 test paths under `%APPDATA%\odin`; each run points it at its own scratch folder |
| **The repo's own target folder** | A fresh `CARGO_TARGET_DIR` rebuilds every proc-macro, and this machine refuses fresh ones (PLAN.md §3). A worktree's odin-core still gets its own artifacts, because its path differs. `os error 4551` is retried after a touch, up to three times, then called ERROR — it measured Windows, not the model |
| **Ollama is started if it is down** | As the apps and the CLI do, so a long run does not die at its first request |

## The baseline — and H4's starting point

`qwen3-vl:8b-instruct-q4_K_M`, every Phase 1 tool, sixteen tasks, three runs at the app's
sampling and one at temperature 0:

| | Pass (within 10 rounds) | Real commits | Removals |
|---|---|---|---|
| App sampling | **6/48 (12%)** | 1/12 | 5/36 |
| Temperature 0 | **2/16 (12%)** | 0/4 | 2/12 |

`cargo run -p bench --bin taskprobe` · saved in `evals/out/H3-task-1790812269/` · about 70 s a run

**This is the number Stage H set out to find.** The probes before it measured single moves, and
the model makes them well: an edit 98% of the time, a found name 88%, a broken build fixed 71–79%.
Strung together into a real task, it finishes one in eight.

Where it passes: `b03` (`Workspace::contain_with`) **4/4**, `b09` (`prior_failures`) 2/4, `b12`
(`display_title`) 1/4, `a04` (create `config.rs`) 1/4. Every other task, 0.

### How it fails

Read from the logs of the 56 that did not pass:

- **Code that does not compile, and an edit that cannot fix it.** The largest group: a `;` the
  file does not have, a quote left unescaped inside a Rust string (`unterminated byte constant`,
  `character literal may only contain one codepoint`), a function pasted inside a struct. The
  check reports it; the model's next edit does not match, the same way twice, and loop-breaking
  ends the turn. Most failures carry *stopped after repeating a failing call*.
- **The wiring around the code.** `a02` and `a04` write the new module and never add `pub mod` to
  `lib.rs`, so its tests never run — though the task says to.
- **Tests that fail.** Compiles, runs, wrong — `display_title`'s truncation, `interpret`'s refusals.
- **Runaway replies.** Four runs ended at exactly five minutes: after reading `toolcall.rs`, long
  and full of escapes, the model wrote one edit until the request's time limit cut it off. Scored
  as fails — the probe now times each request and tells a runaway from a refused connection.
  **In the app this would hang Mímir for five minutes**: nothing caps a tool turn's reply. Noted
  for H7.

Only 1.0–1.3 checks a run: the model mostly runs the check once, if at all, and when it fails it
tends to edit again rather than read the error closely.

### What it means for H4

A 12% baseline leaves room either way. H4 runs the same benchmark on one or two coding models —
measured first for VRAM at 16k and 32k and for tool calling — with the margin that justifies
switching set **before** the run.

## How it was built, and checked

Built by a delegated builder agent from a written design, then checked three ways before anyone
trusted it:

- **Re-run independently**: the self-check (16/16), the unit tests and clippy, and a model smoke run
  on one task — the full loop works: read, edit, check, hidden tests, score.
- **Reviewed by a checker agent**: no serious problem. It confirmed the tail restore, the zero-test
  guard, the line-range checks, the scratch `%APPDATA%` and the cleanup, and spot-checked five
  prompts against their commits for exact signatures and no leaked code.
- **Its miss, caught here**: the review said reads could not reach `.git` — true of edits, not of
  `read_file`, which the probe approved anywhere. Confined, and tested.

## Tests

| | |
|---|---|
| taskprobe | **14** — tail cutting and restoring, the removal and its wrong-range refusal, result parsing (pass, zero ran, failed, does not compile), unique task ids, and reads confined to the worktree |
| Run it | `cargo run -p bench --bin taskprobe` — `--runs`, `--only`, `--selfcheck`, `--keep`, `--model` |
