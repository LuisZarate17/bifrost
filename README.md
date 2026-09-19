# ODIN — On-Device INference

Local AI tools that share one model and one machine. Nothing leaves the computer, ever.

| | |
|---|---|
| **`odin`** | The same pipeline from the command line, plus `warm` / `off` / `status` power control |
| **Citadel** | A local chat app sharing Warden's model and deciding which one is loaded. **Being built** — [stages/STAGE-G.md](stages/STAGE-G.md) |

Things with an icon get a name; things you type get a description (PLAN.md §0b).

**Status.** Stages A–C are complete — `Ctrl+Alt+W` anywhere captures the screen and streams an
answer into the popup. What is left of the original plan is one afternoon of testing, see
[docs/BUILD.md](docs/BUILD.md). Stage G's groundwork is in progress.

## The app

```
npm run app                    # build it
target/debug/warden.exe        # or the Desktop / Start Menu shortcut
```

**Use `npm run app`, not `cargo build`.** A plain `cargo build` bakes `devUrl` into the binary, so
the window loads from `localhost:1420` and shows WebView2's "can't reach this" page unless a Vite
dev server happens to be running. `npm run app` is `tauri build --debug --no-bundle`: same debug
profile, but the frontend is embedded, so the app opens and works on its own.

Release builds are refused by Smart App Control on this machine; debug builds usually run, and when
one is blocked, rebuilding for a fresh hash clears it. See [docs/PLAN.md](docs/PLAN.md) §3.

**Every control Warden has is in [docs/shortcuts.md](docs/shortcuts.md)** — there is no menu and no
window chrome, so that file is the interface.

## The CLI

The same pipeline without the GUI in the way. It is also ahead of the app on controls: a capture
delay, a model flag, and a prompt you can edit without rebuilding.

The binary is `odin` — short and guessable, because it is a name you type rather than one that
carries an icon (PLAN.md §0b).

```
cargo build                                    # builds every crate into target/
target/debug/odin                              # answer about your screen, now
target/debug/odin 12                           # wait 12s first
target/debug/odin --model qwen3-vl:4b-instruct-q8_0

target/debug/odin warm                         # load the model and pin it in VRAM
target/debug/odin off                          # release it, give the card its memory back
target/debug/odin status                       # what is loaded, per Ollama and per the card
```

The prompt lives in `crates/odin-cli/prompt.txt`, not in source. Ollama is started automatically
if it is not already running.

## How it works

A global hotkey triggers a full-screen capture via Windows Graphics Capture, which gets sent with a
default prompt to a local vision model running in Ollama. The answer streams into a small popup
pinned to the bottom-right corner. That popup is excluded from screen and window capture
follow-up screenshot.

The model holds **~8 GB** while resident, so `warm` and `off` are explicit rather than on a timer —
a timeout decides for you and leaves you guessing whether the next press is instant or a stall.
Power state reads the card through NVML, not `ollama ps`: the two disagree by ~2,500 MiB, and only
one of them is the truth.

## Layout

One Cargo workspace and one npm workspace, so there is a single `target/`, a single lockfile of
each kind, and a single `node_modules`.

```
odin/
├── apps/
│   └── warden/        the overlay — hotkey, popup, tray (src-tauri/ + its React frontend)
├── packages/
│   └── ui/            @odin/ui — the Markdown renderer both apps share
├── crates/
│   ├── odin-core/     shared — Ollama power control, NVML VRAM readout
│   ├── odin-cli/      the CLI, binary `odin`
│   └── bench/         model × image × prompt benchmark harness
├── evals/             rubric, fixtures, saved runs
├── docs/              plan, build log, shortcuts, results  →  docs/README.md
└── stages/            execution packets — paste one into a fresh chat
```

*(Restructured at Before-G0 step 1 of Stage G. The project is **ODIN**; **Warden** is the overlay
and **Citadel** will be the chat app — PLAN.md §0b.)*

## Docs

**[docs/README.md](docs/README.md) is the index** — it says what each document is and when to reach
for it. The four you will actually open:

| | |
|---|---|
| [docs/shortcuts.md](docs/shortcuts.md) | Every control. This is the interface |
| [docs/PLAN.md](docs/PLAN.md) | Scope, the machine, decisions already closed, hard rules |
| [docs/BUILD.md](docs/BUILD.md) | The build plan, chunk by chunk, and what is left |
| [stages/STAGE-G.md](stages/STAGE-G.md) | The next thing — the local chat app |

## Stack

Rust, Tauri 2, React + TypeScript, Ollama, Windows Graphics Capture.

## Hard rules

- 100% local. No cloud API calls, ever, for any reason. If a local model is bad at something,
  the fix is a different local model, not a fallback.
- Summoned, never ambient — it only runs when the hotkey is pressed.
- This is a personal daily-use tool, not a portfolio project. Features only earn their keep if
  Luis actually uses them.
