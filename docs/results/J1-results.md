# J1 — the tests the models are judged on

**Built 2026-11-01.**
- **realprobe**: 10 real tasks in this repo, with hidden tests (`evals/realprobe/`).
- **askprobe**: 12 code questions, scored by facts and files read (`evals/askprobe/`).
- **speedprobe**: speed and memory.

All three are pinned at `0940923`. The answers and patches are rechecked against that commit, not
against HEAD.

## Self-checks (no model)

- **realprobe:** for all 10 tasks, the reference is correct, the pin is FAIL, and every one of 26
  wrong patches is FAIL. The hidden tests can't be reached from the pin.
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
| 08 disk-full wording | FAIL | failed, 415 s | 4 of 5 hidden tests |
| 09 empty check is none | **PASS** | done, 42 s | |
| 10 recent folders, trailing slash | FAIL | failed, 235 s | its own new test fails |

Tasks 02 and 03 are I5 tasks the same model got wrong in Stage I. They now pass, which is credit to
the Phase 0 engine fixes and the guards.

### askprobe ×1: 6 right, 5 partly, 1 wrong, 13.2 s per answer

- **Wrong:** Q01 (where a folder's check is looked up). It answered from `turn.rs` and read neither
  expected file.
- **Partly:** Q04, Q08 (named no memory limits), Q09, Q11 and Q12.
- One Q11 answer ended in a stray `<tool_call>` tag. That's an engine leftover, noted for later.
- **Scoring fixed after the run:** Q09's capability fact missed ``detail.has("thinking")``, because
  the dot in the code broke the pattern. A pattern for it was added before the lock. Q09 would still
  score partly, since `request_body` wasn't named.

### speedprobe

- **Load:** 21,061 MiB in all. 13,928 MiB is on the card and **33.9% on the CPU**; Windows counts
  7,214 MiB of shared GPU memory for the runner. The card went from 1,101 to 15,250 MiB used. Loaded
  in 10.8 s.
- **Prefill:** 2,537 tok/s at 2.7k prompt tokens and 2,493 tok/s at 22k.
- **Decode:** **73.0 tok/s** at 2.7k and **64.1 tok/s** at 22k.
- **RAM:** with the model loaded, free RAM was 6,177 MiB, and a cold `cargo build -p bifrost-core`
  took it to a low of **4,485 MiB**.
- **Digest:** `06c1097efce0…`.

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

**J2's screen gate**, from STAGE-J: realprobe ×1 runs only if toolprobe ≥ 9/10 and taskprobe is no
more than 3/16 below B. At most two finalists go to J3, each above B on the screen.

## The lock

The `j1-frozen` tag marks the commit after which nothing in `evals/realprobe`, `evals/askprobe`, or
realprobe, askprobe and speedprobe changes until Phase 2 is over.
