# Eval cases

Known-correct answers for the screenshots in `evals/shots/`. The images themselves are
**gitignored** — screenshots of a real desktop can contain anything, so the set stays local.
This file is tracked because the expected answers are the valuable part.

Used to grade A2's model bake-off, and the seed for the F1 harness and the F2 habit of adding
every real-world failure as a new case.

---

## case-01 · `terminal-rust-error.png`

**Source:** a real `cargo build` on a project written for this purpose. Not a mockup — this is
genuine rustc output.

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

**Source:** rendered directly (1920×1080, Consolas, VS Code Dark+ palette, line numbers, tab
bar, status bar). Rendered rather than screenshotted so it is reproducible and contains nothing
private.

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
