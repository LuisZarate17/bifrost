# I4b — The Bifrost tray, and a launch that feels instant

Two things Luis asked for once I4 was in use: **a smoother start** from the Desktop icon, and — once
the cause of the wait was found — **the tray as Bifrost's control panel**, which also catches
Ctrl+Alt+H. **Built 2026-10-16**, in two commits on `stage-i`: `2916a0a` (the splash) and `c541532`
(the tray).

## Where the wait was

A first look said Windows Terminal starts cold in about 3.5 s. Measured properly, the splash added to
cover that appeared **3.0–3.25 s after the key** — so most of the wait came before `hugin-launch` even
started: **Windows waits about three seconds before acting on a shortcut key on a `.lnk`**, every time,
not only the first. Opened from the Desktop icon, the launcher is up in about 95 ms. A key a running
program has registered is caught at once — so Ctrl+Alt+H moved into the tray, the one Bifrost
program always running.

| From the key to… | Shortcut key on `Hugin.lnk` | Caught by the tray |
|---|---|---|
| the splash | 3.0–3.25 s | **0.3 s** |
| the terminal window | 3.3–3.85 s | **0.7–1.0 s** |

## The splash

`hugin-launch` shows a small card the moment it starts — the Hugin shield, its name, the folder it
opens, and a Raven Violet bar sweeping beneath — and fades it out once the new Windows Terminal window
is up (8 s at most; 3 s for the plain console). It never takes focus and has no taskbar button.

## The Bifrost tray

Vidar's tray icon is now the **Bifrost mark**, and its menu is short:

```
 Ready · 18.2 GB
 ✓ Model loaded            one switch: on loads the model, off releases it
   Model ▸                 the installed models; the configured one ticked; "text only" if it cannot see
   Open Vidar   Ctrl+Alt+W  summons the overlay, as the key does
   Open Mimir
   Open Hugin   Ctrl+Alt+H
 ✓ Start at login
   Quit Bifrost
```

- **Picking a model** sets the shared model; with the switch on, the old one is released and the new
  one loaded, Warming shown meanwhile. With it off, nothing loads.
- **Start at login** is on by default: a `Bifrost.lnk` in the Startup folder, made on the first run and
  repointed if the program has moved. **Starting loads nothing**: the tray only asks Ollama what is
  loaded — it never starts Ollama or a model.
- The overlay is still Vidar, and Ctrl+Alt+W is unchanged.

## Checked on the real desktop

- Starting the tray: both keys registered, Start at login made the shortcut on the first run, and the
  loaded models were the same before and after.
- The Startup shortcut pointed at the wrong program on purpose: the tray read it, saw it, and rewrote it.
- Ctrl+Alt+H: the splash at 0.3 s, the terminal at 0.7 s.
- The tray menu itself cannot be opened by a script here (the taskbar hides itself), so its clicks are
  a hand check.

## The review

An independent review confirmed that starting the tray never starts Ollama or loads a model, and that
the overlay is untouched. It found, and these were fixed: picker ticks left wrong after a click that
changed nothing; menu clicks that could wait on Ollama on the main thread; a Start at login turned off
coming back after a failed first run; paths embedded in PowerShell text (now passed as environment
variables); a Startup shortcut never repointed; two quick clicks racing; a slow release blocking a
pick; and the idle memory figure sampled while a model was resident.

## Hugin opens with its mark

Asked for in the same session: a start screen with art. Of four directions — the shield, a raven, a
pixel wordmark, a rune card — **Luis chose the raven**. Finding the right one took many rounds, each
looked at in a real Windows Terminal: a 44 × 22 pixel raven in Raven Violet (built in as `eec0042`,
then taken back out), a misty mountain scene, a moon, a crescent, the raven traced from photographs,
and pixel ravens as small as the words. What stayed is **a raven drawn as a logo**: dark, with a violet
outline, no eye, three lines tall.

In Windows Terminal, Hugin now opens with that mark as a small image (Sixel, 9 columns by 3 rows,
its background left undrawn so the terminal's own shows through), and three lines beside it: the name
and *thought, sent out to work*, the folder and branch, the model and shell, in muted dark colors.
Other color terminals, VS Code's among them, get the three lines without the mark; under 48 columns or
with `NO_COLOR`, the old single line. Line mode, which scripts read, keeps its plain header. The mark's
source is `apps/hugin/assets/raven.svg`; `tools/raven/make.ps1` turns it into the image Hugin carries.

Checked on the real desktop: in a fresh Windows Terminal window; at the bottom of a full one, where the
screen has to scroll first and the mark still lands beside its words; and in the old console host,
which gets the words alone.

## For a person

[I-testing.md](../I-testing.md) §I4b: the menu, click by click.
