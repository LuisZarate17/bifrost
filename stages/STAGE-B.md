# Stage B — Execution Packet

**Paste this into a fresh chat.** Self-contained; it doesn't assume the earlier conversations.

---

## Context

I'm building **Warden**: a private, fully local AI that screenshots my screen when I press a

**Stage A is done.** The CLI at `crates/odin-cli/` already captures the screen, sends it to a local
vision model, and streams a real answer to the terminal. Stage B wraps that in an app.

**About me:** Stage A was my first Rust ever. Explain what you're doing rather than just handing
me code. I want to actually learn this.

**Hard rule:** everything runs locally. No cloud API calls, ever, for any reason. If a local model
is bad at something the answer is a different local model, not a fallback.

---

## Machine (verified — don't re-check)

- RTX 5070 Ti, 16 GB VRAM, Blackwell (sm_120), driver 610.62
- Ryzen 7 9700X, 31 GB RAM
- **One monitor, 1920×1080, 100% scaling** — no DPI handling needed
- Windows 11 Pro build 26200
- Rust 1.98.1 MSVC · MSVC Build Tools 2022 (17.14) + Windows SDK 10.0.26100
- Node 24.18 · npm 11.16 · WebView2 Runtime installed
- Ollama 0.33.3 · model `qwen3-vl:8b-instruct-q4_K_M`
- Repo root: `C:\Users\you\source\repos\warden`

---

## What already exists

```
warden/
├── crates/odin-cli/  Stage A CLI — screen → PNG → base64 → Ollama → streamed answer
│   ├── prompt.txt    the default prompt, NOT in source (PLAN.md §2)
│   └── src/main.rs
├── src/              React + TypeScript frontend (B1)
├── src-tauri/        the Tauri Rust crate (B1)
├── evals/            bench.mjs harness, cases.md rubric, fixtures/
└── docs/             A1–A4 + B1 results
```

The CLI stays working throughout Stage B. Its logic migrates into `src-tauri/` at B2 and B3,
but keep the CLI — it's the way to test the pipeline without the GUI in the way.

---

## Hard-won facts that carry into Stage B

**The Ollama contract** (measured across 40+ requests at A2, re-confirmed at A4):
- `images` is **top-level**; `num_ctx` goes **inside `options`**. Either wrong fails silently.
- Raw base64, **no `data:` prefix**.
- **`num_ctx: 8192` must be set explicitly.** Ollama derives a default from VRAM — 4k on this card
  — and a 1080p screen costs **2,042 image tokens**, so the default silently truncates.
- Response is NDJSON. Metrics only on the final object where `done: true`, durations in **ns**.
- Sanity check: `prompt_eval_count` should be ~**2,077** for a 1080p screen. Much lower means
  `num_ctx` didn't take.
- Expect **1.2 s to first token**, ~9 s total.

**`reqwest` must keep `native-tls`** or the Windows build demands NASM — 0.13 switched its default
TLS backend to `aws-lc-rs`. Two variants are in use:
```toml
# crates/odin-cli/ — synchronous CLI
reqwest = { version = "0.13", default-features = false, features = ["blocking", "json", "native-tls"] }
# src-tauri/ — async, streams the body
reqwest = { version = "0.13", default-features = false, features = ["native-tls", "json", "stream"] }
```
The `stream` feature is what provides `bytes_stream()`. Going async drops `blocking` and adds
`stream`; the TLS choice never changes.

**Smart App Control is ON.** It can block `rustc` from loading unsigned proc-macro DLLs. Symptom:
`error[E0463]: can't find crate for <x>_derive` while the DLL is present and correctly sized.
**Not stale cargo state.** Confirm against `Microsoft-Windows-CodeIntegrity/Operational`
(events 3077/3033), then `cargo clean -p <crate> -p <crate>-derive` and rebuild. Seen once at A4;
did **not** recur across Tauri's 362-unit build at B1.

