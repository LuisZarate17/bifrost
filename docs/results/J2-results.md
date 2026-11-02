# J2 — the screen, one model per step

The probes are locked at `j1-frozen`. Ollama 0.40.11, `num_ctx` 32768. Each model runs:
speedprobe, toolprobe, askprobe ×1, taskprobe propose ×1, and realprobe ×1 when the gate holds
(toolprobe ≥ 9/10 and taskprobe app ≥ B − 3). The step scripts unload every model afterwards.
Run overnight 2026-11-01→02 on autopilot.

| Model | realprobe PASS | false done | askprobe R/P/W | toolprobe | taskprobe app / t=0 | decode tok/s 2k / 16k | on CPU |
|---|---|---|---|---|---|---|---|
| **B: qwen3-coder:30b** | **4/10** | 0 | 4 / 6 / 2 | 10/10 | 10/16 / 9/16 | 73.8 / 67.5 | 33.9% |
| gpt-oss:20b, think high | — (gate: taskprobe 4 < 7) | — | 2 / 8 / 2 | 9/10 | 4/16 / 4/16 | 167.6 / 155.8 | 0% |
| **qwen3.6:35b-a3b** (thinks by default) | **7/10** | **3** | 6 / 6 / 0 | 10/10 | 12/16 / 11/16 | 107.9 / 111.5 | 43.1% |

## B: qwen3-coder:30b (20:02–21:03)

- **realprobe 4/10:** PASS on 02, 03, 07 and 09. FAIL on:
  - 01, 05 and 10: its code doesn't compile (unbalanced braces);
  - 04: Hugin `review`, because it deleted two existing tests;
  - 06: 4 of 5 hidden tests;
  - 08: 5 of 6 hidden tests.
- **askprobe:** 4 right, 6 partly, 2 wrong. The locked practice baseline was 5/6/1, so one answer
  moved between runs.
- **Speed:** prefill about 2,640 tok/s.

## gpt-oss:20b, think high (21:04–22:11)

- **Out at the screen gate:** taskprobe 4/16 at app settings and 4/16 at t=0, more than 3 below
  B's 10, so realprobe was not run.
- **What went wrong:** no errors. It took 9.1 rounds per run against B's 4.5, and averaged 20
  automatic calls per run (B: 1.6), so it spent its rounds looking around. 111 s per run.
- **askprobe:** 2 right, 8 partly, 2 wrong.
- **The fastest model here:** it fits on the card entirely, with decode at 168 tok/s at 2k and
  prefill about 10,000 tok/s.

## qwen3.6:35b-a3b, its own think default (22:12–23:17)

- **Download:** 21 GB in about 5 minutes; digest `a7eb95c5…`. It reports completion, vision,
  tools and thinking (on by default).
- **realprobe 7/10**, PASS on 02, 03, 04, 06, 07, 09 and 10. **3 false dones**, where it said done
  but was wrong: 01 and 05 (the unknown-flag case) and 08 (a colon elsewhere).
- **askprobe:** 6 right, 6 partly, 0 wrong, the best so far.
- **taskprobe:** 12/16 at app settings, 11/16 at t=0.
- **2 infrastructure errors** in taskprobe (b06 and b07 at app settings; 1 at t=0): Ollama
  returned `500 {"error":"EOF"}`, so its runner dropped the request. Not the model's code and not
  the harness, but switching rule 4 allows at most 1 in J3, so watch it there.
- **Speed:** decode 108–112 tok/s, about 1.5× B's, even with 43% on the CPU. Prefill about 1,950
  tok/s.
