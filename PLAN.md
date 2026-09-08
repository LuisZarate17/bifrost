# Warden — Handoff Packet

> Press one key. Warden looks at your screen and answers immediately, in a corner popup that
> nobody you're screen sharing with can see. Entirely on your own machine.

**Status:** v1 fully scoped 2026-09-07. Not started.
**Repo root:** `C:\Users\you\source\repos\warden`

---

## 0. What this is

A **tool Luis uses daily**, not a portfolio piece. If a feature only pays off when a stranger
reads the repo, it doesn't belong.

Long term it becomes a general private AI with chat and terminal modes. **None of that is in
v1.** Screen mode gets used most, so it gets built first and alone.

Fixed:

- **A platform eventually, one tool right now.**
- **Summoned, never ambient.**
- **Local for privacy, cost, and ownership — all three. No cloud fallback, ever.**

---

## 1. v1, exactly

1. Press the hotkey.
2. Full screen (1920×1080, single monitor) is captured.
3. A **default prompt** goes with it — no typing.
4. The answer streams into a popup in a **fixed corner**.
5. A follow-up box asks again against the same capture.
6. `Esc` dismisses. Nothing is saved.

**Not in v1:** chat window, terminal mode, region select, SQLite, history, voice, files,
tool calling, installer.

---

## 2. The default prompt is the product

With no question to read, the default prompt does all the intent-guessing. Generic
"describe this screen" produces useless narration.

Screen content is **mostly code and terminals, with a bit of everything else**, so it should
lead with problem-detection without assuming a problem exists — roughly:

> *What am I looking at? If there's an error or something broken, explain it and the fix.
> Otherwise explain what matters here.*

**Most important tunable in v1, and it's a string.** Config file, not source. Expect twenty
rewrites in week one.

---

## 3. Environment (verified — do not re-derive)

| | |
|---|---|
| GPU | RTX 5070 Ti, **16 GB** VRAM, Blackwell, compute cap 12.0, driver 610.62 |
| CPU | Ryzen 7 9700X, 8c/16t · 31 GB RAM |
| Display | **One monitor, 1920×1080, 100% scaling** — no DPI handling needed at all |
| Machine | **Not domain-joined**, Windows Defender only — no endpoint security to fight |
| Installed | Docker Desktop 4.82, Node 24.18 |
| **Not** installed | Rust toolchain, Ollama, Python, CUDA toolkit |
| Author's Rust | **Read about it, never written it.** Plan accordingly. |
| Time pressure | None stated. Size phases by scope, never by weeks. |

---

## 4. Capture: use Windows Graphics Capture, not GDI

The popup must work over **anything**, including fullscreen apps. That decides the capture API:

- **Use WGC** (Windows.Graphics.Capture) via the [`windows-capture`](https://crates.io/crates/windows-capture) crate.
- **Do not use GDI / BitBlt.** It returns black frames on hardware-accelerated and fullscreen
  content — precisely the case that matters here.

### Overlaying fullscreen — what actually works

| Case | Works? |
|---|---|
| Normal windowed apps | Yes |
| Borderless-windowed games | Yes |
| Composited fullscreen (most Win11 apps, video, browsers) | Yes |
| **True DirectX exclusive fullscreen** | **No** |

Exclusive fullscreen can only be overlaid by injecting into the game's render pipeline — what
Steam and Discord overlays do. It breaks on every game update and anti-cheat flags it.
**Not worth building.** The fix is setting that game to borderless windowed; most default to it.

Note both halves fail together there: in exclusive fullscreen the capture returns a black frame
too, so there's no half-working state to debug.

Overlay window flags: `WS_EX_TOPMOST` + `WS_EX_NOACTIVATE`, layered, no taskbar entry.

---

## 5. The invisibility requirement

its own follow-up screenshot. One Win32 call does both:

```
```

**Honest limits:**
- Nothing against a phone camera or a hardware capture card on the display output.
- Varies by each sharing app's capture path — test the ones you actually use.
- Tauri doesn't expose it. Call Win32 via the `windows` crate on the raw HWND.
- Hides Warden from **your own** recordings too. Toggle off to film a demo.

---

## 6. Model

- **Q8, not Q4.** Screen content is mostly code and terminals, so reading small dense text
  accurately is the whole job. Vision models degrade under quantization harder than text
  models, and a misread stack trace is worse than a slow answer.
- Qwen2.5-VL / Qwen3-VL class, 7–8B. Check ollama.com for current tags.
- **Resident, `keep_alive: -1`.** With instant-answer there's no typing time to hide a cold
  start behind.
- A 1080p screen is a few hundred image tokens. Measure the real number at P0.

---

## 7. Roadmap

### P0 — Prove it, and warm up on Rust
Install Rust and Ollama, pull a vision model, feed it a screenshot from the CLI.
Since this is the first real Rust: write a throwaway CLI that screenshots and prints, just to
meet the compiler before the app matters.
**Done when** you've watched it read one of your real screenshots and know the seconds-to-answer.

### P1 — Build it, in checkpoints
Each is a place you can stop:

1. Tauri window that streams tokens from Ollama. *(Use `tauri::ipc::Channel` — see traps.)*
2. WGC capture to a PNG in memory, saved to disk so you can eyeball it.
3. Capture + default prompt → streamed answer, in the main window.
4. Global hotkey triggers the above.
5. Move the answer into a corner popup: topmost, no-activate, no focus steal.
7. Follow-up box against the same capture.
8. Tray icon, launch on login, keep-warm on start.


**Expect the borrow checker to bite in two places:** the lifetime of the capture frame buffer,
and moving streamed chunks across the async Tauri boundary. Both are normal; neither means the
design is wrong.

### P2 — Live with it
Tune the default prompt. Fix only what actually annoyed you.
**Done when** you reach for it without deciding to.

### After P2 — ordered by what you actually miss
history & persistence · region select · chat window · terminal mode (TUI,
[OpenClaw](https://github.com/openclaw/openclaw)-shaped) · files & notes · voice · acting via MCP

---

## 8. Known traps

- **The popup captures itself.** Without the exclusion flag, a follow-up screenshot contains
  Warden's own last answer and the model reads itself back.
- **GDI capture returns black** on fullscreen and hardware-accelerated content. Use WGC.
- **Ollama unloads the model** after five minutes. `keep_alive: -1` — and it matters more here
  than in a chat app, since nothing masks the cold start.
- **Tauri buffers streamed responses** through `proxy_localhost_stream` — fine in dev, broken in
  the packaged `.exe`. Use `tauri::ipc::Channel` or events.
- **The popup must not steal focus.** `WS_EX_NOACTIVATE`.
- **Hotkey registration can fail silently.** Fail loudly instead.
- **HDR** makes captures wash out, and the model misreads a screenshot that looked fine to you.
- **Blackwell:** driver 610.62 is fine with prebuilt binaries. Building llama.cpp from source
  later needs CUDA 12.8+ and the `sm_120` arch flag.
- *(No longer applicable: multi-monitor and DPI scaling — one 1080p display at 100%.)*

---

## 9. Still open — all tunables, none blocking

- Which hotkey?
- Which corner?
- Exact default prompt wording — a P2 job, not a P1 decision.
