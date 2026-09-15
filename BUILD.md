# Warden — Build Plan

Companion to [PLAN.md](PLAN.md). Twenty chunks in six stages. **Each chunk is one sitting
and one execution chat.** Every one ends in something observable, so you can always stop.

Author has read about Rust but never written it. Chunks are sized for that.

---

## Decisions closed 2026-09-07

| Open item | Decision |
|---|---|
| **Model family** | **Qwen3-VL** — tops DocVQA (8B: 96.1), beats Qwen2.5-VL on OCR, charts and screenshots at every benchmark, native vision support in Ollama, and 20–40% faster time-to-first-token than Qwen2.5-VL-7B |
| **Size / quant** | **Bake off at A2 — this is not settled by benchmarks** (see below) |
| **Hotkey** | `Ctrl+Alt+W` — left-hand, one-handed, unclaimed. Fallback `Ctrl+Alt+Space`. Never `Alt+Space` (Windows system menu). |
| **Corner** | **Bottom-right.** Offset above the taskbar rather than flush to the edge. Windows toasts land there too and will occasionally overlap — accepted. |
| **Default prompt** | Drafted in PLAN.md §2. Tuned at E1, not decided now. |

### The size/quant question — settle it with your own screenshots

> **SETTLED 2026-09-08 — `qwen3-vl:8b-instruct-q4_K_M`.** Measured on the full 2×2, not the pair
> below. **Size decides accuracy; quantization decides nothing.** Both 4B models called a file
> that does not compile "fine"; both 8B models found the bug. Q8 changed time-to-first-token by
> under 2% while costing +3.2 GB of VRAM. The section below is kept as the reasoning that went
> *into* A2 — see [docs/A2-results.md](docs/A2-results.md) for what came out.

The benchmark gap between Qwen3-VL **4B (95.3 DocVQA)** and **8B (96.1)** is **under one point**.

> **Corrected 2026-09-08.** This section used to say the VRAM difference was large — *"reports
> put 8B at Q4_K_M around 12 GB"* — and framed A2 as a forced choice between a bigger model at
> lower precision and a smaller one at higher precision. **Checked against the Ollama registry,
> that gap mostly isn't there.** The earlier figure was never measured on this machine.

| Candidate — real tag | Download | Why it might win |
|---|---|---|
| `qwen3-vl:8b-instruct-q4_K_M` | **6.14 GB** | More capable model; the benchmark leader |
| `qwen3-vl:4b-instruct-q8_0` | **5.09 GB** | Higher precision where it matters for small text, faster TTFT |

**About 1 GB apart, not seven.** On a 16 GB card both fit with headroom, so memory no longer
forces the decision — it comes down to which one reads your screenshots correctly. Note these
are *download* sizes; resident VRAM will be higher once the KV cache and vision encoder are
counted, which is exactly why A2 measures it rather than assuming.

**So A2 is still a bake-off, not a download** — but it's an accuracy bake-off, not a budget one.

**Use the Instruct variants.** `-thinking-` builds exist at the same sizes (e.g.
`qwen3-vl:8b-thinking-q4_K_M`). Latency is the product here, so Instruct is the default —
record that as a deliberate choice rather than an oversight.

Judge them on **your own screenshots of code and terminals**, not on benchmark numbers.
Also **cap the context well below the 256K default** — you're sending one screenshot, not a book,
and context is where the VRAM goes.

**Backup if both disappoint:** Gemma 3 — day-one Ollama integration, OCR across 32 languages,
and GUI grounding that identifies buttons and fields in screenshots.

---

## Stage A — Foundations (no app yet)

**A4 is already a useful tool**, in a terminal, before any GUI exists. If everything after this
stalls, you still have something.

### A1 — Toolchain — DONE 2026-09-07
Rust 1.98.1 (MSVC), Ollama 0.33.3, `llama3.1:8b`. GPU verified: **100% GPU, 152 tok/s warm**
vs 12.74 on CPU. Full write-up: **[docs/A1-results.md](docs/A1-results.md)**.

Two findings that change later chunks:
- **Cold load is 17.9 s; warm is 0.002 s.** This first led to "keep the model resident forever,"
  but measuring the cost — 5.2 GB of a 16 GB card — reversed that. See **C4**, which replaces
  the always-resident idea with explicit on/off.
- **Ollama has no autostart entry.** It won't survive a reboot. **C5** has to solve this.

Gotchas if repeating: `ollama app.exe` opens a blocking first-run GUI (this killed two automated
attempts) — use `ollama.exe serve` detached instead. A broken CUDA extraction fails **silently**
and simply runs on CPU, so check `ggml-cuda.dll` exists before debugging anything else.

