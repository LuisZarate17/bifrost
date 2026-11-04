# J1 — the tests the models are judged on

**Built 2026-11-01.**
- **realprobe**: 10 real tasks in this repo, with hidden tests (`evals/realprobe/`).
- **askprobe**: 12 code questions, scored by facts and files read (`evals/askprobe/`).
- **speedprobe**: speed and memory.

All three are pinned at `0940923`. The answers and patches are rechecked against that commit, not
against HEAD.

## Self-checks (no model)

- **realprobe:** for all 10 tasks, the reference is correct, the pin is FAIL, and every one of 26
  wrong patches is FAIL. The hidden tests can't be reached from the pin, and no reference edits an
  existing test (Hugin flags any such edit as `review`, so it could never count as `done`).
- **askprobe:** for all 12 questions, a hand-written right answer scores right and a wrong one
  scores wrong. Every expected file exists at the pin.
- **Both:** a model Ollama doesn't list is refused before any work.

## Practice run: qwen3-coder:30b, the baseline (B)

Ollama 0.40.11, `num_ctx` 32768, think unset. Run 2026-11-01, 17:58–18:44. No harness errors in any
of the three.

### realprobe ×1: 4 of 10 PASS, 1 false done, 5 FAIL, 0 ERROR

| Task | Verdict | Hugin | Why |
|---|---|---|---|
| 01 taskprobe `--help` | FAIL | failed, 435 s | the model removed `arg`, so its code doesn't compile |
| 02 `hugin --version` | **PASS** | done, 132 s | |
| 03 npx/pnpm/yarn `.cmd` | **PASS** | done, 53 s | |
| 04 `LoadedModel` size/digest | **PASS** | done, 241 s | |
| 05 toolprobe `--help` | **FALSE DONE** | done, 69 s | 8 of 9 hidden tests; an unknown flag isn't handled |
| 06 `--ask` flag as question | FAIL | failed, 334 s | 4 of 5 hidden tests |
| 07 model_settings keeps unknown keys | FAIL | failed, 437 s | its own new tests fail |
| 08 disk-full wording | FAIL | failed, 415 s | 4 of 5 hidden tests. **Task replaced after review**, see below |
| 09 empty check is none | **PASS** | done, 42 s | |
| 10 recent folders, trailing slash | FAIL | failed, 235 s | its own new test fails |

Tasks 02 and 03 are I5 tasks the same model got wrong in Stage I. They now pass, which is credit to
the Phase 0 engine fixes and the guards.

### askprobe ×1: 5 right, 6 partly, 1 wrong, 13.2 s per answer (rescored with the frozen patterns)

