# H4b — Choosing a folder offers the coding model

Gate 2 chose Qwen3-Coder 30B-A3B for working in code, and asked for a way to get to it that does
not mean opening the Switchboard every time. **Built 2026-10-02.**

## What it does

- **Choosing a project folder** — from the picker or the recent list — offers to switch to the
  coding model, when it is installed and is not already the current model.
- **Clearing the folder** offers to switch back to the model Mímir switched away from.
- **"Not now"** (or "Keep …") holds for that folder for the rest of the session. A switch that
  landed, or failed, is not a no — choosing the folder again later offers it again.
- **Picking a model by hand** in the Switchboard ends the switch-back: what "back" would mean is
  the user's call from then on.

The bar sits just above the composer, in the approval card's style, and its buttons are disabled
while an answer is running.

## How it decides, and what crosses the IPC

- **Two config fields.** `coding_model` (default `qwen3-coder:30b`; empty turns the offer off) and
  `model_before_coding` (set when Mímir switches to the coder on the user's say-so; cleared by the
  switch back or by any manual pick). Old config files load with the defaults.
- **Rust decides.** `offer_for(config, installed)` is a pure function, and `accept_model_offer`
  recomputes the offer from the config and `/api/tags` at the moment of the click. The webview
  sends only which way it clicked — never a model tag — so nothing reaching the IPC can choose what
  gets loaded, and an offer that went stale (the folder was cleared meanwhile) is refused rather
  than acted on.
- **The config is written before the load.** Heimdall decides whether it can see by reading
  `config.model` on every summon (G3). Writing it first means Heimdall refuses for the whole switch,
  instead of reloading the vision model and fighting the coder for the card. Then the old model is
  unloaded and the new one loaded and pinned at the app's `num_ctx`. A load that fails keeps the new
  choice and says so — the model loads with the next question.
- **Heimdall is unchanged.** G3 already refuses before capturing when the current model has no
  `vision`, and names the model.

## Checked

| | |
|---|---|
| `offer_for` unit tests | 9: every combination that decides an offer — coder installed or not, already on it, offer turned off, a remembered model gone or present, a folder still open — plus `llama3.1` ≡ `llama3.1:latest` |
| `tools/e2e/mimir-switch.mjs` | **14/14** against the real app and the real models (below) |
| `tools/e2e/mimir-check.mjs` (H1/H2) | 24/24 — **on Qwen3-Coder**: these scripts run on the config's model, which named the coder that day |
| `tools/e2e/mimir-edit.mjs` (G7) | 16/16 — on Qwen3-Coder too, so its edits, Undo, Deny and refusals hold on the new model |
| `?preview=offer` screenshots | dark and light at 1440, 1100 and 720 px: aligned with the composer, nothing clipped, wraps to two lines, one filled button |
| Independent review | a checker read the diff; its findings are fixed (below) |

Those two older scripts write the folder straight into the config rather than picking it, so the
offer bar never shows in them; they are here as regression checks, and as the first run of the
check, search and edit flows on the new model in the real app.

The e2e, scenario by scenario:

1. Picking folder A shows the offer; **Not now** closes it and leaves `config.model` alone.
2. Picking folder B and **Switch**: the config names `qwen3-coder:30b` and remembers the vision
   model; `/api/ps` has the coder and not the vision model; the model pill names the coder.
3. A new chat, "List the files in this folder.": a `list_files` card, Allow, and the coder answers
   — Qwen3-Coder working in the app end to end, tool call included.
4. Clearing the folder offers the way back, the offer is **still up 5 s later**, and **Switch back**
   leaves the config on the vision model with nothing remembered, `/api/ps` agreeing.

The user's `config.json` is restored byte for byte at the end (checked with `cmp` after every run),
and the conversation scenario 3 creates is deleted.

## What the e2e found — both fixed

It took three runs to pass, and the first two failures were real bugs, not flaky tests:

- **After a reboot the offer never appeared.** Ollama is not running until something starts it,
  and a folder is usually chosen before the first question. `model_offer` treated "Ollama
  unreachable" as "no offer". It now starts Ollama the way sending a question does — but only when
  the config could produce an offer at all, so choosing a folder never starts Ollama for nothing.
  The second run began with Ollama stopped and passed this step.
- **The switch-back offer was closed by the bar before it.** After a switch the bar says "… is
  ready" and hides itself 4 s later. Cleared within those 4 s, the new offer reused the same bar —
  showing the old "ready" note — and the old timer then hid the new offer. Found because the warm
  coder answered scenario 3 in under 4 s. Each offer is now its own bar (React `key`), so nothing
  carries over and unmounting clears the timer; scenario 4 now checks the offer is still up 5 s on.

## Review findings — fixed

- Two folder choices in quick succession could let the first `model_offer` answer land last and
  replace the second. A request counter drops stale answers.
- A successful switch was recorded as "declined", so the same folder never offered again that
  session. Hiding and declining are now separate.
- **Ctrl+C during any of the three Mímir e2e scripts** skipped their `finally` and left the user's
  config replaced with test data. All three now restore it on SIGINT too.

## Not covered by a script — needs a person

Heimdall's side, because a script cannot press its hotkey. In [H-testing.md §3](../H-testing.md)
and [PENDING-CHECKS.md](../PENDING-CHECKS.md):

- with the coder current, Heimdall refuses before capturing and names it;
- after switching back, Heimdall answers about the screen again;
- a manual pick in the Switchboard, then clearing the folder, offers nothing.

## Files

`crates/odin-core/src/config.rs` (the two fields) · `apps/mimir/src-tauri/src/lib.rs`
(`offer_for`, `model_offer`, `accept_model_offer`, `set_model`) · `apps/mimir/src/ModelOfferBar.tsx`
(new) · `apps/mimir/src/App.tsx`, `App.css`, `preview.ts` · `tools/e2e/mimir-switch.mjs` (new) ·
`tools/mimir-shots.mjs`
