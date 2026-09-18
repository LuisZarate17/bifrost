# A2 — The bake-off: results

**Completed 2026-09-08.** Chunk A2 of [BUILD.md](../BUILD.md).

## The decision

**`qwen3-vl:8b-instruct-q4_K_M`.**

One sentence: *it is the only model that correctly concluded the editor screenshot would not
compile, and it does so inside the VRAM budget that C4 needs.*

Everything else in the grid was eliminated for a stated reason:

| Model | Outcome |
|---|---|
| `4b-instruct-q4_K_M` | **Out — accuracy.** Called the broken file "mostly correct and should compile and run without errors" |
| `4b-instruct-q8_0` | **Out — accuracy.** "There's no actual error in the code — it will compile and run without crashing" |
| `8b-instruct-q8_0` | **Out — VRAM.** 11,373 MiB resident, over the 10 GB ceiling. Also the slowest (24.6 s) and wrongly claimed the code compiles |
| **`8b-instruct-q4_K_M`** | **Chosen.** Correct conclusion on both cases, 8,151 MiB, 1.2 s to first token |

---

## Scope: the full 2×2, not the planned pair

BUILD.md originally specified `8B-Q4` vs `4B-Q8`. Those differ in **both** size and quantization,
so whichever won, the reason would have been unknowable. Testing all four separates the factors.

That change paid for itself. See "What the grid proved" below — the pair could not have produced
either finding.

**Tags corrected before the run.** The tags in BUILD.md (`qwen3-vl:8b-q4_K_M`, `qwen3-vl:4b-q8_0`)
do not exist and 404 on pull. The real tags carry `-instruct-`. The claim that 8B@Q4_K_M needs
~12 GB was also wrong — the download is 6.14 GB.

---

## Accuracy — the thing that decided it

Graded against [evals/cases.md](../../evals/cases.md). All three repetitions of every cell produced
**byte-identical output** (temperature 0, seed 42), so single-rep grading is safe.

### case-01 · terminal with the compiler error visible

Every model identified `E0502` and the borrow checker. This case does not discriminate — the
answer is on screen.

| Model | E0502 | Cause | Locates line 25/22 | Fix compiles |
|---|---|---|---|---|
| 4B-Q4 | yes | yes | **no line numbers at all** | yes — copy the length first |
| 4B-Q8 | yes | yes | yes | writes a correct fix, then **talks itself out of it**: *"⚠️ But wait — this doesn't work…"* |
| **8B-Q4** | yes | yes | yes | **yes — `clone()`**, explicitly an accepted fix |
| 8B-Q8 | yes | yes | yes | **no** — `let mut first_frame = frames[0];` moves out of a `Vec` index and would not compile |

### case-02 · editor, no compiler output to lean on

This is the case that decided A2, exactly as cases.md predicted it would.

| Model | Verdict | What it actually said |
|---|---|---|
| 4B-Q4 | **FAIL** | "The code is mostly correct and should compile and run without errors." Also hallucinated program output (`7680 bytes`; the real value is 8,294,400) |
| 4B-Q8 | **FAIL** | "There's no actual error in the code — it will compile and run without crashing," while quoting the two conflicting lines and marking them ✅ |
| **8B-Q4** | **PASS** | Correctly concluded it will not compile and the borrow checker catches it. Caveat: named the wrong code (`E0597`, not E0502) and fabricated a compiler-output block. **C1b caveat:** its proposed fix does not compile either — see docs/C1b-results.md |
| 8B-Q8 | **PARTIAL** | Right mechanism, wrong conclusion: *"The code compiles successfully (Rust's borrow checker doesn't catch this at compile time)"* — which case-01 disproves on the adjacent screen |

**Both 4B models read the code accurately and still concluded it was fine.** That is the exact
split cases.md was built to expose: reading text well is not the same as reasoning about code.

---

## What the grid proved — and the pair could not have

### 1. Size buys reasoning. Quantization does not.

Both 4B models fail case-02; both 8B models identify the mechanism. The split falls cleanly on
the size axis, with no crossover.

### 2. Q8 bought nothing at all

| Comparison | Result |
|---|---|
| TTFT, 4B-Q4 vs 4B-Q8 | **528 ms vs 526 ms** — identical |
| TTFT, 8B-Q4 vs 8B-Q8 | **1,165 ms vs 1,151 ms** — identical |
| Accuracy | Q8 did not fix case-02 at 4B, and at 8B it made the conclusion *worse* |
| Cost at 8B | **+3,222 MiB VRAM** and decode halved, 133 → 87 tok/s |

Time to first token is set **entirely by model size** — quantization moves it by under 2%, well
inside noise. The higher-precision build costs 3.2 GB and half the decode speed to buy nothing
measurable. PLAN.md §6 assumed the opposite ("bias toward precision when it's close"); on this
eval set that assumption is wrong and should be treated as retired.

---

## Latency and VRAM

Medians of 3 repetitions. Every prefill measured cold — see "Corrections" below for why that
matters.

| Model | TTFT | Total, case-01 | Decode | Resident VRAM | Cold load |
|---|---|---|---|---|---|
| 4B-Q4 | 528 ms | 6.0 s | 197 tok/s | 5,469 MiB | 2.0 s |
| 4B-Q8 | 526 ms | 8.2 s | 143 tok/s | 7,183 MiB | 3.2 s |
| **8B-Q4** | **1,165 ms** | **8.2 s** | **133 tok/s** | **8,151 MiB** | **3.7 s** |
| 8B-Q8 | 1,151 ms | 24.6 s | 87 tok/s | 11,373 MiB | 4.7 s |

