# C4a — Power control in the CLI

`capture warm` · `capture off` · `capture status`. **Done 2026-09-15.**

The model costs ~8 GB of a 16 GB card while resident. Holding it there permanently is wrong when
the popup has to work over games wanting 8–12 GB, and a timeout is wrong too — it decides for you
and leaves you guessing whether the next press is instant or a stall. So: states you set and can
see, verified against the card rather than against Ollama's opinion.

## Measured

A tight cycle, seconds apart:

```
capture off     card  9093 MiB ->  900 MiB    delta  -8193 MiB
capture warm    card   900 MiB -> 8773 MiB    delta  +7873 MiB   in 2.2s
capture off     card  8773 MiB ->  900 MiB    delta  -7873 MiB
```

**The warm and off deltas match exactly — ±0 MiB.** That is the acceptance criterion, and it is a
comparison of two figures measured minutes apart in the same session, never against a constant.

An earlier pair taken ~10 minutes apart differed by 320 MiB, because other applications had
released video memory in between. **The two-delta test is only exact across a tight cycle**, which
is worth knowing before treating a wider gap as a failure.

## Three things the run settled

**NVML agrees with `nvidia-smi`.** 9,093 MiB against 9,094 MiB, sampled moments apart — so the
in-process binding costs nothing in accuracy and needs no subprocess per sample.

**The card and Ollama genuinely disagree.** With the model resident, the card read **8,773 MiB**
while `/api/ps` claimed **`size_vram` 6,233 MiB**. The gap is the baseline plus everything else
holding video memory. This is why PLAN.md §6b requires the readout come from the GPU; `ollama ps`
is not proof, and a UI showing only its number would be quietly wrong.

**The baseline really does drift.** Observed at **900 MiB** and **985 MiB** within one session, and
`900` is nowhere near the `454` BUILD.md recorded. Anything that hard-codes it is already broken.

## The trap that would have made this feature useless

Ollama keys a loaded runner by the model **and** by the options that change its memory layout,
`num_ctx` among them. Warming at Ollama's default context and then capturing at 8192 tears the
runner down and loads it again — so `warm` would spend 3.7 s and 8 GB and buy nothing, **silently**.

`load()` therefore sends the same `options` block as the capture path, and `struct Chunk` now
parses `load_duration` so the normal footer reports it:

```
capture off     ->  card 985 MiB
capture warm    ->  +7873 MiB in 2.2s, pinned (keep_alive -1)
capture         ->  [2077 tokens in, 504 out | first token 1.27s | total 5.0s | load 0.01s]
```

**`load 0.01s` is the proof.** A mismatch would show ~2 s there. It is now visible from the
ordinary capture path forever, not just while this chunk was being built.

## What a one-shot CLI cannot tell you

`/api/ps` has no "loading" entry — a model appears only once it is resident. So **`status` reports
Off or Ready, never Warming.** Warming is a state `warm` displays while it blocks, and a state
C4b's tray can show because it is long-lived and knows it issued a load that has not returned.
That is in-process knowledge, not observable state, and `status` does not pretend otherwise.

`expires_at` distinguishes pinned from expiring, which was previously invisible. Confirmed
empirically: `keep_alive: -1` yields a year past 2100, the 5-minute default yields a timestamp
minutes out.

## Failure paths, all exercised

| Situation | Behaviour | Exit |
|---|---|---|
| `capture warmm` | `error: unknown argument: warmm` + usage | **2** |
| `capture --model` with no value | `error: --model needs a value` | 2 |
| Ollama not running | one line: `cannot reach Ollama at http://127.0.0.1:11434 — is `ollama serve` running?` | 1 |
| NVML unavailable, `status` | reports Ollama's view, says the card is unavailable and that this is not proof | **0** |
| NVML unavailable, `off` | **still unloads** — confirmed by a following `status` — then reports it could not verify | 1 |
| `off` when not loaded / `warm` when resident | `nothing to do` | 0 |

**NVML failure is not Ollama failure.** Loading and unloading are Ollama calls and must still work;
NVML only supplies the verification. So the operation happens and the exit code records that the
check did not.

### The footgun this closed

The old argument loop silently ignored anything it did not recognise:

```rust
other => { if let Ok(secs) = other.parse::<u64>() { delay_secs = secs; } }
```

So `capture warm` **took a screenshot**, and a typo cost nine seconds of GPU with no clue why.
Unknown tokens are now an error, which is also what makes a mistyped subcommand fail loudly.
`capture`, `capture 5` and `capture --model TAG` all still behave exactly as before.

## Deliberately deferred

**No shared crate yet.** `capture/` and `src-tauri/` share zero code and already duplicate
`OLLAMA_URL`, `DEFAULT_MODEL`, `NUM_CTX`, `struct Shot`, the capture handler and `rgba_to_png`,
with real drift (base64 0.23 vs 0.22, edition 2024 vs 2021). Extracting now would mean reconciling
that drift while also debugging NVML — three unrelated failure modes at once.

`ollama.rs` and `vram.rs` are written so C4b's extraction is close to a move: **neither prints**,
neither panics, every fallible function returns `Result<_, String>`, and they return named structs
rather than tuples. `power.rs` holds the printing, because a tray has no stdout.

**Auto-starting `ollama serve` is not here.** This closes A1's gap only halfway: `warm` detects
Ollama is down and prints the exact command, but launching it is process management with its own
traps and deserves its own chunk.
