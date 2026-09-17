# Waiting on a human

Things built here that no tool call can verify — they need a keypress, a hover, or a second
machine.

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
2. **The resident check panicked the request that needed it.** `warden_core` is blocking by
   design; calling it from an async command drops a tokio runtime inside an async context and
   kills the worker. The popup hung on "Reading your screen…" forever while Ollama loaded a model
   nobody was waiting for.
3. **`Esc` did not close the follow-up box**, and the tray menu **worked while looking broken** —
   both actions fired every time, but nothing on screen said so.

## Still untested

- [ ] **Does `Ctrl+Alt+F` collide with anything you use?** With an answer on screen, try it in
      VS Code and the browser. C1c established that `RegisterHotKey` reports every
      `Ctrl+Alt+<letter>` as free because it only knows about *global* registrations, never what
      an application binds internally — so this cannot be detected in advance, by us or by the OS.
      `Ctrl+Alt+C` and `Ctrl+Alt+Q`/`A` were all withdrawn for exactly this.

## Open, and bigger than a check

**The app needs a terminal running Vite to work at all.** `cargo build` bakes `devUrl` into debug
builds, and the debug build is the only one Smart App Control lets run. That breaks C5's premise —
"you open Warden when you want the hotkey live" — because right now you also have to keep a dev
server up.

`npx tauri build --debug` looks like the fix: a debug-profile binary that bundles `frontendDist`
and needs no server. Confirmed the flag exists; not yet tried.

## C2 — the picker disclosure

Not a bug, but worth a decision:

- [ ] Zoom lists **"Warden"** by name in its share picker, with a black thumbnail. Contents never
      leak; the window's *existence* does. Left as-is deliberately — renaming is obscurity, and the
      app only runs when you open it. Revisit only if a picker leaks more than the name.
