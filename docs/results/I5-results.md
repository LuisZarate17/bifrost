# I5 — `hugin -p`: Claude Code hands Hugin a task

Claude Code can give Hugin a task and get back a proposal: Hugin's answer, a patch, and the check's
result. The work runs on the local model, so it costs nothing from the Claude plan. Claude reads the
patch and applies it. **Built 2026-10-21**, from the plan approved that day; the check is the one
choice Luis made for it: **Claude names it** (`--check "cargo test -p hugin"`), else the folder's
check from Mimir, else none.

```
hugin -p "fix the failing test in src/parse.rs" --check "cargo test -p mycrate" --json
```

## How it works

- **A throwaway copy.** Hugin copies the folder — the last commit plus your uncommitted and untracked
  files, byte for byte — into `%TEMP%\hugin-p\run-…` and works there. Your folder and its `.git` are
  never written: proven by fingerprinting every file of `.git`, times included, before and after a
  real run on this repository (778 entries, identical).
  - The copy is a shallow local clone (`git clone --no-checkout --depth 1 --no-local`), so it has a
    real commit of its own and borrows nothing from your repository while it works.
  - The spec said "a scratch git worktree, as taskprobe does". `git worktree add` writes into your
    `.git` and leaves stale entries when a run is killed, which taskprobe has hit; and taskprobe
    never carried uncommitted changes. The copy keeps the spec's promise instead of its wording.
  - Links (junctions) are never followed out of the folder, on the way in or out.
- **Nobody to ask.** Edits are allowed (edit.rs already keeps them inside the copy). The check is
  allowed. Everything else is refused, and `run_command` is never offered. If Hugin changed a file
  that makes a check run code — Cargo.toml, package.json, a makefile, conftest.py and the like — the
  check is not run at the end, and the output says why.
