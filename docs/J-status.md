# Stage J — where it stands

Every chunk and gate of [STAGE-J.md](../stages/STAGE-J.md), done or not. **Updated at the end of
every chunk.** Last updated 2026-11-01, when Gate J0 closed Phase 0.

**Started 2026-10-25.** One chunk at a time; in the runs, one model per session, with a
report after each.

| Phase | Chunk | What it is | Status | Result |
|---|---|---|---|---|
| **0. Fair** | J0a | A clean start: commit and push the pending Hugin work, the folder move | ✅ Done | Committed `fd3fa57` and pushed. The folder move ran 2026-10-25: repo, Claude Code memory and config all moved, everything rebuilt; 1,066 tests pass in the new place (2 ignored, need Ollama). The Startup `Bifrost.lnk` still points to `…\odin` until Vidar's next start repairs it |
| | J0b | Tool-call repair for every model, not only Qwen | ✅ Built · local | Every model's unreadable call is fed back; Qwen keeps its own repair; real failures still end the turn; `--model` and `--trace` for the runs — [J0b](results/J0b-results.md) |
| | J0c | Upgrade Ollama (0.33.3 → current) | ✅ Done, one open item | 0.40.11; the current model passes toolprobe 10/10 and taskprobe 9/16; the apps run; one render-fidelity test fails (same token counts, different tool chosen) — [J0c](results/J0c-results.md) |
| | J0c+ | The upgrade's leftovers: the raw retry must sample like `/api/chat`; no stray `<tool_call>` in answers | ✅ Built · local, one check short | Not sampling: the prompt cache. Both paths build the same prompt and sample the same way; the tool chosen at t=0 follows what `llama-server` has cached. The test now warms up first and passes 5/5. Dangling `<tool_call>` tags are cut from answers. toolprobe 10/10; taskprobe propose 10/16 at app settings; the t=0 pass was stopped for low memory after 9 tasks — [J0c+](results/J0c+-results.md) |
| | J0c+ close-out | One clean taskprobe run | ✅ Done 2026-11-01 · local | The first try crashed the PC: b06's model code committed 81 GB. Runs are now capped (`ba3fa74`). The clean run: **10/16** at app settings, 9/16 at t=0 (complete), 0 errors — [J0c+](results/J0c+-results.md) |
| | J0d | Thinking models: `think` and per-model settings | ✅ Built · local | `model_settings` per model; `think` sent only to models that report it; reasoning kept within a turn, cleared after. gpt-oss:20b thinks in Hugin `--ask` and `-p`, toolprobe 9/10 at `high`; qwen3-coder unchanged — [J0d](results/J0d-results.md) |
| | Gate J0 | Review, clippy, `cargo test --workspace`, then push `stage-i` | ✅ Done 2026-11-01 · pushed | The checker's review found no blocker; seven fixes in their own commit: a disk-full check reads as unverified, old thinking can be compacted away, two more runner errors end the turn, the job-assign race, a fair disk floor on a low drive, the whole-run cap raised to 24 GB (a workspace rebuild peaked at 11.8 GB), and `note` in Hugin's trace. 1,089 tests pass, clippy clean |
| **1. Tests** | J1a | realprobe: 10 real tasks with hidden tests | Not started | |
| | J1b | askprobe: 12 code questions | Not started | |
| | J1c | Speed and memory, measured the same way for every model | Not started | |
| | Gate J1 | The harness frozen; the bars set before any run | Not started | |
| **2. Runs** | J2 | Screen: one model per session | Not started | |
| | Gate J2 | At most two finalists | Not started | |
| | J3 | Run of record: the baseline and the finalists | Not started | |
| **3. Switch** | J4 | Switch the coding model, only on a clear win | Not started | |
| | J5 | Tuning: KV cache and context size | Not started | |
