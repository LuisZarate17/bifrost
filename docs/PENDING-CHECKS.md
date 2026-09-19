# Waiting on a human

Things built here that no tool call can verify — they need a keypress, a hover, or a second
machine.

## Open — to do before the next push, 2026-09-19

Three things are deliberately held back until immediately before pushing, so the remote and the
working copy change together rather than drifting apart mid-restructure.

- [ ] **1h — rename the repo root** `C:\Users\you\source\repos\warden` → `…\repos\odin`.
      **Needs a human**: Windows will not rename a directory that a process holds open, and that
      includes VS Code, `warden.exe`, and any agent shell working inside it. Close all three,
      rename, reopen.
- [ ] **Re-run `node tools/shortcuts.mjs`** straight afterwards. Both shortcuts point at the old
      path until it does, and a shortcut that silently does nothing is worse than one that is
      obviously broken. `--check` reports where they point without changing anything.
- [ ] **`gh repo rename odin`**, then update `origin`. GitHub redirects the old URL, so a stale
      clone keeps working.

## Open — Before G0 step 1e, 2026-09-19

- [ ] **Does the popup still look identical after the renderer moved to `@odin/ui`?**
      `Answer.tsx` and `CodeBlock.tsx` moved to `packages/ui/`, and the renderer's CSS was split
      out of `App.css` into `packages/ui/src/answer.css`. **1e changes no behaviour and no
      appearance** — so anything that looks different is a bug in the split.

      Press `Ctrl+Alt+W` and check: heading sizes (15 / 14 / 13 px, separated by weight not scale),
      paragraph spacing, the dark code-block panel and its border, inline code's faint background
      pill, links underlined but not coloured, and the fade under a collapsed answer.

      **What is already proven mechanically**, so it is only the rendering that is open: the split
      moved 149 declaration lines and 149 came back, across 46 selectors with none lost, added or
      duplicated; `npm run check:inert` still passes 12/12 with the same emitted-element list; and
      the built CSS bundle is the same 3.53 kB as before.

      **The three rules deliberately left in `App.css`** are the ones to suspect if the *collapse*
      or `Ctrl+Alt+Z`'s highlight misbehave rather than the prose: `.answer > .copy-pick` and the
      two `.content.collapsed .answer…` rules. They select `.answer` descendants but encode
      Warden's behaviour, not the renderer's.

## All clear as of 2026-09-17

| Check | |
|---|---|
| Cold press shows **"Warming the model…"** | ✅ |
| A real answer follows | ✅ |
| `Ctrl+Alt+F` opens the box **with a live caret**, typing works | ✅ |
| The follow-up answers the **same screenshot**, no re-capture | ✅ |
| The box closes — `Ctrl+Alt+F` again, and `Esc` | ✅ |
| Tray menu tracks `Off` / `Ready` on its status line | ✅ |

**Three bugs came out of this round, and no automated check saw any of them.** Every build was
clean, all four crates checked, both guards green.

1. **The popup could not load at all.** The debug build hard-codes `localhost:1420`, so it needs
   Vite running. Without it the window shows WebView2's "can't reach this" page. See the open
   item below.
2. **The resident check panicked the request that needed it.** `odin_core` is blocking by
   design; calling it from an async command drops a tokio runtime inside an async context and
   kills the worker. The popup hung on "Reading your screen…" forever while Ollama loaded a model
   nobody was waiting for.
3. **`Esc` did not close the follow-up box**, and the tray menu **worked while looking broken** —
   both actions fired every time, but nothing on screen said so.

## Cleared 2026-09-17

- [x] **`Ctrl+Alt+F` collides with nothing in daily use.** Tried against the applications actually
      open on this machine and nothing stopped responding.

> Worth stating precisely what that does and does not prove. It is **absence of a known
> collision**, not proof there is none — `RegisterHotKey` reports every `Ctrl+Alt+<letter>` as free
> because it only knows about *global* registrations, never what an application binds internally.
> `Ctrl+Alt+C` and `Ctrl+Alt+Q`/`A` were withdrawn on exactly this evidence, so if some app ever
> starts behaving oddly while an answer is on screen, this is the first thing to suspect.

**Nothing from Stage C is waiting on a human.** Everything built through 2026-09-17 has been used,
not just compiled. *(Superseded 2026-09-19 — the restructure added one open item, at the top of
this file.)*

## Open, and bigger than a check

**RESOLVED 2026-09-17 — `npm run app`.** The app needed a terminal running Vite to work at all: `cargo build` bakes `devUrl` into debug
builds, and the debug build is the only one Smart App Control lets run. That breaks C5's premise —
"you open Warden when you want the hotkey live" — because right now you also have to keep a dev
server up.

Fixed by `npm run app` (`tauri build --debug --no-bundle`), plus suppressing the console so the app
survives the terminal that launched it, plus Desktop and Start Menu shortcuts so it can be opened
at all. Verified: runs with no dev server, survives closing VS Code and every terminal.

## C2 — the picker disclosure

Not a bug, but worth a decision:

- [ ] Zoom lists **"Warden"** by name in its share picker, with a black thumbnail. Contents never
      leak; the window's *existence* does. Left as-is deliberately — renaming is obscurity, and the
      app only runs when you open it. Revisit only if a picker leaks more than the name.
