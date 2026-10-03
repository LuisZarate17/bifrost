# Stage H — where it stands

Every chunk and gate of [STAGE-H.md](../stages/STAGE-H.md), done or not. **Updated at the end of
every chunk**, alongside its results doc. Last updated 2026-10-02, after H6.

**8 of 12 chunks done · 2 of 3 gates passed.** Next: Gate 3, then Phase 4.

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
| | H6 | Undo a whole turn | ✅ Done · local | A two-file turn back byte for byte — [H6](results/H6-results.md) |
| | Gate 3 | Does it work in daily use? | ⬜ **Next** — your call, after using it | |
| **4. Longer runs** | H7 | Context budget inside a turn | ⬜ | |
| | H8 | Partial reads (line ranges) | ⬜ | |
| | H9 | File operations (delete, move, new folders) | ⬜ | |
| | H10 | AGENTS.md, a task list, a repo map | ⬜ | Measure the repo map first: Qwen3-Coder already answers 15 of 21 vague find-code questions |

## Also open

- **Unpushed:** H4a, H4b, H5 and H6 are local, not pushed to origin/main.
- **Checks by hand waiting** in [PENDING-CHECKS.md](PENDING-CHECKS.md): H1, H2, H4b, H5 and H6, with
  the steps in [H-testing.md](H-testing.md).