- **Wrong:** Q01 (where a folder's check is looked up). It answered from `turn.rs` and read neither
  expected file.
- **Partly:** Q04, Q08 (named no memory limits), Q09, Q10, Q11 and Q12. Q10 first scored right,
  because a pattern matched the plain word "budget". The answer never gave the 75% figure.
- One Q11 answer ended in a stray `<tool_call>` tag. That's an engine leftover, noted for later.
- **Scoring fixed after the run:** Q09's capability fact missed ``detail.has("thinking")``, because
  the dot in the code broke the pattern. A pattern for it was added before the lock. Q09 would still
  score partly, since `request_body` wasn't named.

### speedprobe (the run after the review fixes)

- **2k and 16k:** prompts are now calibrated to land near their target, and every decode runs the
  full 128 tokens.
- **2,006 prompt tokens:** prefill 2,650 tok/s, decode **73.9 tok/s**.
- **15,925 prompt tokens:** prefill 2,605 tok/s, decode **67.0 tok/s**.
- **RAM:** free RAM dropped **1,671 MiB** during a cold `cargo build -p bifrost-core`. Compare the
  drop, not the start or the lowest, because those depend on what else is running.

The first run, before the fixes:


- **Load:** 21,061 MiB in all. 13,928 MiB is on the card and **33.9% on the CPU**; Windows counts
  7,214 MiB of shared GPU memory for the runner. The card went from 1,101 to 15,250 MiB used. Loaded
  in 10.8 s.
- **Prefill:** 2,537 tok/s at 2.7k prompt tokens and 2,493 tok/s at 22k.
- **Decode:** **73.0 tok/s** at 2.7k and **64.1 tok/s** at 22k.
- **RAM:** with the model loaded, free RAM was 6,177 MiB, and a cold `cargo build -p bifrost-core`
  took it to a low of **4,485 MiB**.
- **Digest:** `06c1097efce0…`.

## What the review before the lock changed

An independent review of the three probes found these. All were fixed before the lock, and both
self-checks hold again.

- **Task 08 could never be PASS.** Its reference had to edit existing assertions, which Hugin's
  bent-test guard reports as `review`. It is replaced by `08-search-colon-pattern`: a colon in a
  search pattern is no longer reported as "outside the project folder". 6 hidden tests, 3 wrong
  patches. The self-check now proves no reference edits an existing test.
- **A model's test that hangs** is now that model's FAIL ("tests hung", 300 s test phase), not an
  ERROR. ERRORs count against the infrastructure gate.
- **Scoring runs are sandboxed** (APPDATA, LOCALAPPDATA, TEMP), so a model-written test can't touch
  the real config. Hugin's per-run build cache is deleted after each run.
- **Task 06's and task 01's hidden tests:** one assertion each relaxed to match the task text.
- **askprobe's patterns:** dots in code no longer break a match. Correct phrasings the review
  found rejected are accepted, and loose ones are tightened. About 80 sentences are pinned in a
  unit test.
- **speedprobe:** prompt sizes are calibrated, a truncated prompt is excluded, decode runs the full
  128 tokens, and the RAM drop is reported.

A real task 09 run through the fixed realprobe on qwen3-coder: PASS, no harness error.

## Switching rules, set before any run

Copied from `stages/STAGE-J.md` and **fixed now, before J2**. B is qwen3-coder:30b, measured again
in J3 at ×3.

Switch `coding_model` only if **all** of these hold:
1. realprobe PASS ≥ **B + 7 of 30**, and the candidate wins at least 3 more tasks than it loses (a
   task counts by the majority of its 3 runs).
2. False done ≤ max(B's, 2).
3. taskprobe propose ≥ **25/48**.
4. No turn-ending tool-call errors, at most 1 infrastructure error, and toolprobe ≥ 9/10.
5. Median time per run ≤ **2× B's**, with no paging.

If two models qualify, take the higher realprobe score; if they're within 2 of 30, take the faster.
A separate fast `ask_model` only if it beats the coding model by ≥ 3/12 on askprobe and swapping
takes ≤ 15 s.

**J2's screen gate**, from STAGE-J: realprobe ×1 runs only if toolprobe ≥ 9/10 and taskprobe (at
the app's settings) is no more than 3/16 below B's on the same screen.

**Gate J2, the finalist rule, fixed now:** a finalist has more realprobe PASSes than B on the screen,
or the same number with fewer false dones. At most two go to J3, ranked by PASS count, then fewer
false dones, then the faster median Hugin time. If none qualifies, J3 is not run and the result is
"keep qwen3-coder".

**Screen order:** B = qwen3-coder:30b first, then gpt-oss:20b (think high), qwen3.6 35B-A3B (its own
think default) and ornith-1.5:9b. flint:40b is left out until an Ollama fix.

**Offline:** every cargo run the probes start, Hugin's own check included, has
`CARGO_NET_OFFLINE=true`. A patch that adds a dependency fails to build; nothing is downloaded or
run from the network.

## The lock

The `j1-frozen` tag marks the commit after which nothing in `evals/realprobe`, `evals/askprobe`, or
realprobe, askprobe and speedprobe changes until Phase 2 is over.
