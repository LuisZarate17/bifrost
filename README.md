# Bifrost

Local AI tools that share one model and one machine. Nothing leaves the computer on its own.

| | |
|---|---|
| **Mimir** | The main workspace: a local chat app that reads, finds and edits code in a folder you pick, runs the folder's check, and decides which model is loaded. **Built through Stage H** |
| **Hugin** | A coding agent in the terminal, in the spirit of Claude Code, on Mimir's engine. **Built** (I2–I5): full screen, reads, searches, edits, the folder's check and any shell command, each approved first; **Ctrl+Alt+H** opens it in the last folder, and `hugin` works in any terminal. **`hugin -p`** lets Claude Code hand it a task: it works in a throwaway copy of the folder and hands back an answer, a patch and the check's result — [docs/I-status.md](docs/I-status.md) |
| **`bifrost`** | Vidar's pipeline from the command line, plus `warm` / `off` / `status` power control |

Things with an icon get a name; things you type get a description (PLAN.md §0b). Bifrost is the
bridge between them.

**Status.** Stages A–C are complete — `Ctrl+Alt+W` anywhere captures the screen and streams an
answer into the popup. What is left of the original plan is one afternoon of testing, see
[docs/BUILD.md](docs/BUILD.md). **Stages G and H are built** — Mimir chats, edits files in a folder
you pick, finds code, runs the folder's check and undoes a whole turn; Stage G's checks by hand
still wait in [docs/G-testing.md](docs/G-testing.md). **Now: Stage I** — the rename is done, Mimir's turn loop
is now the shared engine's, and Hugin runs on it in the terminal; see [docs/I-status.md](docs/I-status.md).

## The apps

```
npm run app:vidar              # build Vidar
npm run app:mimir              # build Mimir
target/debug/vidar.exe         # or the Desktop / Start Menu shortcuts
target/debug/mimir.exe
cargo build -p hugin           # Hugin is a plain console app: cargo is right for it
node tools/shortcuts.mjs       # shortcuts for all three; Hugin to %LOCALAPPDATA%ifrostin and the PATH
hugin                          # in any terminal, in the folder you are in — or Ctrl+Alt+H
```

**Build the apps with `npm run`, not `cargo build`.** Cargo alone bakes `devUrl` into the binary,
so the window loads from `localhost:1420` and shows WebView2's "can't reach this" page unless a Vite
dev server happens to be running. `npm run app:vidar` is `tauri build --debug --no-bundle`: same debug
profile, but the frontend is embedded, so the app opens and works on its own. A bare `cargo build`
at the root now skips both apps for exactly this reason (`default-members` in `Cargo.toml`) —
`cargo build -p mimir` would still do it, so don't.

Release builds are refused by Smart App Control on this machine; debug builds usually run, and when
one is blocked, rebuilding for a fresh hash clears it. See [docs/PLAN.md](docs/PLAN.md) §3.

**Every control Vidar has is in [docs/shortcuts.md](docs/shortcuts.md)** — there is no menu and no
window chrome, so that file is the interface.

## The CLI

The same pipeline without the GUI in the way. It is also ahead of the app on controls: a capture
delay and a model flag for one run.

The binary is `bifrost`, the project's own name — a name you type rather than one that carries an
icon (PLAN.md §0b).

```
cargo build                                       # builds the crates (not the apps) into target/
target/debug/bifrost                              # answer about your screen, now
target/debug/bifrost 12                           # wait 12s first
target/debug/bifrost --model qwen3-vl:4b-instruct-q8_0

target/debug/bifrost warm                         # load the model and pin it in VRAM
target/debug/bifrost off                          # release it, give the card its memory back
target/debug/bifrost status                       # what is loaded, per Ollama and per the card
```

The model, the prompt and `num_ctx` come from `%APPDATA%\bifrost\config.json`, the file every app
shares; with no file, the built-in defaults apply. Ollama is started automatically if it is not
already running.

## Where your data lives

Settings, conversations, attachments and undo records are in `%APPDATA%\bifrost`. Until the rename
on 2026-10-12 they were in `%APPDATA%\odin`: the first app or CLI run after the rename moves that
folder once, whole, and leaves a `MOVED.txt` behind saying where it went. If `bifrost` already
exists while `odin` still holds data, nothing is moved, `bifrost` is used, and the apps log a warning.

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
bifrost/
├── apps/
│   ├── vidar/          the overlay — hotkey, popup, tray (src-tauri/ + its React frontend)
│   ├── mimir/          the chat app — conversations, models, reading and editing files
│   └── hugin/          the terminal agent — inline in the terminal, on the same turn loop
├── packages/
│   └── ui/             @bifrost/ui — the Markdown renderer both apps share
├── crates/
│   ├── bifrost-core/   shared — Ollama, VRAM readout, config, chat, tools, safe file edits
│   ├── bifrost-cli/    the CLI, binary `bifrost`
│   └── bench/          benchmark harness, plus the tool-call and file-edit probes
├── evals/              rubric, fixtures, saved runs
├── tools/              shortcuts, icon, the end-to-end tests that drive Mimir, the practice folder
├── docs/               plan, build log, shortcuts, results  →  docs/README.md
└── stages/             execution packets — paste one into a fresh chat
```

*(Restructured at Before-G0 step 1 of Stage G, renamed at I0 of Stage I. The project is
**Bifrost** (was ODIN); **Vidar** is the overlay (was Heimdall) and **Mimir** the chat app (was
Mímir). `stages/` and `docs/results/` keep the names they were written with — PLAN.md §0b.)*

## Docs

**[docs/README.md](docs/README.md) is the index** — it says what each document is and when to reach
for it. The four you will actually open:

| | |
|---|---|
| [docs/shortcuts.md](docs/shortcuts.md) | Every control. This is the interface |
| [docs/PLAN.md](docs/PLAN.md) | Scope, the machine, decisions already closed, hard rules |
| [docs/BUILD.md](docs/BUILD.md) | The build plan, chunk by chunk, and what is left |
| [stages/STAGE-I.md](stages/STAGE-I.md) | The next thing — one engine, and Hugin in the terminal |

## Stack

Rust, Tauri 2, React + TypeScript, Ollama, Windows Graphics Capture.

## Hard rules

- Local. Bifrost never calls the cloud on its own, and if a local model is bad at something, the
  fix is a different local model, not a fallback. **One exception, on purpose (2026-10-12):** Stage
  I's *Check with Claude* button in Mimir — off by default, only on a click, and it says what it
  sends before it sends it. Built in I6. (Claude Code calling `hugin -p` is Claude's side
  reaching in: Hugin itself still calls nothing.)
- Summoned, never ambient — it only runs when the hotkey is pressed.
- This is a personal daily-use tool, not a portfolio project. Features only earn their keep if
  Luis actually uses them.
