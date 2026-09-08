# Warden

Press a hotkey, get an answer about whatever is on screen, from a model that never leaves the
machine.

**Status:** No application code yet. **A1 (toolchain) is done** — Rust 1.98.1 (MSVC), Ollama
0.33.3, GPU inference verified at 152 tok/s. See [docs/A1-results.md](docs/A1-results.md).
Next up: **A2 — the vision model bake-off**.

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
- [STAGE-A.md](STAGE-A.md) — the next execution packet (foundations, no GUI yet)

## Hard rules

- 100% local. No cloud API calls, ever, for any reason. If a local model is bad at something,
  the fix is a different local model, not a fallback.
- Summoned, never ambient — it only runs when the hotkey is pressed.
- This is a personal daily-use tool, not a portfolio project. Features only earn their keep if
  Luis actually uses them.
