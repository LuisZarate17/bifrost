# J3 — the run of record

B and Gate J2's two finalists:
- realprobe ×3 (30 runs), taskprobe propose ×3 and askprobe ×2;
- probes locked at `j1-frozen`, Ollama 0.40.11, `num_ctx` 32768;
- each model's digest checked against J2 before it ran;
- run overnight 2026-11-02 on autopilot.

| Model | realprobe PASS /30 | false done | ERROR | taskprobe app /48 | askprobe R/P/W /24 | median Hugin s/run |
|---|---|---|---|---|---|---|
| **B: qwen3-coder:30b** | **13** | 1 | 0 | 28 (1 error) | 9 / 10 / 5 | 155.6 |

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
