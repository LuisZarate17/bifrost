# I4c — Hugin goes full screen

Asked for in use, from screenshots of Hugin in a split VS Code terminal. **Built 2026-10-16 to
2026-10-18**; checked by Luis in VS Code on 2026-10-21, all steps passing.

## What was wrong

Two things showed in a narrowed pane: the raven came apart into bars with blank rows between them,
and long answer lines broke in the middle of a word. Both had one cause. ratatui's `insert_before`
writes every cell of a scrollback row, so each line was stored as spaces out to the terminal's edge,
and when the pane got narrower the terminal wrapped those lines again. The first fix drew each row
only to its last visible cell; the next pinned the input box to the bottom, Claude Code style. Then
Luis asked for his own messages to stand out, and for the last one to stay on the top row once it
has scrolled off, "like in Claude".

## The decision

A line pinned to the top of an **inline** interface cannot work: whatever scrolls off past it never
reaches the terminal's scrollback. Offered the choice — the last message shown above the input box,
staying inline, or the top row with Hugin full screen — **Luis chose full screen**, knowing the
terminal's own scrollback and search no longer see Hugin's conversation. This reverses STAGE-I.md's
"inline, not a full-screen app" (Decisions, 4), on his word; the plan itself is history and is left
as written.

## How it looks and works now

```
› list every file under apps/hugin/src with one sentence each      ← pinned once it scrolls off
1. `frontend.rs` - …
   …answer rows…
──────────────────────────────────────────────────────────
› 
──────────────────────────────────────────────────────────
  enter send · shift+enter new line · esc stop · ctrl+d quit · shift+drag select
```

- **The alternate screen**, with Hugin keeping its own history (`transcript.rs`): everything said is
  laid out once per width, so a resize simply lays it out again.
- **Your messages** have a blank line above them, a bold violet `›`, and a gray band across the
  width. With `NO_COLOR`: the blank line and bold only.
- **The pinned row:** the latest prompt at or above the top of the view, drawn on the top row while
  its own first row is out of sight.
- **Scrolling:** the wheel (3 rows), PgUp/PgDn (a page), Ctrl+Home and Ctrl+End; Home and End stay
  with the input. At the bottom the view follows new output; scrolled up, it stays put and the hint
  line says `↓ new below · ctrl+end`. Sending a prompt or an approval brings it back to the bottom.
- **Selecting text** is Shift+drag, because Hugin has the mouse for the wheel.
- **Leaving** — Ctrl+D, `/exit`, an error or a panic — puts the terminal back (main screen, mouse
  off) and prints a dim `hugin · session saved`.
- Line mode, which scripts read, is unchanged.

## Checked

- `cargo test -p hugin`: 116 passed (plus 6 in `hugin-launch`); clippy clean.
- A real Windows Terminal, by script: a 19-file answer with the prompt pinned on the top row; the
  real mouse wheel and PgUp; a narrow window; Ctrl+D back to a clean shell, with no escape codes
  printed when the mouse moved over it afterwards.
- Luis in VS Code's terminal: [I-testing.md](../I-testing.md) §I4c, every step.
