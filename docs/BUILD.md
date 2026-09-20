# ODIN — Build Plan

Companion to [PLAN.md](PLAN.md). Stages A-F built **Warden**, the overlay, in nineteen chunks;
Stage G builds **Citadel** and is tracked in its own packet. **Each chunk is one sitting
and one execution chat.** Every one ends in something observable, so you can always stop.

Author has read about Rust but never written it. Chunks are sized for that.

---

## Where this stands — 2026-09-19

**Stages A–C complete; Warden is finished and in daily use.** Stage G is under way — the groundwork
that turns this repo into a workspace two apps can live in, then **Citadel**, the local chat app.
Chunk-by-chunk detail is in [`../stages/STAGE-G.md`](../stages/STAGE-G.md); this is the summary.

### Stage G so far — 2026-09-19

| | | |
|---|---|---|
| **1a–1d** | ✅ | Repo restructured. Crates under `crates/`, `warden-core` → `odin-core`, Warden into `apps/warden/`, four Cargo projects merged into one workspace |
| **1e** | ✅ | Renderer extracted to `@odin/ui`, stylesheet split with it. *(Visual check still open)* |
| **1f** | ✅ | ~30 stale doc paths fixed, `tools/shortcuts.mjs` written, ~17,200 stale build files swept |
| **1g** | ◐ | Shortcuts repointed. `gh repo rename odin` held until the next push |
| **1h** | ☐ | **Needs a human** — rename the repo root folder to `odin` |
| **Step 2** | ✅ | Five false claims corrected across the docs |
| **Step 3** | ✅ | Shared config at `%APPDATA%\odin\config.json`; six duplicated constants removed |
| **G0** | ✅ | Tool-call probe. **[G0-results.md](G0-results.md)** — `qwen3-vl:8b` 10/10, `llama3.1:8b` 6/10 |
| **G1** | ✅ | **Citadel** — one window, one conversation, `/api/chat` streamed over a `Channel`, rendered with `@odin/ui` |
| **G2** | ✅ | The switchboard — capability-labelled picker, load/unload, choice written to the shared config |
| **G3** | ✅ | Warden reads the config on every summon and refuses cleanly when the model has no `vision` |
| **G4** | ☐ | **Next** — persistence: conversations survive a restart |

**What the groundwork actually found**, none of which a build would have caught:

- **`num_ctx` had diverged** between the CLI (8192) and Warden (32768). Ollama keys a runner by
  model *and* context, so `odin warm` was being thrown away and the model reloaded — measured at
  ~3s in **both** directions. Now one value in one place; the same sequence reads `load 0.00s`.
- **`[profile.release]` stopped applying** the moment the workspace appeared. Cargo only honours
  profiles at the workspace root and demotes a package-level one to a *warning*, so Warden's `lto`,
  `strip` and `panic = "abort"` were silently ignored.
- **Both Desktop shortcuts had been dead for three chunks**, pointing at a path that stopped
  existing. A broken shortcut does nothing and says nothing.
- **The CLI read `prompt.txt` by relative path**, so running it from anywhere else silently used an
  inline fallback — which was itself a fourth copy of the prompt.
- **`odin help` still said `capture`** six chunks after the binary was renamed. A string literal,
  so no compiler had reason to object.

Three things are waiting on a human before the next push — see
[PENDING-CHECKS.md](PENDING-CHECKS.md).

---

## Where Stages A–F stood — 2026-09-15

**16 of 19 chunks.** *(C5, E1 and F3 dissolved; D1, D2 and F2 deferred to one testing round.)* Stage A complete, Stage B complete, C1 done early alongside B4,
C1b, C1c, C2 and C4a done 2026-09-15; C4b, C3 and F1 done 2026-09-16; F1b and the
verification round 2026-09-17. **Stage C is complete.**

> **Twenty, after a lot of moving.** C4 split into **C4a** (CLI) and **C4b** (tray), the keyboard
> and wheel work landed as **C1c**, and **C5** and **E1** were both dissolved once it was clear they
> were not tasks anyone could sit down and do. Lettered rather than renumbered throughout, so every
> existing reference stays valid.

