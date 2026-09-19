import { useEffect, useRef, useState } from "react";

/**
 * Half the code fences the model emits carry no language tag (46 of 89, measured
 * across every saved answer in evals/out/), and many of those are rustc *output*
 * rather than Rust source. So only tagged languages get highlighted — guessing at
 * untagged blocks would colour compiler errors as if they were code.
 */
const HIGHLIGHTABLE = new Set(["rust", "rs"]);

// One highlighter for the app's lifetime. Shiki's default import pulls megabytes
// of grammars; this loads two. The JS regex engine avoids instantiating WASM,
// which matters for a popup that should feel instant.
let highlighterPromise: Promise<any> | null = null;

function getHighlighter() {
  if (!highlighterPromise) {
    highlighterPromise = (async () => {
      const [{ createHighlighterCore }, { createJavaScriptRegexEngine }] =
        await Promise.all([
          import("shiki/core"),
          import("shiki/engine/javascript"),
        ]);
      return createHighlighterCore({
        langs: [import("@shikijs/langs/rust")],
        themes: [import("@shikijs/themes/github-dark-default")],
        engine: createJavaScriptRegexEngine({ forgiving: true }),
      });
    })();
  }
  return highlighterPromise;
}

export function CodeBlock({ code, lang }: { code: string; lang?: string }) {
  const [html, setHtml] = useState<string | null>(null);
  const alive = useRef(true);

  const language = (lang ?? "").toLowerCase();
  const shouldHighlight = HIGHLIGHTABLE.has(language);

  useEffect(() => {
    alive.current = true;
    if (!shouldHighlight) return;

    getHighlighter()
      .then((hl) => {
        if (!alive.current) return;
        setHtml(
          hl.codeToHtml(code, { lang: "rust", theme: "github-dark-default" })
        );
      })
      // Highlighting is decoration. If it fails, the plain block below still
      // shows the code, which is the part that matters.
      .catch(() => {});

    return () => {
      alive.current = false;
    };
  }, [code, shouldHighlight]);

  // Shiki's output is generated from `code` by its own tokenizer — it is not the
  // model's text passed through. The unhighlighted path below never uses
  // dangerouslySetInnerHTML at all.
  if (shouldHighlight && html) {
    return (
      <div className="code" dangerouslySetInnerHTML={{ __html: html }} />
    );
  }

  return (
    <pre className="code">
      <code>{code}</code>
    </pre>
  );
}
