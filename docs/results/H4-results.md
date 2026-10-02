# H4 — Coding models against the baseline

The same benchmark as H3, on three coding models, to decide whether Mímir's model should change.
**Started 2026-09-30.**

## Set before any run

Written here before a single candidate was pulled, so the numbers cannot move the bar.

- **The bar: at least 20% on taskprobe at the app's sampling — 10 or more of 48 — and not worse
  than the baseline at temperature 0.** The baseline, `qwen3-vl:8b-instruct-q4_K_M`, is 6/48 (12%)
  and 2/16. Chosen by Luis, 2026-09-30.
- **What a switch costs.** The candidates are text-only. Mímir chooses the one loaded model for
  both apps (STAGE-G decision #4), so while a coding model is loaded, Heimdall cannot see — it
  refuses cleanly (G3). Clearing the bar makes a model *worth* that; it does not make the choice.
- **The candidates**, chosen by Luis: Qwen3-Coder 30B-A3B, Devstral Small 24B, gpt-oss 20B.
- **For each, before the benchmark:** VRAM resident at `num_ctx` 16k and 32k and whether it spills
  to system RAM; `toolprobe` (G0's ten prompts, four of them needing no tool).
- **The same conditions as the baseline:** `num_ctx` 32768, every Phase 1 tool, 3 runs at the app's
  sampling plus 1 at temperature 0, sixteen tasks, `evals/out/H3-task-*`.
- **Known in advance:** the recovery for malformed tool calls renders Qwen's chat layout and is only
  tried for `qwen*` models (G7). For the other two, a call Ollama cannot parse ends that run.
- **Nothing stays installed.** Each model is removed after its runs (`ollama rm`), and the default
  model is left as it was. The choice is Luis's, later, from this document.

## The run of record — restarted 2026-10-01

Started again from the beginning, with nothing skipped and nothing carried over from the
interrupted first attempt (kept below, superseded): **every model, every step, on the same day,
same code, same conditions.**

1. **A fresh baseline** on `qwen3-vl:8b-instruct-q4_K_M` — the same steps as the candidates: VRAM
   at 16k and 32k, toolprobe, the full benchmark. H3's 6/48 was measured the day before on code
   that has changed since (the runaway reclassification); this one is like for like. It stays
   installed.
2. **Devstral Small 2 24B**, **Qwen3-Coder 30B-A3B**, **gpt-oss 20B**, in that order, each: pull,
   VRAM at 16k and 32k, toolprobe, the full benchmark (16 tasks × 3 runs at the app's sampling +
   1 at temperature 0), remove.

Run by `tools/model-trial.sh`, output in `evals/out/H4-2026-10-01/`.

### Baseline — `qwen3-vl:8b-instruct-q4_K_M`

| | |
|---|---|
| VRAM, `num_ctx` 16384 | +9,021 MiB on the card · Ollama 7.8 GB · **100% GPU** |
| VRAM, `num_ctx` 32768 | +11,403 MiB on the card · Ollama 10 GB · **100% GPU** |
| toolprobe | 10/10 |
| **taskprobe, app sampling** | **6/48 (12%)** · real commits 2/12 · removals 4/36 · 9.1 rounds · 1.2 checks · 82 s a run |
| taskprobe, temperature 0 | 1/16 (6%) |

The same 6/48 as H3's, a day later — the number is stable. What passes: `b03` 4/4 again, `a01`,
`a04` and `b09` once each. **The bar for a candidate is therefore 10/48 at the app's sampling, and
at least 1/16 at temperature 0.** Saved in `evals/out/H3-task-1790876155/`.

### Devstral Small 2 24B — `devstral-small-2:24b` · **clears the bar**

| | |
|---|---|
| VRAM, `num_ctx` 16384 | +14,121 MiB on the card · Ollama 17 GB · **21% CPU / 79% GPU** |
| VRAM, `num_ctx` 32768 | +13,999 MiB on the card · Ollama 20 GB · **33% CPU / 67% GPU** |
| toolprobe | 10/10 |
| **taskprobe, app sampling** | **24/48 (50%)** + 1 late · real commits **8/12** · removals 16/36 · 5.1 rounds · 1.2 checks · 156 s a run |
| taskprobe, temperature 0 | **9/16 (56%)** |

**Four times the baseline, at both settings, well past the bar of 10/48.** It finishes in fewer
rounds (5.1 against 9.1): it reads, writes the function, and usually stops. Every run of `a03`,
`a04`, `b03`, `b04`, `b08` and `b10` passed.

How it fails is different too. The baseline mostly wrote code that would not compile and then
could not re-edit it; Devstral's failures mostly *compile and run* and get a case wrong —
`tidy_deletion`'s join inside a line, `display_title`'s truncation, `build_files_changed`'s
quoted path. Loop-breaking stopped it three times in 64 runs. `a02` (create `tools.rs`) failed every
time, missing `serde_json::json!`'s import. And `b05` (`render_chatml`, in the long, escape-heavy
`toolcall.rs`) ran away three times out of four — the same five-minute runaway the baseline had.

The cost: it does not fit the card, so a third of it runs on the CPU at 32k, and a task takes 2.6
minutes against 1.4. Removed after its runs. Saved in `evals/out/H3-task-1790881879/`.

### Qwen3-Coder 30B-A3B — `qwen3-coder:30b` · **clears the bar**

| | |
|---|---|
| VRAM, `num_ctx` 16384 | +14,193 MiB on the card · Ollama 20 GB · **28% CPU / 72% GPU** |
| VRAM, `num_ctx` 32768 | +14,147 MiB on the card · Ollama 22 GB · **34% CPU / 66% GPU** |
| toolprobe | 9/10 — no spurious call; one call missed (below) |
| **taskprobe, app sampling** | **25/48 (52%)** · real commits 5/12 · removals **20/36** · 5.2 rounds · 1.1 checks · **32 s a run** |
| taskprobe, temperature 0 | **9/16 (56%)** |

**As good as Devstral, and five times faster** — 32 s a task against 156 s, and faster than the
baseline's 82 s. It spills to the CPU like Devstral, but it is a mixture of experts: only about 3B
of its 30B parameters work on each token. Every run of `b03`, `b04`, `b09`, `b10`, `b11` and `b12`
passed; it is strongest on the function-body tasks (20/36), weaker on the real commits (5/12).

**Its weakness is the tool-call format.** It writes calls as XML —
`<function=read_file><parameter=path>…` — not the JSON the baseline uses. **8 of its 64 runs ended
in ERROR**: Ollama 0.33.3 rejected a call with *XML syntax error … unexpected EOF* (`b05`, `b06`,
one `a03`). Its one toolprobe miss is the same thing from the other side: a correct call in that
XML, which Ollama passed through as text. Mímir's recovery for malformed calls is Qwen-only by
name, so it is *tried* here — but it renders and repairs the JSON format, not this one. **Counting
all eight as fails, it is still 52%.** If this model is chosen, a recovery path for its XML format
is the first thing to build, and both the misses and the errors are likely to go with it.

Other failures: `a02` again (`serde_json::json!` not imported), `a04` adding a crates.io
dependency (`dirs`) that an offline build cannot fetch, `tidy_deletion`'s end-of-file case.
Removed after its runs. Saved in `evals/out/H3-task-1790891838/`.

### gpt-oss 20B — `gpt-oss:20b` · **just clears the bar**

| | |
|---|---|
| VRAM, `num_ctx` 16384 | +12,755 MiB on the card · Ollama 12 GB · **100% GPU** |
| VRAM, `num_ctx` 32768 | +13,165 MiB on the card · Ollama 12 GB · **100% GPU** |
| toolprobe | 10/10 |
| **taskprobe, app sampling** | **11/48 (23%)** + 3 late · real commits 3/12 · removals 8/36 · **11.9 rounds** · 0.7 checks · 30 s a run |
| taskprobe, temperature 0 | 2/16 (12%) + 1 late |

**The only candidate that fits the card** — all of it on the GPU even at 32k — and fast. But it
wanders: twelve rounds a task where the other two take five, eleven of its 64 runs still calling
tools at the probe's twenty (six at the app's sampling, five at temperature 0), and it ran the
check less than once a task. **8 of its 64 runs ended in ERROR**:
Ollama could not parse its tool calls (*error parsing tool call: raw='The user wants to …'* — its
reasoning leaking into the call). gpt-oss is not a Qwen model, so Mímir's recovery is not tried at
all. Removed after its runs. Saved in `evals/out/H3-task-1790894218/`.

