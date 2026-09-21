# G3 — Warden follows the app, and refuses cleanly when it cannot see

Switch the model in Citadel, press the hotkey, and the popup follows — no restart.
**Built 2026-09-19.**

This is the chunk the whole stage started from. **No IPC, no file watching, no daemon:** Warden is
summoned, so it re-reads the config when it wakes up. Step 3 had already made the popup do that;
G3 finishes it and adds the half that is new.

## Measured

| | |
|---|---|
| `/api/show`, uncached | **3.02 ms** |
| The same call, cached | **4.10 µs** — ~740× |
| `llama3.1:8b` capabilities | `completion, tools` — **no `vision`** |

## A model without vision does not refuse a screenshot

This is the finding, and it is worse than an error.

**Ollama drops the image and answers the prompt anyway.** A model with no vision capability
produces a fluent, confident description of a screen it never saw. Nothing about the output looks
wrong — there is no error, no warning, and no missing field. You would simply believe it.

So the check happens **before the capture**, not after. The popup names the model, says it cannot
see images, and points at the fix.

`llama3.1:8b` is the case that proves it, and it is the same model G0 caught firing spurious tool
calls. Two independent failures from one tag is why it stays installed: it is this project's
negative test.

## An unreachable `/api/show` does **not** block the request

Capabilities are a label. Refusing to answer because a *secondary* lookup failed would be a worse
failure than the one the check guards against — the screenshot path would break every time Ollama
was briefly busy, to protect against a case that only matters for one model.

So a failed lookup lets the request through, and **failures are not cached**. Ollama being down is
temporary; remembering "unknown" would outlive the outage.

## Why the cache exists at all

**3.02 ms against 4.10 µs.** Three milliseconds is nothing in isolation, and it would be easy to
argue the cache is premature.

It is not, because of *where* it lands: before the screenshot, on the summon path, in the app whose
entire appeal is being instant. The popup's budget is spent on the capture and the model. A round
trip added ahead of both is the wrong place to spend one, however small — and it is a round trip
that can also hang, which a cached lookup cannot.

Cached per tag, in `odin-core`, so the CLI gets it too.

## What needed a human

Pressing the hotkey after switching models, and seeing the refusal for a model with no vision. In
[PENDING-CHECKS.md](../PENDING-CHECKS.md).