> **Correction, 2026-09-15: nothing is blocked.** The **debug build runs** with Smart App Control
> enforced — `target/debug/warden.exe` launches, and C2 was verified against Discord, Zoom and
> Teams using it. Only `target/release/warden.exe` is refused: *"An Application Control policy has
> blocked this file."*
>
> Six chunks were listed as blocked on code signing and were not. **Signing is a shipping problem,
> not a building problem** — it buys a binary that runs on a machine other than this one. That also
> settles the open SignPath caveat: local development never depended on it.

### What is left

**Nothing to build.** One deferred testing round, and distribution if it is ever wanted.

#### The testing round — an afternoon, whenever

Everything remaining is the same activity: sitting down and putting the app through things.

| | |
|---|---|
| **D1 — can they see it?** | Seven capture paths left. OBS counts twice; display and window capture are different mechanisms and can disagree. Then browser `getDisplayMedia`, Snipping Tool, Game Bar, Steam, Slack. **Two answers per app**: can it *see* the window, and does it *list* it |
| **D2 — can I see it?** | Over a borderless game, fullscreen video, another always-on-top window. Invisible over an exclusive-fullscreen game and the lock screen is correct — confirm it fails cleanly |
| **F2 — grow the eval set** | Put a variety of screens in front of it and record what it gets wrong. Two screenshots of one bug is thinner than the conclusions drawn from it |

**The deferral has a cost, and it is not zero.** Seven capture paths are assumption rather than
evidence. That is a fine trade for one person who knows which three were tested. It stops being
fine the moment anyone else runs it, or the moment it matters in a meeting that was not planned.

#### Not tasks

- **F3** — fires when a model ships. `bench` makes it ten minutes. Gemma 3 first
- **Signing** — a LICENSE and CI. Buys a binary for machines other than this one, and nothing else

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
> *into* A2 — see [A2-results.md](results/A2-results.md) for what came out.

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
vs 12.74 on CPU. Full write-up: **[A1-results.md](results/A1-results.md)**.

Two findings that change later chunks:
- **Cold load is 17.9 s; warm is 0.002 s.** This first led to "keep the model resident forever,"
  but measuring the cost — 5.2 GB of a 16 GB card — reversed that. See **C4**, which replaces
  the always-resident idea with explicit on/off.
- **Ollama has no autostart entry.** It won't survive a reboot. **C5 was dissolved**, so this now belongs to the `capture warm`/`off`/`status` work and C4's tray.

Gotchas if repeating: `ollama app.exe` opens a blocking first-run GUI (this killed two automated
attempts) — use `ollama.exe serve` detached instead. A broken CUDA extraction fails **silently**
and simply runs on CPU, so check `ggml-cuda.dll` exists before debugging anything else.

### A2 — The bake-off — DONE 2026-09-08
**Chosen: `qwen3-vl:8b-instruct-q4_K_M`** — the only model that correctly concluded the editor
screenshot would not compile, inside C4's VRAM budget. Full write-up:
**[A2-results.md](results/A2-results.md)**.

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
`crates/odin-cli/` captures the primary monitor via `windows-capture` 2.0.1 and writes a 1920×1080
`out.png`. Verified against fullscreen hardware-decoded video — full colour, no black frame,
so this is genuinely WGC and not GDI. Full write-up: **[A3-results.md](results/A3-results.md)**.

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
`target/debug/odin` captures the screen and streams a real answer about it. Prompt lives in
`crates/odin-cli/prompt.txt`; `--model` swaps models without recompiling. Full write-up:
**[A4-results.md](results/A4-results.md)**.

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
`com.warden.app`. First build 55 s, 362 units. Full write-up: **[B1-results.md](results/B1-results.md)**.

> **Never run `create-tauri-app` at the repo root.** Passing `.` and accepting the overwrite
> prompt **recursively deletes everything except `.git`** — verified in its source. `evals/shots/`
> is gitignored and would not survive. Scaffold into a sibling temp directory and move files in by
> hand. Push first.

Two notes: the scaffold derives every identity field from the folder name, so `productName`,
`identifier`, both Cargo names, the window title and `index.html`'s `<title>` all needed resetting.
And **Smart App Control did not fire** across 362 units — the A4 block was narrower than assumed.

### B2 — Streaming into the UI — DONE 2026-09-12
An async command streams Ollama tokens to React through `tauri::ipc::Channel`. Verified in both
dev and the packaged `.exe`. Full write-up: **[B2-results.md](results/B2-results.md)**.

