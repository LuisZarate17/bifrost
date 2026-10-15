# Bifrost — Handoff Packet

> Press one key. **Warden** looks at your screen and answers immediately, in a corner popup that
> nobody you're screen sharing with can see. Entirely on your own machine.

**Status:** **Stages A–C are COMPLETE** and Warden is finished and in daily use — the hotkey
captures the screen and streams an answer into the popup. Model chosen at A2:
`qwen3-vl:8b-instruct-q4_K_M`. What is left of the original plan is one deferred testing round
(BUILD.md D1/D2), not a feature.

**Stage H is complete and pushed** — Mimir finds code, checks its work and runs longer.
The independent review's two rounds of fixes are done, and re-checked 2026-10-11; see [`H-status.md`](H-status.md)
and [`../stages/STAGE-H.md`](../stages/STAGE-H.md).

**Now: Stage I, set 2026-10-12.** I0, the rename, is done: ODIN is now **Bifrost**, Heimdall
**Vidar**, and Mímir **Mimir** (§0b). The colors and badges (I0b) are done, and so is I1: Mimir's
turn loop now lives in the shared crate, where the probes run it too. Next is Gate 1, then **Hugin**,
a terminal agent on that loop. See
[`../stages/STAGE-I.md`](../stages/STAGE-I.md) and [`I-status.md`](I-status.md). The checks by
hand for Stage G still wait in [`PENDING-CHECKS.md`](PENDING-CHECKS.md).

**Repo root:** `C:\Users\you\source\repos\odin` *(renamed from `warden` on 2026-09-25, the same
day the GitHub repo became `LuisZarate17/odin`)*

> **Names, since this document predates them.** **Bifrost** is the project (it was **ODIN** until
> 2026-10-12); **Warden** is the overlay this packet was originally written about, now **Vidar**;
> **Citadel** is the chat app, now **Mimir**. Settled in §0b. Where the text below says "Warden" it
> means the overlay unless it says otherwise.

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
  *Amended once, on purpose, 2026-10-12:* Bifrost never calls the cloud on its own. The single
  exception is Stage I's **Check with Claude** button in Mimir (I6) — off by default, only on a
  click, and it says what it sends before it sends it. Claude Code calling `hugin -p` is Claude's
  side reaching in; Hugin itself stays local. See [`../stages/STAGE-I.md`](../stages/STAGE-I.md).

---

## 0b. Names — settled 2026-09-18, renamed 2026-09-26 and 2026-10-12

**Bifrost** — the bridge between the realms. The project, and the command you type.

| Piece | Name | Why |
|---|---|---|
| The project | **Bifrost** (was ODIN) | The bridge connecting the realms: the shared system that joins Vidar, Mimir and Hugin into one |
| The overlay | **Vidar** (was Heimdall, first Warden) | A god of strength, resilience and silence: the quiet, always-available assistant that observes the screen and helps when called upon |
| The chat app | **Mimir** (was Mímir, first Citadel) | Wisdom and knowledge: the main workspace, where the deeper thinking, conversation, coding and co-work happen |
| The terminal agent | **Hugin** (planned, Stage I) | One of Odin's ravens, associated with thought, who travels the world bringing information back: an agent that works through the terminal |
| The CLI | `bifrost` | |
| The harness | `bench` | |
| The shared crate | `bifrost-core` | |

**Renamed 2026-10-12, Stage I's I0: ODIN → Bifrost, Heimdall → Vidar, Mímir → Mimir.** The names
are Luis's own set, chosen for how they feel rather than to avoid a clash, and Mimir drops the
accent. The rename is full again: folders (`apps/vidar`, `crates/bifrost-core`,
`crates/bifrost-cli`), packages (`@bifrost/ui`), exe names (`vidar.exe`, `bifrost.exe`), app
identifiers (`com.bifrost.vidar`, `com.bifrost.mimir`), window titles, the tray, shortcuts, event
names (`vidar://…`), the `VIDAR_*` switches and `BIFROST_NVML_PATH`. The data folder moved from
`%APPDATA%\odin` to `%APPDATA%\bifrost`, once and whole, by whichever binary started first. History
keeps the names it was written with: `stages/`, `docs/results/`, the H review, `evals/out/` and
BUILD.md. So do the pinned benchmarks, which run on commits from before the rename.