## Side by side

| | Baseline qwen3-vl 8B | **Devstral Small 2 24B** | **Qwen3-Coder 30B-A3B** | gpt-oss 20B |
|---|---|---|---|---|
| **taskprobe, app** (bar ≥10/48) | 6/48 (12%) | **24/48 (50%)** | **25/48 (52%)** | 11/48 (23%) |
| taskprobe, t=0 (bar ≥1/16) | 1/16 | **9/16** | **9/16** | 2/16 |
| Real commits | 2/12 | **8/12** | 5/12 | 3/12 |
| Function bodies | 4/36 | 16/36 | **20/36** | 8/36 |
| Rounds a task | 9.1 | 5.1 | 5.2 | 11.9 |
| **Time a task** | 82 s | 156 s | **32 s** | 30 s |
| Tool-call errors (of 64) | 0 | 0 | 8 | 8 |
| toolprobe | 10/10 | 10/10 | 9/10 | 10/10 |
| VRAM at 32k | 11.4 GB, all GPU | 20 GB, 33% CPU | 22 GB, 34% CPU | 13.2 GB, all GPU |
| Sees images (Heimdall) | **yes** | no | no | no |

All three clear the bar Luis set. Two clear it by a mile, and land on almost exactly the same
score by different routes: Devstral is the better at a real commit, Qwen3-Coder at writing a
function, and Qwen3-Coder does it five times faster.

