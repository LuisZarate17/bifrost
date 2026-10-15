# I1 — The turn loop moves into bifrost-core

The tool turn — ask the model, show what it asked for, wait for a decision, run it, send the result
back, round after round — lived in Mimir's webview, as a TypeScript loop over one Rust command per
round. It is now `bifrost_core::agent::turn`, in Rust, behind a small `Frontend` trait. **Mimir and
every probe run that one loop**, so the probes measure what ships, and Hugin will run it next.
**Built 2026-10-14** in three commits on `stage-i`, and measured the next morning.

## Why

Hugin needs the same turn Mimir has — the same tickets, the same Stop, the same rules for what runs
without asking — and a second copy in a terminal would drift from the first. The probes already had
a copy of their own, kept in step by hand ([STAGE-I.md](../../stages/STAGE-I.md), I1).

## What changed

| Commit | What |
|---|---|
| **Step 1** `c2eb5c9` — the plain pieces | The Stop counters, the approval tickets, the read, find and automatic runners, and sending with compaction move unchanged from Mimir's backend into `bifrost_core::agent`, with their 20 tests. The JSON the webview saw was identical: all 28 serde and derive attributes compared |
| **Step 2a–b** `478273d` — the loop, and Mimir on it | `agent::round` is the old `ask_with_tools` command without the Tauri: one request, its reply sorted into an answer, a call to ask about, one already run, one turned away, or a stuck call. `agent::turn` is `askWithTools` ported step for step, with 30 tests on scripted replies. Mimir drives it through one command; App.tsx keeps only the rendering |
| **Step 2c** `692b1a1` — the probes on it | `agent::Env`: what a probe varies about a round (sampling, transport, the tools offered, its own system prompt, compaction, where backups go, a check's environment). The app's own is the default, so `round` and `run_approved` are `round_in` and `run_approved_in` in the app's setting. `probe::run_loop` keeps its signature and now drives `run_turn_with` |

Mimir's backend went from 2,247 lines before I1 to 1,294; App.tsx from 2,309 to 2,111. The copied
`MAX_ROUNDS` and `MAX_FREE_CALLS`, `refusalFor`, and the `ask_with_tools`, `run_tool` and
`ollama_running` commands are gone.

## How Mimir drives it

- **One command, `run_turn`.** It runs the loop on a blocking thread with the window as its
  `Frontend`, and everything the loop says goes up a Tauri `Channel` as it happens: a status, a
  round's numbers, a card, a change to a card, the answer, an error, a save. **`End` comes last**:
  a channel keeps its order, but a large event can still be on its way when the command returns, so
  the window waits for `End` before it saves.
- **The window applies the events strictly in order**, by the rules the TypeScript loop kept: a card
  only in its own thread, an answer only while the turn is live, and a save after a Stop that landed
  with a card on screen — into the stopped thread's own file if another conversation is open.
- **A decision comes back through `decide(card, allow)`.** The card waiting holds a sender in one
  slot; a click on any other card — an older turn's, one already answered — changes nothing. Card
  numbers are counted for the whole process, so a new turn's card is never mistaken for an old one.
- **The ticket is still Rust's.** The window never sends a token: Rust armed the ticket when it made
  the card, and redeems it itself after Allow. G6 and H5's guarantee holds — nothing that was not
  armed can run.

## Stop, at every point

The review (below) found two ways Stop could be lost once the loop left the page. Both fixed:

- **A Stop that reaches Rust before its turn has begun.** Sending a message with images stores them
  first; a Stop pressed then freed the window, but `run_turn` arrived afterwards and took a Stop
  number above it — the turn would have run with nobody watching, its cards still answerable. **A
  Stop now names the request it stops**, by the page's name (made when it loads) and its request
  number, and a turn asks about its own as well as about the counter, whichever arrived first.
- **A reload while a card waits.** The page and its channel were gone, but the Rust turn waited on
  for ever — and the next turn's card would have taken its slot and set it going again, unwatched.
  **A page load now stops whatever was running** and refuses a waiting card, as Stop does.

Checked by a scenario driven over the debug port, 18/18: Stop while a card waits (the card says
*denied*, as before; nothing runs; the window frees in 4 ms), a decision naming a stale card, a
Stop sent before its turn (the turn ends at once having asked nothing; the page's next request is
answered), and a reload with a card waiting (the new turn's card stays waiting for 25 s with nothing
answering it, then runs on Allow).

## The probes on the shared loop

`probe::run_loop` keeps its signature, so no probe changed but taskprobe's confinement test. What a
probe still adds is what a probe is: every card is approved, a read outside the folder is declined
when reads are confined, and the counts are written down from what the loop says.

**What reaches the model differently, now that the probes run the app's loop:**
- **A confined read is declined, as a person would decline it**: the model is told *"The user
  declined to let you read …"*, not an `error:`. So repeating it no longer counts towards the
  loop-breaking. **It has not happened**: the last three Stage H taskprobe runs (192 runs, about 250
  reads) tried no read outside the folder.
- **A failed approved call gets no repeat note**, as in the app. The probe used to add one to every
  repeated failure.
- **editprobe's `write_file`** is planned only because editprobe offers it (the app never does), as
  an `edit_file` plan; its result goes back under the name `edit_file`. Only editprobe's *rewrite*
  format sees this, and editprobe is not one of I1's measures.
- The third identical failure is no longer in a run's list of calls (the loop ends before it is
  one), and a closing answer that is empty is not recorded.

