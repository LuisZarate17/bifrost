# Prompt variants

Tracked so a prompt experiment can be reproduced exactly. `evals/out/` is gitignored, so these
files plus the answer hashes recorded in `docs/` are the durable record.

| File | Status |
|---|---|
| `current.txt` | **Shipping.** Byte-identical to `DEFAULT_PROMPT` in `bench.mjs` and to `config::DEFAULT_PROMPT` in `crates/bifrost-core/src/config.rs`, the only copy in source |
| `brief.txt` | **Rejected at C1b.** Regressed case-02: concluded the code compiles when it does not |
| `tight.txt` | **Rejected 2026-09-18.** Same failure as `brief.txt` |
| `ordered.txt` | **Rejected 2026-09-18.** Same failure as `brief.txt` |
| `lead.txt` | **Correct, but not shorter.** Keeps case-01 and case-02; answers grew ~10–35% |

## Asking for concision costs the answer — three for three

`tight.txt` and `ordered.txt` were written to get `brief.txt`'s brevity without its failure, by
constraining the *shape* of the output rather than telling the model to stop early. It made no
difference. Measured against a control that reproduced A2's recorded answer hash exactly
(`9f84dcc7d8c5…`, 3,732 chars — so the comparison was sound, not drift):

| prompt | case-01 | case-02 | knows rustc **rejects** the code |
|---|---|---|---|
| `current` | 3,715 | 3,732 | **yes** |
| `brief` | 699 | 754 | no |
| `tight` | 1,287 | 1,932 | no |
| `ordered` | 800 | 797 | no |
| `lead` | 4,085 | 5,103 | **yes** |

The shared failure is more specific than "gets case-02 wrong". Every shortened prompt describes
the bug as a **runtime** problem — a dangling reference, undefined behaviour after reallocation.
That is the C++ answer. In Rust the borrow checker rejects this program and it never runs at all.
`brief.txt` says so outright ("even though the code compiles"); `tight` and `ordered` imply the
same thing more politely. Only the unconstrained prompt reaches "compile-time error by design".

So the A2 finding is not about one badly worded variant. **This model spends its length on getting
there**, and any instruction that trades length for directness trades away the conclusion with it.

`lead.txt` is the way out that works: it *adds* a `**Short answer:**` line and forbids nothing.
Both cases stay correct and the verdict is readable at a glance — but the answers get longer, not
shorter. Glanceability and brevity turned out to be different purchases.

## Write these files byte-exactly — no trailing newline

`bench.mjs` reads a `--prompt-file` verbatim. `crates/bifrost-cli/src/main.rs` calls `.trim()`; the
harness does not. At C1b a control built with `cp crates/odin-cli/prompt.txt` carried one trailing
newline that
A2's baseline did not, and at `temperature: 0` with a fixed seed **that single byte flipped the
case-02 verdict** — from "will not compile" (correct) to "compiles successfully, segfaults at
runtime" (wrong). The measurement was invalid and looked fine.

So: write them with a script, not an editor, and **pin the control to a known answer hash before
trusting any comparison**. The A2 baseline for `8b-instruct-q4_K_M` on `editor-rust-code` is
`9f84dcc7d8c5…` at 3,732 chars.
