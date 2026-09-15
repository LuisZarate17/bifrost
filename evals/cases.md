# Eval cases

Known-correct answers for the screenshots in `evals/shots/`. The images themselves are
**gitignored** — screenshots of a real desktop can contain anything, so the set stays local.
This file is tracked because the expected answers are the valuable part, and so is
`evals/fixtures/`, the source code both cases are built from — that way the pair can be
regenerated from scratch rather than depending on images that only exist on one machine.

Used to grade A2's model bake-off, and the seed for the F1 harness and the F2 habit of adding
every real-world failure as a new case.

---

## case-01 · `terminal-rust-error.png`

**Source:** a real `cargo build` on `evals/fixtures/frametest/`, which is **tracked in this
repo** — run `cargo build` in that directory to regenerate the error at the same line numbers.
Not a mockup; genuine rustc output.

**Known artifact:** the capture carries one stray `System.Management.Automation.RemoteException`
line between the error body and the `For more information` footer. That's an artifact of how the
terminal was captured, not something rustc printed. Kept deliberately — a model derailed by one
junk line in an otherwise unambiguous error is telling us something worth knowing.

**What's on screen:** a maximized PowerShell window showing `error[E0502]: cannot borrow
'frames' as mutable because it is also borrowed as immutable`, with the code excerpt, carets,
line numbers 22/25/27, and the "immutable borrow later used here" trail.

**A correct answer must:**
- Identify it as a Rust **borrow-checker** error, ideally by code **E0502**
- Explain the cause: an immutable borrow of `frames[0].pixels` is held across
  `frames.push(...)`, which needs a mutable borrow and can reallocate the `Vec`
- Locate it at **`src/main.rs` line 25** (the push), with the borrow originating at line 22
- Suggest a real fix — clone the data, copy the length before pushing, or scope the borrow so
  it ends before the push

**Counts as a failure if it:** misreads the error code, blames the wrong line, calls it a
generic "syntax error", or invents a fix that wouldn't compile.

**Why this case matters:** this is the exact mistake BUILD.md warns about in B3 — holding a
reference into a capture frame's buffer while the Vec reallocates. If the model can't read
this, it can't help with the actual project.

---

## case-02 · `editor-rust-code.png`

**Source:** a real screenshot of VS Code — 1919×1079 full screen, maximized, dark theme, with
menu bar, activity bar, line numbers, minimap and status bar all in frame — showing
`evals/fixtures/frametest/src/main.rs`.

> **Replaced 2026-09-08.** The first version of this case was *rendered* — text drawn
> programmatically into a mock editor — because scripted window management kept capturing the
> wrong window. That was the wrong call. The eval set exists to measure whether a model can read
> a **real** screen; a hand-drawn approximation tests something Warden will never send, and when
> it fails you can't tell whether the model misread the code or the renderer drew it wrong.

**What's on screen:** `main.rs` — the source that produces case-01. A `CaptureFrame` struct, a
`capture_screen()` constructor, and a `main()` that takes `let first_buffer = &frames[0].pixels`
and then pushes to `frames`.

**A correct answer must:**
- Read the code accurately — struct fields `width: u32`, `height: u32`, `pixels: Vec<u8>`
- Recognize the bug **without a compiler error to lean on**: the borrow at line 22 conflicts
  with the push at line 25
- Not hallucinate code that isn't shown

**Counts as a failure if it:** garbles identifiers (`first_buffer`, `capture_screen`), misreads
the generics (`Vec<u8>`, `HashMap<String, u32>`), or describes the file as fine.

**Why this case matters:** it's the harder half of the pair. Case-01 hands the model the answer
in the error text; here it has to find the bug from the code alone. Note also that the code
comments *hint* at the problem — a model that only paraphrases the comments hasn't actually
read the code.

---

## Grading the pair together

The two images are the same bug from both directions. A model that passes case-01 but fails
case-02 is reading text well but not reasoning about code — worth knowing before committing to
it, since Warden's whole job is looking at a screen and working out what's wrong.

---

## Grading the fix, not just the diagnosis

Added at C1b. Every case above grades whether the model **identifies** the bug. None graded whether
the **fix it proposes** actually works, and that gap hid a real defect: the case-02 answer A2
recorded as a PASS explains E0502 correctly and then proposes a fix that fails with the identical
E0502. Two other plausible-looking fixes (`Vec::reserve`, `Vec::with_capacity`) fail the same way,
because capacity is irrelevant to a *static* borrow conflict — which is exactly the kind of thing
that reads fine on a screen.

**A proposed fix is graded by compiling it, never by reading it.** `npm run check:fixes` runs each
candidate through `rustc --emit=metadata`, which borrow-checks without linking.

**An answer counts as fully correct only if its primary suggested fix compiles.** An answer that
diagnoses correctly and then proposes a fix that doesn't compile is a **PARTIAL**, and the run
notes should say so. Add every new proposed fix to `evals/fixcheck.mjs` as a candidate.
