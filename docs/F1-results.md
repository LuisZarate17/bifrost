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

## Not done here

**Answers are not saved to disk yet**, so grading still means reading the terminal. `bench.mjs`
writes `runs/*.json` and an `answers.md`; that belongs with F2, which is where grading gets a home.
Until then the Node harness remains the one to use for a full graded bake-off, and this one is for
timing, VRAM and quick comparisons.