**Colors and badges, 2026-10-14 (I0b).**
- **The colors.** Each app has a color with one value for dark grounds and one for light, kept in
  `packages/ui/src/colors.json`, which the icons, both apps and the Rust side read:
  - **Vidar, Wolf's Bane:** #e0525e / #a8273a.
  - **Mimir, Deep Well:** #5b9fe0 / #1f5f9e.
  - **Hugin, Raven Violet:** #8b7cf8 / #5a47d1.
- **The badges.** Each app's icon is a round shield in its color with one painted pattern: Vidar's
  quarters, Mimir's ring, Hugin's diagonal half. The Bifrost mark cuts a third from each shield
  and sets the three pieces around one boss.
- **The outline.** On light backgrounds every badge has a black outline.

**Renamed 2026-09-26: Warden → Heimdall, Citadel → Mímir.** The icons became the Helm of Awe,
split between the two apps as two badges that complete one mark, and the fortress names no longer
matched a Norse umbrella and a Norse logo. The rename is full: folders (`apps/heimdall`,
`apps/mimir`), packages, exe names, app identifiers (`com.odin.heimdall`, `com.odin.mimir`), window
titles, the tray, shortcuts, event names and the `HEIMDALL_*` environment switches. **The rest of
this plan, `stages/` and `docs/results/` keep the names they were written with** — they record what
happened, when it was Warden and Citadel. The paragraphs below are the 09-18 reasoning, unedited.

**The rule: things with an icon get a name, things you type get a description.**

Warden and Citadel are launched from the Desktop and live in the tray or the taskbar, so they carry
the theme — it costs nothing and it makes them memorable. `odin` and `bench` are typed, and a typed
name should be short, honest and guessable. `odin warm`, `odin status`, the same shape as `git` and
`cargo`.

That rule is also why **`capture/` was renamed and `bench/` was not.** `capture` describes one of
its four subcommands and had been wrong since C4a added `warm`, `off` and `status`; `bench`
describes all of itself. A themed name would have made the CLI worse — `courier warm` reads no
better than `capture warm`.

**Warden keeping its name is load-bearing.** `warden.exe`, `com.warden.app`, the tray tooltip, the
Desktop shortcut and all of `shortcuts.md` stay correct. Only the umbrella moves.

---

## 1. v1, exactly

1. Press the hotkey.
2. Full screen (1920×1080, single monitor) is captured.
3. A **default prompt** goes with it — no typing.
4. The answer streams into a popup in a **fixed corner**.
5. A follow-up box asks again against the same capture.
6. The hotkey dismisses it. Nothing is saved. *(**Revised at B4:** this said `Esc`. A `focusable: false` window never receives key events, so Esc cannot reach it. C3 owns the focus transition once follow-up typing exists.)*

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

### The prompt gets tested, not just tweaked

Because it does all the intent-guessing, **the prompt deserves the same treatment as the model
— measured against a fixed set of screenshots, not adjusted by feel.** Two reasons this matters
more than it looks:

- A prompt change that fixes the screenshot in front of you often quietly breaks a case you
  fixed last week. Without a fixed set to re-run, you won't notice.
- Prompt and model interact. A prompt tuned for one model can be the wrong prompt for its
  replacement, so a model swap means re-testing prompts too — not assuming the old one carries.

So the bench harness (BUILD.md F1) varies **model × prompt × screenshot**, and a prompt change
ships only if it doesn't regress the set. Keep the last few variants in the config with a note
on what each was trying to fix — the failed ones are as informative as the winner.

---

## 3. Environment (verified — do not re-derive)

