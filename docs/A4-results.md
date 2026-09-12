# A4 — First useful thing: results

**Completed 2026-09-12.** Chunk A4 of [BUILD.md](../BUILD.md). **Stage A is complete.**

## What was built

`capture/` now does the whole loop: grab the screen → encode PNG in memory → base64 → POST to
Ollama → stream the answer to stdout as it arrives.

```
cd capture
cargo run                                  # answer about your screen, now
cargo run -- 12                            # wait 12s first
cargo run -- --model qwen3-vl:4b-instruct-q8_0    # A/B a different model
```

The prompt lives in `capture/prompt.txt`, not in source, because PLAN.md §2 requires it and E1
expects ~20 rewrites. The model is a flag defaulting to the A2 winner, so F3 comparisons cost
nothing.

**This is the first point where Warden is worth using.** BUILD.md's framing holds: if everything
after this stalls, there is still a working tool.

---

## The streaming lesson

The chunk deliberately shipped the wrong version first. Same command, same model, same answer —
the only difference is whether the response body is read as it arrives:

| | Buffered — `response.text()` | Streaming — `BufReader::lines()` |
|---|---|---|
| Capture done | 1.6 s | 1.9 s |
| **First text on screen** | **16.2 s** | **3.4 s** |
| Silence before anything | **14.5 s** | 1.5 s |
| Then | the entire answer at once | progressively: 3.9s, 4.0s, 4.3s, 4.6s … |

**The compiler cannot catch this.** Both versions compile, run, and produce identical output. The
only symptom is a wait, and the only defence is the instinct to notice it.

That matters because BUILD.md calls a buffering bug **the single worst trap in this project** —
B2 warns that `proxy_localhost_stream` buffers, works in dev, and breaks silently once packaged.
Now there is a felt reference point for what "it buffered" looks like.

Two subtleties worth keeping:

- `BufReader::lines()` handles both splitting problems the A2 harness had to solve by hand — a
  network chunk can end mid-line, and it can end mid-UTF-8-character.
- **`stdout().flush()` after each token is not optional.** stdout is line-buffered, so without it
  the tokens pile up until a newline and the bug is quietly rebuilt one layer down.

---

## Cross-validation against A2

The final line reports `[2077 tokens in, 1031 out | first token 1.19s | total 9.0s]`. Every figure
matches what A2 measured independently with a completely separate harness (`evals/bench.mjs`,
Node, different HTTP client):

| | A2 (`bench.mjs`) | A4 (Rust) |
|---|---|---|
| Image+prompt tokens, 1080p | 2,077 | **2,077** |
| Time to first token | 1.165 s | **1.19 s** |
| Total, case-01 | 8.2 s | **9.0 s** |

`prompt_eval_count: 2077` is the specific check that `num_ctx: 8192` took. Ollama derives a
default from VRAM that lands at **4k** on this card, which would have silently truncated the
screenshot — and looked like a stupid model rather than a config bug.

---

## Smart App Control blocks Rust proc-macros — and Warden found it

**The most important finding in this chunk, and the tool discovered it, not the author.**

Partway through, `cargo build` failed with:

```
error[E0463]: can't find crate for `zerovec_derive`
```

The DLL existed, was 1.5 MB, had been built minutes earlier, and was the same size as other
working proc-macros. It was diagnosed as stale cargo fingerprint state and "fixed" with
`cargo clean -p zerovec -p zerovec-derive`, which did make the build succeed. **That diagnosis was
wrong.**

On the very first end-to-end run, Warden read a Windows Security toast off the screen that the
author had not noticed, and reported it: *"Part of this app has been blocked… we can't confirm who
published zerovec_derive-….dll that the app tried to load."*

Confirmed directly in the event log:

```
Microsoft-Windows-CodeIntegrity/Operational, 11:08:38
Event 3077: Code Integrity determined that a process (…\rustc.exe) attempted to load
…\capture\target\debug\deps\zerovec_derive-d9168372f77368cb.dll that did not meet the
Enterprise signing level requirements or violated code integrity policy
Event 3033: …did not meet the Enterprise signing level requirements
```

**Smart App Control is ON** (`SmartAppControlState: On`, `VerifiedAndReputablePolicyState: 1`).
It blocks unsigned binaries by reputation — and **every proc-macro crate is an unsigned DLL that
`rustc` must load at compile time.**

The clean-and-rebuild only worked because it produced a DLL with a *new hash* that SAC had not yet
evaluated. **This will recur** — on new crates, on fresh clones, after any `cargo clean`.

**PLAN.md §3 is corrected by this.** It claimed *"Not domain-joined, Windows Defender only — no
endpoint security to fight."* There is an enforcement layer, and it targets exactly this workflow.

