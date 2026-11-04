# G0 — can a local model call tools? · 2026-09-19

**Verdict: yes, and G6 is worth building.** `qwen3-vl:8b-instruct-q4_K_M` — the model Warden
already uses — scored **10/10**, including all four cases where calling a tool would have been
wrong.

**The surprise is which model failed.** `llama3.1:8b`, the one with the strongest tool-use
reputation, scored **6/10** and fired a spurious call on **every single negative case**.

| Model | Correct | Wrong tool | Bad args | Missed | **Spurious** | Pass |
|---|---|---|---|---|---|---|
| `qwen3-vl:8b-instruct-q4_K_M` | 10/10 | 0 | 0 | 0 | **0** | **100%** |
| `qwen3-vl:4b-instruct-q4_K_M` | 10/10 | 0 | 0 | 0 | **0** | **100%** |
| `llama3.1:8b` | 6/10 | 0 | 0 | 0 | **4** | 60% |

Run it yourself: `cargo run -p bench --bin toolprobe -- --model <tag>`

---

## What was measured

Ten prompts against three tools — `get_time` (no arguments), `read_file(path)` (one string),
`add(a, b)` (two numbers). Six prompts need a tool; **four must not produce one**.

Per model: does the API accept a `tools` array at all, how many calls come back well-formed, how
many carry correct arguments, and how often a call is invented when none was needed.

**Temperature 0, and that changed the answer.** The first pass left sampling at Ollama's default
and scored llama3.1 at 7/10. At temperature 0 it scores 6/10 — the extra point was luck. Two
consecutive runs now produce byte-identical verdicts. *A verdict that moves between runs is not a
measurement*, which is the same reason `evals/` pins its prompt files.

## The negative cases are the whole test

A model that calls a tool every time is as useless as one that never does, and **nothing will tell
you which you have except reading the output** — a spurious call is a perfectly well-formed reply.
It has an id, a real tool name, and arguments that parse. Only knowing the question was answerable
without it makes it wrong.

llama3.1 failed all four, always reaching for `read_file`:

| Prompt | What it did |
|---|---|
| What is the capital of France? | called `read_file` |
| Explain in one sentence what a borrow checker does. | called `read_file` |
| Write a haiku about rain. | called `read_file` with `{"path": "/path/to/haiku.txt"}` |
| What does the Rust keyword `impl` mean? | called `read_file` |

The haiku case is the clearest: **it invented a placeholder path** for a file that does not exist,
to write a poem it could simply have written. In a real loop that is a round trip, a file-read
error fed back as context, and an answer built on a failure.

**Offering fewer tools changed the behaviour.** With only `read_file` available, llama3.1 answered
the borrow-checker prompt in prose — correctly. With all three offered, it called the tool. Worth
knowing before G6 grows the tool list: this failure gets worse as more tools are offered, not
better.

## Consequences

- **G6 is worth building**, on `qwen3-vl:8b-instruct-q4_K_M`. No model switch is needed — the tag
  Warden already uses handles the chat window, screenshots and tool calling at once, which is what
  makes one resident model affordable (STAGE-G decision #4).
- **`qwen3-vl:4b-instruct-q4_K_M` also scores 100%**, so a smaller resident model stays an option
  if VRAM is ever contested. Note A2's finding that size decides accuracy — 4B being perfect on
  *tool selection* says nothing about answer quality.
- **`llama3.1:8b` must not be used for tool calling here**, on this evidence. It remains the useful
  negative test for G3's capability check: `/api/show` reports `completion, tools` for it and
  **not** `vision`.

## What `/api/chat` actually looks like — previously unmeasured

STAGE-G flagged the request shape as the one thing in the packet that had not been verified.
Measured against Ollama 0.33.3:

```jsonc
// response to a prompt that needs a tool
{
  "message": {
    "role": "assistant",
    "content": "",                     // empty when it calls a tool
    "tool_calls": [{
      "id": "call_omx1ktai",
      "function": { "name": "add", "arguments": { "a": 847, "b": 1259 } }
    }]
  },
  "done_reason": "stop",               // "stop", not a distinct tool-call reason
  "total_duration": 349866800,         // same metrics envelope as /api/generate, in ns
  "load_duration": 1018200
}
```

Three things worth carrying into G1 and G6:

- **`arguments` is a JSON object, not a string.** OpenAI's API serializes it and makes the caller
  parse again; Ollama does not. Code ported from there will double-parse and fail.
- **`message` replaces `response`.** Otherwise the envelope and its metrics match `/api/generate`.
- **`done_reason` is `"stop"` even when a tool was called**, so it cannot be used to detect one.
  Check whether `tool_calls` is present.

## Capabilities, per `/api/show`

| Model | Capabilities |
|---|---|
| `qwen3-vl:8b-instruct-q4_K_M` | `completion, vision, tools` |
| `qwen3-vl:4b-instruct-q4_K_M` | `completion, vision, tools` |
| `llama3.1:8b` | `completion, tools` — **no `vision`** |

**A declared capability is not a working one.** All three report `tools`, and one of them cannot be
trusted with them. `/api/show` is good enough to *label* a model in G2's picker and to *refuse*
cleanly in G3 — it is not evidence the model is good at the thing it claims.

## What this did not test

- **Multi-turn tool loops.** One call, judged in isolation. G6 feeds the result back and continues;
  nothing here says the model handles its own tool output sensibly.
- **Parallel calls.** The probe judges the first call when several come back, and none did here.
- **Tools plus an image** in the same request, which is what Warden would need if tool calling ever
  reached the popup.
- **Answer quality.** Selecting the right tool is not the same as being useful, and A2 is explicit
  that size decides accuracy.

## How it was built

`odin-core::chat` — `/api/chat` with messages, tools, and typed `tool_calls` — was written for this
rather than at G1, so the client would land with a probe depending on it instead of a whole app.
`bench`'s own harness could not run G0: it goes through `/api/generate`, which cannot carry a
`tools` array at all.

The probe is `crates/bench/src/bin/toolprobe.rs`.
