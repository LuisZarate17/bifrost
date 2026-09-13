# B2 — Streaming into the UI: results

**Completed 2026-09-12.** Chunk B2 of [BUILD.md](../BUILD.md).

## What was built

The Ollama call now lives in `src-tauri`, and tokens reach React through a
`tauri::ipc::Channel` as they arrive. Click a button, watch the answer build up.

`capture/` is untouched and still works — Stage A remains the way to exercise the pipeline
without the GUI in the way.

| | |
|---|---|
| Command | `async fn ask_about_image(image_path, prompt, model, on_event: Channel<StreamEvent>)` |
| Sends | `evals/shots/terminal-rust-error.png` — A2 recorded exactly what this model says about it |
| Added deps | `reqwest` (async, `native-tls`/`json`/`stream`), `futures-util`, `base64` |
| Release build | 2 m 17 s, `warden.exe` **4.8 MB** — plus MSI and NSIS installers, unprompted |

---

## It streams, in dev *and* packaged

BUILD.md's bar is specifically *"tokens appear word by word in a built `.exe`, not just in dev."*

| | Dev | **Packaged `.exe`** |
|---|---|---|
| Rust-side time to first token | 0.21 s | **0.17 s** |
| **UI actually saw it at** | **0.21 s** | **0.17 s** |
| Image + prompt tokens | 2,077 | **2,077** |
| Output tokens | 1,211 | 1,181 |
| Total | 10.8 s | 10.2 s |

**The two TTFT measurements are identical in both builds.** That comparison was instrumented
deliberately: one clock in Rust when the first token arrives from Ollama, another in JavaScript
when it reaches the screen. If anything between Rust and the webview batched, those numbers would
diverge. They do not, to the hundredth of a second.

`prompt_eval_count: 2077` matches A2 and A4 exactly — the check that `num_ctx: 8192` took and the
image was not silently truncated. Three independent implementations now agree on that number:
`bench.mjs` (Node), `capture/` (blocking Rust), and this (async Rust in Tauri).

TTFT of 0.17–0.21 s against A2's 1.165 s is not a discrepancy — it is **A2's prompt cache**. Same
image, same prompt, repeat request, so prefill came from cache. That is the exact behaviour that
nearly invalidated A2's latency measurements.

---

## The warning this chunk was built around was wrong

BUILD.md, PLAN.md and STAGE-B.md all carried this, written during planning:

> Do **not** route this through `proxy_localhost_stream` — it buffers, works in dev, breaks
> silently when packaged.

**Two things about it were false.**

**1. `proxy_localhost_stream` is not a Tauri symbol.** It does not appear in the Tauri config
schema, which contains only `proxy` and `proxyUrl` — the latter being *"the proxy URL for the
WebView for all network requests"*, macOS-gated, entirely unrelated. Research traced the phrase to
a hand-written command in an unrelated third-party project; that specific attribution could not be
independently confirmed, but its **absence from Tauri is verified**.

**2. The dev-versus-packaged framing was wrong**, and this chunk measured it. Tauri sends exactly
one IPC reply per `invoke()`, so a command that returns its payload buffers **by construction** —
identically in dev and release. There is no divergence to be surprised by later. The table above
is the evidence: release behaves the same as dev, marginally faster.

**The actionable advice survived; the mechanism did not.** Restated correctly:

> **Never return the answer as the command's `Result`.** Let `Result<(), String>` carry only
> success or failure, and send every token through the `Channel`.

Tauri's own docs back this: *"The Tauri channel is the recommended mechanism for streaming data
such as streamed HTTP responses to the frontend."*

**All five occurrences were corrected** — BUILD.md, PLAN.md, STAGE-B.md, and the passing references
in the A4 and B1 results docs — each with a dated note saying what the old claim got wrong.

> **This is the third inherited assumption this project has carried without checking**, after
> *"8B needs ~12 GB VRAM"* (A2) and *"toolchain verified"* (A1, which hid a missing linker for two
> chunks). All three were plausible, all three were written down confidently, and none had been
> measured. **A stale warning is as dangerous as a missing one** — this one would have had us
> distrust a working dev build and hunt a packaged-only bug that cannot exist.