**Streams identically in dev and release** — Rust-side and UI-side time-to-first-token match to
the hundredth of a second (0.21 s dev, 0.17 s packaged), so nothing batches between Rust and the
webview. `prompt_eval_count` 2,077 again, agreeing with `bench.mjs` and the CLI.
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
**[B3-results.md](results/B3-results.md)**.

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
focus. Full write-up: **[B4-results.md](results/B4-results.md)**.

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
> retroactive, no workaround. **This blocks the rest of C, all of D, and E1.** The CLI is
> unaffected. See [PLAN.md §3](PLAN.md) for the full analysis and the four options.

---

## Stage C — Make it real

### C1 — The corner popup — DONE 2026-09-14 · landed early, alongside B4
**Do:** Second Tauri window — small, undecorated, **bottom-right**, always on top,
`WS_EX_NOACTIVATE`.
**Done when:** You hotkey from your editor and your cursor is still in the editor.
**Watch for:** Position against the *work area*, not the raw screen bounds, so it sits above the
taskbar. Windows toasts share that corner and will sometimes overlap — accepted, not a bug.

### C1b — Make the answer readable — DONE 2026-09-15
**Added 2026-09-12.** Results: [C1b-results.md](results/C1b-results.md).

**Outcome:** Markdown rendering with the img/link lockdown, lazy Shiki for Rust, grow-to-fit
window, and a CSP. The prompt experiment ran and was **rejected on the evidence** — brevity kept
the mechanism but lost the conclusion on case-02, so length is solved in the UI instead. Two new
checks came out of it: `npm run check:inert` and `npm run check:fixes`.
 Lettered rather than renumbered so existing references to C2–C5 in PLAN.md
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

### C1c — Reach the answer without a pointer — DONE 2026-09-15
**Did:** `Ctrl+Alt`+wheel scrolls the answer; `Ctrl+Alt+Z` picks a top-level block and copies it.
Every control is written down in [shortcuts.md](shortcuts.md) — there is no menu, tray
item or window chrome, so that file *is* the interface.

**This answers C1b's open overflow question above:** a long answer scrolls, and the window grows to
a cap rather than clamping or truncating the response.

**Why the wheel and not letters.** `Ctrl+Alt+C` went first, then `Ctrl+Alt+Q`/`A`. A global hotkey
outranks anything an application binds internally, so claiming `Ctrl+Alt+<letter>` takes that chord
away from every app while the popup is up — and **`RegisterHotKey` reports every one of them as
free**, because it only knows about other *global* registrations, never what an application binds
for itself. There is no way to test for the clash in advance. The wheel claims no key at all.

**Both the hook and `Ctrl+Alt+Z` are released on dismiss**, not held for the life of the process.
The hook's callback only increments a counter — Windows silently removes a low-level hook whose
callback overruns `LowLevelHooksTimeout` (300 ms), and this one sits in front of every mouse event
in the session. A separate thread drains it, separate because the window animator blocks on
`SetWindowPos` by design.

**Escape hatches:** `WARDEN_NO_CLICKTHROUGH=1` puts the mouse back on the popup, and
inspecting how it *looks* means temporarily giving that up.

**Watch for:** the obvious fix for verbosity is a terser prompt — **don't.** A2 tested exactly that
and both finalist models started inventing errors that didn't exist. If the prompt changes to
control length, it gets re-run against the eval set first (F1). Also: whatever renders the Markdown
must not autofocus anything, or C1's no-focus-steal guarantee breaks.

`windows` crate — landed with B4; C2 owed only the test.
**Verified:** **Discord, Zoom and Teams all show nothing.** Affinity reads back `17`.
Results: [C2-results.md](results/C2-results.md).

**One finding worth carrying into D1.** The exclusion hides a window's *contents*, not its
*existence* — **Zoom's share picker lists "Warden" by name** with a black thumbnail, and will let
you share it (the recipient sees an empty rectangle). Nothing it says leaks; that it exists does.
So D1 should record two answers per app, not one: can it **see** the window, and does it **list**
it.

### C3 — Follow-ups — DONE 2026-09-16 · verified 2026-09-17
**Did:** `Ctrl+Alt+F` opens a box that asks again about the screenshot already taken. The PNG is
kept in a managed `LastCapture` and reused; a follow-up with nothing stored **errors rather than
quietly capturing**, because answering about a freshly-grabbed screen would be the wrong answer to
the right question.

