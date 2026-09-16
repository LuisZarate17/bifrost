# Warden

Press a hotkey, get an answer about whatever is on screen, from a model that never leaves the
machine.

**Status:** **Stage A is complete.** One terminal command captures the screen and streams a real
answer about it. No GUI yet — that's Stage B.

```
cd capture
cargo run                                       # answer about your screen, now
cargo run -- 12                                 # wait 12s first
cargo run -- --model qwen3-vl:4b-instruct-q8_0  # try a different model

cargo run -- warm                               # load the model and pin it in VRAM
cargo run -- off                                # release it, give the card its memory back
cargo run -- status                             # what is loaded, per Ollama and per the card
```

Needs `ollama serve` running. The prompt lives in `capture/prompt.txt`, not in source.

Every control the app has is in **[docs/shortcuts.md](docs/shortcuts.md)** — there is no menu, tray
item or window chrome, so that file is the interface.

The model holds **~8 GB** while resident, so `warm` and `off` are explicit rather than on a timer —
a timeout decides for you and leaves you guessing whether the next press is instant or a stall.
`status` reads the card through NVML, not `ollama ps`: the two disagree, and only one of them is
the truth. After a `warm`, a capture reports `load 0.00s`, which is how you know the warm took.

- **A1 — toolchain.** Rust 1.98.1 (MSVC), Ollama 0.33.3, GPU verified at 152 tok/s.
  See [docs/A1-results.md](docs/A1-results.md).
- **A2 — model bake-off.** Ran the full 2×2 (4B/8B × Q4_K_M/Q8_0) on real screenshots of this
  repo's own code. **Chose `qwen3-vl:8b-instruct-q4_K_M`** — the only candidate that correctly
  concluded a broken file wouldn't compile. Both 4B models called it fine, and higher precision
  bought nothing. See [docs/A2-results.md](docs/A2-results.md).
- **A3 — screen capture.** `capture/` grabs the primary monitor via Windows Graphics Capture and
  writes a 1920×1080 PNG. Verified against fullscreen hardware-decoded video — no black frame,
  so it is genuinely WGC and not GDI. See [docs/A3-results.md](docs/A3-results.md).
- **A4 — the whole loop.** PNG encoded in memory, base64, POSTed to Ollama, answer streamed
  token by token. Cross-validates A2 exactly: 2,077 tokens in, 1.19 s to first token.
  See [docs/A4-results.md](docs/A4-results.md).
- **B1 — the app shell.** Tauri 2 + React + TypeScript. A window titled Warden opens; `capture/`
  still works untouched. See [docs/B1-results.md](docs/B1-results.md).
- **B2 — streaming into the UI.** Tokens reach React through a Tauri `Channel` as they arrive,
  in dev and in the packaged 4.8 MB `.exe`. See [docs/B2-results.md](docs/B2-results.md).

- **B3 — capture behind a button.** Click and Warden screenshots your screen and streams an answer
  screenshot. See [docs/B3-results.md](docs/B3-results.md).

- **B4 + C1 — hotkey and corner popup.** `Ctrl+Alt+W` summons a translucent panel bottom-right
  that streams an answer and never steals focus. See [docs/B4-results.md](docs/B4-results.md).

> **Blocked here.** Smart App Control now refuses to run `warden.exe` - content-based, retroactive,
> and with no per-app exception. This stops the rest of Stage C, all of Stage D, and E1.
> **`capture/` is unaffected and still works.** See [PLAN.md §3](PLAN.md) for the analysis and the
> options � code signing is the only route that keeps Smart App Control on.

## How it works

A global hotkey triggers a full-screen capture via Windows Graphics Capture, which gets sent
with a default prompt to a local vision model running in Ollama. The answer streams into a
small popup pinned to the bottom-right corner. That popup is excluded from screen/window
own follow-up screenshot.

## Stack

Rust, Tauri 2, React + TypeScript, Ollama, Windows Graphics Capture.

## Docs

- [PLAN.md](PLAN.md) — scope, environment, decisions closed, hard rules
- [BUILD.md](BUILD.md) — the full build plan, stages A–F
- [STAGE-A.md](STAGE-A.md) — Stage A execution packet (foundations, no GUI) — complete
- [STAGE-B.md](STAGE-B.md) — Stage B execution packet (the app shell) — in progress

## Hard rules

- 100% local. No cloud API calls, ever, for any reason. If a local model is bad at something,
  the fix is a different local model, not a fallback.
- Summoned, never ambient — it only runs when the hotkey is pressed.
- This is a personal daily-use tool, not a portfolio project. Features only earn their keep if
  Luis actually uses them.