**Ollama has no autostart entry.** It won't survive a reboot — start it with `ollama.exe serve`
detached, never `ollama app.exe` (that opens a blocking first-run GUI). **C5 was dissolved** — this now belongs to the `capture warm`/`off`/`status` subcommands and C4's tray.

---

## B1 — Tauri opens a window — DONE 2026-09-12

Tauri 2.11.5 + React + TypeScript at the repo root. Window titled **Warden**, identifier
`com.warden.app`. First build 55 s. See [docs/B1-results.md](../docs/results/B1-results.md).

> **If ever re-scaffolding: never run `create-tauri-app` at the repo root.** Passing `.` and
> accepting the overwrite prompt **recursively deletes everything except `.git`** — verified in
> its source. `evals/shots/` is gitignored and would not come back. Scaffold into a sibling temp
> directory and move files in by hand.

---

## B2 — Streaming into the UI — DONE 2026-09-12

> Async command + `tauri::ipc::Channel`. Streams in dev and in the packaged `.exe`; the Rust-side
> and UI-side TTFT match exactly, so nothing batches in between.
> See [docs/B2-results.md](../docs/results/B2-results.md).


**Do:** Move A4's Ollama call into `src-tauri`. Stream tokens up to React with
`tauri::ipc::Channel`.

**Done when:** tokens appear word by word **in a built `.exe`**, not just in `dev`.

**Watch for — the trap this chunk exists to avoid.** **Never return the answer as the command's
`Result`.** Tauri sends exactly one IPC reply per `invoke()`, so a command whose return value
carries the payload buffers the entire thing. `Result<(), String>` should carry only success or
failure; every token goes through the `Channel`.

*(An earlier version of this packet blamed `proxy_localhost_stream` and claimed it breaks only
when packaged. That symbol does not exist in Tauri, and `Result` buffering is identical in dev and
release — see docs/B2-results.md.)*

A4 built the reflex for this deliberately. Reading the response with `.text()` gave **14.5 s of
silence then a dump**; incremental reads gave first text at **1.5 s**. Both compiled, both
produced identical output. **The tell is that it arrived all at once.**

Also from A4: `stdout().flush()` mattered on the CLI side because stdout is line-buffered. The
equivalent question here is whether anything between Rust and the webview batches.

---

## B3 — Button-triggered capture — DONE 2026-09-13

> Capture runs inside `spawn_blocking` — mandatory, because WebView2 puts Tauri's main thread in
> to exclude from WGC. See [docs/B3-results.md](../docs/results/B3-results.md).


**Do:** Move A3's capture into the app behind a button. Click → screenshot → answer streams in.

**Done when:** clicking works end to end.

**Watch for:** the borrow-checker fight, already met at A3. `Frame` and `FrameBuffer` are
`!Send`/`!Sync`, and their buffer is unmapped when the callback returns — so copy the pixels out
with `.to_vec()` before they can leave. `crates/odin-cli/src/main.rs` already does exactly this;
reuse the
shape rather than rediscovering it.

---

## B4 — Global hotkey — DONE 2026-09-14 (with C1)

> Hotkey, corner popup, translucency and taskbar-following all work. Translucency is CSS alpha,
> **not** a DWM backdrop � acrylic cannot go dark, mica cannot see through.
> **Then Smart App Control blocked `warden.exe`** � see PLAN.md §3.
> Full write-up: [docs/B4-results.md](../docs/results/B4-results.md).


**Do:** `Ctrl+Alt+W` via Tauri 2's global-shortcut plugin. Fallback `Ctrl+Alt+Space`. **Never**
`Alt+Space` — that's the Windows system menu.

**Done when:** it fires while a completely different app is focused.

**Watch for:** registration **fails silently** if another app already owns the combo. It doesn't
error, it just never fires. Fail loudly on purpose.

---

## Done with Stage B when

Pressing `Ctrl+Alt+W` anywhere captures the screen and streams an answer into the app window.


**Don't style anything yet either.** The window still shows the Tauri starter template on purpose
— it's all replaced at C1/C1b when it becomes a corner popup.
