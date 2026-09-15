# B4 + C1 — Hotkey, corner popup, translucency: results

**Completed 2026-09-14.** Chunk B4 of [BUILD.md](../BUILD.md), plus C1 and the translucency half
of C1b, done together at the user's request.

## What was built

Press `Ctrl+Alt+W` from anywhere and a translucent panel appears bottom-right, streams an answer
about your screen, and stays until you press it again. Your cursor never leaves the editor.

| | |
|---|---|
| Hotkey | `Ctrl+Alt+W`, fallback `Ctrl+Alt+Space`, registered in `setup()` with the `Result` checked |
| Position | Bottom-right of the **work area**, and it follows the auto-hiding taskbar |
| Focus | Never stolen — `WS_EX_NOACTIVATE` plus `ShowWindow(SW_SHOWNOACTIVATE)` |
| Translucency | `transparent: true` + CSS alpha. **Not** a DWM backdrop — see below |
| Click-through | Available and verified compatible with capture exclusion; currently off |
| Capture exclusion | Still holds, verified again with the layered bit set |

---

## Translucency took five attempts, and four were the wrong layer

Worth recording in full, because the failure mode was mine and it repeated.

| Attempt | Result | Why |
|---|---|---|
| Mica | Light grey slab | Mica samples the *wallpaper*, and rendered light |
| Acrylic | Light grey slab | Acrylic samples what's behind — correct behaviour — but rendered light |
| Acrylic + `.color(dark)` | No change | **`color` is never read on Windows 11.** `apply_acrylic` takes the `DWMWA_SYSTEMBACKDROP_TYPE` branch on builds ≥22523 where the colour is discarded. Tauri's own source comments: *"Doesn't have any effect on Windows 7 or Windows 11"* |
| Acrylic + `DWMWA_USE_IMMERSIVE_DARK_MODE` | No change | Correct call, wrong target: `apply_acrylic` has **no `dark` parameter at all**, unlike `apply_mica`/`apply_tabbed` |
| **MicaDark** | **Dark, but opaque** | Genuinely dark — so the dark path worked — but Mica can never show the window behind it |
| **`transparent: true` + CSS alpha** | **Works** | No DWM involvement at all |

Also discovered inert: **`.state(EffectState::Active)` is macOS-only** and silently dropped by the
Windows code path. Two of the settings being "configured" were doing nothing whatsoever.

**The lesson is not about DWM.** Each fix was a smaller variation on the same assumption — that
the system backdrop was the right mechanism and just needed the correct knob. The question worth
asking after the second failure was *"is a DWM backdrop the right tool here at all?"* It wasn't:

- **Acrylic** sees through but cannot go dark.
- **Mica** goes dark but cannot see through.
- The popup is **never the active window** (`focusable: false`), and DWM renders inactive
  backdrops as flat neutral material by design.

CSS alpha sidesteps all three. The webview composites with alpha because the window is transparent,
and the page paints its own background — Windows is not asked for a material, so it has no opinion
to impose. `backdrop-filter: blur(24px)` supplies the frost that Mica would have.

---

## Two bugs, both mine, both from mixing abstraction layers

**The toggle only ever showed.** The popup is shown with raw `ShowWindow(SW_SHOWNOACTIVATE)` to
avoid stealing focus, but was hidden through Tauri's `window.hide()`. Tauri therefore never learned
the window was visible: `is_visible()` kept returning `false`, so every press took the "show"
branch. Fixed by asking Windows directly (`IsWindowVisible`) and hiding the same way we show.

The first fix was wrong, and the log said so — `visible=true → hidden` five times in a row, never
once `visible=false`. **Adding the log is what found it**; the guess before it was that `hide()`
was failing.

**The taskbar covered it.** An auto-hiding taskbar does **not** change the work area — Windows
reports full screen height whether the bar is shown or hidden, so a work-area calculation puts the
popup exactly where the taskbar slides over it. Now the popup reads `Shell_TrayWnd`'s live position
and rides 12px above it, polling at 120ms while visible and repositioning only when the edge
actually moves.

---

## Hotkey findings

