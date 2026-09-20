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

## Open — G2, the switchboard, 2026-09-19

**Already proven mechanically.** All five models' capabilities read correctly — `llama3.1:8b`
reports `vision=false`, which is exactly the negative case G3 needs. Load moved the card from 469
to 11,830 MiB, and a conversation straight afterwards reported **`load_duration 0.00s`**: the
switchboard's warm survived, because `load_model` goes through `ollama::load` at the config's
`num_ctx` rather than rolling its own call. That is the packet's "watch for", closed.

What is open is the panel itself.

- [ ] **Open the model panel** — the button in the header showing the current tag. Every installed
      model should be listed with its size, quantization, `vision`/`tools` badges, and whether it
      is resident.
- [ ] **Pick a different model.** The radio writes the choice to `%APPDATA%\odin\config.json`, so
      `odin status` in a terminal should immediately report the new tag. **That is the switchboard
      working** — one source of truth, followed by the CLI without being told.
- [ ] **Load and Unload.** The card figure in the footer should move to match within a few seconds.
      Note Ollama and the card disagree by ~2,100 MiB (9,737 vs 11,830 measured); **the card is the
      truth**, which is why the footer shows it.
- [ ] **`llama3.1:8b` shows `tools` but no `vision` badge.** The one model whose labels differ, and
      the reason G3 can refuse a screenshot cleanly instead of guessing at an image it never got.
- [ ] **16 GB will not hold two 8B models.** Load one while another is resident and watch them take
      turns — worth seeing once so the constraint is familiar rather than surprising.

## Open — G1, 2026-09-19

Citadel builds, launches, and shows a window titled **Citadel** alongside a running Warden. What no
tool call can do is type in it.

**Already proven mechanically**, so only the wiring is open: a three-message conversation through
`odin_core::chat::stream` correctly recalled both facts from turn one — *"Your dog is called
Bramble, and you mentioned the colour vermilion."* The stream delivers separate fragments (8 for a
count to eight), Warden still compiles and runs, and both guards pass.

- [ ] **Hold a real conversation.** Launch `target\debug\citadel.exe`, ask something, then ask a
      follow-up that only makes sense given the first — "what did I just ask you?". **This is the
      chunk's done-when.**
- [ ] **Watch it stream.** Tokens should appear progressively, *not* all at once. Arriving whole is
      the tell for returning the answer as the command's `Result` instead of through the `Channel`.
- [ ] **Markdown renders through `@odin/ui`** — ask for a Rust snippet and confirm the code block
      highlights, inline code gets its pill, headings scale. *(This also settles 1e's open visual
      check from the other direction: same renderer, different app.)*
- [ ] **Switch model mid-conversation** in the header picker; the next turn uses the new one.
      Changing it affects this session only — writing the choice to the shared config is G2.
- [ ] **The status bar moves.** VRAM figure tracks the card as a model loads or is released, and
      the `n / 32,768 ctx` figure appears after the first reply.
- [ ] **Fill the context** — paste something long, repeatedly — and confirm
      `⋯ n earlier turns dropped to make room` appears in the thread rather than the conversation
      quietly going strange.
- [ ] **Warden is undisturbed.** `Ctrl+Alt+W` still answers with Citadel open.

> **Expect Smart App Control to refuse `citadel.exe` at some point.** It did not on the first
> launch, but a new binary is a fresh verdict; a rebuild for a new hash clears it (PLAN.md §3).

## Open — Before G0 step 3, 2026-09-19

The config was verified end to end **through the CLI**: no file at all gives working defaults and
exit 0; a file with a distinctive prompt reaches the model; renaming it away falls back silently
without forcing a reload. `odin warm` then `odin` now reports **`load 0.00s`**, where the same
sequence cost 3.09s before. What none of that exercises is the popup.

- [ ] **Does Warden read the config too?** Write `%APPDATA%\odin\config.json` with an obvious
      prompt — `{"model":"qwen3-vl:8b-instruct-q4_K_M","prompt":"Say only the word BANANA.","num_ctx":32768}`
      — then press `Ctrl+Alt+W`. The popup should answer `BANANA`.
- [ ] **Then delete the file and press it again.** The popup must answer normally on the built-in
      default, **with no error and no stall**. This is the one that matters: PLAN.md decision #5
      says a missing config is a normal state, and it is what keeps Warden working on a machine
      where Citadel was never installed.
- [ ] **Warm survives the hotkey.** `odin warm`, then `Ctrl+Alt+W`. The popup should not show
      "Warming the model…" — the tray and the CLI now load at the same `num_ctx` the popup asks
      with, so the runner is no longer torn down between them.

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
