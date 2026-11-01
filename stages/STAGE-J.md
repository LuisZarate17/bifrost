# Stage J — the model bake-off: find a local model that makes Hugin and Mimir good

> **Status: started 2026-10-25; in Phase 0.** J0a–J0c are done and pushed. J0c+ is built, waiting on
> one clean taskprobe run. The rest of Phase 0 was set 2026-11-01 (below): J0c+'s close-out, J0d and
> Gate J0, each. Live status: [J-status.md](../docs/J-status.md).

## TL;DR

- **Why.** Hugin is now safe and honest, but its model, Qwen3-Coder 30B, got **0 of 3 real tasks** in
  this repo right. Better local models have come out since Stage H's bake-off: Qwen3.6-35B-A3B,
  gpt-oss-20b, Ornith-1.5, and Flint 40B. The current model doesn't even fit your GPU: about a third of
  it runs on the CPU.
- **How.**
  1. Fix the engine so every model is judged fairly.
  2. Upgrade Ollama.
  3. Build two new tests from **real work in today's repo**: tasks with hidden tests, and code
     questions.
  4. Test the candidates **one model at a time, stopping after each one to update you**.
  5. Run the top two head to head against the current model.
- **Switch only on a clear win**, using rules written down before any run.
- **Your choices (2026-10-23):**
  - upgrade Ollama;
  - commit the pending Hugin work first, and **push it** (it isn't on GitHub yet);
  - a full bake-off, but not in one sitting: **stop after each model and update you**.
- **Cost.** About 15–30 h of machine time, spread over many sessions. Downloads of about 60–90 GB,
  with 207 GB free. Your Claude plan isn't used for the runs; they're all local.

## Context

- **Stage I is closed** (Gate 3 passed 2026-10-22, the `stage-i` branch is pushed, the GitHub repo is
  renamed `bifrost`). Still pending: the uncommitted Hugin work (the push-back on failed checks; the
  honest `-p` status; the guards against faked passes and bent tests; `hugin --ask`) and the folder move.
- **The trigger.** On the three real I5 tasks the model faked a pass, bent a test, and claimed failed
  work was done. The guards now catch all of that, but the work itself is still wrong.
- **The plan's own guess.** The model's skill is the limit.
- **What was ruled out.** Handing work from Claude Code to Hugin saves almost no Claude tokens, so the
  goal is Hugin and Mimir being useful *as your local agent*.
- **What changed since H4** (devstral-small-2 24/48 at 156 s a run, a third on the CPU; qwen3-coder
  25/48 at 32 s; gpt-oss 11/48 with tool errors):
  - new models;
  - a newer Ollama with tool-parser fixes;
  - two unfair things in our engine:
    - tool-call repair exists **only for Qwen** (chat.rs:301-310, 338-354), so another model's
      unreadable call **ends the turn**;
    - "thinking" models are never sent `think`, and their reasoning is dropped mid-turn.
- **What taskprobe can't show.** It scores about 55% but doesn't predict real work in today's repo.
  So realprobe is new.

## Phase 0 — make it fair (engine), one chunk at a time

**J0a · A clean start.**
- **What's on GitHub now (checked 2026-10-23):** every commit, with `stage-i` at `9950e66`. **Not on
  GitHub:** 23 files that were never committed:
  - the push-back on failed checks, the honest `-p` status, and the fake-pass and bent-test guards;
  - `hugin --ask`;
  - the I5 doc updates;
  - the move-script fix.
- GitHub's `main` is still at Stage H.
1. **Commit the pending Hugin work** (your OK given), as one local commit with no attribution lines.
   Prune any worktrees.
2. **Push it:** `stage-i` to GitHub, so everything built so far is saved there. `main` stays at
   Stage H, as you chose; say if you want it moved up to Stage I.
3. **You run the folder move** with everything closed (the steps from last time:
   `tools/move-repo.ps1 -Apply`). Claude Code then continues in `…\repos\bifrost`; its memory moves
   along.
4. All tests green in the new place.

**J0b · Tool-call repair for every model.** In `crates/bifrost-core/src/chat.rs` `chat_tools`:
- A last arm catches any HTTP 500 on a tools request that looks like a tool-call parse error. That's
  today's patterns plus "error parsing tool call", "unmarshal" and "XML".
- It returns the existing stand-in invalid call (`invalid_call`, chat.rs:365) carrying Ollama's
  message, so `round.rs::classify` (round.rs:321-364) sends it back to the model and the turn goes on.
- The existing three-identical-failures rule stops loops.
- Infrastructure errors still end the turn: the runner died, out of memory, a 404.
- Reword the retry ("send one valid tool call", not "as valid JSON"). Count `fed_back` separately
  from `recovered`.
- Add `--model` and `--trace <jsonl>` to `hugin -p` and `--ask`. The trace has one line per request:
  tokens, durations, recovered or fed back, calls per reply, thinking length, error text.

**J0c · Upgrade Ollama** (0.33.3 → current 0.40.x). Flint needs it, Qwen3.6 almost certainly does, and
it brings the server's tool-parser fixes.
- **Before:** keep the 0.33.3 installer for rollback, and tell you just before installing.
- **After, checked on the current model:**
  - its tool-call parser still reads;
  - both render-fidelity tests pass;
  - toolprobe passes;
  - taskprobe propose ×1 lands at 8–11/16 with no turn-ending errors;
  - Vidar, Mimir and Hugin still start and answer;
  - Ollama's autostart is unchanged.

**J0c+ · Two leftovers from the upgrade** (added 2026-10-25, before J0d).
- **The raw retry must ask exactly what `/api/chat` asks.** On 0.40.11
  `the_raw_render_matches_what_ollama_builds_natively` fails. The prompt token counts are identical, but
  the two paths choose a different tool at temperature 0. To fix:
  - dump the raw prompt and compare it with the native one;
  - send the model's own sampling settings (`repeat_penalty`, `top_k`, `top_p`, from `/api/show`)
    explicitly on the raw `/api/generate` path, so both paths sample the same way;
  - the test must pass on 0.40.11, run several times.
- **No stray markup in answers.** A Hugin `--ask` answer ended with a leftover `<tool_call>` fragment.
  Strip unmatched tool-call markup from the final answer text, with a test.
- Done when both fidelity tests pass repeatedly, and toolprobe and taskprobe propose ×1 are unchanged.
- **Outcome (2026-10-26):** the sampling step was dropped. Both paths already sample the same way, and
  the mismatch came from `llama-server`'s prompt cache, so the test now warms up first. See
  [J0c+-results.md](../docs/results/J0c+-results.md).

**J0c+ close-out** (set 2026-11-01). Both taskprobe attempts were stopped for low memory; the
second scored 2/16 for code that scored 10/16.
- *2026-11-01, after the crash:* the low memory was never the machine. Task 10 in order is b06, and
  a model's `extract_calls` committed 81 GB there and took the PC down. Every run is now capped (check.rs):
  8 GB per process, 16 GB for the whole run, and it is killed if its drive falls below 10 GB free. A
  run Windows can't put under the caps doesn't start. `/verify` is dropped; the steps below are run
  by name.
- **You** close the browser and VS Code and leave Mimir and Vidar idle. Then `cargo test
  --workspace`, and taskprobe `--runs 1 --mode propose --model <coding model>`, with
  `CARGO_BUILD_JOBS=4` for the scoring builds.
- Done when taskprobe is 8–11/16 at the app's settings, with a complete t=0 pass. If memory is
  still short, stop and report; no retry loop.

**J0d · Thinking models** (detailed 2026-11-01). A `model_settings` map in the config, per model tag;
for now just `think` (off/low/medium/high). No per-model sampling: the app sends none, so each model's
own settings apply. It lives in `round_in`, so Mimir, Hugin and the probes all share it.
- **Already there:** `ModelDetail.capabilities`, cached by `show_cached` (ollama.rs); a
  `thinking_len` field in the round metrics and Hugin's `--trace`, always 0 so far; Hugin's
  `thinking · 12s` status for any wait on the model. The request doesn't stream, so a reasoning
  phase can't be told apart from writing: **no Hugin UI change**.
- **Config** (config.rs): `model_settings: {tag: {think}}`, lenient like `checks`. A bad entry is
  dropped, a bad `think` value is unset. `settings_for(model)` ignores case.
- **The request** (chat.rs): `Options.think`. `chat()` and `stream()` send `"think"` only when it's
  set: `false` for off, `"low"`/`"medium"`/`"high"` otherwise. Whether a model without levels takes
  a string or needs `true` is **confirmed in the live check, not assumed**.
- **The reply:** `Message.thinking`, sent back in history. `Message` derives `Default`, and the ~23
  struct literals get `thinking: None`. `passed_through` keeps the thinking; the raw retry has none.
- **`round_in`:** sends `think` when it's set and the model reports `thinking`; otherwise it's
  ignored, with a note. `thinking_len` gets filled.
- **The turn:** `Round.thinking` goes onto the paired assistant tool-call message (turn.rs `pair()`,
  all four callers). It's cleared from the whole `convo` at the end of `run_turn_with`, so later
  turns and saved conversations never carry it.
- **Compaction:** `message_chars` counts the thinking.
- **Probes:** taskprobe and toolprobe get `--think <level>`, for J2's gpt-oss at `high`.
- **Tests:** config parsing and lookup; `think` in the body only when set; thinking deserialized and
  kept through `passed_through`; sent only with the capability; kept within a turn and gone after
  it; counted by compaction.
- **Live check:** pull gpt-oss:20b (about 13 GB, on your OK, given 2026-11-01). With
  `{"gpt-oss:20b": {"think": "high"}}`:
  - `hugin --ask` and a small `hugin -p`, both with `--trace`. Each must show `thinking_len > 0`,
    no tool-call errors, and an answer.
  - toolprobe at think `high`, with its score recorded. There's no bar yet; J2 sets it.
  - The same Hugin runs on qwen3-coder:30b, which must be unchanged, with `thinking_len` 0.

**Gate J0:**
1. An independent review of J0b, J0c+ and J0d, by the checker agent or `/code-review`. Fixes go in
   their own commit.
2. `cargo clippy --workspace --all-targets` clean. It hasn't run since J0b.
3. `cargo test --workspace` passes.
4. Your OK.
5. **Push `stage-i`** (your choice, 2026-11-01); `main` stays at Stage H.

## Phase 1 — tests built from real work

**J1a · realprobe** (new probe in `crates/bench`). **10 real Rust tasks pinned at the last J0
commit:** I5's three, plus seven from the backlog, spread across crates and sizes.
- **Each task holds:**
  - its text;
  - its visible check (`cargo test -p X`);
  - **hidden tests**, either a new `tests/*.rs` that runs the built program, or a test module
    appended the way taskprobe does it;
  - a **reference solution**;
  - **at least two wrong solutions** the hidden tests must reject.
- **Each run goes through the real thing:** a copied `hugin.exe -p … --check … --json --time 900
  --model M`, in a fresh worktree at the pin.
  - APPDATA, TEMP and LOCALAPPDATA point at a sandbox, so your own config and data are never touched.
  - The hidden tests aren't in the pinned tree, so Hugin can't see them. A self-check proves this.
- **Scoring**, in a second worktree:
  1. Apply the patch and add the hidden tests.
  2. The hidden tests must pass at their exact count; early-exit detection is on.
  3. The touched crates' full suites must pass.
  4. Every test that existed at the pin must still exist.
- **PASS = correct and reported `done`.** Also shown: correct but not done; **false done** (`done`
  but wrong); and a pass within 540 s.
- **Self-check (no model):**
  - every reference passes;
  - the pin fails as a FAIL, not an ERROR;
  - every wrong solution fails;
  - the hidden tests can't be reached from the pin;
  - the taskprobe task runs under a config naming a model that doesn't exist, so a missing `--help`
    can't start a real sweep.

**J1b · askprobe.** **12 code questions:** the six used for `--ask` plus six more. Each is scored by
required facts and the right files appearing in `looked_at`, over 2 runs.

**J1c · Speed and memory, the same way for every model:**
- Ollama's CPU/GPU split (`/api/ps` size vs size_vram).
- The NVML change in card memory, with everything unloaded first.
- Windows' hidden spill into system RAM (the runner's GPU *shared* memory counter).
- Prefill and decode speed (tokens per second) at 2k and 16k prompts.
- The lowest free RAM while cargo builds beside the model.
- The model's digest.

