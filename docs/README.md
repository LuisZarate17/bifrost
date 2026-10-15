# Navigator

**Everything in this repo, with a line on what it is.** Start here rather than searching — if you
know what you want but not where it lives, it is on this page.

## Start here

- **What does this key do?** → [shortcuts.md](shortcuts.md)
  Vidar has no menu and no window chrome, so that file *is* the interface.
- **Where does the project stand?** → [BUILD.md](BUILD.md)
- **Why was this decided?** → [PLAN.md](PLAN.md)
- **How do I build and run it?** → [../README.md](../README.md)
  `npm run app:vidar`, never a plain `cargo build`.
- **What's next?** → [../stages/STAGE-I.md](../stages/STAGE-I.md), and where it stands in
  [I-status.md](I-status.md)
- **Where is that function?** → [Where things live in code](#where-things-live-in-code)
- **What was actually measured?** → [Results](#results)

---

## Living docs

These change as the project does.

**[PLAN.md](PLAN.md)** — scope, the machine, decisions already closed, hard rules, roadmap.
Read it before re-deriving something, or when wondering whether a feature belongs at all.

**[BUILD.md](BUILD.md)** — the build plan, stages A–F, chunk by chunk, and what is left.

**[shortcuts.md](shortcuts.md)** — every control Vidar has, and why each one is that key and
not another. The most-opened file here.

**[D-testing.md](D-testing.md)** — the deferred testing round, as a worksheet to fill in at the
machine. For when you have an afternoon.

**[G-testing.md](G-testing.md)** — Stage G's checks as a worksheet: exact text to type into
Mimir and Vidar, and what should happen. `node tools/practice.mjs` builds the folder it uses.

**[H-testing.md](H-testing.md)** — Stage H's checks, the same way: the check command's chip and
card, Stop, finding code, switching to the coding model. Grows a section per chunk that adds
something you can press.

**[H-status.md](H-status.md)** — every Stage H chunk and gate, done or not, with its commit and
one-line result. Updated at the end of each chunk.

**[I-status.md](I-status.md)** — the same chart for Stage I.

**[I-testing.md](I-testing.md)** — Stage I's checks by hand, the same way as H's. Starts with the
rename: Vidar's hotkey and tray, Mimir's conversations and Undo, the taskbar pins.

**[PENDING-CHECKS.md](PENDING-CHECKS.md)** — things no tool call can verify, because they need a
keypress, a hover, or a second machine. Where "built but never actually pressed" is tracked.

## Execution packets

In [`../stages/`](../stages/), not here — they are inputs you paste into a fresh chat, rather than
records of what happened.

- **[STAGE-A.md](../stages/STAGE-A.md)** — foundations, no GUI. Complete.
- **[STAGE-B.md](../stages/STAGE-B.md)** — the app shell. Complete.
- **[STAGE-G.md](../stages/STAGE-G.md)** — **Mimir**, the local chat app sharing Vidar's
  model. Built through G7 (editing files); the testing round is open.
- **[STAGE-H.md](../stages/STAGE-H.md)** — Mimir learns to find code, check its work and run
  longer: twelve chunks (H1–H10, plus H4a and H4b) in four phases, each gated on a measurement. H1 (the check command), H2
  (finding code) and H3 (the benchmark of real work) built; H4 measured three coding models;
  Gate 2 chose Qwen3-Coder. H4a reads its tool calls, H4b offers it when a folder is chosen. H5 and H6 built. H7 built. H8 built. H9 built. H10 built — Stage H complete.
- **[STAGE-I.md](../stages/STAGE-I.md)** — the rename to **Bifrost** (Heimdall becomes **Vidar**,
  Mímir becomes **Mimir**), the turn loop moved into the shared crate, and **Hugin**, a terminal
  agent on it that works with Claude Code both ways. Eight chunks in four phases, each gated; where
  it stands is [I-status.md](I-status.md). Set 2026-10-12.

---

## Where things live in code

Four Rust crates, one shared React package and one app frontend. Line numbers drift; function
names don't, so grep the name if a number has gone stale.

> **Paths changed at Before-G0 step 1** (Stage G). Everything moved under `apps/`, `packages/` and
> `crates/`, and `heimdall-core` became `odin-core`. **And again at I0** (Stage I, 2026-10-12):
> `odin-core` became `bifrost-core`, `odin-cli` became `bifrost-cli`, and `apps/heimdall` became
> `apps/vidar`. The headings below are the current locations; the results docs keep the old ones.

### `crates/bifrost-core/` — shared by everything

- **What Ollama believes is loaded** — `ollama.rs` → `ps()`, `find()`
- **Load or release a model** — `ollama.rs` → `load()`, `unload()`, via `keep_alive: -1` and `0`
- **Start Ollama if it is down** — `ollama.rs` → `is_running()`, `ensure_running()`
- **What the card is *actually* holding** — `vram.rs` → `Card::open()`, `sample()`
- **The default prompt** — `config.rs` → `DEFAULT_PROMPT`, the only copy in source
- **Editing a file safely** — `edit.rs` → `Workspace` (containment), `plan_replace()`,
  `plan_create()`, `apply()`, `undo()`. Plan first, write the plan, refuse over a changed file
- **The tools the model is offered, and their wording** — `tools.rs` → `available()`,
  `system_prompt()`, `interpret()`, `prior_failures()` (loop-breaking)
- **Recovering a tool call Ollama could not parse** — `chat.rs` → `chat_tools()`, and
  `toolcall.rs` → `render_chatml()`, `parse_call()`. The G7 `"{` escaping failure, and its fix
- **Running the folder's check command** — `check.rs` → `candidates()`, `run()` (stoppable, kills
  the whole tree, output capped), `for_model()` (never reads as a failed call; names a Smart App
  Control block for what it is)
- **Finding code in the folder** — `search.rs` → `list()`, `search()` (literal, smart-case),
  `validate()` (an impossible call is refused before its card). Skips `.git`, `target`,
  `node_modules`, links; capped at 8 KB
- **One agent turn, the loop every frontend runs** — `agent/turn.rs` → `run_turn()`, behind a
  `Frontend` that shows cards, asks for decisions and reports Stop; `agent/round.rs` → `round()`
  (one request, its reply sorted); `agent/run.rs` → `run_approved()` (redeems a ticket). Mimir's
  `run_turn` command drives it and its window only shows what it says (I1)
- **The suite's colors, for the Rust side** — `colors.rs` → `palette()`, which reads
  `packages/ui/src/colors.json` at compile time. Hugin's Raven Violet comes from here.

> Everything here is **blocking by design**. Calling it from an `async` Tauri command drops a tokio
> runtime inside an async context and kills the worker. Go through `spawn_blocking`.

### `apps/vidar/src-tauri/src/lib.rs` — Vidar's Rust side

About 2,000 lines, and most of the app is in it.

- **Hotkey registration, and which keys exist** — `run()` (~1756), `popup_keys()` (~1384)
- **Summon and dismiss** — `toggle_popup()` (~1647)
- **Capture → Ollama → stream** — `ask_about_screen()` (~228)
- **Screen capture itself** — `capture_screen_blocking()` (~148)
  Must run in `spawn_blocking` — WebView2 puts the main thread in COM STA, and WGC needs MTA.
- **The invisibility feature** — `exclude_from_capture()` (~549)
  Re-applied on every show and read back by `report_affinity()` (~1307), never assumed.
- **Where the popup sits, and the pointer dodge** — `target_rect()` (~702)
- **The glide between positions** — `animate()` (~998)
- **Showing without stealing focus** — `show_without_focus()` (~846), `hide_now()` (~1202)
- **The one place focus *is* taken** — `take_focus()` (~1250), `return_focus()` (~1285)
  Only while the follow-up box is open, which is also the only moment `Esc` works.
- **`Ctrl+Alt`+wheel scrolling** — `mouse_hook()` (~1475), `watch_mouse()` (~1554)
  A low-level hook, installed on Tauri's main thread because that is the only loop that pumps.
- **Copying a block** — `copy_text()` (~1711)
  Written from Rust, because the webview is never focused.
- **How tall the popup should be** — `fit_window()` (~1935)
- **Where the logs go** — `redirect_log_to_file()` (~1732)
  The app is windowless, so this is the only way to see anything it printed.

### `apps/vidar/src-tauri/src/tray.rs` — the tray

- **The menu, and Off / Warming / Ready** — `install()` (~324), `Power`
- **Warming exists only here** — `/api/ps` has no "loading" entry, so only a long-lived process
  that knows it issued a load can show it

### `apps/vidar/src/` — Vidar's own React frontend

- **The popup's markup, header hints, pick-and-copy state** — `App.tsx`
- **The popup's chrome** — `App.css`. The renderer's styles are *not* here; see `@bifrost/ui` below.
  Three rules that select `.answer` descendants do stay, because they are Vidar behaviours rather
  than renderer ones: `.answer > .copy-pick` and the two `.content.collapsed .answer…` rules.

### `packages/ui/` — `@bifrost/ui`, the renderer both apps share

Extracted at Before-G0 step 1e, while Vidar was still its only consumer. Mimir imports the same
package from G1, so **one renderer means one security review rather than two that drift**.

- **Markdown rendering, and the security refusals** — `src/Answer.tsx`
  No raw HTML, `img` renders nothing, links render as plain text.
- **Syntax highlighting** — `src/CodeBlock.tsx`, Shiki, lazy, JS regex engine so no WASM
- **The renderer's styles** — `src/answer.css`
- **Proof the refusals hold** — `src/Answer.inert-check.tsx`, run by `npm run check:inert`
- **The suite's colors** — `src/colors.json`, exported as `@bifrost/ui/colors.json`. Each app's
  color for dark and light grounds, plus ink and ivory. Read by Mimir's default accent, by
  `bifrost-core::colors` and by `tools/icon/`, which draws the shields for each app's `icon.ico`
  (`node tools/icon/install.mjs --preview <dir>` to look at them all).

> **The component does not import its own CSS; the consumer does** — `import "@bifrost/ui/answer.css"`.
> `check:inert` SSR-builds `Answer` and runs the output under plain `node`, where a CSS import
> would fail outright.

### `crates/bifrost-cli/` — the CLI, binary `bifrost`

- **Capture, encode, stream, print** — `src/main.rs`
- **`warm` / `off` / `status`** — `src/power.rs`
- **The prompt, editable without a rebuild** — `prompt` in `%APPDATA%\bifrost\config.json`,
  the file every app shares

### `crates/bench/` and `evals/` — measuring models

- **Run model × image × prompt** — `crates/bench/src/main.rs`
- **Can the model call tools?** — `crates/bench/src/bin/toolprobe.rs` (G0); `--recover` scores
  the app's path, malformed-call recovery included (H4a)
- **Can the model edit files correctly?** — `crates/bench/src/bin/editprobe.rs` (G7), scored byte
  for byte on CRLF files: `cargo run -p bench --bin editprobe`
- **Does the check help it fix what it breaks?** — `crates/bench/src/bin/fixprobe.rs` (H1): eight
  tasks whose obvious edit breaks the build, with `run_check` and without
- **Can it find code nobody pointed it at?** — `crates/bench/src/bin/findprobe.rs` (H2): eighteen
  questions about this repo itself, in a git worktree at a pinned commit
- **Can it do real work?** — `crates/bench/src/bin/taskprobe.rs` (H3): sixteen tasks — four real
  commits and twelve function bodies removed — scored by the project's own hidden tests.
  `--selfcheck` proves every task solvable and not already solved, without a model
- **The loop they all share** — `crates/bench/src/probe.rs`: the app's own turn loop
  (`bifrost_core::agent::turn`, since I1) with every call approved, so a probe measures what ships
- **Mimir's file editing, end to end** — `tools/e2e/mimir-edit.mjs`. Drives the running app
  over its WebView2 debug port with the real model; 16 checks, leaves nothing behind
- **The check and finding code, end to end** — `tools/e2e/mimir-check.mjs`: approval, denial,
  Stop, a late result after switching threads, the fix loop, a search and a declined listing;
  24 checks, `--only N` for one scenario
- **The grading rubric** — `evals/cases.md`
- **Cases that regenerate from tracked source** — `evals/fixtures/`
- **Saved runs** — `evals/out/` (local and gitignored; see the note under Results)
- **The Node harness F1 was ported from** — `evals/bench.mjs`

> `evals/shots/` is gitignored on purpose. Screenshots of a real desktop can contain anything, so
> the eval set stays local.

---

## Results

Finished chunks: what was built, what was measured, what the measurement changed. Never rewritten
— a correction goes in the next one, with a note saying so.

`evals/out/` is local and gitignored, so a clone cannot open the saved runs the results docs name.
Each results doc quotes its numbers in full. The raw logs and `results.json` files stay on the
machine that ran them.

**Stage A — foundations, no GUI**

- **[A1](results/A1-results.md)** — toolchain. Rust 1.98.1 MSVC, Ollama 0.33.3, GPU at 152 tok/s
- **[A2](results/A2-results.md)** — the model bake-off, full 2×2.
  **Size decides accuracy; quantization decides nothing.** Chose `qwen3-vl:8b-instruct-q4_K_M`.
- **[A3](results/A3-results.md)** — screen capture. Verified genuinely WGC, not GDI
- **[A4](results/A4-results.md)** — the whole loop. 2,077 tokens in, 1.19 s to first token

**Stage B — the app shell**

- **[B1](results/B1-results.md)** — Tauri 2 + React + TypeScript. A window opens
- **[B2](results/B2-results.md)** — streaming over a `Channel`.
  **Never return the answer as a command's `Result`** — it buffers the whole thing.
- **[B3](results/B3-results.md)** — capture behind a button, exclusion verified against Heimdall's
  own screenshot
- **[B4](results/B4-results.md)** — hotkey and corner popup. Translucency is CSS alpha, not a DWM
  backdrop

**Stage C — make it real**

- **[C1b](results/C1b-results.md)** — making the answer readable, and the injection cases that
  must stay inert
  and Teams
- **[C4a](results/C4a-results.md)** — power control in the CLI.
  **off −8,193 MiB · warm +7,873 MiB in 2.2 s** — the deltas match, which is the test.

**Stage G — Mímir, the local chat app**

- **[G0](results/G0-results.md)** — the tool-call probe. **qwen3-vl:8b 10/10, llama3.1:8b 6/10** with a
  spurious call on every negative case. G6 is worth building, on the model Heimdall already uses.
- **[G1](results/G1-results.md)** — the spine. A conversation that remembers, streamed in fragments.
  **`ttft` measured 85.3µs until the clock was started before `.send()`** — then 150ms warm, 3.77s cold
- **[G2](results/G2-results.md)** — the switchboard. **`load_duration 0.00s`** is the proof the warm
  model survived a model switch; a runner is keyed by model *and* `num_ctx`
- **[G3](results/G3-results.md)** — Heimdall follows the config.
  **A model without `vision` does not refuse a screenshot** — it describes a screen it never saw
- **[G4](results/G4-results.md)** — conversations survive a restart. One JSON file each, no database
- **[G5](results/G5-results.md)** — attachments. **An image costs ~2,042 tokens whatever it shows**,
  because the model tiles it — so two images and a little history is most of 32k
- **[G6](results/G6-results.md)** — the tool loop. Off by default, no allowlist, no “always allow”;
  approval is a one-shot ticket redeemed in Rust against the exact path that was shown. *(H5 later
  let reads, listings and searches inside the folder run without asking; everything else still does.)*
- **[G7](results/G7-results.md)** — editing files. **76% → 98% byte-exact** over three probe
  rounds; search-and-replace beat whole-file rewrite, which corrupted lines it was never asked to
  touch. Found Ollama crashing on the model's own tool calls, and recovered them
- **[H1](results/H1-results.md)** — the check command. **33% → 79%** on edits whose obvious form
  breaks the build, once the model can run `cargo check` and read what it says
- **[H2](results/H2-results.md)** — finding code. Named questions about odin **0% → 88%**; the
  cross-file rename now finds its call site every time. Vague questions stay at 10%, and better
  wording did not move them — the case for H10's repo map
- **[H3](results/H3-results.md)** — a benchmark of real work: sixteen tasks on odin's own code,
  scored by its hidden tests. **The current model finishes 12%** — every single move works, strung
  together they mostly do not. H4's baseline
- **[H4](results/H4-results.md)** — three coding models on that benchmark. **Qwen3-Coder 30B-A3B
  52% and 5× faster, Devstral Small 2 50%, gpt-oss 23%**, against 12%. None but gpt-oss fits the
  card, and all are text-only — Heimdall goes blind while one is loaded. Gate 2 chose Qwen3-Coder
- **[H4a](results/H4a-results.md)** — reading Qwen3-Coder's own tool-call layout, which Ollama's
  parser loses whenever an edit contains `</tool_call>`. **28/48 with no errors**, against 25/48
  and 8 errors; the prompt it re-asks with matches Ollama's token for token
- **[H4b](results/H4b-results.md)** — choosing a folder offers the coding model, clearing it offers
  the way back. Found two real bugs in its own e2e before passing 14/14
- **[H5](results/H5-results.md)** — the approval policy. **Reads, listings and searches inside the project folder run without
  asking** — 2.5 cards per coding task instead of 3.9, none to find code — while edits, checks,
  outside reads, secret-looking and gitignored files still ask. `ask_every_call` turns it off.
  Reverses G6 in part
- **[H6](results/H6-results.md)** — undo a whole turn. Two or more edits in a turn end in an **Undo all**, asked twice, that
  checks every file's whole chain before writing anything and puts them back newest first; a real
  two-file turn came back byte for byte. Also fixed: a single Undo could save into the wrong
  conversation
- **[H7](results/H7-results.md)** — a context budget inside the turn. **Old tool results are set aside before a turn outgrows the context**,
  instead of Ollama silently cutting the request out of the prompt (measured). At 8k the request
  survives; at 32k the benchmark stays under budget and is unharmed (30/48 on, 26/48 off, no cuts)
- **[H8](results/H8-results.md)** — partial reads. **`read_file` reads a range of lines**, so the files over 64 KB — five of
  odin's own core files — can be read and edited. A 120 KB file edited at line 2102 byte for byte;
  the benchmark holds (29/48 with ranges, 28/48 without), at almost one more round a task
- **[H9](results/H9-results.md)** — file operations. **`create_file` makes folders; `delete_file` and `move_file` are new** —
  asked, undoable, and unable to overwrite anything (Windows' rename would). A new probe of renames,
  deletes, nested creates and moves passes 15/15, Undo all restoring every run byte for byte
- **[H10](results/H10-results.md)** — project notes, a task list, a repo map. **Project notes** (AGENTS.md, CLAUDE.md…, off until turned on per
  folder) **and a task list ship; the repo map was measured and left out.** Stage H's own finish
  line — rename across files, add a tested function — passed 2 of 3 real runs, Undo all restoring
  every one

**Stage F — keep testing models**

- **[F1](results/F1-results.md)** — the bench harness in Rust, validated against `bench.mjs`'s own
  numbers rather than just "it runs"

---

## Two conventions worth knowing before you change anything

**Nothing is measured against a constant.** Every VRAM figure here is a delta against a sample
taken moments earlier, because the idle baseline drifts — seen at 667, 900, 945, 985 and 1,033 MiB
inside a single session. Anything that hard-codes it is already broken.

**A successful bind is not proof a key works.** `RegisterHotKey` reports every `Ctrl+Alt+<letter>`
as free, because it only knows about other *global* registrations and never about what an
application binds internally. Only a real keypress proves it — which is what
[PENDING-CHECKS.md](PENDING-CHECKS.md) exists to track.
