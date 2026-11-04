# H4a — Reading Qwen3-Coder's tool calls

Gate 2 chose Qwen3-Coder 30B-A3B, on one condition: first stop Ollama losing its tool calls.
**Built 2026-10-02.**

## The problem, as H4 measured it

Qwen3-Coder does not write JSON calls. It writes tags:

```text
<tool_call>
<function=edit_file>
<parameter=path>
src/main.rs
</parameter>
<parameter=old_text>
…
</parameter>
</function>
</tool_call>
```

Ollama 0.33.3 parses this itself (`model/parsers/qwen3coder.go`), and lost **8 of the 64 runs** in
H4 to it, in two ways:

- **A value that contains `</tool_call>` ends the call there.** The parser cuts at the first
  `</tool_call>` it sees, wherever it is, and Go's XML decoder then hits the end of an unclosed
  `<parameter>`: `HTTP 500 XML syntax error on line 45: unexpected EOF`. Every one of these came
  from editing `toolcall.rs`, whose code is full of that very tag — and the turn dies, so the run
  is an ERROR, not a fail.
- **A call without its opening tag is not a call.** In the tool probe the model once wrote
  `<function=read_file>…` with no `<tool_call>` before it, and Ollama passed the whole thing
  through as the answer's text.

Mímir already had a recovery for this kind of thing (G7, `toolcall.rs`), but it renders Qwen's
JSON layout. Qwen3-Coder's family is `qwen3moe`, so it would even have been sent down that path —
with a prompt it was not trained on.

## What was built

- **The layout is read, not guessed.** `/api/show`'s modelfile names the model's tool-call parser
  (`PARSER qwen3-coder`; the vision models say `qwen3-vl-instruct`). `ModelDetail.parser` carries
  it, and `chat::chat_tools` picks the recovery by it, before any family check.
- **`xmlcall::render`** — Ollama's Go renderer for this model, ported line for line: the default
  system text, the tool list as XML, past calls as tags, consecutive tool results in one user turn,
  and Go's `json.Marshal` escaping of `<`, `>` and `&` where it applies.
- **`xmlcall::extract_calls`** — a parser that knows what Ollama's does not: the names of the tools
  offered and of each tool's arguments. A call starts at `<function=NAME>` only for an offered
  `NAME`; a value ends at a `</parameter>` followed by another argument of the same tool, or at the
  last one before `</function>`. So `</tool_call>`, `</parameter>` or even `</function>` inside an
  edit are just text. Values are trimmed and typed exactly as Ollama's `parseTypedToolValue` does.