- **The check** is one program and its arguments, run in the copy with no shell. A build gets its
  own cache, `%LOCALAPPDATA%\bifrost\hugin-p\target-…`, never your `target\`, which holds the exes
  your shortcuts and the tray run.
- **The time limit.** A run stops itself at 9 minutes (`--time`), under Claude Code's 10-minute cap,
  and still prints what it has, marked partial. The clock runs from the start, and the report is
  printed before the copy is cleaned up.
- **The patch** is made by git between two snapshots of the copy, so your own git settings can't
  change it, and saved to `%TEMP%\hugin-p\patches\` for 7 days. The output has the exact command:
  `git -C "<repo>" apply --whitespace=nowarn "<patch>"`. Files the check itself wrote (a
  Cargo.lock) are marked as not Hugin's edits. A patch over 16 KB is not printed — its files and
  line counts are, with the file to read it in.
- **The output:** plain text, or `--json` with the summary first and the patch last. Exit 0 when a
  turn ran, whatever the check said; 1 when it could not run (still with JSON); 2 for a usage error.
  Nothing is saved to Hugin's sessions or the config.

## The review

An independent review found, and these were fixed:

1. **The first copy borrowed your repository's objects**, and git refreshes the time on any object
   it finds there — so every run touched file times in your `.git/objects`. The copy is now a
   shallow clone, and the fingerprint test compares times as well as contents (it fails on the old
   design).
2. **`--check ".\script.cmd"` ran your folder's script**, not the copy's. A program named with a
   folder now runs from the copy; a path into your repository is refused.
3. **A junction in the folder was followed out of it**, so an outside file reached the model. Every
   part of every path is checked now, and links made during the run are left out of the patch.
4. **Long patches buried the check line and the apply command** in Claude's output. The inline patch
   is capped at 16 KB, and the summary comes first.
5. **The check runs code Hugin wrote before anyone reads it.** Inherent to having a check; it is now
   said in `hugin --help` and in the note, and the list of files that stop the check is wider for
   unattended runs (Mimir's is unchanged).
6. The copy had no commit, so `git rev-parse HEAD` failed inside a check. The clone fixed it.
7. Setup steps now count against `--time`, and the report prints before cleanup.

## Checked

| What | Result |
|---|---|
| Tests | bifrost-core 576, hugin 154, bench 187; clippy clean |
| Propose self-check (no model): copy → patch → apply → hidden tests | **16/16** |
| Your `.git` after a real run on this repository | identical, times included (778 entries) |
| A run past `--time` | stopped on time, marked partial; no git, cargo or test process left |
| taskprobe, propose vs interactive (qwen3-coder:30b), one paired sweep | **propose 27/48 and 10/16** · interactive 26/48 and 9/16 · margins ≥ 25/48 and ≥ 7/16 · **0 patches that didn't apply · 0 task folders touched** — `evals/out/I5-task-1792623950` |
| Real tasks handed over from Claude Code | 3 tasks in this repository: the plumbing right on all 3, the model on none (below) |
| Claude's tokens on one task, alone vs with Hugin | about the same: −3% notional cost, half the cache reads, 4× the time (below) |

### Propose against interactive, run for run

| | interactive | propose | both pass | only interactive | only propose | neither |
|---|---|---|---|---|---|---|
| app's settings (48) | 26 | 27 | 24 | 2 | 3 | 19 |
| temperature 0 (16) | 9 | 10 | 9 | 0 | 1 | 6 |

The two modes pass the same tasks, give or take a run: handing the work back as a patch costs
nothing in quality. Propose runs took 46.4 s on average against 39.1 s (the copy and the patch).

### Three real tasks

Handed from Claude Code to `hugin -p --json`, each with `--check "cargo test -p <crate>"`:

1. **taskprobe gets a `--help`** (it had none, and an unknown flag starts a real sweep): the model
   ran out of its 40 tool calls; the output said *partial*, and the check showed a type error.
2. **hugin gets `--version`**: the patch deleted the tests module's closing brace; the check failed
   to compile it.
3. **`npx`, `pnpm` and `yarn` become `.cmd`, like `npm`**: the model changed the existing behaviour,
   rewrote the old test to match, and typed `NXP`; the check failed.

All three patches applied cleanly (`git apply --check`) and left the folder untouched, and every
failure was there in the check result — **but in all three the answer said the work was done**.
So the note now says: trust the check, not the answer. Qwen3-Coder 30B does well on the taskprobe
tasks and badly on this repository's own code, which is long and dense; it is a first-draft tool
here, not a finisher.

### Tokens

The third task, done twice by `claude -p` (Opus 5.5, subagents off) in scratch copies — once alone,
once told to hand it to Hugin first, with the note given by `--append-system-prompt`:

| | Claude alone | Claude + Hugin |
|---|---|---|
| turns | 10 | 6 |
| output tokens | 2,255 | 2,465 |
| cache written / read | 53.6k / 303k | 54.6k / 152k |
| notional cost | $0.535 | $0.517 |
| time | 29 s | 121 s |
| result | right | right — Hugin's code kept, its tests rewritten |

On a task this small Claude is about as cheap alone: Hugin halves what Claude reads, not what it
writes. The saving grows with tasks where Claude would otherwise read a lot of code — and only when
Hugin's work is right.

## The note for Claude Code

`hugin --claude-note` prints a short section for `~/.claude/CLAUDE.md` saying when to hand work to
Hugin and how: a small, well-described, checkable task; `--json`; a Bash timeout of 600000 ms; read
the patch before applying it; trust the check, not the answer. It is never written there without
Luis's OK.

The check's build cache lives in `%LOCALAPPDATA%\bifrost\hugin-p\target-…`, one per repository, and
can be deleted at any time (the next check rebuilds it).

## Known limits

- The copy has no ignored files, so a check that needs `node_modules` won't find them.
- Submodules, LFS files, sparse checkouts and partial clones aren't carried; the output says so.
- The first Rust check in a new cache builds everything; after that, the workspace's own crates.
- Smart App Control may block a first build; the output says so.
- A patch goes stale if the same files change before it is applied.
- If Claude Code's Bash sandbox is on, `hugin` may need to be in `sandbox.excludedCommands`.

## For a person

[I-testing.md](../I-testing.md) §I5, saved for later.
