# Stage H — where it stands

Every chunk and gate of [STAGE-H.md](../stages/STAGE-H.md), done or not. **Updated at the end of
every chunk**, alongside its results doc. Last updated 2026-10-09, after review round 1.

**12 of 12 chunks done · 3 of 3 gates passed. Stage H is complete.**

| Phase | Chunk | What it is | Status | Result |
|---|---|---|---|---|
| **1. Find and check** | H1 | `run_check`: the model runs your build check | ✅ Done · pushed (f95365f) | Fix tasks 33% → 79% — [H1](results/H1-results.md) |
| | H2 | `list_files` and `search` | ✅ Done · pushed (6653c27) | Finding named code 0% → 88% — [H2](results/H2-results.md) |
| | Gate 1 | Do the new tools measurably help? | ✅ Passed | |
| **2. Benchmark and model** | H3 | taskprobe, the coding benchmark | ✅ Done · pushed (2e89e1a) | Old model: 12% — [H3](results/H3-results.md) |
| | H4 | Three coding models tested | ✅ Done · pushed (0a3796c) | Qwen3-Coder 25/48 — [H4](results/H4-results.md) |
| | Gate 2 | Which model? | ✅ Qwen3-Coder | |
| | H4a | Qwen3-Coder's tool calls recovered | ✅ Done · local (f951507) | 28/48, no errors — [H4a](results/H4a-results.md) |
| | H4b | Choosing a folder offers the coding model | ✅ Done · local (06e72e9) | e2e 14/14 — [H4b](results/H4b-results.md) |
| **3. Approval** | H5 | In-folder reads run without asking | ✅ Done · local (ee6c1e5) | Cards per task 3.9 → 2.5 — [H5](results/H5-results.md) |
| | H6 | Undo a whole turn | ✅ Done · local (8e98862) | A two-file turn back byte for byte — [H6](results/H6-results.md) |
| | Gate 3 | Does it work in daily use? | ✅ Passed on your "continue" — daily use is still the real test | |
| **4. Longer runs** | H7 | Context budget inside a turn | ✅ Done · local (7d131af) | At 8k the request survives; benchmark unharmed — [H7](results/H7-results.md) |
| | H8 | Partial reads (line ranges) | ✅ Done · local (768b508) | A 120 KB file edited at line 2102; benchmark holds — [H8](results/H8-results.md) |
| | H9 | File operations (delete, move, new folders) | ✅ Done · local (4f91bab) | opsprobe 15/15, every run restored by Undo all — [H9](results/H9-results.md) |
| | H10 | AGENTS.md, a task list, a repo map | ✅ Done · local (897f38b) | Notes and a task list ship; the map was measured and left out; acceptance 2/3, every run at the round cap — 3/3 with no cap since review round 1 — [H10](results/H10-results.md) |

## The independent review

[H-review.md](H-review.md) (2026-10-09): 1 high, 10 medium, 31 low findings, and doc drift.

| Round | What | Status | Result |
|---|---|---|---|
| 1 | R-01 to R-11: the high and the ten mediums | ✅ Done · local (fdfac8a, 5fc2d5a, c290697, b1909a6, dbb5826; follow-up c5a127c) | Protected files stay protected when an edit misses; a turn always ends in the model's answer; acceptance 3/3 with no cap — [round 1](results/H-review-round1.md) |
| 2 | R-12 to R-42 (the lows) and the rest of the doc list | Not started | |

## Also open

- **Unpushed:** H4a through H10 and review round 1 are local; they go up together when Luis says so.
- **Checks by hand waiting** in [PENDING-CHECKS.md](PENDING-CHECKS.md): H1, H2, H4b, H5, H6, H7, H8, H9 and H10, with
  the steps in [H-testing.md](H-testing.md).