**Registration failure is detectable — but only half of it.** `register()` returns a real `Err` on
Windows when another process holds the combo (`ERROR_HOTKEY_ALREADY_REGISTERED`, confirmed at the
`RegisterHotKey` call site). **But** apps that grab keys with a low-level keyboard hook — macro
tools, RGB software, AutoHotkey — swallow it with **no error at all**. Registration succeeds and
the handler never fires ([plugins-workspace#2646](https://github.com/tauri-apps/plugins-workspace/issues/2646),
closed without root cause). So a hotkey can be confirmed *bound* but never *working*.

**Registered by hand, not via the plugin builder.** `Builder::with_shortcut()` propagates a failed
bind out of plugin setup and **aborts the whole app at startup** rather than falling back.

**The handler fires twice per press** (`Pressed` and `Released`) and runs **on the main thread**,
holding the Win32 message pump — touching windows inline there is a confirmed Windows 11 freeze
([#1923](https://github.com/tauri-apps/plugins-workspace/issues/1923)). Both guarded.

**Esc does not dismiss**, deliberately: a `focusable: false` window never receives key events. The
hotkey toggles instead. C3 owns the focus transition when follow-up typing arrives.

---

## Click-through does not break capture exclusion

doesn't.** With click-through enabled, affinity read back as `17` and a WGC capture taken while the
popup was visible contained no trace of it.

The documented failures were WPF windows calling `UpdateLayeredWindow` to paint themselves. Merely
*carrying* the layered bit — which is all `set_ignore_cursor_events` does — is not the same thing.

**So the C1 contradiction recorded at B3 is resolved**: translucent, topmost, click-through,
by default (`WARDEN_CLICKTHROUGH=1` enables it) — its only cost is losing mouse-scroll on long
answers, and answers are long.

---

## Then Windows blocked the app

**Smart App Control now refuses to run `warden.exe`.** Full analysis in
[PLAN.md §3](../PLAN.md). In short: the block follows file *content*, is retroactive (the Sept 12
release build ran then and is blocked now), and **`capture.exe` is unaffected** — only the GUI
binary. The A4 rebuild-for-a-new-hash workaround does not apply.

This is **pinned, not solved.** It blocks the rest of Stage C, all of Stage D, and E1 — everything
that runs the GUI. Stage A is untouched and still works.

**Everything in this chunk was verified working before the block**, including the final
translucency, the taskbar-following, and the toggle.

---

## Delegation tracking

| | B4/C1 agent 1 | agent 2 | agent 3 | agent 4 |
|---|---|---|---|---|
| Task | Global shortcut plugin | Transparency vs capture exclusion | Dark acrylic recipe | SAC workarounds |
| Spend | 95,274 | 146,077 | ~90,000 | ~95,000 |
| Corrections needed | None | None | **Partly wrong** | None |

**Agent 3 is the interesting one.** It correctly identified that `color` is discarded and that
`apply_acrylic` never sets dark mode — both confirmed in source, both real. But its *causal story*
— that `tao` had read a light system theme — was wrong, and checking the registry disproved it in
one command (`AppsUseLightTheme = 0`, already dark). **The mechanism was right and the diagnosis
was wrong**, which is a failure mode worth naming: an agent can be precisely correct about how code
behaves and still misattribute why *your* machine behaves as it does.

**Agent 4 produced the most valuable negative result so far.** "There is no way around this" —
sourced, with each candidate workaround individually ruled out — saved building a controller app
around a toggle that cannot exist.

### Running comparison

| Chunk | Agents | Spend | What it caught |
|---|---|---|---|
| A2 | 3 | ~310K | Ollama API contract; the harness |
| A3 | 1 | 131K | `windows-capture` 2.0 breaking rewrite |
| A4 | 1 | 73K | `reqwest` TLS change needing NASM |
| B1 | 1 | 85K | A scaffolder that deletes the repo |
| B2 | 1 | 86K | A false warning in our own docs |
| B3 | 2 | 226K | COM apartment conflict; two future landmines |
| **B4/C1** | **4** | **~426K** | Dead config options; a project-level blocker with no workaround |

**Seven chunks, seven catches.** The B4 amendment: **research is excellent at "how does this code
behave" and unreliable at "why is this machine behaving this way."** Three of four agents here were
right about mechanism; the one that also guessed at cause got the cause wrong. Machine facts get
measured, not researched — the same lesson as the missing MSVC linker and the Smart App Control
discovery, both of which agents could never have found.

---

## Next

**Blocked on the GUI.** C1b (readable answers), C2, C3, C4, C5, all of D, and E1 need a running
`warden.exe`.

**Not blocked:** the F-stage harness work and anything in `capture/`. An LLM controller for Ollama
lifecycle and model management — discussed during this chunk — overlaps C4/C5 and is a CLI-shaped
problem, so it would run today.
