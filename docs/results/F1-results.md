# F1 — The bench harness in Rust

`bench/` — the port of `evals/bench.mjs`. **Done 2026-09-16.**

A model choice goes stale. New VLMs ship constantly, and the public benchmarks that informed A2
measure scanned documents, not syntax-highlighted code on a dark theme. Nobody benchmarks this use
case except us, so swapping a model has to be a ten-minute job rather than a vibes decision.

## Validated against the reference, not just "it runs"

The only meaningful test of a port is whether it reproduces the original's numbers. Same model,
same image, same prompt:

| | A2 (`bench.mjs`) | F1 (Rust) | |
|---|---|---|---|
| Prefill | 1,165 ms | **1,214 ms** | 4% apart — noise |
| Decode | 133 tok/s | **131 tok/s** | |
| Resident VRAM | 8,151 MiB | **8,187 MiB** | |
| Cold load | 2.0–4.7 s | **2.2 s** | in range |

## The port found a real ambiguity in "time to first token"

The first run reported **5,537 ms** to first token against A2's 1,165 ms — nearly 5× worse, on the
same machine and model.

Neither number was wrong. **`bench.mjs` paid the cold load outside its measurement**, by warming up
on a small synthetic image first and keeping `load_duration` as a separate column. This port timed
from request start, so the load sat *inside* the figure. Subtract it and the two reconcile exactly.

Both are worth knowing, and they answer different questions, so the table now reports both:

- **TTFT** — what pressing the hotkey actually feels like, cold load included. The number the
  product lives or dies by.
- **PREFILL** — TTFT with the load taken back out: what the model costs once it is resident, and
  the only column comparable with A2's table.

A run that paid a cold load says so in a note, because its TTFT is not comparable with a warm one.

## What carried over unchanged

**The unload-before-every-run rule.** Ollama caches prefill across calls to the same loaded model
whenever the token prefix matches, and it does so **per image** — so alternating between two eval
images does *not* defeat it. Once both have been seen, prefill for a repeat collapses from ~450 ms
to ~5 ms and the measurement is worthless. An explicit unload is the only thing that reliably
clears it. That was established empirically in `bench.mjs` and is the single most important thing
the port had to preserve.

**The prompt is a dimension, not a constant.** A2 found the prompt moved answer quality more than
the choice between a 4B and an 8B model did, and C1b found a *single trailing newline* flipping a
verdict. `--prompts` takes a list, every prompt is trimmed identically, and with none given the run
still has exactly one — whatever the app ships — so the shape of the matrix never changes.

**Unknown arguments error.** C4a learned this expensively: a silently-swallowed argument means the
run you thought you configured is not the run that happened.

## What it reuses

`warden-core` now carries `generate_measured`, so the harness, the CLI and the app all talk to
Ollama through the same code. The VRAM sampling is the same median-of-three `warden-core::vram`
that `capture status` uses. The port added one shared function rather than a second client.

## F1b — the answers, saved and graded blind

Added 2026-09-17. A run now writes `evals/out/F1-<stamp>/`:

| File | What it is |
|---|---|
| `answers.md` | The grading surface. Model identity **blinded and shuffled per image** |
| `blind-key.json` | The mapping, separate so unblinding is a deliberate second step |
| `runs/*.json` | Every rep: all metrics, VRAM either side, the answer and its sha256 |
| `manifest.json` | Models, images and prompts with **content hashes**, plus num_ctx and card |
| `summary.md` | The timing table, not blinded |

**Why blind at all.** The eval set exists to catch a model being wrong in a way you did not
expect, and a grader who knows which answer came from the bigger model reads it more generously —
especially the grader who picked that model last week. Ported from `evals/bench.mjs:770-808`.

**No accuracy column, anywhere.** Whether an answer is *true* is a judgement about the screen it
describes. A column would imply the harness measured something it did not.

**Prompt hashes are not decoration.** At C1b a single trailing newline flipped a case-02 verdict at
`temperature: 0`. A manifest recording a prompt's filename but not its bytes cannot tell you which
of two such runs you are reading.

### Two bugs the verification caught

**The blind shuffle did not shuffle.** Three consecutive real runs produced the identical
assignment. The cause was seeding xorshift straight from `SystemTime` and taking bits off the high
end of a multiply: Windows' clock is coarse, so the low bits barely move between runs while the
high bits barely move within a session. **A shuffle that quietly returns input order is worse than
no shuffle**, because the file still says it is blind.

Now the randomness comes from a hash of the clock, where every output bit depends on every input
bit. `bench --selftest-shuffle` keeps it honest — 2,000 samples, expects roughly half, and checks
the clock-seeded path varies too. Proven in both directions: breaking the swap makes it report
100% and exit 1.

**A failed run was recorded as a measurement.** One 4B run degenerated into a repetition loop and
the stream ended without Ollama's final metrics object. Every metric stayed at its initialised
zero, and the table printed a model at **0 tok/s** as though that had been measured.

`generate_measured` now refuses a stream that never sends `done`, and the harness skips that cell,
keeps going, and lists what it dropped at the end. A benchmark that hides its failures is one you
will quote without knowing what is missing from it.

### One deliberate divergence from `bench.mjs`

Prompts are **trimmed** before hashing and sending; `bench.mjs` reads them verbatim. Trimming
prevents the C1b failure rather than merely recording it, but it does mean a prompt file with
trailing whitespace hashes the same as one without. That is the intended trade, stated so the two
harnesses' hashes are not assumed interchangeable.

## Still not done here

Grading itself is still by eye, and `evals/cases.md` holds the criteria. F2 is where a real failure
becomes a tracked case.