**The focus transition C5 handed here.** Typing needs a caret and the window is built never to take
one. `WS_EX_NOACTIVATE` comes off while the box is open and goes back on when it closes, with the
previous foreground window remembered and restored. `SetForegroundWindow` is refused unless the
calling process already owns the foreground, so it attaches to the foreground thread's input queue
first — without that it silently does nothing.

**A side effect worth knowing:** `Esc` works inside the box and nowhere else in the app, because
those few seconds are the only time the window can receive a key event at all.

**Cost:** a second letter claimed while the popup is up. `Ctrl+Alt+F` is unavailable to every other
app for as long as an answer is on screen, and as C1c established, nothing detects that in advance.

### C4a — Power control in the CLI — DONE 2026-09-15
**Did:** `capture warm` / `off` / `status`. Results: [C4a-results.md](results/C4a-results.md).

Measured: **off −8,193 MiB · warm +7,873 MiB in 2.2 s · off −7,873 MiB.** The warm and off deltas
match exactly in a tight cycle, which is the acceptance test — two measured figures compared to
each other, never to a constant.

**Carried into C4b:** a one-shot CLI cannot observe **Warming** — `/api/ps` has no "loading" entry,
so a model appears only once resident. Warming is in-process knowledge, which is exactly why the
tray can show it and `status` cannot.

**And the trap:** Ollama keys a loaded runner by the model *and* by `num_ctx`, so warming at the
wrong context silently reloads and buys nothing. The capture footer now prints `load 0.01s` as
standing proof that a warm actually served the request.

### C4b — Power control in the tray — DONE 2026-09-16 · verified 2026-09-17
**Why this exists:** the model costs **~8 GB of a 16 GB card** while loaded (A2: +8,151 MiB over
baseline). Holding it there permanently is wrong when the popup has to work over games that want
8–12 GB. A timeout is also wrong — it decides for you and leaves you guessing whether the next
press is instant or a stall. So: explicit states you control and can see.

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

**Done when:** the tray shows the three states and the figure in its tooltip is the card's, not
Ollama's. C4a proved they disagree — 8,773 MiB on the card against a `size_vram` claim of 6,233.

**Reference numbers — re-measured at A2 and again at C4a, superseding A1's:**

| | |
|---|---|
| Baseline, idle | **~900–985 MiB, and it drifts.** Seen at 667, 900, 945, 985 and 1033 MiB across sessions. **Sample it fresh; never hard-code it** |
| Model resident | **+7,873 to +8,193 MiB** over baseline (A2 recorded +8,151) |
| Cold load | **2.2–3.7 s** |

> The numbers previously here — off 454 MiB, loaded 5,705 MiB, warm-up ~7 s / ~18 s — were A1's,
> measured against `llama3.1:8b` before the vision model was chosen. They were wrong by roughly 8×
> on the baseline, and the old acceptance test ("watch VRAM return to the 454 MiB baseline") could
> never have passed. Corrected at C4a.

### ~~C5 — Stop launching it~~ — DISSOLVED 2026-09-14

**Cut.** Not deferred — every part of it is gone or lives somewhere else now. **Nineteen chunks.**

The original was *"launch on login, `Esc` dismisses, tray menu tidy."* Taken apart:

| Part | Where it went |
|---|---|
| **Launch on login** | **Cut deliberately.** You open Warden when you want the hotkey live. This is *more* faithful to PLAN.md §0's **"summoned, never ambient"** than autostart was — and it shrinks the window in which the app needs to run at all |
| **`Esc` dismisses** | **Already impossible, resolved at B4.** A `focusable: false` window never receives key events, so Esc can't reach it. The hotkey toggles instead. C3 owns the focus transition when typing arrives |
| **Tray menu tidy** | **C4.** That chunk already builds a tray showing Off / Warming / Ready |
| **Ollama has no autostart** | **The `capture warm`/`off`/`status` work**, which runs today, plus C4's tray later. Still a real gap — it just never needed its own chunk |

**What this changes downstream.** The app running all day was the thing that made Smart App Control
a permanent problem rather than an occasional one. Opening it on purpose means it runs in bounded
windows — which doesn't remove the blocker, but it does change its shape.

It also moves E1's bar: *"you reach for it without deciding to"* is harder when there's a step
before the step. **That's the real cost of this decision, and it was taken knowingly.** In practice
the app gets opened once and left running, so the friction is per-session rather than per-use.

---

## Stage D — Verify

