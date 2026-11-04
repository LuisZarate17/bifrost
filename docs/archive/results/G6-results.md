# G6 — A tool loop, with every call approved before it runs

One tool, hardcoded, off by default. **Built 2026-09-19.**

Gated on [G0](G0-results.md), which cleared the model at **10/10** — including all four cases where
calling a tool would have been wrong.

## Measured

| | |
|---|---|
| Given a temp file holding a fact the model could not know | it asked for `read_file` |
| The tool returned the contents | the next turn answered with the fact |
| Tests | **5** for `tools`, **6** for approval in Citadel; 18 in `odin-core`, 24 in all |
| Read cap | **64 KB**, refused rather than truncated |
| Loop bound | **4 rounds** |

## The safety design is the chunk, not a detail of it

Every tool call is **code the model chose to run**. So:

- **Tools are off by default.**
- **No allowlist. No remembered folder. No "always allow".**
- Each call shows the exact path and waits.

That is deliberately **the most annoying design that is still usable**, because the alternative is
a model reading whatever it names and nobody noticing. Every convenience on that list works by
removing the moment where you look at the path.

## The approval has to bind to the call

`run_tool` takes the path **the UI displayed**, not the model's request re-read at execution time.
Anything else makes the confirmation a formality: you would be approving a description while a
different string went to the filesystem.

That was not enough on its own. The command was still *callable* with any path — the binding lived
in the frontend, which is the side an attacker-shaped input reaches first. So approval is now a
**one-shot ticket issued in Rust**:

- `arm(name, path)` issues a token for **exactly that call** and stores it.
- `redeem(token, name, path)` spends it, and **fails unless all three match**.
- The slot is `take()`n on redemption, so **a ticket cannot be spent twice**.

`redeem_in` holds the decision without the global slot, which is what makes it testable — six tests
drive a local slot, because a global static is shared across parallel tests and they would
interfere with each other rather than with the bug.

## Refused at interpretation, not at execution

**Unknown tool names are rejected before anything runs.** The model can name anything it likes;
only the offered list is real. Checking at execution would mean an invented tool name travels
further into the system than it needs to.

A model that cannot carry tools at all is told so **before** the round trip, rather than answering
in confusing prose about a tool it does not have.

## Tool errors are text, not failures

A tool that cannot answer is a **normal conversational event**. The model should be told *"that
file does not exist"* and continue — not have the turn collapse with an error the user has to
interpret.

Reads are capped at **64 KB and refused rather than truncated**, because half a file answered
confidently is worse than being told it was too big. Same reasoning as G5's 256 KB text cap.

## Bounded at four rounds

Without a bound, a model that keeps asking for the same file loops forever and **the only symptom
is the app getting warm.** There is no error, no output, and nothing in the log.

## Not streamed, deliberately

A tool call **arrives whole or not at all**, and half of one cannot be acted on. Ordinary
conversational turns still stream.

## The bug the probe hid

The app appended the tool result **without the assistant turn carrying `tool_calls`**. So the model
saw an answer to a question it could not see itself having asked — and simply asked again, until
the four-round bound tripped. The bound is the only reason this was a bug rather than a hang.

**G0's probe did this correctly.** That is exactly why a passing probe was not coverage: a probe
proves the mechanism exists; it says nothing about whether the app uses it correctly. The two were
written days apart and only one of them was tested end to end.

## What needed a human

Approving a call, denying one, and watching the loop close. In
[PENDING-CHECKS.md](../PENDING-CHECKS.md).

---

**Reversed in part by H5 (2026-10-02).** This chunk's rule that *every* read shows its path and
waits no longer holds inside the folder you chose. Since H5, a `read_file`, `list_files` or `search`
**inside the project folder** runs without asking and shows as a quiet card marked *no approval
needed*. **Still asking, every call:** a read outside the folder (including through a junction or
symlink that points out, a hard link to a file elsewhere, or a path on another machine — never
resolved before Allow), a file the project's `.gitignore` hides, a secret-looking file such as
`.env`, every read, listing and search when the folder is a drive root or the user's profile,
every edit and create, and every `run_check`. **The off switch:** `"ask_every_call": true`
in `config.json` brings back the behaviour described above, for everything. The one-shot ticket and
the exact-path redemption are unchanged for every call that still asks. The reasoning and the
numbers are in [H5-results.md](H5-results.md); the text above is left as it was written.