**Gate J1:** one dry run on the current model, to fix harness bugs and warm the build caches. Then the
harness is **frozen** and the bars below are written into the results doc as "set before any run",
with your OK.

## Phase 2 — the runs, one model per session

Each session: **one model → stop → I update you** (score, speed, what it got wrong) → you say go
on. A finished model is removed if it's out, on your OK.

**J2 · Screen**, one session per model, in this order:
1. **B = qwen3-coder:30b**, the baseline, on the upgraded Ollama.
2. **qwen3.6:35b-a3b-coding**, at its own think default. The strongest on paper; same memory class
   as B.
3. **gpt-oss:20b** at think `high`. The only strong model that fits fully on the GPU, so the fastest.
4. **ornith-1.5:9b**. Small and cheap; its tool use is suspect.
5. **flint:40b**, optional, last, after an Ollama fix release. A day old on Windows.

- **Each screen:** pull, speed and memory, toolprobe, askprobe ×1, taskprobe propose ×1.
  **realprobe ×1** only if toolprobe ≥ 9/10 and taskprobe is no more than 3/16 below B.
- **Not on the list:** Devstral (3–5× B's time per run, and deprecated), glm-4.7-flash and gemma4
  (they spill, or have open tool-call bugs).
- Calls per reply are counted. Parallel calls get built only if a finalist sends several in more than
  10% of replies.

**Gate J2:** at most two finalists, each above B on the screen.

**J3 · Run of record**, one session per model: B, then each finalist. Each gets **realprobe ×3** (30
runs), taskprobe propose ×3 and askprobe ×2, with the digests unchanged since J2.

### Switching rules (set before J2; written into the results doc)

Switch `coding_model` only if **all** of these hold:
1. realprobe PASS ≥ **B + 7 of 30** (about 2 standard deviations), and the candidate wins at least 3
   more tasks than it loses (a task counts by majority of its 3 runs).
2. False done ≤ max(B's, 2).
3. taskprobe propose ≥ **25/48**.
4. No turn-ending tool-call errors, at most 1 infrastructure error, and toolprobe ≥ 9/10.
5. Median time per run ≤ **2× B's**, with no paging.

If two models qualify, take the higher realprobe score; if they're within 2 of 30, take the faster.

A separate fast `ask_model` (e.g. gpt-oss) only if it beats the coding model by ≥ 3/12 on askprobe and
swapping takes ≤ 15 s. The two can't both stay loaded on 16 GB.

## Phase 3 — only if a model wins

**J4 · Switch.**
- Set `coding_model` and its `model_settings` in your config (config.json names the model, so
  changing the default in code alone does nothing).
- cmdprobe ×1, since interactive Hugin offers shell commands and `-p` doesn't.
- Your hand check of Hugin and Mimir: how long a thinking round sits silent, and how long Stop takes.
- Remove the losing models, on your OK.

**J5 · Tuning.** KV-cache q8_0 and `num_ctx`, each measured on its own. They're server-wide settings,
so Vidar is checked too.

**Vidar** keeps its vision model (qwen3-vl:8b). **Mimir** gets every engine change through
`round_in`, and the winner through its existing coding-model offer: a hand check, not a separate run.

## Files

| File | Change |
|---|---|
| `crates/bifrost-core/src/chat.rs` | generic tool-call feedback arm; `think`; `Message.thinking` |
| `crates/bifrost-core/src/agent/round.rs`, `turn.rs` | the retry wording; thinking kept within the turn; counters |
| `crates/bifrost-core/src/config.rs` | `model_settings` (lenient) |
| `apps/hugin/src/print.rs`, `ask.rs`, `main.rs` | `--model`, `--trace`, `thinking…` |
| `crates/bench/src/bin/realprobe.rs`, `askprobe.rs` (new) | the two new probes |
| `evals/realprobe/` (new; hidden tests kept out of the pinned tree) | tasks, hidden tests, references, wrong solutions |
| `stages/STAGE-J.md`, `docs/J-status.md`, `docs/results/J*-results.md` | the stage's plan, status and results |

**Reused:**
- taskprobe's worktree and appended-tests pattern, and its touch-and-retry for Smart App Control;
- `check::Outcome::passed()` with early-exit detection;
- the bent-test detector (`apps/hugin/src/bent.rs`);
- `scratch::fingerprints`;
- `tools/model-trial.sh` (pull, VRAM);
- `ollama::show_cached` (capabilities, parser);
- vram.rs (NVML).

## Verification

- **Each chunk:**
  - `cargo test -p bifrost-core -p hugin -p bench -p mimir`, and clippy clean;
  - an independent review;
  - the results doc and status row;
  - a local commit on your OK, with no attribution lines.
- **J0c:** on the new Ollama, the current model passes toolprobe and taskprobe propose ×1 at 8–11/16
  with no turn-ending errors. Vidar, Mimir and Hugin all answer.
- **J1:** both self-checks pass: every reference passes, every wrong solution fails, and the hidden
  tests are unreachable. The dry run on B completes.
- **J2–J3:** every session ends with a short update to you: score, false done, speed, memory, what it
  got wrong. The final pick follows the rules above, with no exceptions made after the fact.

## Risks

- **Ollama's support for the new models is young** (Qwen3.6's malformed-call errors; Flint is a day
  old on Windows). The J0b feedback path covers it, and it's counted.
- **RAM pressure:** a 24 GB model plus cargo on 31 GB can cause paging and timeouts. Cap
  `CARGO_BUILD_JOBS`, and record the lowest free RAM.
- **The 300 s request limit:** long thinking on a model that spills can hit it; that's counted.
- **Smart App Control** may block fresh test binaries: reuse the touch-and-retry.
- **Bias:** Claude writes the real tasks. Behaviour-level hidden tests and wrong solutions limit
  that.
- **Scope:** results are for Rust in this repo, since the copies carry no `node_modules`.
