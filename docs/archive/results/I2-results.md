# I2 — Hugin, in the terminal

Hugin is the third app: a coding agent that runs **inline in a normal terminal**, like Claude Code,
on the same turn loop Mimir runs. It opens in the folder it was started in, uses the coding model,
reads the project's notes, runs reads and searches on its own and asks before an edit or a check.
**Built 2026-10-15**, in three commits on `stage-i`: `77bf850` (the core and a line mode),
`98bbd8f` (the inline interface), `ecc7886` (the review's fixes).

## What you see

```
hugin · greetdemo · main, 2 changed · qwen3-coder:30b
› Rename greet to welcome in src/greet.rs only.
● read src/greet.rs → 13 lines
? edit src/greet.rs +2 −2
  -pub fn greet(name: &str) -> String {
  +pub fn welcome(name: &str) -> String {
       format!("Hello, {name}!")
   }
  ⋯
approve? [y/n/a]
```

- **One header line**, then everything finished goes into the terminal's own scrollback. Only the
  status line and the input box at the bottom are redrawn (ratatui's inline viewport over
  crossterm), so the history scrolls and copies like any other output.
- **Tool lines** say a verb, a target and an outcome: `● search "greet" → 6 hits`,
  `● edit src/lib.rs +2 −2 → applied`, `● check cargo check → failed (exit 101)`.
- **Approvals are inline**: the diff (green and red, a `⋯` between hunks), the command for a check,
  the full path for a read outside the folder — then `approve? [y/n/a]`.
- **Color is quiet**: Raven Violet only on `hugin`, the `›` prompt, approvals and the `●` of a result.
  `NO_COLOR` removes every color.
- **Keys**: Enter sends; Shift+Enter, Alt+Enter, Ctrl+J or a `\` at the end of a line adds a line (VS
  Code's terminal cannot tell Shift+Enter from Enter, so the others are there for it); Esc or Ctrl+C
  stops a turn; Ctrl+C at an idle prompt clears it; Ctrl+D or `/exit` leaves. Up and Down walk the
  session's earlier prompts.
- **The status line** says what the turn waits for and for how long: `thinking · 12s`,
  `running cargo check · 4s`, `approve? [y/n/a]`; idle, the keys.

## How it is built

- **The turn is bifrost-core's** (`agent::turn::run_turn_with`, I1): Hugin is one more `Frontend`.
  Each round reads the config fresh with Hugin's folder as the workspace and its project notes on,
  in memory only — Mimir's chosen folder and notes switches are never touched. The folder's check is
  the one chosen for it in Mimir, if any.
- **One seam for two interfaces** (`ui.rs`): the **line mode**, plain text over stdin and stdout for
  scripts (`› ` before a prompt, `approve? [y/n/a] ` before an answer, `· stopped`, `/stop`), and
  the **inline interface** for a person, chosen when stdin and stdout are a terminal (`--line`
  forces the other). The session runs on a worker thread; the interface talks to it over one
  channel, so nothing arrives out of order.
- **Stop** is one handle the keyboard, the session and the loop share. It also reaches the shared
  Stop counter, so a running check is killed with everything it started.
- **Sessions** are saved after every turn, marked `app: "hugin"`. `store::list()` returns only
  conversations without an app, so Mimir's sidebar is exactly what it was.

## Checked

| | Result |
|---|---|
| Stage H's finish line — rename `greet` across files, add `shout` with a test, `cargo test` passes — through Hugin's line mode and the real model, every approval answered `y` | **3/3**, 14–21 s a run |
| Stop at an approval | denied, nothing written, `· stopped` |
| Stop while the model thinks | `· stopped` in 2 s, nothing asked |
| Stop during a running check (`ping -n 60`) | `· stopped` in 125 ms, no process left |
| **A real Windows Terminal window**, driven through its console's own input and read back from its screen | the header and colors as above; an approval answered `y` and written to disk; a new line by Shift+Enter, Alt+Enter and Ctrl+J; Ctrl+C clearing the input; Esc while thinking → `· stopped`; Ctrl+D leaving; `NO_COLOR` honored |
| Tests | hugin 66 (render tests on ratatui's test backend, the input editor by key sequences, whole sessions with scripted keys and scripted rounds); core 496, mimir 11; clippy clean |

The first launch in the real terminal came out without color. The shell it was launched from had
`NO_COLOR=1` set, so that was Hugin doing as told; launched without it, the violet is there.

## The review

An independent review checked six guarantees: nothing runs unapproved, Stop at every point, the
terminal always put back, the line mode scriptable, sessions, and threads. It found two problems,
both fixed in `ecc7886`:

- **A key typed ahead could answer an approval.** A `y` typed while the model was thinking could reach
  an approval that arrived a moment later, before anyone had seen its diff. **An approval now takes
  y, n and a only once it has been on screen for 300 ms**; keys before that go to the input, as
  anything typed during a turn does. Esc still stops at once.
- **In line mode, a `/stop` piped right behind its prompt could be lost**, and stay queued to deny
  some later approval. **A `/stop` is never queued now**: it stops the turn that runs, or the turn
  whose prompt is still waiting, decided under the same lock a prompt is handed out under. With
  nothing to stop it is dropped.

## Different from the packet

- **Answers arrive whole, not streamed.** The shared loop's request is not streamed — a tool call
  arrives whole or not at all, and Mimir's tool turns are the same. The status line shows
  `thinking · Ns` meanwhile. Streaming the final answer means changing the shared round for both
  apps; it is left as a later step.
- **`a` allows like `y` for now.** "Allow this exact command for the session" needs memory, which
  comes with shell commands in I3.
- **VS Code's terminal was not driven**: it is the editor this work happens in. It is the first hand
  check below.

## For a person

[I-testing.md](../I-testing.md) §I2: a real coding task in Hugin from start to finish, in Windows
Terminal and in VS Code's terminal — the packet's "done when".

## Next

I3: shell commands, approved before they run, with `a` remembering an exact command for the session.