### A2 — The bake-off — DONE 2026-09-08
**Chosen: `qwen3-vl:8b-instruct-q4_K_M`** — the only model that correctly concluded the editor
screenshot would not compile, inside C4's VRAM budget. Full write-up:
**[docs/A2-results.md](docs/A2-results.md)**.

Ran the full 2×2 (4B/8B × Q4_K_M/Q8_0) rather than the planned pair, because the pair varies size
and quantization together and could not have explained its own result. Harness: `evals/bench.mjs`.

Three findings that change later chunks:
- **Q8 buys nothing.** TTFT is set entirely by model size (528 vs 526 ms at 4B; 1165 vs 1151 ms at
  8B — quantization moves it under 2%). At 8B, Q8 costs +3.2 GB VRAM and halves decode, and made
  case-02's conclusion *worse*. **PLAN.md §6's "bias toward precision" is retired.**
- **Both 4B models called the broken file fine.** Reading text accurately is not reasoning about
  code — exactly the split case-02 was built to expose.
- **Cold load is 2.0–4.7 s**, not the 7–18 s A1 measured and C4 assumes. **C4 should be re-read
  against this.** Also: a 1080p screen costs **2,042 image tokens**.

**The prompt matters more than the model.** A terser, fault-presupposing variant ("What's wrong on
this screen?") made *both* surviving models hallucinate errors that don't exist and miss the real
bug. Do not adopt one at E1 without re-running the eval set.

Gotcha if repeating: **Ollama's prompt cache invalidates the obvious protocol.** It caches every
image at once, so repeat runs return prefill in 5 ms instead of ~450 ms and every model looks
identical. Only unloading clears it — see A2-results.md.

### A3 — First Rust: capture a PNG — DONE 2026-09-10
`capture/` captures the primary monitor via `windows-capture` 2.0.1 and writes a 1920×1080
`out.png`. Verified against fullscreen hardware-decoded video — full colour, no black frame,
so this is genuinely WGC and not GDI. Full write-up: **[docs/A3-results.md](docs/A3-results.md)**.

Two findings that change later chunks:
- **The MSVC build tools were missing entirely.** A1 recorded the toolchain as verified but only
  ever exercised Ollama; nothing had reached the link step, because `frametest` fails at the
  borrow check first. Installed at A3. **B1 needs them too** — Tauri on Windows expects MSVC.
- **The B3 borrow fight happened here, early.** `Frame`/`FrameBuffer` are `!Send`/`!Sync` and
  their buffer is unmapped when the callback returns, so the pixels must be copied out with
  `.to_vec()` before they can leave. B3 is the same problem behind a button.

Gate worth reusing: a throwaway `cargo new` + `cargo run` printing `Hello, world!` proves compile
**and link** in ten seconds, before any dependency can muddy the diagnosis.

### A4 — First useful thing — DONE 2026-09-12 · **STAGE A COMPLETE**
`cd capture && cargo run` captures the screen and streams a real answer about it. Prompt lives in
`capture/prompt.txt`; `--model` swaps models without recompiling. Full write-up:
**[docs/A4-results.md](docs/A4-results.md)**.

Cross-validates A2 exactly — **2,077 tokens in, 1.19 s to first token, 9.0 s total** — from a
completely separate harness, which is the check that `num_ctx` took and the screen wasn't
silently truncated.

Three findings that change later chunks:
- **Smart App Control blocks Rust proc-macros.** `rustc` is denied loading unsigned proc-macro
  DLLs; the symptom is `can't find crate for <x>_derive` while the DLL sits on disk intact.
  Workaround is `cargo clean -p` that crate and rebuild. **Recurs.** See PLAN.md §3.
- **`reqwest` 0.13 needs NASM on Windows with default features** (switched to `aws-lc-rs`). Use
  `default-features = false, features = ["blocking","json","native-tls"]`. **B2 inherits this.**
- **The buffering bug B2 warns about was felt here deliberately.** `.text()` gave 14.5 s of
  silence then a dump; `BufReader::lines()` gave first text at 1.5 s. Note `stdout().flush()`
  after each token is not optional — stdout is line-buffered and rebuilds the bug without it.

**Warden found its own bug.** On the first end-to-end run it read a Windows Security toast off the
screen and corrected a misdiagnosis of the proc-macro failure. The thesis works.

---

## Stage B — The app shell

### B1 — Tauri opens a window — DONE 2026-09-12
Tauri 2.11.5 + React + TypeScript at the repo root, window titled **Warden**, identifier
`com.warden.app`. First build 55 s, 362 units. Full write-up: **[docs/B1-results.md](docs/B1-results.md)**.

> **Never run `create-tauri-app` at the repo root.** Passing `.` and accepting the overwrite
> prompt **recursively deletes everything except `.git`** — verified in its source. `evals/shots/`
> is gitignored and would not survive. Scaffold into a sibling temp directory and move files in by
> hand. Push first.

Two notes: the scaffold derives every identity field from the folder name, so `productName`,
`identifier`, both Cargo names, the window title and `index.html`'s `<title>` all needed resetting.
And **Smart App Control did not fire** across 362 units — the A4 block was narrower than assumed.

### B2 — Streaming into the UI — DONE 2026-09-12
An async command streams Ollama tokens to React through `tauri::ipc::Channel`. Verified in both
dev and the packaged `.exe`. Full write-up: **[docs/B2-results.md](docs/B2-results.md)**.

**Streams identically in dev and release** — Rust-side and UI-side time-to-first-token match to
the hundredth of a second (0.21 s dev, 0.17 s packaged), so nothing batches between Rust and the
webview. `prompt_eval_count` 2,077 again, agreeing with `bench.mjs` and `capture/`.
**Watch for:** **Never return the answer as the command's `Result`.** Tauri delivers exactly one
IPC reply per `invoke()`, so a command whose return value carries the payload buffers the whole
thing by construction. Send tokens through a `Channel` instead and let `Result<(), String>` carry
only success or failure.

> **Corrected 2026-09-12 at B2.** This chunk previously warned against `proxy_localhost_stream`,
> "works in dev, breaks silently when packaged." **`proxy_localhost_stream` is not a Tauri symbol**
> — it is absent from the config schema (which has only `proxy` / `proxyUrl`, an unrelated WebView
> network proxy). The warning appears to have been absorbed from an unrelated third-party project
> during planning and mis-attributed. The dev-versus-packaged framing was wrong too: `Result`
> buffering is architectural and behaves identically in both. The actionable advice above survives;
> the mechanism did not.

### B3 — Button-triggered capture — DONE 2026-09-13
Click → live screenshot → streamed answer. `ask_about_screen` captures the primary monitor inside
`spawn_blocking`, encodes a PNG in memory, and streams through B2's `Channel`. Full write-up:
**[docs/B3-results.md](docs/B3-results.md)**.

**C2's invisibility call landed early here**, and was verified rather than assumed:
say so explicitly, so it was measured. Window visible, not minimised, foreground, affinity reading
back `17`, and absent from a capture taken at that moment. **D1 still owns the cross-app testing.**

Three findings that change later chunks:
- **COM apartment conflict.** WebView2 puts the main thread in STA; `windows-capture` needs MTA.
  Capture must run on a fresh thread via `spawn_blocking` or it fails with `FailedToInitWinRT`.
- **The affinity flag resets after `hide()`/`show()`** ([tauri#14189](https://github.com/tauri-apps/tauri/issues/14189),
  open). **B4** creates exactly that shape — reapply after every show, and verify it sticks.
- **The flag fails on `WS_EX_LAYERED` windows**, but PLAN.md §4 specifies the popup as layered.
  **C1 must resolve that conflict** — the headline feature depends on it.

### B4 — Global hotkey — DONE 2026-09-14 · with **C1** and C1b's translucency
`Ctrl+Alt+W` summons a translucent panel bottom-right that streams an answer and never steals
focus. Full write-up: **[docs/B4-results.md](docs/B4-results.md)**.

**The C1 layered-window contradiction is resolved.** Click-through does **not** break
capture taken while visible. The documented failures were WPF windows calling `UpdateLayeredWindow`.

Three findings that change later chunks:
- **DWM backdrops are a dead end for this popup.** Acrylic sees through but cannot go dark;
  MicaDark is dark but never see-through; `.color()` and `.state()` are inert on Windows 11.
  Translucency comes from `transparent: true` + CSS alpha instead. **C1b inherits this.**
- **Esc cannot dismiss a `focusable: false` window** � it never receives key events. The hotkey
  toggles instead. **C3 must design the focus transition** before follow-up typing works.
- **An auto-hiding taskbar does not change the work area.** Position against `Shell_TrayWnd`'s
  live rect, not `work_area`, or the popup sits under the bar.

> **BLOCKED AFTER THIS CHUNK.** Smart App Control now refuses to run `warden.exe` � content-based,
> retroactive, no workaround. **This blocks the rest of C, all of D, and E1.** `capture/` is
> unaffected. See [PLAN.md §3](PLAN.md) for the full analysis and the four options.

---

## Stage C — Make it real

### C1 — The corner popup
**Do:** Second Tauri window — small, undecorated, **bottom-right**, always on top,
`WS_EX_NOACTIVATE`.
**Done when:** You hotkey from your editor and your cursor is still in the editor.
**Watch for:** Position against the *work area*, not the raw screen bounds, so it sits above the
taskbar. Windows toasts share that corner and will sometimes overlap — accepted, not a bug.

### C1b — Make the answer readable
**Added 2026-09-12.** Lettered rather than renumbered so existing references to C2–C5 in PLAN.md
and the results docs stay valid.

**Why this exists:** nothing else in this plan owns what the popup *looks* like. C1 specifies only
behaviour — position, topmost, no focus steal. Two measured facts make that a gap rather than
polish:

- **A2 measured every candidate producing 3,600–7,950 characters.** None of them fit a
  bottom-right corner popup. The problem was recorded and assigned to nobody.
- **The model emits Markdown** — headers, `**bold**`, code fences, emoji. Rendered raw, that's a
  wall of asterisks and hashes.

Under this project's own rule — a feature earns its keep only if it actually gets used — an answer
you can't read means you don't reach for it, and **E1 fails**.

**Do:**
- Render Markdown, **code blocks first** — the screen is mostly code and terminals.
- Decide what happens when an answer overflows: scroll, clamp with an expand affordance, or cap
  the response.
- Set type size, line length and contrast for something read at a glance, in a corner, over
  whatever is behind it.
- Make streaming not reflow the layout as tokens land.

**Done when:** you can read a real answer about a real stack trace at a glance, without leaning in.

**Watch for:** the obvious fix for verbosity is a terser prompt — **don't.** A2 tested exactly that
and both finalist models started inventing errors that didn't exist. If the prompt changes to
control length, it gets re-run against the eval set first (F1). Also: whatever renders the Markdown
must not autofocus anything, or C1's no-focus-steal guarantee breaks.

`windows` crate.

### C3 — Follow-ups
**Do:** A text box that asks again against the capture already in memory.
**Done when:** Second question, no new screenshot taken.

### C4 — Power control (explicit on/off, verified against the card)
**Why this exists:** the model costs **5.2 GB of a 16 GB card** while loaded. Holding it there
permanently is wrong when the popup has to work over games that want 8–12 GB. A timeout is also
wrong — it decides for you and leaves you guessing whether the next press is instant or an
18-second stall. So: explicit states you control and can see.

**Do:**
- Three states, surfaced in the tray: **Off** (card free) · **Warming** · **Ready**.
- Warm up = `POST /api/generate` with `keep_alive: -1`. Turn off = same call with
  `keep_alive: 0`. Both are already proven to work on this machine.
- A hotkey (or tray click) for each.
- **Read the state from the GPU, not from Ollama.** Use NVML via the `nvml-wrapper` crate.
  `ollama ps` reports what Ollama *believes*; NVML reports what the card actually holds.
  Show the real MB figure in the tray tooltip.
- Decide what the capture hotkey does while Off: auto-warm then answer (showing a "warming"
  state so the delay is explained), rather than failing silently.

**Done when:** you can turn it off and watch VRAM return to the 454 MiB baseline — confirmed
against `nvidia-smi`, not against Ollama's own claim.

**Reference numbers measured 2026-09-08:** off 454 MiB · loaded 5,705 MiB · unloaded 454 MiB ·
warm-up ~7 s in-session, ~18 s after a reboot (disk cache).

### C5 — Stop launching it
**Do:** Launch on login, `Esc` dismisses, tray menu tidy.
**Done when:** You never open it on purpose.
**Watch for:** Ollama itself has **no autostart entry** (found in A1) — it won't survive a
reboot. Warden either starts it or tells you clearly that it's not running.

---

## Stage D — Verify

Before two weeks of daily use, prove the invisibility actually holds. **Different apps capture
every one.** Assume nothing; test each.

### D1 — "Can they see it?" — every result must be NO

| App | Capture path | Result |
|---|---|---|
| Zoom — screen share | native | ☐ |
| Microsoft Teams — screen share | native | ☐ |
| Discord — Go Live | native | ☐ |
| Slack — huddle share | native | ☐ |
| Google Meet in Chrome | browser `getDisplayMedia` | ☐ |
| OBS — **Display Capture** | desktop duplication | ☐ |
| OBS — **Window Capture** | per-window (different path — test separately) | ☐ |
| Windows Snipping Tool / PrtScn | GDI / WGC | ☐ |
| Xbox Game Bar capture | native | ☐ |
| Steam broadcast | injected | ☐ |

**Also:** trigger a follow-up capture with the popup open and confirm Warden's own last answer
is not in the new screenshot.

**If any row fails:** that app is a known gap. Write it down in the README rather than pretending
the feature is absolute — you'll be relying on it in a meeting one day.

### D2 — "Can I see it?" — expected results vary

| Situation | Expect |
|---|---|
| Over a normal windowed app | Visible |
| Over a borderless-windowed game | Visible |
| Over fullscreen video — YouTube, VLC | Visible |
| Over **exclusive-fullscreen** game | **Not visible — known limitation.** Confirm it fails cleanly, no crash or hang |
| Over another always-on-top window | Test z-order; last-topmost usually wins |
| Over UAC prompt / lock screen | Not visible, and that's correct — secure desktop |

**Also check capture still works** while a borderless game or hardware-accelerated video is
fullscreen. Protected content (Netflix in a browser) will come back black — that's DRM, not
your bug.

---

## Stage E — Live with it

### E1 — Two weeks
**Do:** Use it. Tune the default prompt — expect ~20 rewrites, but run each candidate against
the eval set (F1) rather than judging it on the screenshot in front of you. Keep a scratch list of what
actually annoyed you.
**Done when:** You reach for it without deciding to.
**Then:** pick the next thing off the "after v1" list in PLAN.md, ordered by what you missed —
not by what looked interesting today.

---

## Stage F — Ongoing: keep testing models

A2 picks a model with the information available in one sitting. **That decision goes stale** —
new VLMs ship constantly, and the benchmarks that informed A2 measure scanned documents, not
syntax-highlighted code on a dark theme. Nobody benchmarks your actual use case except you.

So make swapping models a ten-minute job instead of a vibes decision.

### F1 — Build a personal bench harness
**Do:** Extend A4's code to loop over **screenshots × model tags × prompt variants**, and print
a table: model · prompt · did it get it right · time to first token · time to full answer ·
resident VRAM.

**The prompt is a dimension, not a constant.** It does all the intent-guessing in this app, so
it gets tested exactly like a model does. A prompt tweak that fixes today's screenshot routinely
breaks one you fixed last week, and you will not notice without a fixed set to re-run. Prompt
and model also interact — a prompt tuned for one model may be wrong for its replacement, so a
model swap means re-testing prompts, not carrying the old one over.

**Seed it with A2's artifacts** — keep those screenshots and timings, don't throw them away.

**Note:** `evals/shots/` is gitignored. Screenshots of a real desktop can contain anything, so
the eval set stays local and is never committed, private repo or not.

**Done when:** testing a new model *or a new prompt* is one command, and you can see whether a
change regressed anything.

### F2 — Grow the eval set from real failures
**Do:** Every time the model misreads something during normal use, **save that screenshot into
the eval folder** with a note on what it should have said.

This is the highest-value habit in the whole project. Your E1 annoyance list becomes a test
suite that's specific to how you actually work — which is exactly what no public benchmark
gives you. Twenty real failures beats any leaderboard.

### F3 — Re-run it when something new lands
**Candidates to try over time:**

| Model | Why it's worth a slot |
|---|---|
| **Gemma 3** | The real contender. GUI grounding identifies buttons and fields in screenshots — arguably closer to this use case than document scores are. Try it first. |
| **MiniCPM-V 4.5** | Punches above its weight at ~6 GB. The option if resident VRAM starts hurting. |
| **InternVL** | Historically strong at OCR; check whether Ollama support has landed yet. |
| **Qwen3-VL 30B** | Only if partial offload proves fast enough — probably not, given latency is the product. |
| Whatever shipped last month | The point of the harness. |

**Rule:** a new model replaces the incumbent only if it wins on **your** eval set. Not on a
benchmark, not on a release post, not because it's newer.

---

## Crates you'll end up with

| Job | Crate / API |
|---|---|
| Screen capture | `windows-capture` (Windows Graphics Capture) |
| HTTP to Ollama | `reqwest` |
| Streaming JSON | `serde_json` |
| Global hotkey | Tauri 2 global-shortcut plugin |
| Launch on login | Tauri 2 autostart plugin |
| Tray | Tauri 2 built-in |
| Win32 window flags | `windows` crate |

Confirm current versions when you get there rather than pinning from this doc.

---

## Where it will hurt

1. **B2** — streaming through the Tauri boundary. Gets everyone.
2. **B3** — the capture frame's lifetime. Your first real borrow-checker fight.
3. **C2** — Win32 through Rust: raw HWND, unsafe block, exact types. Short but fiddly.

Nothing else is conceptually hard. It's wiring.
