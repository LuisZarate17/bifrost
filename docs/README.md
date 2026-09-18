# Navigator

**Everything in this repo, with a line on what it is.** Start here rather than searching — if you
know what you want but not where it lives, it is on this page.

## Start here

- **What does this key do?** → [shortcuts.md](shortcuts.md)
  Warden has no menu and no window chrome, so that file *is* the interface.
- **Where does the project stand?** → [BUILD.md](BUILD.md)
- **Why was this decided?** → [PLAN.md](PLAN.md)
- **How do I build and run it?** → [../README.md](../README.md)
  `npm run app`, never a plain `cargo build`.
- **What's next?** → [../stages/STAGE-G.md](../stages/STAGE-G.md)
- **Where is that function?** → [Where things live in code](#where-things-live-in-code)
- **What was actually measured?** → [Results](#results)

---

## Living docs

These change as the project does.

**[PLAN.md](PLAN.md)** — scope, the machine, decisions already closed, hard rules, roadmap.
Read it before re-deriving something, or when wondering whether a feature belongs at all.

**[BUILD.md](BUILD.md)** — the build plan, stages A–F, chunk by chunk, and what is left.

**[shortcuts.md](shortcuts.md)** — every control Warden has, and why each one is that key and
not another. The most-opened file here.

**[D-testing.md](D-testing.md)** — the deferred testing round, as a worksheet to fill in at the
machine. For when you have an afternoon.

**[PENDING-CHECKS.md](PENDING-CHECKS.md)** — things no tool call can verify, because they need a
keypress, a hover, or a second machine. Where "built but never actually pressed" is tracked.

## Execution packets

In [`../stages/`](../stages/), not here — they are inputs you paste into a fresh chat, rather than
records of what happened.

- **[STAGE-A.md](../stages/STAGE-A.md)** — foundations, no GUI. Complete.
- **[STAGE-B.md](../stages/STAGE-B.md)** — the app shell. Complete.
- **[STAGE-G.md](../stages/STAGE-G.md)** — the local chat app sharing Warden's model. Not started.

---

## Where things live in code

Four Rust crates and one React frontend. Line numbers drift; function names don't, so grep the
name if a number has gone stale.

### `warden-core/` — shared by everything

- **What Ollama believes is loaded** — `ollama.rs` → `ps()`, `find()`
- **Load or release a model** — `ollama.rs` → `load()`, `unload()`, via `keep_alive: -1` and `0`
- **Start Ollama if it is down** — `ollama.rs` → `is_running()`, `ensure_running()`
- **What the card is *actually* holding** — `vram.rs` → `Card::open()`, `sample()`
- **The default prompt** — `ollama.rs` → `DEFAULT_PROMPT`

> Everything here is **blocking by design**. Calling it from an `async` Tauri command drops a tokio
> runtime inside an async context and kills the worker. Go through `spawn_blocking`.

### `src-tauri/src/lib.rs` — Warden's Rust side

About 2,000 lines, and most of the app is in it.

- **Hotkey registration, and which keys exist** — `run()` (~1756), `popup_keys()` (~1384)
- **Summon and dismiss** — `toggle_popup()` (~1647)
- **Capture → Ollama → stream** — `ask_about_screen()` (~228)
- **Screen capture itself** — `capture_screen_blocking()` (~148)
  Must run in `spawn_blocking` — WebView2 puts the main thread in COM STA, and WGC needs MTA.
- **The invisibility feature** — `exclude_from_capture()` (~549)
  Re-applied on every show and read back by `report_affinity()` (~1307), never assumed.
- **Where the popup sits, and the pointer dodge** — `target_rect()` (~702)
- **The glide between positions** — `animate()` (~998)
- **Showing without stealing focus** — `show_without_focus()` (~846), `hide_now()` (~1202)
- **The one place focus *is* taken** — `take_focus()` (~1250), `return_focus()` (~1285)
  Only while the follow-up box is open, which is also the only moment `Esc` works.
- **`Ctrl+Alt`+wheel scrolling** — `mouse_hook()` (~1475), `watch_mouse()` (~1554)
  A low-level hook, installed on Tauri's main thread because that is the only loop that pumps.
- **Copying a block** — `copy_text()` (~1711)
  Written from Rust, because the webview is never focused.
- **How tall the popup should be** — `fit_window()` (~1935)
- **Where the logs go** — `redirect_log_to_file()` (~1732)
  The app is windowless, so this is the only way to see anything it printed.

### `src-tauri/src/tray.rs` — the tray

- **The menu, and Off / Warming / Ready** — `install()` (~324), `Power`
- **Warming exists only here** — `/api/ps` has no "loading" entry, so only a long-lived process
  that knows it issued a load can show it

### `src/` — Warden's React frontend

- **The popup's markup, header hints, pick-and-copy state** — `App.tsx`
- **Markdown rendering, and the security refusals** — `Answer.tsx`
  No raw HTML, `img` renders nothing, links render as plain text.
- **Syntax highlighting** — `CodeBlock.tsx`, Shiki, lazy, JS regex engine so no WASM
- **Proof the refusals hold** — `Answer.inert-check.tsx`, run by `npm run check:inert`

### `capture/` — the CLI

- **Capture, encode, stream, print** — `src/main.rs`
- **`warm` / `off` / `status`** — `src/power.rs`
- **The prompt, editable without a rebuild** — `prompt.txt`

### `bench/` and `evals/` — measuring models

- **Run model × image × prompt** — `bench/src/main.rs`
- **The grading rubric** — `evals/cases.md`
- **Cases that regenerate from tracked source** — `evals/fixtures/`
- **Saved runs** — `evals/out/`
- **The Node harness F1 was ported from** — `evals/bench.mjs`

> `evals/shots/` is gitignored on purpose. Screenshots of a real desktop can contain anything, so
> the eval set stays local.

---

## Results

Finished chunks: what was built, what was measured, what the measurement changed. Never rewritten
— a correction goes in the next one, with a note saying so.

**Stage A — foundations, no GUI**

- **[A1](results/A1-results.md)** — toolchain. Rust 1.98.1 MSVC, Ollama 0.33.3, GPU at 152 tok/s
- **[A2](results/A2-results.md)** — the model bake-off, full 2×2.
  **Size decides accuracy; quantization decides nothing.** Chose `qwen3-vl:8b-instruct-q4_K_M`.
- **[A3](results/A3-results.md)** — screen capture. Verified genuinely WGC, not GDI
- **[A4](results/A4-results.md)** — the whole loop. 2,077 tokens in, 1.19 s to first token

**Stage B — the app shell**

- **[B1](results/B1-results.md)** — Tauri 2 + React + TypeScript. A window opens
- **[B2](results/B2-results.md)** — streaming over a `Channel`.
  **Never return the answer as a command's `Result`** — it buffers the whole thing.
- **[B3](results/B3-results.md)** — capture behind a button, exclusion verified against Warden's
  own screenshot
- **[B4](results/B4-results.md)** — hotkey and corner popup. Translucency is CSS alpha, not a DWM
  backdrop

**Stage C — make it real**

- **[C1b](results/C1b-results.md)** — making the answer readable, and the injection cases that
  must stay inert
  and Teams
- **[C4a](results/C4a-results.md)** — power control in the CLI.
  **off −8,193 MiB · warm +7,873 MiB in 2.2 s** — the deltas match, which is the test.

**Stage F — keep testing models**

- **[F1](results/F1-results.md)** — the bench harness in Rust, validated against `bench.mjs`'s own
  numbers rather than just "it runs"

---

## Two conventions worth knowing before you change anything

**Nothing is measured against a constant.** Every VRAM figure here is a delta against a sample
taken moments earlier, because the idle baseline drifts — seen at 667, 900, 945, 985 and 1,033 MiB
inside a single session. Anything that hard-codes it is already broken.

**A successful bind is not proof a key works.** `RegisterHotKey` reports every `Ctrl+Alt+<letter>`
as free, because it only knows about other *global* registrations and never about what an
application binds internally. Only a real keypress proves it — which is what
[PENDING-CHECKS.md](PENDING-CHECKS.md) exists to track.
