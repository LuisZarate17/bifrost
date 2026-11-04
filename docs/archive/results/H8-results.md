# H8 — Partial reads

`read_file` can read part of a file, so the files too big to read whole are no longer out of reach.
**Built 2026-10-03.**

## The problem

A read over 64 KB was refused outright — by design, since half a file answered confidently is worse
than being told it was too big. But five of odin's own core files are over that line:

| File | Size |
|---|---|
| `apps/heimdall/src-tauri/src/lib.rs` | 87 KB |
| `crates/odin-core/src/edit.rs` | 84 KB |
| `apps/mimir/src/App.tsx` | 81 KB |
| `apps/mimir/src-tauri/src/lib.rs` | 79 KB |
| `crates/bench/src/bin/taskprobe.rs` | 69 KB |

`edit_file` changes files up to 1 MiB, but it needs the exact text, and only a read gives it. So
Mímir could not change the files it would most often be asked to change.

## What it does

- **`read_file(path, start_line?, end_line?)`**, 1-based and inclusive. The result opens with one
  header line, `[lines 2100–2105 of 5992]`, and then the lines exactly — **never numbers in front of
  each line**, which a small model copies into `old_text`. A range that would pass 64 KB is cut at
  the last whole line and says so: `…; cut at 65536 bytes — read on from line 1201]`. The header
  names no path (a Windows path copied into JSON breaks it) and its numbers have no separators.
- **A file over 64 KB read whole** answers with an error that gives its size and line count and says
  how to read it in parts; search already gives line numbers. It stays an error, so a model that
  keeps asking for the whole file is stopped by the repeat rule.
- **The same rules as search:** a NUL in the first 8 KB means binary (or UTF-16) and is not read as
  text — whole reads included, a change: UTF-16 text used to come through garbled. A range can be
  read from files up to 1 MiB, where search and editing stop too. Line numbers match search's
  `path:line:` and what `edit_file` matches, CRLF and BOM files included.
- **Numbers as the model sends them:** `120`, `"120"`, `" 180 "`, `120.0`. A start of 0 means line 1.
  The recovery of malformed JSON calls (G7) now reads a bare number for an integer argument instead
  of losing the whole call; Qwen3-Coder's layout already typed them by the schema.
- **The range is kept everywhere it matters:** in the approval ticket (the window cannot change it —
  Allow reads what the card showed), on the card (`lines 2100–2105`), in the replay of later turns
  (the range Rust read, never the model's raw arguments), and in H7's set-aside notes, which now
  treat two ranges of one file as two reads.
- **A header pasted into an edit** is refused with a message that says exactly that, rather than
  "not found". It is never stripped silently: `new_text` may carry it too, and would write it into
  the file.

## Measured

**Same build, `taskprobe --read parts` (H8) then `--read whole` (the read before H8)**, Qwen3-Coder.
The bar, set before the run: parts at least 26/48 at the app's sampling, with no errors.

| | Ranges on | Ranges off |
|---|---|---|
| Pass, app sampling | **29/48** (60%) | 28/48 (58%) |
| Pass, temperature 0 | 9/16 | 10/16 |
| Errors · runs cut | 0 · 0 | 0 · 0 |
| Rounds per run | **6.2** | 5.3 |
| Time per run | 40.8 s | 35.4 s |
| Ranged reads | 21 (+5 at t=0) | — |

**It clears the bar, and the pass rate holds.** Every file in the benchmark is under 64 KB, so it
cannot show the gain; the e2e below does. **What it does show is a cost:** offered ranges, the model
sometimes reads a small file in parts — 21 times in 48 runs — and a task takes almost one more round
and about five seconds longer. Not a harm to the result; worth watching, and the description could
be tightened if it matters.

**findprobe** (the read description changed): 47/54 — named 24/24, vague 15/21, no tool 8/9 —
against 48/54 before; 16/18 at temperature 0, the same as before. Finding code is unaffected.

Saved in `evals/out/H3-task-1791058513/` (parts), `…1791061039/` (whole) and
`evals/out/H2-find-1791063639/`; logs in `evals/out/H8-2026-10-03/`.

**The prompt is still Ollama's.** The integer types and number arguments are new to Qwen3-Coder's
render (H4a), so the render-parity test gained a conversation with a ranged read: native and raw
gave the same prompt token count (1,164), as did the other three.

## End to end — `tools/e2e/mimir-bigfile.mjs`, 18/18

On a 120 KB file with CRLF endings, 5,992 lines:

1. "How many lines?" — the whole read fails, unasked, with *120060 bytes and 5992 lines*, and the
   answer gives the count.
2. A change inside a function at line 2102: the model read **lines 2100–2105** (an automatic read,
   its card showing the range, the result opening with `[lines 2100–2105 of 5992]`), then edited;
   approved, the file changed exactly there and was byte for byte the same elsewhere, every line
   ending still CRLF.
3. A gitignored big file read with a range: the card asked, showing **lines 2999–3004**, and Allow
   read exactly that range.

The other scripts on the final build: mimir-context 8/8 · mimir-edit 25/25 · mimir-check 26/26 ·
mimir-policy 44/44 · mimir-switch 14/14.

## Checked

| | |
|---|---|
| Unit tests | odin-core 270 (+28: argument parsing in every spelling, every read case — ranges, clamps, cuts, CRLF/BOM, a lone `\r`, no final newline, empty, binary, over 1 MiB — search-then-read agreement, both read paths identical, the header refusal, bare-number repair, ranged set-aside) · mimir 29 · bench 24 · clippy clean |
| Render parity, live | 4/4 conversations, ranged read included |

## Review

A checker found nothing that touches safety: every ranged read still goes through H5's checks (the
open handle's path and link count, network paths never resolved before Allow), every read is bounded,
and the range lives in the ticket. Fixed from it: a card explaining a missed edit in a big file said
"the lines it asked for make no sense", because the new advice mentions `start_line`; and the error
for a bad line number said "1 or more" though 0 is accepted as 1. Accepted: `120` and `"120"` count
as different calls for the repeat rule — at most one extra round. (The benchmarks ran on the build
before these two wording fixes; neither touches a valid read.)

## Accepted

- **More rounds** when the model pages small files (above).
- **No relief for H7 on files of 30–64 KB:** they are still read whole, since the description only
  invites ranges past 64 KB.
- **UTF-16 text is refused as binary** where it used to arrive garbled.

## Files

`crates/odin-core/src/tools.rs` (`Lines`, `line_arg`, `read_text`, the schema, both read paths) ·
`edit.rs` (the header refusal, the big-file hint) · `toolcall.rs` (bare numbers) · `compact.rs` (keyed
by range) · `chat.rs` (the parity conversation) · `xmlcall.rs` (a test) ·
`apps/mimir/src-tauri/src/lib.rs` (range in the ticket and the replies) · `App.tsx`, `ToolCard.tsx`,
`App.css`, `preview.ts` · `crates/bench/src/probe.rs`, `bin/taskprobe.rs` (`--read`, ranged reads) ·
`tools/e2e/mimir-bigfile.mjs` (new)
