# Waiting on a human

Things built here that no tool call can verify — they need a keypress, a hover, or a second
machine.

**To work through Stage G's checks, use [G-testing.md](G-testing.md)** — the same checks as a
worksheet, with exact text to type and what should happen. This file keeps the reasoning.

## Open — H2, finding code, 2026-09-29

**Already proven mechanically.** 17 unit tests on the walk itself — `.git` (a folder, or a
worktree's file), `target/` and `node_modules/` skipped without a `.gitignore`, nested
`.gitignore` honoured through the `\\?\` root, a junction out of the folder not followed, `../**`
and drive or UNC patterns refused, Mímir's own folder skipped, answers capped at 8 KB, Stop ending
a walk. End to end: a search card names what it looks for and what it finds reaches the model; a
declined listing reads nothing. **What needs a person** is how the cards read.

- [ ] **The cards read well** — a search, a listing, patterns the right way round.
      [G-testing.md §9](G-testing.md)
- [ ] **A big folder stays stoppable.** Choose a large folder (your `source\repos`), ask for every
      file, allow it, press **Esc** — the card says *Stopped*, the window stays responsive.

## Open — H1, the check command, 2026-09-29

**Already proven mechanically.** `node tools/e2e/mimir-check.mjs` — 18/18 on the built app: a
Cargo project offers `cargo check` and `cargo test`; a check card runs nothing until allowed, and a
denied one runs nothing at all; output reaches the card and the model; a failing command shows red
and the model quotes its error; Stop turns the card to *Stopped* in about 400 ms and leaves no
child process running; and a type change whose first edit breaks the build ends, through the
check, in a build that passes. `mimir-edit.mjs` still 16/16. **What it skips is what a person
operates** — the picker on the chip, and whether a window flashes.

- [ ] **Choose the check from the chip.** Only detected commands are offered, nothing can be typed,
      and the choice is remembered per folder. [G-testing.md §8](G-testing.md)
- [ ] **No console window flashes** while a check runs.
- [ ] **Stop with Esc** during a real `cargo check` leaves no `cargo.exe` or `rustc.exe` behind.

## Open — G7, editing files, 2026-09-24

The model, the diff, the approval, Undo, Deny, a file changed behind the diff's back and a path
outside the folder are all covered by `node tools/e2e/mimir-edit.mjs` — 16/16 on the shipped
build. **What it deliberately skips is the one thing a person operates:** the folder picker. The test
writes the folder into the config instead, because the app has no command that takes a folder path —
by design.

- [ ] **Turn Tools on, click Choose folder.** The Windows folder picker must open. Pick a real
      project; the chip must show its name.
- [ ] **Cancel the picker once.** The chip must keep whatever it had — cancelling is not clearing.
- [ ] **Ask for a small real edit** in that project. Read the diff before you apply it: it must be
      exactly the change, with the file's own line numbers.
- [ ] **Undo it**, then check the file in your editor — it should be as it was, with no trace.
- [ ] **Click the × on the folder chip.** Editing turns off; asking for an edit should now get
      *"no project folder is chosen"* in the thread, and nothing written.

## Open — G6, the tool loop, 2026-09-19

Gated on G0, which cleared it: `qwen3-vl:8b` scored 10/10 including four cases where calling a tool
would have been wrong. One tool, hardcoded, no MCP — MCP is a transport over this loop, and a loop
that already works is far easier to debug than one arriving with a protocol attached.

**The safety design is the chunk, not a detail of it.** Tools are **off by default**. There is no
allowlist, no remembered folder and no "always allow": every read shows the exact path and waits.
That is deliberately the most annoying design that is still usable, because the alternative is a
model reading whatever it names and nobody noticing.

**Already proven mechanically.** Given a temp file containing a fact it could not know, the model
asked for `read_file`, the tool returned the contents, and the next turn answered *"The launch code
for project Bramble is **7731-VERMILION**"*. 18 tests, including that an unknown tool name is
refused outright and that a directory is rejected clearly.

- [ ] **Turn Tools on** (header), then ask about a file on this machine — something like
      *"read C:\\…\\something.txt and summarise it"*. **The approval card must appear with the exact
      path, and nothing may be read until you click Allow once.** That is the chunk's done-when.
- [ ] **Click Deny on one.** The call should be marked denied and the model told, rather than left
      waiting on something that silently never happened.
- [ ] **Expand a completed call.** The result is in the thread, so what ran is recorded where the
      conversation is rather than in a log.
- [ ] **Ask for a file that does not exist.** The tool returns an error *as text* and the model
      continues — a tool that cannot answer is a normal conversational event, not a failed turn.
- [ ] **Try with tools on and `llama3.1:8b` selected.** It reports `tools`, so it is allowed — but
      G0 measured it firing a spurious call on every negative case, so expect it to reach for
      `read_file` when it should just answer. Worth seeing once.
- [ ] **Turn Tools off and ask the same thing.** It should answer in prose without ever offering
      to read anything.

## Open — G5, attachments, 2026-09-19

Images ride as base64 on the message; the bytes live in `%APPDATA%\odin\attachments\` and the
conversation records the filename. Text files are folded into the question as a labelled code
fence instead, so a model with no `vision` can still read them.

**Already proven mechanically.** A 51 KB PNG stored and read back byte-identical, then sent to the
model, which answered *"Rust compilation error message"* at **2,065 prompt tokens** — about 2,042
of those being the image, matching A2. Deleting a conversation takes its attachments with it.

- [ ] **Drop a screenshot onto the composer and ask about it.** The composer highlights while you
      drag. **This is the chunk's done-when.**
- [ ] **The `+` button** picks files too, for when dragging is awkward.
- [ ] **The chip shows the cost before you send** — file size and `~2,042 tok`. An image costs
      roughly that whatever it shows, so two of them plus a little history is most of the budget.
      Worth seeing early rather than discovering.
- [ ] **Drop a `.rs` or `.md` file.** It folds into the question as a fenced block rather than
      riding as an image, so it works on any model.
- [ ] **Drop something unsupported** — a `.zip`, say. It should be refused **with a reason**, not
      silently ignored and not read as garbage text.
- [ ] **Reopen a conversation that had an image.** The turn should still say how many were
      attached, and a follow-up should re-send them.

## Open — G4, persistence, 2026-09-19

Conversations are one JSON file each in `%APPDATA%\odin\conversations\`, beside `config.json`. No
database: G4 needs save, list, rename and delete, and none of that needs a query. What a plain file
buys is that you can read it in an editor, back it up by copying it, and delete one conversation by
deleting one file.

**Already proven mechanically.** Save → reload is byte-identical, the list sorts newest first,
rename and delete work, deleting something already gone returns `false` rather than erroring, and
a record written without the G5/G6 fields still parses. 12 tests cover the record shape, the title
derivation, the date arithmetic and the path-traversal refusal.

- [ ] **⭐ Close Mímir, reopen it, and continue a conversation.** Ask something, quit, relaunch,
      hit **History**, open the thread, and ask a follow-up that depends on what was said before.
      **That is the chunk's done-when.**
- [ ] **Titles derive from the first question.** A list of conversations all called "New
      conversation" is a list you cannot use, so the first line of the first question becomes the
      title until you set one.
- [ ] **Rename** by double-clicking a row. Enter commits, Escape cancels.
- [ ] **Delete** with the `×`, which arms an in-place Delete/Keep rather than opening a dialog —
      one click to arm, one to confirm, and nothing that can be dismissed into deleting by accident.
      Deleting the conversation that is currently open should clear the thread.
- [ ] **Look at a file.** `%APPDATA%\odin\conversations\` — it should be readable JSON, with
      `model` recorded on assistant turns and `images`/`tool_calls` absent until G5 and G6 fill them.

## Open — G3, Heimdall follows the app, 2026-09-19

**This is the chunk that answers the original question**, and its checks are the ones that matter
most in Stage G — two of them are about whether a new app broke a finished one.

**Already proven mechanically.** `show_cached` reports `vision=true` for `qwen3-vl:8b` and
**`vision=false` for `llama3.1:8b`**, so the refusal path has a real trigger. The cache turns a
3.02 ms round trip into a **4.10 µs** lookup — about 700× — which is what keeps the popup instant.
With no config file present, `config::load()` still yields `qwen3-vl:8b-instruct-q4_K_M` at 32,768.

- [ ] **Switch the model in Mímir, then press `Ctrl+Alt+W` — without restarting Heimdall.**
      The popup should answer using the new model. Heimdall re-reads the config on every summon, so
      there is no IPC, no file watching and no daemon; it simply looks when it wakes up.
- [ ] **Pick `llama3.1:8b` in Mímir and press the hotkey.** The popup must say
      **`cannot see images`** in the header and name the model, *instead of* capturing and
      answering. Ollama does not refuse a screenshot sent to a text-only model — it drops the image
      and answers anyway, which is a fluent, confident description of nothing. That is worse than
      an error, because nothing about it looks wrong.
- [ ] **The tray follows too.** After switching, use the tray's Warm — it should load the model you
      chose, not the one Heimdall started with. Confirm with `odin status` in a terminal.
- [x] **⭐ Delete `%APPDATA%\odin\config.json` entirely and press the hotkey.** The popup must
      answer normally on its built-in default, **with no error and no stall**. This is decision #5
      and the single most important check in the stage: it is the one that says **a new app did not
      break the finished one**. Heimdall must work on a machine where Mímir was never installed.
      **Passed 2026-09-25**, config renamed away: answered in ~6s on `qwen3-vl:8b-instruct-q4_K_M`,
      no warning, and the file was not recreated. Pressed with a synthetic keypress and read over
      the debug port rather than by eye.

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

Mímir builds, launches, and shows a window titled **Mímir** alongside a running Heimdall. What no
tool call can do is type in it.

**Already proven mechanically**, so only the wiring is open: a three-message conversation through
`odin_core::chat::stream` correctly recalled both facts from turn one — *"Your dog is called
Bramble, and you mentioned the colour vermilion."* The stream delivers separate fragments (8 for a
count to eight), Heimdall still compiles and runs, and both guards pass.

- [ ] **Hold a real conversation.** Launch `target\debug\mimir.exe`, ask something, then ask a
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
- [ ] **Heimdall is undisturbed.** `Ctrl+Alt+W` still answers with Mímir open.

> **Expect Smart App Control to refuse `mimir.exe` at some point.** It did not on the first
> launch, but a new binary is a fresh verdict; a rebuild for a new hash clears it (PLAN.md §3).

## Open — Before G0 step 3, 2026-09-19

The config was verified end to end **through the CLI**: no file at all gives working defaults and
exit 0; a file with a distinctive prompt reaches the model; renaming it away falls back silently
without forcing a reload. `odin warm` then `odin` now reports **`load 0.00s`**, where the same
sequence cost 3.09s before. What none of that exercises is the popup.

- [ ] **Does Heimdall read the config too?** Write `%APPDATA%\odin\config.json` with an obvious
      prompt — `{"model":"qwen3-vl:8b-instruct-q4_K_M","prompt":"Say only the word BANANA.","num_ctx":32768}`
      — then press `Ctrl+Alt+W`. The popup should answer `BANANA`.
- [x] **Then delete the file and press it again.** *(Passed 2026-09-25 — the ⭐ check under G3.)*
      The popup must answer normally on the built-in default, **with no error and no stall**. This is the one that matters: PLAN.md decision #5
      says a missing config is a normal state, and it is what keeps Heimdall working on a machine
      where Mímir was never installed.
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
      Heimdall's behaviour, not the renderer's.

## Cleared 2026-09-25 — the rename

- [x] **1h — rename the repo root** `C:\Users\you\source\repos\heimdall` → `…\repos\odin`.
      **Done 2026-09-25.** Closing VS Code was not enough: **Ollama was holding the folder.** Heimdall
      had started `ollama serve` on 09-17, it inherited the repo as its working directory, and so
      did every model runner it spawned. odin-core now starts Ollama from the temp directory, so a
      future rename only needs the editor and the apps closed. **After a rename, `cargo clean`
      first:** Tauri's build scripts cache absolute paths, so the next build fails reading
      `…\heimdall\target\…\permissions\…` rather than rebuilding.
- [x] **Re-run `node tools/shortcuts.mjs`.** **Done 2026-09-25** — `--check` shows all four, Desktop
      and Start Menu, at the new path.
- [x] **`gh repo rename odin`**, then update `origin`. **Done 2026-09-25** — the repo is
      `LuisZarate17/odin` and `origin` points at it. GitHub redirects the old URL, so a stale clone
      keeps working.

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
"you open Heimdall when you want the hotkey live" — because right now you also have to keep a dev
server up.

Fixed by `npm run app` (`tauri build --debug --no-bundle`), plus suppressing the console so the app
survives the terminal that launched it, plus Desktop and Start Menu shortcuts so it can be opened
at all. Verified: runs with no dev server, survives closing VS Code and every terminal.

## C2 — the picker disclosure

Not a bug, but worth a decision:

- [ ] Zoom lists **"Heimdall"** by name in its share picker, with a black thumbnail. Contents never
      leak; the window's *existence* does. Left as-is deliberately — renaming is obscurity, and the
      app only runs when you open it. Revisit only if a picker leaks more than the name.
