# J0b — Tool-call repair for every model

Before a model bake-off can be fair, every model needs the same safety net. **Built 2026-10-25.**

## The problem

When a model writes a tool call Ollama can't read, Ollama answers HTTP 500. Until now only Qwen models
got a second chance (a raw retry the engine repairs itself: ChatML for Qwen, XML for Qwen3-Coder);
for any other model — Mistral, gpt-oss, Gemma, Ornith, Flint — **the whole turn ended**. H4's bake-off
counted those as plain failures, which made non-Qwen models look worse than they are.

## What changed

- **Every model's unreadable call is fed back.** The model is told what Ollama could not read and asked
  to "send one valid tool call"; the turn goes on. The existing three-identical-failures rule, and the
  round bound, stop a model that keeps failing.
- **Qwen keeps its better repair.** The repair is chosen by the model's own format first
  (`retry_layout`): Qwen3-Coder retries XML-shaped errors as XML — including Go's `XML syntax error …
  invalid character entity`, which used to be mistaken for a JSON error — and other Qwens retry JSON
  errors as ChatML. Everything else is fed back.
- **Real failures still end the turn:** a runner that died, out of memory, a CUDA error, a missing
  model, a refused connection, a timeout, a bare `EOF` from a non-Qwen model. Ollama echoes the model's
  call inside its error, so that echo is cut out before these words are looked for — code containing
  "timed out" is not a crash.
- **Capped:** what goes back to the model is at most 1,500 bytes, with a note of how much was cut —
  Ollama's echo can be a whole file.
- **Counted:** `fed_back` beside `recovered`, in the turn's numbers, the probes and Mimir's log.
- **For the runs:** `hugin -p` and `hugin --ask` take `--model <tag>` (that run only, never saved) and
  `--trace <file>`, one JSON line per request: tokens, durations, recovered, fed back, calls in the
  reply, and the error or fed-back reason. A trace that can't be written never stops the run.

## Checked

- Tests: bifrost-core 611, hugin 231, bench 199, mimir 12; clippy clean.
- Table tests for the classifier (parse error or real failure, by wording), for the repair choice
  (model format × error), for the echo being ignored, and for the cap.
- A scripted turn: a model's first reply is unreadable, it is fed back, and its next, valid call runs;
  a third identical unreadable call ends the turn.
- Not yet against a live model: Ollama is upgraded in J0c, and the bake-off's first run is the check.

## The review

An independent review confirmed the main guarantees — real failures still end the turn, and no path
loops without bound — and found, now fixed: the XML entity error skipping Qwen3-Coder's own repair; the
model's echoed code being read as a crash; the fed-back text not being capped; the trace missing the
fed-back reason; and a bare `EOF` whose retry failed returning the wrong message.