All four returned VRAM to baseline on unload — the C4 mechanism works.

**Two numbers that revise earlier docs:**

- **Cold load is 2.0–4.7 s**, not the 7–18 s recorded in A1 and assumed by C4. A1's 18 s was a
  4.9 GB text model read off cold disk; these load faster despite being larger.
- **A 1080p screenshot costs 2,042 image tokens** (2,077 with image, 35 without). That number was
  at risk of being unobtainable — see Corrections.

**Verbosity is a product problem.** Every model wrote 3,600–7,950 characters. 8B-Q8 hit the 2,048
token cap on case-01 in all three runs. None of these fit a bottom-right corner popup. That is an
E1/C1 problem, not a model-selection one, but it is real and none of the candidates avoid it.

---

## The most important finding is about the prompt, not the model

A robustness probe ran case-02 against a terser, more directive prompt:

> *What's wrong on this screen? Be specific about the file, the line number, and the fix.*

**Both models got dramatically worse, and neither found the real bug.**

- **4B-Q4** invented four errors that do not exist — including "Missing `use` for `HashMap`"
  while quoting the line that imports it, and "import the `println!` macro from `std::fmt`."
- **8B-Q4** contradicted itself inside one sentence — *"the syntax is correct, but `vec!` is not
  the correct macro… which is actually correct"* — and claimed `cache.insert("frames".to_string(), …)`
  passes a non-String key.

**Hypothesis: "What's wrong on this screen?" presupposes a fault, so the model manufactures one.**
The default prompt's *"Otherwise explain what matters here"* gives it permission to find nothing,
which appears to suppress confabulation.

The ranking did not flip, so the model decision stands. But the prompt moved answer quality more
than the choice between 4B and 8B did. PLAN.md §2 argued the prompt deserves testing like a model;
this is the evidence. **Do not adopt a fault-presupposing prompt at E1 without re-running the set.**

---

## Corrections discovered during the run

### The prompt cache invalidates the obvious way of measuring this

The approved protocol alternated images to defeat Ollama's prompt cache. **It does not work.**
Ollama caches both images simultaneously:

| Run | prefill | cached tokens |
|---|---|---|
| image A, genuinely cold | **446 ms** | 4 |
| image B | 455 ms | 4 |
| image A again | **5 ms** | 2,076 |
| every run after | **5 ms** | 2,076 |

Run naively, all four models would have posted near-identical timings and the latency half of A2
would have measured a cache. The only thing that clears it is unloading the model. The harness now
does **unload → throwaway warm-up on a synthetic non-eval image → measure** for every repetition.
The warm-up must not use an eval image, or it caches the very thing about to be measured.

Verified reproducible: image A prefill 446/447 ms, image B 455/453 ms across independent cycles.

### `num_predict: 1024` truncated answers

The first real run returned `done_reason: "length"` mid-sentence. Raised to 2048. Even then,
8B-Q8 hit the cap on case-01 in all three runs.

### The 6-second image encode was not the Blackwell bug

The first measurement showed 6,132 ms of prompt eval, matching the signature of upstream #14548
(qwen3-VL's vision encoder silently on CPU). It was not. That figure conflated model load with
first-kernel warmup. True warm encode is **~450 ms for 2,077 tokens — about 4,500 tok/s**,
unambiguously GPU. A1's rule held: always measure warm.

### `prompt_eval_cached_count == 0` is never true

An honestly cold run still reports 4 cached tokens. The contamination threshold is >100, or >5%
of `prompt_eval_count`.

---

## Limitations — read these before trusting the table

1. **The eval set is two views of one bug.** One Rust borrow error, one dark theme, one font, one
   language. A2 chose a default; it did not establish this model reads *any* screen well. F2 is
   the remedy and it starts on day one of E1.
2. **A 2×2 on 2 images shows a ranking, not an effect.** The size/quantization conclusions are
   consistent across every cell measured, but 2 images cannot carry a causal claim. Read
   "no difference detectable," never "no difference."
3. **The winner has a real flaw.** 8B-Q4 named the wrong error code on case-02 and fabricated a
   compiler-output block. It reached the right conclusion by the right mechanism, but it
   hallucinates supporting detail. Worth watching in daily use.
4. **Latency is version- and load-conditional** — Ollama 0.33.3, idle GPU, nothing else contending.
   Warden's actual job is a popup over a game wanting 8–12 GB, where these numbers will be worse.
5. **Grading was not blind.** The harness emits blind labels, but timing and VRAM data reviewed
   first uniquely identify each model. The objective criteria (does the text contain `E0502`,
   `Vec<u8>`, line 25) are immune to that; the judgment calls are not.
6. **Only the Instruct variants were tested.** `-thinking-` builds exist at the same sizes. Since
   both 8B models struggled to reason about case-02 without compiler output, a thinking variant is
   the obvious next probe if 8B-Q4 disappoints in daily use.

---

## Artifacts

Raw data in `evals/out/A2-2026-09-08T23-31-09Z/` (gitignored): `manifest.json` pins Ollama version,
model digests, prompt hash and image hashes; `runs/` holds all 38 runs verbatim; `runs.csv` and
`vram.csv` carry every measurement. Regenerate with:

```
node evals/bench.mjs --models <tags> --reps 3 --num-ctx 8192 --num-predict 2048
```