| | |
|---|---|
| GPU | RTX 5070 Ti, **16 GB** VRAM, Blackwell, compute cap 12.0, driver 610.62 |
| CPU | Ryzen 7 9700X, 8c/16t · 31 GB RAM |
| Display | **One monitor, 1920×1080, 100% scaling** — no DPI handling needed at all |
| Machine | **Not domain-joined**, Windows Defender only — but see the Smart App Control note below |
| Installed | Docker Desktop 4.82, Node 24.18 |
| **Not** installed | CUDA toolkit. *(Rust 1.98.1 and Ollama 0.33.3 installed at A1.)* |
| Python | **Installed, via `uv`** — corrected 2026-09-19, this row used to say it was absent. Four interpreters in `%APPDATA%\uv\python\` (3.13.14, 3.14.6). Reach them with `uv run python`: plain `python` on PATH is a Microsoft Store **alias stub**, the `python3.14.exe` shim is SAC-blocked, and uv writes no `PythonCore` registry key — so every conventional check reports "not installed" |
| Author's Rust | **Read about it, never written it** *(true when written, 2026-09-07)*. Stages A–C have since been built in it, so the chunks no longer need to assume a first-timer — but explanation is still wanted over bare code. |
| Time pressure | None stated. Size phases by scope, never by weeks. |

### Smart App Control blocks Rust proc-macros — found at A4, 2026-09-12

This section used to say *"no endpoint security to fight."* **That was wrong.**

**Smart App Control is ON** (`SmartAppControlState: On`). It blocks unsigned binaries by
reputation — and **every proc-macro crate is an unsigned DLL that `rustc` must load at compile
time.** A build failed with `error[E0463]: can't find crate for zerovec_derive` while the DLL sat
on disk, intact, 1.5 MB. The real cause, from `Microsoft-Windows-CodeIntegrity/Operational`:

```
Event 3077: Code Integrity determined that a process (…\rustc.exe) attempted to load
…\zerovec_derive-….dll that did not meet the Enterprise signing level requirements
```

**Symptom to recognise:** a dependency fails with "can't find crate" for a **proc-macro** whose
DLL is present and correctly sized. Not stale cargo state — a load block.

**Workaround:** `cargo clean -p <crate> -p <crate>-derive` and rebuild. The new DLL has a new hash
SAC has not evaluated, so it loads. **This recurs** on new crates, fresh clones, and after any
`cargo clean`.

**Not fixed by choice.** Rebuilding costs seconds, so the proc-macro case stays a nuisance rather
than a blocker.

### It escalated at B4 — SAC now blocks `warden.exe` itself (2026-09-14)

> **Retracted 2026-09-15. This was never a project-level blocker.** The line here used to read
> *"This is a project-level blocker, not a build nuisance. Pinned, not solved."* That was wrong,
> and wrong in the costly direction — it discouraged the one-command test that disproved it.
>
> **The debug build runs** with SAC enforced. C2 was verified against Discord, Zoom and Teams
> using it. Only *release* builds are refused every time. When a debug binary is blocked, a
> rebuild for a fresh hash clears it — seen repeatedly during the Stage G restructure, where
> relinking produced a genuinely different binary from identical source.
>
> **So signing is a shipping problem, not a building problem.** It buys a binary that runs on a
> machine other than this one. Six chunks were listed as blocked on it and none of them were.
> The account below is kept because the measurements in it are real.

Smart App Control now refuses to run the built application:

```
Event 3077: … attempted to load …\target\debug\warden.exe that did not meet
the Enterprise signing level requirements   (Policy {0283ac0f-…})
```

**What was established by testing, not assumed:**

- The block follows the file's **content**, not its name or path — copying the exe to `%TEMP%`
  under a random name is blocked identically.
- It is **retroactive**: the release build from 2026-09-12 ran fine at the time and is blocked now.
  SAC's cloud verdict is per-hash and re-evaluated on Microsoft's schedule, not ours.
- **`capture.exe` still runs.** Same toolchain, same machine. Only the GUI binary is blocked —
  plausibly because it captures the screen, hides itself from capture, registers a global hotkey
  and makes network calls. That reasoning is *unconfirmed*; only the outcome is measured.
- **The A4 workaround does not apply.** Rebuilding for a fresh hash fixed the proc-macro case and
  does nothing here.

**Ruled out, with sources:** per-app exclusions (do not exist — confirmed by a Microsoft
moderator), self-signed certificates (SAC needs the Microsoft Trusted Root Program, not the local
store), different folder or filename, running as Administrator, and Windows Sandbox (SAC runs
inside it too, and GPU inference plus Windows Graphics Capture are unlikely to survive there).
RustDesk — the same category of app — hit this and found no fix but disabling SAC.

**Toggling SAC is not automatable.** Microsoft's documented procedure boots into Windows Recovery
and edits the *offline* registry hive, setting both `VerifiedAndReputablePolicyState` and the
anti-rollback guard `VerifiedAndReputablePolicyStateMinValueSeen`. No running program can do it,
and that is clearly deliberate. **An "LLM controller app with a SAC toggle" is therefore not
possible** — worth recording, since it was considered.