**Open decision, deliberately not taken:** Smart App Control can be disabled, but **cannot be
re-enabled without resetting Windows**. That is irreversible and belongs to the user. The
alternative — rebuild when it bites — costs seconds, not minutes, and is survivable.

> Worth recording plainly: the first time this tool ran end to end, it corrected its own author's
> misdiagnosis by reading the screen. That is the entire thesis of the project, validated on day
> one, against a real bug.

---

## Build gotcha: reqwest 0.13 needs NASM on Windows by default

`reqwest` 0.13 (Dec 2025) switched its default TLS backend from `ring` to `aws-lc-rs`, which on
Windows frequently requires **NASM** installed to compile `aws-lc-sys`
([reqwest#2937](https://github.com/seanmonstar/reqwest/issues/2937), open).

Avoided by selecting `native-tls`, which uses Windows SChannel and needs nothing installed:

```toml
reqwest = { version = "0.13", default-features = false, features = ["blocking", "json", "native-tls"] }
```

`reqwest` was chosen over `ureq` on the pre-registered tiebreak: when two options are close, take
the one that survives into B2. `reqwest`'s blocking API is the same shape as its async API, so the
move into Tauri is `.await` rather than a rewrite.

---

## Delegation tracking

| | A4 research agent |
|---|---|
| Task | Current APIs for `base64`, `reqwest`/`ureq`, in-memory PNG, `serde_json` |
| Model | Sonnet |
| Its own spend | **72,784 tokens**, 54 tool calls, 8.6 min |
| Returned into context | ~2,500 tokens |
| Compression | **~29×** |
| Corrections needed | **None** |
| Did verification find problems? | **No** — the build compiled first try on its recommended config |

### The prediction was wrong, and that is the finding

Before running it, the plan predicted: *"value lower than A3's, concentrated in `base64`."*

**Both halves were wrong.**

- **`base64` turned out to be low-stakes.** `encode()` still exists as a deprecated shim in 0.23.1,
  so stale knowledge would have compiled with a warning, not failed.
- **The real catch was `reqwest`'s TLS backend change** — a hard build failure on Windows with a
  cryptic `aws-lc-sys` error, arriving immediately after a chunk already lost to a missing linker.

So the value was *not* lower than A3's; it was **differently placed**, and in a spot that had not
been anticipated. That is the argument for delegating research even when the topic feels familiar:
the cost of being confidently wrong about *where* the risk sits is exactly what the agent absorbs.

### Running comparison

| Chunk | Agents | Their spend | Character | Value |
|---|---|---|---|---|
| **A2** | 3 | ~310K+ | Measurement, scripting, protocol design | Very high |
| **A3** | 1 | 131K | Teaching + one research question | Narrow, decisive (caught a breaking API rewrite) |
| **A4** | 1 | 73K | Teaching + one research question | Narrow, decisive (caught a Windows build trap) |

**The rule from A3 held, with one amendment.** Delegate research and mechanical work; never
delegate work whose product is your own understanding; verify the machine yourself. The amendment:
**do not skip research because the topic feels well-trodden.** `reqwest` is one of the most widely
used crates in the ecosystem, and it was still the thing that would have broken the session.

**The counter-example is also worth keeping:** the two biggest discoveries across A3 and A4 — the
missing MSVC linker and Smart App Control blocking proc-macros — were **both facts about this
machine**, and neither came from an agent. One came from direct verification, the other from
Warden itself.

---

## Notes and limitations

1. **`expect` everywhere still.** Fine for a CLI; a popup must not panic because a frame did not
   arrive. Replaced around B3/C1.
2. **Answers are far too long for a popup.** 1,031 output tokens here; A2 saw 3,600–7,950
   characters across every model. This is a C1/E1 problem and no model avoids it.
3. **`keep_alive` left at Ollama's default**, so the model unloads after 5 minutes. Deliberate —
   explicit power control is C4's job.
4. **Ollama must be started manually.** Still the A1 gap; C5 owns it.
5. **Single monitor assumed**, per PLAN.md §3.
6. **Smart App Control will block future proc-macros.** Unresolved by choice — see above.

---

## Stage A is complete

> *"I can run one terminal command and get a useful answer about whatever is on my screen, and I
> know which model I'm using and why."* — STAGE-A.md

Both halves are true. **B1 is next:** scaffold Tauri 2 + React + TypeScript. Note that B1 needs
the MSVC build tools installed at A3, and that B2 — moving this Ollama call behind
`tauri::ipc::Channel` — is the chunk where the buffering bug felt here returns in a harder form.