## What this suggests — the choice is Luis's (Gate 2)

**Qwen3-Coder 30B-A3B is the strongest case:** the top score, the fastest of all four including
the current model, and its one real weakness is a fixable one. Its eight errors and its one
toolprobe miss are the same thing — tool calls in an XML layout Ollama 0.33.3 sometimes fails to
parse — and Mímir already recovers malformed calls for the current model. A recovery path for this
layout is a small, well-understood chunk, measured the same way G7's was.

**Devstral is the safe second:** the same score, no tool-call errors at all, the best at real
commits — but 2.6 minutes a task, five times slower than Qwen3-Coder, with a third of it on the CPU.

**gpt-oss is the one that fits the card,** and that is its whole case: it clears the bar narrowly,
wanders, and its tool calls fail with nothing to recover them.

**Whatever is chosen costs Heimdall its eyes while it is loaded** — every candidate is text-only,
and the two apps share one model (STAGE-G decision #4). The obvious shape, if Luis switches, is
the one H4 deferred to Gate 2: choosing a project folder in Mímir offers to load the coding model,
and clearing it offers to go back — with Heimdall refusing cleanly in between (G3).

Nothing is installed now but the models that were there before the trial.

## Gate 2 — decided 2026-10-01

**Luis chose Qwen3-Coder 30B-A3B, with its tool-call format fixed first.** Before it becomes
Mímir's coding model: a recovery path for its XML tool calls — the eight errors and the one
toolprobe miss — measured with toolprobe and taskprobe the way G7 measured the first one. Then the
switch itself: choosing a project folder offers to load it, clearing the folder offers to go back,
and Heimdall refuses cleanly in between.

**Both done 2026-10-02:** [H4a](H4a-results.md) — 28/48 with no errors, against 25/48 and 8 here —
and [H4b](H4b-results.md), the switch.

## First attempt, 2026-09-30 — superseded

### Devstral Small 2 24B (`devstral-small-2:24b`) — partial, stopped for the night

Pulled 2026-09-30, measured, then **stopped by choice after 18 of its 64 benchmark runs** and
removed. Raw output kept in `evals/out/H4-partial-2026-09-30/`.

| | |
|---|---|
| VRAM, `num_ctx` 16384 | 17 GB resident — **does not fit the card**: 21% CPU / 79% GPU |
| VRAM, `num_ctx` 32768 | 20 GB — 33% CPU / 67% GPU |
| toolprobe | **10/10**, including the four prompts that need no tool |
| Speed | about 2.4 min a benchmark run, against about 1 min for the baseline |

The benchmark so far, app sampling, the first six tasks:

| Task | Devstral | Baseline |
|---|---|---|
| a01 `LoadedModel::pinned` | **3/3** | 0/3 |
| a02 create `tools.rs` | 0/3 | 0/3 |
| a03 `store::attachment_path` | **3/3** | 0/3 |
| a04 create `config.rs` | **3/3** | 1/3 |
| b01 `tidy_deletion` | 1/3 | 0/3 |
| b02 `Text::parse` | 2/3 | 0/3 |
| **Total** | **12/18** | 1/18 |

Not yet a result — ten tasks and the temperature-0 runs are left, and the first six include the
real commits, where the baseline was weakest. But it is far past the bar on what it has done.

Superseded by the run of record above, which runs Devstral again in full rather than combining
this partial run with a second one.