> **Correction:** this project briefly recorded that an April 2026 update made the SAC toggle
> reversible from Settings. Microsoft's own documentation says *"Configuring Smart App Control to
> Off or On (enforcement) is a one-way operation"* unless the current state is Evaluation. The two
> could not be reconciled. **Do not rely on the Settings toggle being reversible.** Recovery-boot
> registry editing can restore any state, so it is recoverable — just not by clicking.

**The four options, unchanged:**

| | |
|---|---|
| **Code-sign** | ~$10/month (Azure Artifact Signing, US/Canada individuals). The only path where SAC stays enforced *and* the GUI runs |
| **Open-source it** | SignPath Foundation signs qualifying open-source projects free |
| **Disable SAC** | Free; costs a real protection layer. Defender and SmartScreen are unaffected |
| **Stay on the CLI** | Free, SAC stays on, works today — the CLI is not blocked |

**Which chunks this blocks:** everything that runs the GUI — the rest of **C**, all of **D**'s
verification, and **E1**'s two weeks of daily use. **Stage A is unaffected.** BUILD.md's own hedge
holds: *"A4 is already a useful tool. If everything after this stalls, you still have something."*

**Also missing until A3: the MSVC build tools.** A1 recorded the toolchain as verified but only
exercised Ollama, and nothing ever reached the link step. Installed at A3; **B1 needs them too.**

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

- **Settled at A2 on 2026-09-08: `qwen3-vl:8b-instruct-q4_K_M`.** 8,151 MiB resident, 1.2 s to
  first token, 3.7 s cold load. Chosen because it was the only model that correctly concluded a
  broken file would not compile. Full write-up: [A2-results.md](results/A2-results.md).
- Reading small dense text accurately is the whole job; a misread stack trace is worse than a
  slow answer.
- ~~Vision models degrade under quantization harder than text models, so bias toward precision
  when it's close.~~ **Retired 2026-09-08.** Measured across a full 2×2, quantization changed
  time-to-first-token by under 2% and did not fix a single accuracy failure — while Q8 cost
  +3.2 GB of VRAM and half the decode speed at 8B. **Size decides accuracy here, not precision.**
- **A 1080p screen costs 2,042 image tokens** (measured: 2,077 with the image, 35 without).
  `num_ctx` must be set explicitly — Ollama silently defaults to 4k on a card under 24 GiB, which
  would truncate exactly this.

---

## 6b. Power control — explicit on/off, never a timeout

**Revised 2026-09-08.** An earlier draft said `keep_alive: -1` — keep the model resident
forever. That was written before we measured what resident actually costs.

**Re-measured at A2 on 2026-09-08 with the model actually chosen.** The numbers below originally
came from `llama3.1:8b`, a stand-in from A1. The real figures for `qwen3-vl:8b-instruct-q4_K_M`:

| State | GPU memory | Notes |
|---|---|---|
| Off | **~950 MiB** | Baseline **drifts** — it was 667, 1033 and 945 MiB at different points in one session, depending on what else is open. Sample it fresh; never hard-code it |
| Model loaded | **9,108 MiB** total, **+8,151 MiB** over baseline | Half the card |
| Unloaded again | **~950 MiB** | Card gives it back exactly — verified on all four candidates |

**Cold load is 3.7 s**, not the ~7–18 s this section previously assumed. A1's 18 s figure was a
4.9 GB text model read off cold disk; the vision models load faster despite being larger.
That materially softens the "first press after lunch" problem this whole section was built around.

**Holding 8 GB permanently is still the wrong default** — more wrong than when this said 5 GB.
The popup has to work over games wanting 8–12 GB, and 8,151 MiB leaves only ~7 GB of a 16 GB card.
Explicit on/off stands.

**So: no timeout at all. Three explicit states, and you always know which one you're in.**

| State | Meaning |
|---|---|
| **Off** | Default. Card free. Nothing loaded. |
| **Warming** | Loading, **~3.7 s** (measured at A2 for the chosen model). |
| **Ready** | Loaded and instant. |

Warm up and turn off are both deliberate actions — hotkey or tray. A timeout is rejected
because it decides for you and leaves you guessing whether the next press is instant or a stall.

**The state readout must come from the GPU, not from Ollama.** `ollama ps` reports what Ollama
*believes* is loaded. Read actual VRAM via NVML (the `nvml-wrapper` crate) so "Off" means the
card genuinely gave the memory back — verified, not assumed. Polling `nvidia-smi` also works but
spawns a process per sample.

