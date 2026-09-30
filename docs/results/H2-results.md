# H2 — The model looks for code instead of guessing where it is

`list_files` and `search`, inside the project folder, approved each time. **Built 2026-09-29.**

Until H2, `read_file` was only any use when someone named the file. H1's fix probe watched the
model read a compiler error that could only be in `main.rs` and keep editing `config.rs`, because
nothing let it ask *where is `address` called?*

## The result

### It finds what it is told the name of

On `qwen3-vl:8b-instruct-q4_K_M`, eighteen questions about odin itself — a detached worktree at
f95365f — passing when the answer names the right file **and** states the fact:

| | Named (8) | Vague (7) | No tool (3) |
|---|---|---|---|
| **With search**, app sampling (3 runs) | **21/24** | 2/21 | 9/9 |
| With search, temperature 0 | 7/8 | 1/7 | 3/3 |
| Read only, as before H2 | 0/24 | 0/21 | 9/9 |
| Read only, temperature 0 | 0/8 | 0/7 | 3/3 |

`cargo run -p bench --bin findprobe` · saved in `evals/out/H2-find-1790725447/`

**Named** questions — *where is `DEFAULT_NUM_CTX` defined?* — go from none to nearly all. The one
that still fails, `q05` (*what kind of hash does `fingerprint` compute?*), finds `edit.rs` by
search every time and then answers from the hit without reading the file: the doc comment saying
FNV-1a is the line above.

**No-tool** questions — a haiku, a sum — stay 9/9 with the tools on offer. The model does not
search when there is nothing to find, which was G0's first worry about every tool.

### It fixes the build across files

The fix probe again, now with search on offer beside the check:

| | App sampling (3 × 8) | Temperature 0 |
|---|---|---|
| **With the check** | **17/24 (71%)** | 7/8 |
| Without | 9/24 (38%) | 2/8 |

**The case H2 was for — a rename whose call site is in another file — went from 2/3 and 0/1 to
3/3 and 1/1**, the same way every time: edit, check fails naming `config::address`, `search` for
`config::address()`, `src/main.rs:9`, edit, check passes. In H1 the model never opened `main.rs`.

The total is two lower than H1's 19/24 — `f01` (the `;` mismatch, 0/3 against 1/3) and `f06`. Within
the noise measured in H1, and not in the tasks search touches.

`saved in evals/out/H1-fix-1790725923/`

### Nothing else moved

The edit probe with the two new tools on offer: **28/30** at app sampling, **10/10** at
temperature 0 — against 27/30 and 10/10 at H1. Offering more tools did not make the model reach for
them where an edit was asked for.

End to end, the real app: `tools/e2e/mimir-check.mjs` **24/24** — a search card naming what it
looks for, what it finds on the card and in the answer, a listing declined — and `mimir-edit.mjs`
still **16/16**.

## Vague questions are the model, not the wording

*Where is an edit outside the folder refused?* fails 19 times in 21 with search on offer. Read from
the logs, three ways:

- **It does not look.** It answers from what projects usually do — *"enforced by the `edit_file`
  function"* — with no call at all.
- **It searches for the question.** `"chat history"`, `"save location"`, `"history folder"`: no
  code says those, and it can spend all twenty rounds rephrasing.
- **It finds the file and does not read it** — `check.rs`, then an answer about `CHECK_TIMEOUT` to a
  question about output size.

A second round of wording told it exactly those three things — search for words the code would
contain, never a question; read what the search turns up before answering. **Vague 1/21, named
21/24, no tool 9/9**: no better. The longer wording was taken out again; it cost context and bought
nothing.

**This is what H10's repo map is for** — paths and each file's first comment line in the prompt,
so a question about *chat history* can meet `store.rs`'s *Conversations on disk*. It is measured
with this probe's vague half. Embeddings stay behind it.

## The design, and why each part is there

| | |
|---|---|
| **Literal and smart-case** | A small model writes a malformed regex far more easily than a malformed string, and its question is almost always *where is this name*. Case matters only when the text has a capital, as in ripgrep |
| **Glob patterns, read like `.gitignore`** | A pattern without a slash matches file names anywhere — `*.rs` is every Rust file; a folder's name lists what is in it |
| **Inside the folder only** | Patterns are relative; `..`, a drive, a UNC or verbatim path is refused, and refused **before a card is shown** — like an edit whose text is not in the file, a call that could never run goes back to the model rather than to you |
| **Not the build's files** | `.git` (a folder, or a worktree's file), `target/`, `node_modules/` and Mímir's own folder are skipped even with no `.gitignore`; `.gitignore` files inside the folder are honoured; parent and global ignore files are not read. Links and junctions are not followed |
| **It ends** | 20,000 files, 64 MB read, 15 s, files over 1 MB and binary files skipped — and Stop reaches a walk under way, through a number taken when the card is shown |
| **It fits** | About 8 KB, sorted so a rerun reads the same, each line cut at 200 characters, with the full count and *narrow the pattern* when lines were left out |
| **Approved each call** | The ticket holds the folder and exactly what the card shows. Every finding call asks, as H1 decided, until H5 revisits it with evidence |

## Found along the way

- **A git worktree's `.git` is a file**, not a folder — the find probe's codebase is one, and
  `list_files` showed it. Now skipped whatever it is.
- **`*.rs` showed as `rs.*`.** The card's path runs right to left so a long path loses its start,
  not its file name, and that reordered punctuation at the ends. The text is now an isolated
  left-to-right run inside it.
- **Smart App Control blocked freshly linked test binaries** three times in this chunk
  (`os error 4551`). Touching a source file and relinking cleared it each time, as PLAN.md §3 says.
  H1's `check.rs` reports exactly this to the model as *not a problem in the code*.

## Tests

| | |
|---|---|
| `odin-core` | **149** — 19 new for `search` (the walk's skips, a nested `.gitignore` through the `\\?\` root, a junction, patterns outside, smart case, caps, Stop, validation before the card), 1 for `tools` |
| Mímir | **11** — one new: a search ticket redeems exactly what its card showed, and not as a read or a listing |
| Probe | `cargo run -p bench --bin findprobe` — `--runs`, `--only`, `--variant search|read` |
| End to end | `node tools/e2e/mimir-check.mjs` — 24 checks in eight scenarios |

## What the review found

An independent review against the chunk's guarantees — containment, caps, approval, output, the
probe's fairness, the frontend — ran the tests, clippy and the type check and found **one gap, no
escape**:

- **An alternation could hide a parent.** The check refused a pattern part that *is* `..`, but
  `{..,x}/**` has no such part. Nothing outside was ever read — the walk starts at the folder and
  never leaves it, whatever the pattern says — but the refusal the card promises did not happen;
  the pattern matched nothing instead. **Fixed:** `..` or `//` anywhere, read through `[ ]`, is
  refused; `{..,x}/**`, `src/{a,..}/*`, `[.][.]/x`, `{C:/,x}/**` are in the test.

Confirmed sound, in its words: the walk's skips, caps and Stop; that a search ticket redeems only
as itself; `validate` before any card; the sort, counts, smart case and binary detection; the
probe's word matching and worktree cleanup; the read-only variant lacking both tools and their
sentence; `FindCard`'s states and the e2e helpers. It asked for an explicit CRLF test; one is added.

(The first review of this chunk stopped part-way on an account usage limit; this one ran on the
checker agent.)
