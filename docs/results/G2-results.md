# G2 — The switchboard

A panel listing every installed model, what it can do, whether it is resident, and buttons to load
or release it. **Built 2026-09-19.**

This is what makes Citadel a hub rather than a chat box. The choice is **written to the shared
config**, which is the single source of truth for "current model" — the CLI follows it already,
Warden follows it at G3.

## Measured

| | |
|---|---|
| Models read via `/api/show` | **5 of 5**, correctly |
| `llama3.1:8b` | `completion, tools` — **no `vision`**, the negative case G3 needs |
| `qwen3-vl:8b` own maximum context | **262,144**, against the 32,768 that fits on this card |
| Loading, card | **469 → 11,830 MiB** |
| The same load, per `ollama ps` | **9,737 MiB** — a **~2,100 MiB** disagreement |
| A conversation after the switchboard loads | `load_duration` **0.00 s** |

## The two numbers that decide things

**262,144 against 32,768.** `/api/show` carries more than capabilities — it reports the model's own
maximum context. That is what tells *"this model cannot do that"* apart from *"this card cannot
afford it"*, which are different problems with different fixes and look identical from the UI.

The context key is **namespaced by architecture** (`qwen3vl.context_length`, `llama.context_length`),
so it is found by suffix. Hard-coding one family would have silently returned `None` for every
other model — the kind of failure that shows up as a blank field nobody investigates.

**~2,100 MiB of disagreement.** The card says 11,830 while Ollama says 9,737 for the same load.
C4a found the same gap and reached the same conclusion: the footer shows **the card**, through
NVML, because only one of those two figures is what you actually have left.

## `load_duration 0.00 s` is the whole point of the chunk

`load_model` goes through `ollama::load` at the config's `num_ctx` rather than rolling its own
call. That is the packet's watch-for and the entire reason `num_ctx` moved into the shared config
at step 3.

**An Ollama runner is keyed by model *and* `num_ctx`.** Load at one value and chat at another and
you have not reused the warm model — you have silently evicted it and loaded a second copy,
measured at roughly **3 s each way**, every turn, in both directions. The symptom is a first token
that stalls for no reason you can see.

`load_duration 0.00 s` on the next conversation is the proof the warm survived.

## A failed lookup must not delete a fact

**A model whose `/api/show` fails still appears in the list**, still selectable, with a
`capabilities unknown` badge.

Capabilities are a *label*; being installed is a *fact*. Dropping the row because the label could
not be fetched would make the picker lie about what is on disk — the worst available outcome, since
the user would conclude the model was gone.

Only `vision` and `tools` are badged. Everything has `completion`, so showing it is noise; the two
that change what a model is *for* are the two that show.

## What needed a human

The panel itself needs clicking — load, release, switch mid-conversation, and confirm the choice
survives a restart. In [PENDING-CHECKS.md](../PENDING-CHECKS.md).
