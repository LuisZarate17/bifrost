# J3 — the run of record

B and Gate J2's two finalists:
- realprobe ×3 (30 runs), taskprobe propose ×3 and askprobe ×2;
- probes locked at `j1-frozen`, Ollama 0.40.11, `num_ctx` 32768;
- each model's digest checked against J2 before it ran;
- run overnight 2026-11-02 on autopilot.

| Model | realprobe PASS /30 | false done | ERROR | taskprobe app /48 | askprobe R/P/W /24 | median Hugin s/run |
|---|---|---|---|---|---|---|
| **B: qwen3-coder:30b** | **13** | 1 | 0 | 28 (1 error) | 9 / 10 / 5 | 155.6 |
| qwen3.6:35b-a3b (stopped) | **19 of 25 run** | 2 | 0 | not run | not run | — |
| ornith-1.5:9b (early stop) | **12 of 21 run** | 1 | 0 | not run | not run | — |

## B: qwen3-coder:30b (02:24–05:27)

Per task, across 3 runs:

| Task | Runs | Majority |
|---|---|---|
| 01 taskprobe `--help` | FAIL FAIL FAIL | fail |
| 02 `hugin --version` | FAIL PASS PASS | pass |
| 03 npx/pnpm/yarn | FAIL FAIL PASS | fail |
| 04 `LoadedModel` size/digest | PASS PASS PASS | pass |
| 05 toolprobe `--help` | FAIL FAIL FALSE-DONE | fail |
| 06 `--ask` flag | FAIL FAIL FAIL | fail |
| 07 model_settings unknown keys | FAIL FAIL FAIL | fail |
| 08 colon in a pattern | FAIL FAIL PASS | fail |
| 09 empty check | PASS PASS PASS | pass |
| 10 recent folders slash | PASS PASS PASS | pass |

- **realprobe 13/30** (12 within 540 s), 1 false done. Majority passes on 4 tasks.
- **taskprobe:** 28/48 at app settings, with 1 infrastructure error. 9/16 at t=0.
- **askprobe:** 9 right, 10 partly, 5 wrong, at 14.6 s per answer.

## qwen3.6:35b-a3b (05:27–06:34), stopped partway

Luis called the bake-off at 06:34 on 2026-11-02, partway into qwen3.6's realprobe ×3: 25 of 30
runs were done. taskprobe ×3, askprobe ×2 and ornith's J3 were never run. Everything was then
stopped: no model loaded, no worktree, no probe running.

| Task | Runs | Majority |
|---|---|---|
| 01 taskprobe `--help` | CORRECT-NOT-DONE, FALSE-DONE, PASS | — |
| 02 `hugin --version` | PASS PASS PASS | pass |
| 03 npx/pnpm/yarn | PASS PASS PASS | pass |
| 04 `LoadedModel` size/digest | PASS PASS PASS | pass |
| 05 toolprobe `--help` | FALSE-DONE PASS PASS | pass |
| 06 `--ask` flag | PASS PASS PASS | pass |
| 07 model_settings unknown keys | CORRECT-NOT-DONE PASS PASS | pass |
| 08 colon in a pattern | FAIL FAIL PASS | fail |
| 09 empty check | PASS (1 of 3 run) | — |
| 10 recent folders slash | not run | — |

## Where this leaves the switching rules

The bake-off is **incomplete**, so the rules can't be applied in full:
- taskprobe ×3 isn't measured for either finalist;
- 5 of qwen3.6's realprobe runs are missing;
- ornith wasn't run at all.

What the data does say:

- **Rule 1 (realprobe ≥ B + 7 = 20/30): qwen3.6 has 19 of 25.** Its pass rate (76%) projects to
  about 23/30, against B's 13/30 (43%). It wins at least 6 tasks by majority (02, 03, 05, 06, 07,
  plus 04 as a tie) against B's 4, and loses none: 08 fails for both.
- **Rule 2 (false done ≤ max(B's 1, 2)): qwen3.6 has 2 so far.** At the limit, so it would fail
  this rule with one more.
- **Rule 4:** qwen3.6 had 2 infrastructure errors (Ollama `500 EOF`) in its J2 taskprobe, against
  a limit of 1. Not re-measured.
- **Rule 5 (speed):** qwen3.6 decodes at about 1.5× B's speed, so it passes easily.

**No switch was made.** J4 needs Luis's OK. The recommendation, if he wants one, is in the session
summary.

## ornith-1.5:9b (08:49–10:18), compared run for run with an early stop

Run after the stop. Realprobe ran task by task, 3 runs each, the same order as
qwen3.6. After each task, from 12 runs on, it stopped early if ornith couldn't reach 17/25 (worse),
or led qwen3.6 by 5 or more on the same runs (better). Otherwise it stopped at 25. The digest
matched J2.

**Verdict: early stop, worse.** After 21 runs: ornith 12 PASS, qwen3.6 17 on the same runs, so it
could no longer reach 17/25.

| Task | ornith | qwen3.6 |
|---|---|---|
| 01 taskprobe `--help` | FAIL FAIL PASS | CORRECT-NOT-DONE, FALSE-DONE, PASS |
| 02 `hugin --version` | PASS PASS PASS | PASS PASS PASS |
| 03 npx/pnpm/yarn | CORRECT-NOT-DONE PASS PASS | PASS PASS PASS |
| 04 `LoadedModel` size/digest | PASS PASS PASS | PASS PASS PASS |
| 05 toolprobe `--help` | FAIL PASS PASS | FALSE-DONE PASS PASS |
| 06 `--ask` flag | CORRECT-NOT-DONE, FALSE-DONE, PASS | PASS PASS PASS |
| 07 model_settings unknown keys | FAIL FAIL FAIL | CORRECT-NOT-DONE PASS PASS |

It matched qwen3.6 on tasks 02–05, then fell behind on 06 and 07, the harder multi-part changes.
1 false done in 21 runs (qwen3.6: 2 in 25). Its runs were slow: 01 took 9–13 minutes per try.

## Conclusion

On real tasks in this repo, **qwen3.6:35b-a3b is the best coder tested**:
- 19/25 (76%), against B's 13/30 (43%) and ornith's 12/21 (57%);
- 1.5× B's decode speed;
- the best answer record after ornith in J2: 6 right, 0 wrong.

Its risks:
- 2 false dones in 25 runs;
- Ollama `500 EOF` errors in its J2 taskprobe.

The full switching rules couldn't be applied, because taskprobe ×3 and askprobe ×2 were never run
for it. **J4 (switching `coding_model`) waits on Luis's OK.**
