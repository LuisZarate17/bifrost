# I6 — Check with Claude, in Mimir

Mimir can ask Claude to check a turn's changes, on a click: the one exception to Bifrost's no-cloud
rule ([STAGE-I.md](../stages/STAGE-I.md), Context). **Built 2026-10-17**, with I5 and Gate 2
skipped ("Move onto I6"); checked against the real `claude` on 2026-10-21.

## The open question, settled

The plan left one thing to decide in this chunk: whether `claude -p` works from the app or an API key
field is needed. It works on Luis's existing claude.ai login, so **there is no API key field**.
`claude auth status` answers in about 0.1 s with JSON and no model call, which gives the status line.

## How it works

- **The switch:** a **Claude** section in the Ctrl+, dialog. **Claude checks** is off by default
  (`claude_checks` in config.json). Its status line says "Signed in as …", "Not signed in" or that
  the command was not found — and with the switch off, "Not checked while Claude checks are off":
  `claude` is not even started to ask.
- **The button:** **Check with Claude** under a finished turn that changed files, only while the
  switch is on.
- **Before sending:** a row says exactly what goes — "Sends your question and 1 file's changes
  (305 B) to Claude" — with Send and Cancel. The size is the whole prompt, the question included
  (capped at 20 KB). A hash of that prompt goes with Send; if the files changed since, the check is
  refused: "The files changed since the preview — check again."
- **What goes:** the question and the turn's unified diff, built from the undo records (before: the
  earliest backup of each file, nothing for a create; after: the file on disk now). Binary and
  oversize files are named, not sent; the diff is capped at 300 KB.
- **The run:** `claude -p --output-format json --tools "" --no-session-persistence --safe-mode
  --strict-mcp-config --append-system-prompt <a reviewer's brief>`, the prompt on standard input, in
  an empty folder under `%TEMP%`, with no window, a job object, 180 s at most, and its own Stop.
- **The answer** is a card labeled **Claude**, saved with the conversation, and sent to the local
  model in later turns as "Claude's check of the last turn:", so "fix what Claude found" works. A
  failure is a card labeled "Check failed", not saved.

## The review

An independent review found that `--tools ""` turns off only Claude Code's built-in tools: the child
still loaded Luis's own CLAUDE.md, hooks and MCP servers, so **more than the question and the diff
would have gone out**. `--safe-mode` and `--strict-mcp-config` fixed it — tested on the real login,
the context sent fell from about 47k tokens to about 2.4k. Also fixed from the review: the preview not
matching what is sent (now the hash), the question missing from the preview, Stop shared with the
turns (a check now has its own, and the composer is locked while it runs), a Stop landing before the
check began, `claude.cmd` installs not found, a Smart App Control block reported as an unreadable
reply, and the answer's place in the thread taken from a stale index.

## Checked

- `cargo test -p bifrost-core` 545 passed, `-p mimir` 12; clippy clean on bifrost-core, bench, hugin
  and mimir.
- `mimir-claude.mjs` (a stand-in `claude`): 51/51 — off means no button, Cancel sends nothing, the
  arguments and the neutral folder, a file changed after the preview, Stop before and during a run, a
  turn's Stop leaving a check alone.
- Every other Mimir end-to-end script: edit 25/25, ops 33/33, bigfile 17/17, check 26/26, context
  8/8, notes 17/17, policy 47/47, switch 14/14, acceptance 3/3 runs.
- **The real `claude`, once**, driving the built app ([I-testing.md](../I-testing.md) §I6, 9/9): the
  status signed in, the edit, the confirm row, Cancel starting no `claude`, the card in 9 s, the
  command line and its folder read from the running process, the local model quoting Claude's review
  in the next turn, the card still there after a reload, and the buttons gone with the switch off.