Before two weeks of daily use, prove the invisibility actually holds. **Different apps capture
every one.** Assume nothing; test each.

### D1 — "Can they see it?" — every result must be NO — **DEFERRED 2026-09-17**

> **Parked deliberately, not forgotten.** Three of ten rows are answered — Discord, Zoom and Teams
> all show nothing — and the mechanism is verified: affinity reads back `17`, and B4 disproved the
> warning that click-through would break it. What is left is breadth, not doubt.
>
> **The cost of deferring is knowing less about where *not* to trust it.** Seven capture paths are
> currently assumption rather than evidence. That is a fine trade while the app is used by one
> person who knows which three were tested; it stops being fine the moment anyone else runs it, or
> the moment it matters in a meeting that was not planned.

| App | Capture path | Result |
|---|---|---|
| Zoom — screen share | native | **NO** — verified C2, but *listed by name* in the picker |
| Microsoft Teams — screen share | native | **NO** — verified C2 |
| Discord — Go Live | native | **NO** — verified C2 |
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

### D2 — "Can I see it?" — expected results vary — **DEFERRED 2026-09-17**, with D1

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

### ~~E1 — Two weeks~~ — DISSOLVED 2026-09-17

**Not cut — recognised as the wrong shape.** "Use it for two weeks" was never a task anyone could
sit down and do; it is what happens now that the app opens on its own. Tracking it as an open chunk
implied someone had to schedule it.

**What it was protecting still matters**, and moves to where it belongs:

| Part | Where it went |
|---|---|
| *"Done when you reach for it without deciding to"* | The honest measure of whether this was worth building. Kept as the bar, not as a ticket |
| *"Tune the default prompt, ~20 rewrites"* | Already has a home: the prompt is a dimension in `bench`, and C1b established that a variant ships only if the eval set holds |
| *"Keep a scratch list of what annoyed you"* | **F2.** Every misread during real use becomes a case. That was always the valuable half |

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

### F1 — Build a personal bench harness — DONE 2026-09-16
**Did:** `crates/bench/`, reusing `odin-core`. Results: [F1-results.md](results/F1-results.md).

**Validated against the reference, not just "it runs":** prefill 1,214 ms against A2's 1,165, decode
131 t/s against 133, resident 8,187 MiB against 8,151.

**The port found a real ambiguity in "time to first token."** The first run read 5,537 ms against
A2's 1,165 — because `bench.mjs` paid the cold load *outside* its measurement by warming up on a
synthetic image, and this port timed from request start. Both are worth knowing, so the table now
reports **TTFT** (what the press feels like, load included) and **PREFILL** (load removed, the only
column comparable with A2).

**F1b, 2026-09-17:** answers now save to disk, blinded and shuffled per image, with a
`blind-key.json` to unblind after grading. Verification caught two real bugs: the blind shuffle
**was not shuffling** (three runs, identical assignment), and a run whose stream ended without
Ollama's metrics was **recorded as 0 tok/s** rather than refused. `bench --selftest-shuffle` guards
the first.

#### The original spec
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

### F2 — Grow the eval set — **FOLDED INTO THE TESTING ROUND 2026-09-17**

> **The trade, stated so it is a choice.** The original design was to capture each failure *the
> moment it happens in real use* — because that is the only time the screen still exists. Batched
> into a testing session instead, past failures are unrecoverable; what a session can do is put a
> variety of screens in front of the model deliberately and record what it gets wrong.
>
> That is real and it works. It is **less representative**, because the screens are chosen rather
> than encountered. Accepted knowingly: a session that happens beats a habit that does not.

### F2 — the original intent
**Do:** Every time the model misreads something during normal use, **save that screenshot into
the eval folder** with a note on what it should have said.

This is the highest-value habit in the whole project. Your E1 annoyance list becomes a test
suite that's specific to how you actually work — which is exactly what no public benchmark
gives you. Twenty real failures beats any leaderboard.

### ~~F3 — Re-run it when something new lands~~ — DISSOLVED 2026-09-17

**Not a task — a trigger.** It fires when a model ships, not when anyone schedules it, and `bench`
already makes it a ten-minute job. Tracking it as open implied someone had to decide to do it.

**The candidate list survives** (Gemma 3 first, then MiniCPM-V 4.5, InternVL, Qwen3-VL 30B) as the
thing to reach for when the trigger fires. See the table below.

### F3 — candidates, for when something new lands
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
