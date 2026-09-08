# Warden — Build Plan

Companion to [PLAN.md](PLAN.md). Fifteen chunks in five stages. **Each chunk is one sitting
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

The benchmark gap between Qwen3-VL **4B (95.3 DocVQA)** and **8B (96.1)** is **under one point**,
while the VRAM difference is large — reports put 8B at Q4_K_M around 12 GB.

That inverts the usual advice. At a fixed 16 GB budget you're really choosing between
**a bigger model at lower precision** and **a smaller model at higher precision** — and for a
resident, always-warm tool where latency is the product, 4B at Q8 may well win.

**So A2 is a bake-off, not a download:**

| Candidate | Why it might win |
|---|---|
| Qwen3-VL **8B @ Q4_K_M** | More capable model; the benchmark leader |
| Qwen3-VL **4B @ Q8** | Higher precision where it matters for small text, far more headroom, faster TTFT |

Judge them on **your own screenshots of code and terminals**, not on benchmark numbers.
Also **cap the context well below the 256K default** — you're sending one screenshot, not a book,
and context is where the VRAM goes.

**Backup if both disappoint:** Gemma 3 — day-one Ollama integration, OCR across 32 languages,
and GUI grounding that identifies buttons and fields in screenshots.

---

## Stage A — Foundations (no app yet)

**A4 is already a useful tool**, in a terminal, before any GUI exists. If everything after this
stalls, you still have something.

### A1 — Toolchain
**Do:** Install `rustup` (MSVC toolchain) and Ollama. Pull any small text model and run it.
**Done when:** `ollama run <model>` answers you, and you've seen tokens/sec on the 5070 Ti.
**Watch for:** Rust on Windows needs MSVC build tools; rustup will prompt. Say yes.

### A2 — The bake-off
**Do:** Pull **Qwen3-VL-8B (Q4_K_M)** and **Qwen3-VL-4B (Q8)**. Screenshot your real editor and
a terminal with an error in it. Send both images to both models via `curl` against
`/api/generate` with a base64 image.
**Done when:** You've picked one, for a reason you can state.
**Record for each:** did it read the code correctly · time to first token · time to full answer ·
VRAM resident · image token count.
**Watch for:** If both misread code, the fix isn't more quantization — try Gemma 3 before you
compromise on accuracy. A misread stack trace is worse than a slow answer.

### A3 — First Rust: capture a PNG
**Do:** `cargo new` a CLI. Add [`windows-capture`](https://crates.io/crates/windows-capture).
Grab the full screen, write `out.png`.
**Done when:** `out.png` looks exactly like your screen.
**Watch for:** This is where cargo, `Result` and `?` stop feeling foreign. Take your time.

### A4 — First useful thing
**Do:** Extend A3 — base64 the PNG, POST to Ollama with the default prompt, print the answer.
**Done when:** One command gives you a real answer about your screen. **No GUI.**
**Watch for:** Ollama streams newline-delimited JSON, not one blob. Parse line by line with
`serde_json`.

---

## Stage B — The app shell

### B1 — Tauri opens a window
**Do:** Scaffold Tauri 2 + React + TypeScript. Nothing custom.
**Done when:** `cargo tauri dev` opens a window with your own text in it.

### B2 — Streaming into the UI
**Do:** Move A4's Ollama call into Rust. Stream tokens up with `tauri::ipc::Channel`.
**Done when:** Tokens appear word by word **in a built `.exe`**, not just in dev.
**Watch for:** The most important trap in this project. Do **not** route this through
`proxy_localhost_stream` — it buffers, works in dev, breaks silently when packaged.

### B3 — Button-triggered capture
**Do:** Move A3's capture into the app behind a button.
**Done when:** Click → screenshot → answer streams in.
**Watch for:** First borrow-checker fight — copy the bytes out of the capture frame before the
callback returns.

### B4 — Global hotkey
**Do:** `Ctrl+Alt+W` via Tauri 2's global-shortcut plugin.
**Done when:** It fires while a completely different app is focused.
**Watch for:** Registration fails silently if another app owns the combo. Log it, surface it.

---

## Stage C — Make it real

### C1 — The corner popup
**Do:** Second Tauri window — small, undecorated, **bottom-right**, always on top,
`WS_EX_NOACTIVATE`.
**Done when:** You hotkey from your editor and your cursor is still in the editor.
**Watch for:** Position against the *work area*, not the raw screen bounds, so it sits above the
taskbar. Windows toasts share that corner and will sometimes overlap — accepted, not a bug.

`windows` crate.

### C3 — Follow-ups
**Do:** A text box that asks again against the capture already in memory.
**Done when:** Second question, no new screenshot taken.

### C4 — Stop launching it
**Do:** Tray icon, launch on login, model warmed on start (`keep_alive: -1`), `Esc` dismisses.
**Done when:** You never open it on purpose.

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
**Do:** Use it. Tune the default prompt — expect ~20 rewrites. Keep a scratch list of what
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
**Do:** Extend A4's code to loop over a folder of screenshots × a list of Ollama model tags,
and print a table: model · did it get it right · time to first token · time to full answer ·
resident VRAM.

**Seed it with A2's artifacts** — keep those screenshots and timings, don't throw them away.

**Done when:** testing a new model is one command.

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
