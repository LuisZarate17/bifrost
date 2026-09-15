# Prompt variants

Tracked so a prompt experiment can be reproduced exactly. `evals/out/` is gitignored, so these
files plus the answer hashes recorded in `docs/` are the durable record.

| File | Status |
|---|---|
| `current.txt` | **Shipping.** Byte-identical to `DEFAULT_PROMPT` in `bench.mjs` and, after its `.trim()`, to `capture/prompt.txt` |
| `brief.txt` | **Rejected at C1b.** Regressed case-02: concluded the code compiles when it does not |

## Write these files byte-exactly — no trailing newline

`bench.mjs` reads a `--prompt-file` verbatim. `capture/src/main.rs` calls `.trim()`; the harness
does not. At C1b a control built with `cp capture/prompt.txt` carried one trailing newline that
A2's baseline did not, and at `temperature: 0` with a fixed seed **that single byte flipped the
case-02 verdict** — from "will not compile" (correct) to "compiles successfully, segfaults at
runtime" (wrong). The measurement was invalid and looked fine.

So: write them with a script, not an editor, and **pin the control to a known answer hash before
trusting any comparison**. The A2 baseline for `8b-instruct-q4_K_M` on `editor-rust-code` is
`9f84dcc7d8c5…` at 3,732 chars.