One claim remains **unverified**: that WebView2 aggregates TCP chunks for short responses in
release mode. It did not manifest here, but absence of evidence is not proof.

---

## Two bugs worth recording

**The path escaping.** `"C:\\Users\\..."` written through a heredoc arrived as single backslashes,
so JavaScript read `\t` as a tab and silently ate the rest. The error surfaced cleanly in the UI —
the error path worked on its first real test — but the fix is **forward slashes**, which Windows
accepts and which cannot be mis-escaped.

**The missing serde attribute.** `#[serde(rename_all = "camelCase")]` renames *variants*, not the
*fields inside* them; that needs `rename_all_fields`. Rust sent `prompt_eval_count` while
TypeScript read `promptEvalCount`, producing `undefined` and `NaN` in the stats line while the
answer itself streamed perfectly. The research had supplied the exact annotation and it was
dropped in transcription.

Also: **`base64 0.22.1` was already in Tauri's dependency tree**, so a hand-rolled encoder was
20 lines of avoidable risk for zero build-time saving. Swapping to the crate rebuilt in 0.83 s.

---

## Delegation tracking

| | B2 research agent |
|---|---|
| Task | `tauri::ipc::Channel` API, async commands, async `reqwest` streaming, and **whether our own warning still held** |
| Model | Sonnet |
| Its own spend | **85,792 tokens**, 43 tool calls, 11 min |
| Returned into context | ~4,000 tokens |
| Compression | **~21×** |
| Corrections needed | **None** |
| Did verification find problems? | **Partly** — the core claim was confirmed independently; its third-party attribution was not |

### A new category of catch

The four previous agents all corrected assumptions about **external tools**: a breaking API
rewrite, a Windows build trap, a destructive scaffolder. **This one corrected our own
documentation** — a claim that had propagated through five files and been cited as authoritative
across three chunks.

That is worth separating out, because it is a failure mode no amount of care within a chunk would
catch. The warning was written once, early, and every subsequent chunk treated it as settled fact.
It took explicitly asking *"is this still true?"* to find that it never was.

The question was put to the agent deliberately, with the framing that **a stale warning is its own
failure mode**. Without that instruction it would likely have answered the API questions and left
the premise untouched.

### Running comparison

| Chunk | Agents | Spend | What it caught |
|---|---|---|---|
| **A2** | 3 | ~310K | Ollama API contract; the harness itself |
| **A3** | 1 | 131K | `windows-capture` 2.0 breaking rewrite |
| **A4** | 1 | 73K | `reqwest` TLS change needing NASM on Windows |
| **B1** | 1 | 85K | A scaffolder that deletes the repo |
| **B2** | 1 | 86K | **A false warning in our own docs** |

**Five chunks, five catches, none of them things already known.** The amendment from B2: *ask
whether your own recorded assumptions still hold, not only what the external tools do.*

---

## Notes and limitations

1. **Still no styling.** The window is a button and a `<pre>`. All of it is replaced at C1/C1b.
2. **The verbosity problem is now visible.** 1,181 output tokens in a scrollable block — readable
   here, impossible in a corner popup. Exactly what **C1b** was added to own.
3. **The image path is hardcoded** in `src/App.tsx`. B3 replaces it with a live capture.
4. **Ollama still manual** — the A1 gap; C5 owns it.
5. **`panic = "abort"` in release** means any `expect()` that fires kills the packaged app outright
   rather than surfacing an error. The command itself returns `Result`, but `run()` still uses
   `expect`. Worth replacing before the popup depends on it at C1.

---

## Next

**B3 — button-triggered capture.** Replace the hardcoded image with a live screenshot. The
borrow-checker fight was already met at A3: `Frame`/`FrameBuffer` are `!Send`/`!Sync` and their
buffer is unmapped when the callback returns, so pixels must be copied out with `.to_vec()`.
`capture/src/main.rs` already does exactly this — reuse the shape rather than rediscovering it.
