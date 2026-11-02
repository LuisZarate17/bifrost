# J2 — the screen, one model per step

The probes are locked at `j1-frozen`. Ollama 0.40.11, `num_ctx` 32768. Each model runs:
speedprobe, toolprobe, askprobe ×1, taskprobe propose ×1, and realprobe ×1 when the gate holds
(toolprobe ≥ 9/10 and taskprobe app ≥ B − 3). The step scripts unload every model afterwards.
Run overnight 2026-11-01→02 on autopilot.

| Model | realprobe PASS | false done | askprobe R/P/W | toolprobe | taskprobe app / t=0 | decode tok/s 2k / 16k | on CPU |
|---|---|---|---|---|---|---|---|
| **B: qwen3-coder:30b** | **4/10** | 0 | 4 / 6 / 2 | 10/10 | 10/16 / 9/16 | 73.8 / 67.5 | 33.9% |

## B: qwen3-coder:30b (20:02–21:03)

- **realprobe 4/10:** PASS on 02, 03, 07 and 09. FAIL on:
  - 01, 05 and 10: its code doesn't compile (unbalanced braces);
  - 04: Hugin `review`, because it deleted two existing tests;
  - 06: 4 of 5 hidden tests;
  - 08: 5 of 6 hidden tests.
- **askprobe:** 4 right, 6 partly, 2 wrong. The locked practice baseline was 5/6/1, so one answer
  moved between runs.
- **Speed:** prefill about 2,640 tok/s.
