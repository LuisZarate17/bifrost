# H7 — A context budget inside the turn

Inside one tool turn, every result now counts against the model's context, and old ones are set
aside before Ollama has to cut. **Built 2026-10-02.**

## The problem, measured

Inside a turn, Mímir appended every tool result to the conversation and counted nothing; the window
only trimmed old *turns*, once per send. Since H5, reads inside the folder run without asking, so
nothing made a turn piling up 64 KB reads visible.

What Ollama 0.33.3 does when the prompt is longer than `num_ctx` — it does **not** error:

| Prompt | `num_ctx` | `prompt_eval_count` | What survived |
|---|---|---|---|
| system + 14k tokens of chat + a question | 32768 | 14,360 | everything — answered as told |
| the same | 4096 | **43** | the system message and the last message; the 14k between gone |
| system, **the request**, read `a.rs` (11k), read `b.rs` (11k) | 32768 | 23,695 | everything — answered the request |
| the same | 8192 | **4,098** | only the tail of `b.rs` — **the request was gone**, and the model answered about the code |

The user's request is lost without a word.

## What it does

- **Before every round of a turn, the prompt is estimated** — characters over a characters-per-token
  ratio learned from the round before (Ollama reports what it really counted), carried from turn to
  turn within a conversation; 2.5 when there is nothing to learn from, which is conservative for
  code.
- **Past 75% of the context, old results are set aside, oldest first.** A result becomes one line —
  *[Set aside to fit the model's context: the earlier result of read_file src/x.rs (N lines). Read it
  again if you still need it.]* — and a long old `create_file` or `new_text` argument becomes
  *[N characters, already sent]*. Nothing is dropped: every call stays paired with its result, and
  the model is told how to get it back.
- **Never set aside:** the system prompt, your messages, the last message, the newest result.
  **The newest read of each file goes last** — a first pass leaves those alone, a second takes them
  oldest first only if the turn still does not fit. (Protecting them outright, as first designed,
  would have protected every file read once — the very case this exists for. The builder caught it.)
- **One-way.** What was set aside stays set aside for the rest of the turn, in the window's copy too,
  so each round's prompt starts exactly as the last one did and Ollama's prompt cache keeps working.
  The cards — and the saved conversation — keep every result in full.
- **The thread says so:** *Set aside 2 earlier results to fit the model's context.* Past 90% with
  nothing left to set aside: *This turn no longer fits the model's context — start a new chat, or
  ask about less at once.* (First drawn at 75%, it fired on a turn that fitted at 81% and answered
  correctly; 90% is where the request is really at risk.)
- **The context meter now moves during a turn**, from Ollama's own count each round, not only at
  the end.
- **Rounds stay at 10.** Across the three Qwen3-Coder benchmarks before H7 — 192 runs — no task
  passed after round 10 under the probe's bound of 20. In H7's two runs, one did, once, out of 128.

## Measured

**Same build, `taskprobe --compact off` then `--compact on`**, Qwen3-Coder, `num_ctx` 32768. The
probe now records Ollama's prompt count every round and flags a **cut**: within a turn the prompt
only grows, so a round reported smaller than the one before, with nothing set aside, was cut.

| | Off | On |
|---|---|---|
| Pass, app sampling | 26/48 (54%) | 30/48 (62%) |
| Pass, temperature 0 | 9/16 | 10/16 |
| Errors | 0 | 0 |
| Largest prompt, any round | 20,098 / 15,434 (t=0) | 20,810 / **24,887** (t=0) |
| Runs Ollama cut | **0** | **0** |
| Results set aside | — | 4, in one run |

**On this benchmark at 32k, the turns stay under budget**, so H7 mostly has nothing to do: the
pass-rate difference (26 vs 30) is the run-to-run spread seen before (25–29). It does no harm, which
is what Phase 4 requires of every chunk. **The one run it acted in shows it working:** `b07` at
temperature 0 ran 13 rounds, its prompt climbing 1.7k → 23k tokens; four old results were set aside
and every later round stayed inside the context, with nothing cut — it still failed, as `b07` always
has, but on its own merits. Saved in `evals/out/H3-task-1791002629/` (off) and `…1791005230/` (on);
logs in `evals/out/H7-2026-10-02/`.

**Where it matters, end to end** — `tools/e2e/mimir-context.mjs`, 8/8: at `num_ctx` 8192 the real
model read three ~14,000-character files in one turn; the thread said *Set aside 2 earlier results*,
the answer still named the purple giraffe from the request and the last file's marker, the meter
read 6,701 of 8,192, and the saved conversation kept the full files. At 32k with small files,
nothing was set aside and no note appeared.

## Checked

| | |
|---|---|
| Unit tests | `odin_core::compact` 16 (oldest first; the request, the newest result never; the newest read per file only in the second pass; two files read once each, as measured; one file spelt two ways; long arguments; one-way; over the budget but under the hard limit still fits; calibration, a cut round's ratio thrown away; the count includes arguments and schemas) · mimir 27 · odin-core 242 · bench 19 · clippy clean |
| e2e | mimir-context 8/8 (new) · mimir-edit 25/25 · mimir-check 26/26 · mimir-policy 44/44 · mimir-switch 14/14 |

## Review

A checker found one real bug and five smaller things, all fixed:

1. **A false "no longer fits" could stick for a whole turn.** The first round of every turn guessed
   the ratio, could judge a long chat over budget, and nothing cleared the flag once the next round
   measured it. The flag is now set every round, and a turn starts from the ratio the conversation's
   last turn measured.
2. A late reply after Stop could move the meter of a conversation opened meanwhile.
3. **A cut round's count is too small to learn from** (14,000 tokens reported as 43): such a ratio
   is now thrown away rather than clamped to the ceiling, which would have made the next round's
   estimate twice as generous and invited another cut.
4. The probe counted a reply with no prompt count as a cut.
5. `./src/A.rs` and `src\a.rs` counted as different files.
6. The claim behind keeping 10 rounds now cites the runs it rests on.

## Accepted

- **It is an estimate.** Characters over a measured ratio, not a tokenizer; the 75% target leaves
  the slack, and the probe's cut detector watches for misses.
- **The window's own turn trimming** (once per send, a quarter of characters a token) is unchanged.
- **The other probes now set results aside too** (`fixprobe`, `findprobe`, `editprobe`), as the app
  does; none of them comes near the budget.

## Files

`crates/odin-core/src/compact.rs` (new) · `tools.rs` (the `MAX_ROUNDS` note) ·
`apps/mimir/src-tauri/src/lib.rs` (`ask_with_tools`: calibration in, counts and set-aside out) ·
`apps/mimir/src/App.tsx`, `App.css` (applying it, the meter, the notes) · `crates/bench/src/probe.rs`,
`bin/taskprobe.rs` (`--compact`, prompt counts, cuts) and the other probes ·
`tools/e2e/mimir-context.mjs` (new)