Mechanism, already proven to work: load with `keep_alive: -1`, unload with `keep_alive: 0`.

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
8. Tray icon and power control. **No launch on login** — C5 was dissolved 2026-09-14; you open Warden when you want the hotkey live, which is what §0's "summoned, never ambient" actually asks for.


**Expect the borrow checker to bite in two places:** the lifetime of the capture frame buffer,
and moving streamed chunks across the async Tauri boundary. Both are normal; neither means the
design is wrong.

### P2 — Live with it
Tune the default prompt. Fix only what actually annoyed you.
**Done when** you reach for it without deciding to.

### After P2 — ordered by what you actually miss
history & persistence · region select · chat window · terminal mode (TUI,
[OpenClaw](https://github.com/openclaw/openclaw)-shaped) · files & notes · voice · acting via MCP

### P3 — the model hub (scoped 2026-09-14, deliberately not started)

One place to pull, run, power-cycle and **test** local models, which Warden and any future LLM app
uses as its backend. This is the "platform eventually" in §0 becoming concrete, and it grows into
the terminal and desktop modes already listed above.

**Most of the hard part already exists** — this is assembly, not invention:

| Piece | Where it already lives |
|---|---|
| Model testing | `evals/bench.mjs` — model × screenshot, TTFT, decode rate, resident VRAM, verbatim answers, blind labels |
| Grading rubric | `evals/cases.md`, with `evals/fixtures/` so cases regenerate from tracked source |
| Power control | `keep_alive: -1` / `0`, measured against NVML rather than Ollama's own claim (§6b, C4) |
| Downloading | `ollama pull` — the hub wraps it, never reimplements it |
| Shared backend | **Already true.** Ollama is the server; anything on the machine can reach `127.0.0.1:11434`. Warden is a client, not a bundler |

**What the hub actually adds** beyond what's listed above:

1. A front end over pull / list / remove
2. Power control with **visible** VRAM — Ollama surfaces this poorly
3. The eval harness with a UI, so comparing models is a click rather than a command
4. **A shared "current model" config that every app reads** — switch once, switches everywhere.
   Nearly free, and the piece that makes it a hub rather than a launcher

**Build it CLI-first, and not for style reasons.** Smart App Control blocks `warden.exe` but
permits `capture.exe` — a GUI hub would inherit the same block, a console one very likely wouldn't.
**A desktop hub therefore depends on the code-signing decision** (§3), exactly as the rest of
Stage C does.

**The stepping stone that pays three times:** `capture warm` / `off` / `status` as subcommands on
the existing binary. Runs today under SAC, closes A1's unresolved "Ollama has no autostart" gap,
and the logic is precisely what **C4** needs and what the hub needs after that.

**Not before v1.** *(Written while Warden was unfinished. It shipped at Stage C, and the model hub
is now being built as **Citadel** — Stage G. §0's rule still stands: a feature earns its keep by
being used, not by sounding good.)*

---

## 8. Known traps

- **The popup captures itself.** Without the exclusion flag, a follow-up screenshot contains
  Warden's own last answer and the model reads itself back.
- **GDI capture returns black** on fullscreen and hardware-accelerated content. Use WGC.
- **Ollama unloads the model** after five minutes by default, and nothing masks the cold start
  here. Handled by the explicit power control in §6b — not by a longer timeout.
- **`ollama ps` is not proof.** It reports what Ollama believes is loaded. Only NVML / the card
  itself proves memory was actually released.
- **A broken CUDA install fails silently** — Ollama just runs on CPU with no error. If inference
  feels slow, check `ggml-cuda.dll` exists before debugging anything else. Cost real time in A1.
- **`ollama app.exe` opens a blocking first-run GUI.** Use `ollama.exe serve` detached.
- **Ollama has no autostart entry** — it does not survive a reboot.
- **A Tauri command that returns its payload buffers it.** One IPC reply per `invoke()`, so the
  whole answer lands at once — identically in dev and packaged. Stream through
  `tauri::ipc::Channel`; Tauri's own docs call it "the recommended mechanism for streaming data
  such as streamed HTTP responses." *(Corrected at B2: the earlier note blamed
  `proxy_localhost_stream`, which is not a Tauri symbol at all.)*
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