## Measured

On `qwen3-coder:30b`, each probe at its defaults (3 runs at the app's sampling, 1 at temperature
0). **The bars were set before any probe ran on the shared loop** (2026-10-14, ~22:35).

**fixprobe has no Stage H run on this model** — H1 measured `qwen3-vl:8b`. So it was measured
twice, the same evening: first on the old probe loop (the working tree before 2c, its executable
copied aside), then on the shared one. Bar: within 4 of 24 of the old loop on each variant.

| fixprobe | Old probe loop | **Shared loop** |
|---|---|---|
| With the check, app sampling | 23/24 | **23/24** |
| With the check, temperature 0 | 8/8 | **8/8** |
| Without, app sampling | 18/24 | **18/24** |
| Without, temperature 0 | 6/8 | **6/8** |

**Identical**, task for task: f03 and f04 fail without the check every time, and one f04 run fails
with it. Saved in `evals/out/H1-fix-1792042162/` (old) and `H1-fix-1792045580/` (shared).

| findprobe (`--variant search`) | Stage H (H8–H10) | **Shared loop** |
|---|---|---|
| App sampling | 47/54 · named 24/24 · vague 15–18/21 · no tool 7–8/9 | **50/54** · named 24/24 · vague 17/21 · no tool 9/9 |
| Temperature 0 | 16–17/18 | **16/18**, one of them an ERROR: Ollama refused a connection |

Bar: ≥ 44/54 with named ≥ 23/24. **Cleared.** Saved in `evals/out/H2-find-1792046005/`.

| taskprobe | Stage H (last runs) | **Shared loop** |
|---|---|---|
| App sampling | 27/48, 28/48, 29/48 (spread 25–32 on the same code) | **30/48** |
| Temperature 0 | 9/16, 10/16 | **9/16** |
| Errors | 0 | **0** |
| Rounds a run · checks a run · asks a run | 5.8 · 1.4 · 2.8 (H9) | 4.2 · 1.4 · 2.8 |
| Time a run | 35.6 s (H9) | 43.3 s |

Bar: ≥ 25/48 and ≥ 7/16. **Cleared.** The same tasks pass and fail as at H9, run for run, except that
b02 now passes twice (never at H9). No read was declined and no run wrote until the limit. Saved
in `evals/out/H3-task-1792083006/`.

*The first taskprobe run did not count.* At about 01:00 git stopped starting on this machine — every
`git worktree add` failed with no message, and a pruned worktree stayed registered — so every run
from b09 on was booked ERROR. Up to then, 49 of its 64 runs, it matched H9 task for task. With git
working again in the morning, the run was repeated from the same executable; the table above is
the repeat.

## End to end

| | Result |
|---|---|
| The nine Mimir scripts, after 2b and its fixes | **9/9**, 208/208 checks |
| The same, on 2c's core | 9/9 scripts; 207/208 checks the first time — `mimir-check` 3, *the model quotes the error it was given*: it "fixed" `Cargo.toml` instead of quoting. Rerun alone twice: 2/2 both times. The model's choice, not the loop's: with the app's `Env`, `round_in` and `run_approved_in` do exactly what `round` and `run_approved` did |
| The Stop scenario above | 18/18, on 2b and again on 2c |

## Checks

- `cargo test`: bifrost-core 494 (+2 ignored), mimir 11, bench 123 (each probe binary carries
  probe.rs's 19 tests). `cargo clippy` on all five crates: clean. `npx tsc`, `check:inert`: clean.
- `taskprobe --selfcheck`: every task solvable and not already solved.
- The pinned benchmark strings: untouched (`git diff`).
- **An independent review** walked the old TypeScript loop against the Rust one, branch by branch,
  and `round` against the old command: the bounds, every Stop check, the card states, the refusal
  wording, the failed-edit rule, the pairs sent back, the set-aside replacement, calibration, the
  closing request and its banner, the event shapes. **No behavior lost**; the two Stop findings
  above were the only regressions, and both are fixed.

## Different from the packet

- **`decide(card, allow)`, not `decide(token, …)`.** The window names the card; the token never
  leaves Rust, which is the stronger form of the same guarantee.
- **`AllowForSession` exists but acts as `Allow`** until Hugin's I3 gives it memory.
- **"Persistence after each card"**: the old loop saved after a card only when a Stop landed with
  one on screen, and the thread at the end of every turn. Kept exactly so.
- **Known, as before:** a Stop that lands within milliseconds of Rust's own check after a result
  arrives saves that result on the next save rather than at once. The TypeScript loop had the same
  gap between its check and its update.

## Next

Gate 1: Mimir as it was, on the shared loop — the numbers above, and a day of real use.