- **Three triggers**, native first always:
  1. Ollama answers `500` with an XML error → the same turn is asked again through raw
     `/api/generate` in the model's own layout, and read by `extract_calls`.
  2. Ollama answers with no calls but the text holds `<function=` → read out of the text, no
     second request.
  3. A bare `{"error":"EOF"}` (found in this chunk's first run, below) → as 1.
- **`toolprobe --recover`** scores the app's path; plain `toolprobe` still scores Ollama's alone.

The remaining ambiguity is the same class as G7's: a value that itself contains `</parameter>`
followed by `<parameter=` and another argument *of the same tool*, or a later `<tool_call>`
followed by an offered tool's name, is split there. Every call still goes to the user for approval.

## Is the render the prompt Ollama builds?

Proved by sending the same conversation both ways — native `/api/chat`, and raw `/api/generate`
with our render — and comparing the prompt token counts Ollama reports. An `#[ignore]` test in
`chat.rs`, `cargo test -p odin-core --lib -- --ignored`:

| Conversation | Native | Raw render | Same tool called |
|---|---|---|---|
| One request: replace a word in a file | 970 tokens | 970 | `read_file` |
| A call, its result, a follow-up | 1,013 | 1,013 | `edit_file` |
| System prompt, words before two calls at once, two results back to back, markup in a result | 1,071 | 1,071 | `edit_file` |

**The prose is not compared, on purpose.** Sent three times natively, one identical request came
back with the same 490 prompt tokens each time, but its prose diverged after about 30 tokens
between a cold and a warm prompt cache. The third conversation's `new_text` likewise came back as
`1` one way and `return 1;` the other. So the token count is the fidelity check.

## Tool probe

| | |
|---|---|
| Qwen3-Coder, Ollama's parsing alone | 9/10 — case 4 missed, the call passed through as text |
| **Qwen3-Coder, the app's path** | **10/10** — the one call read back out of the text |
| qwen3-vl 8B, the app's path | 10/10 — the vision model's recovery is unchanged |

## The benchmark

The same benchmark and conditions as H4: sixteen tasks, 3 runs at the app's sampling plus 1 at
temperature 0, `num_ctx` 32768, every Phase 1 tool. `cargo run -p bench --bin taskprobe -- --model
qwen3-coder:30b`; logs in `evals/out/H4a-2026-10-01/`.

| `qwen3-coder:30b` | App sampling | Temperature 0 | Errors | Recovered calls |
|---|---|---|---|---|
| H4, before this chunk | 25/48 (52%) | 9/16 | **8** | — |
| First run, before the bare-EOF trigger | 28/48 (58%) | 9/16 | 2 | 13 |
| **Run of record, 2026-10-02** | **28/48 (58%)** | **9/16** | **0** | 10 |

Run of record in detail: real commits 7/12, removals 21/36, 5.5 rounds and 1.3 checks a run, 41 s
a run. Saved in `evals/out/H3-task-1790966450/`.

| Task | H4 | First run | Record |
|---|---|---|---|
| a01-pinned | 2/3 | 3/3 | 3/3 |
| a02-tools | 0/3 | 0/3 | 0/3 |
| a03-attachment | 2/3, 1 error | 3/3 | 3/3 |
| a04-config | 1/3 | 2/3 | 1/3 |
| b01, b02, b07 | 0/9 | 0/9 | 0/9 |
| b03, b04, b09, b10, b12 | 15/15 | 15/15 | 15/15 |
| b05-render-chatml | 0/3, 2 errors | 0/3, 1 error | 0/3 |
| b06-extract-calls | 0/3, **3 errors** | 0/3 | 0/3 |
| b08-unescape | 2/3 | 3/3 | 3/3 |
| b11-build-files | 3/3 | 2/3 | 3/3 |

**What it bought, honestly.** Every turn now ends in an answer or a real attempt — no run of the
64 died on a tool call. The score moved only from 25 to 28: `b05` and `b06`, where the errors
were, now fail on the model's own code rather than on Ollama's parser, and the gains are on tasks
that passed some of the time already. The point of the chunk was the dead turns, and those are
gone; the score says the model, not the plumbing, is now the limit on those two tasks.

**Why the first run was not the record.** It had two errors. One was a shape nobody had seen:
`HTTP 500 {"error":"EOF"}` — the model wrote `<tool_call>` and `</tool_call>` in its prose before
its real call (the task was about that very layout), Ollama took the text between them as a call,
and Go's decoder found nothing in it. `extract_calls` already skips such mentions, so the fix was
only to recognise the error, matched as the whole body so that an `EOF` inside another message
(a runner dying mid-request) is still an error. The other was exactly that: Ollama's runner
dropped its connection at temperature 0 (`wsarecv: An existing connection was forcibly closed`) —
infrastructure, not a tool call, and it did not recur. The whole benchmark was then run again on
the final code.

**41 s a run against H4's 32 s.** Each recovery asks the model a second time, and runs that used to
end in an error now run to their end. But the first run of nearly the same code averaged 35 s, so
part of the gap is run-to-run variation, not the recovery.

## Review

A checker read the diff by itself. Nothing broke; fixed from it: the unknown-tool failure is now
worded like every other one the model hears back, and one doc comment claimed more than the code
did. One limit is left as it was in the JSON path: when a reply holds a call that parses and a
second that does not, the second's failure is not reported — Mímir acts on a reply's first call
only.

## Files

`crates/odin-core/src/xmlcall.rs` (new) · `chat.rs` (layouts, triggers, the live test) ·
`ollama.rs` (`ModelDetail.parser`) · `crates/bench/src/bin/toolprobe.rs` (`--recover`)
