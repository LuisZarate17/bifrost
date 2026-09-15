import { useCallback, useEffect, useRef, useState } from "react";
import { invoke, Channel } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./App.css";

// PLAN.md section 2 wants this in a config file — capture/prompt.txt already does
// that for the CLI. E1 owns making it tunable without a rebuild.
const PROMPT =
  "What am I looking at? If there's an error or something broken, explain it and the fix. Otherwise explain what matters here.";

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
  const busy = useRef(false);
  const answerRef = useRef<HTMLPreElement>(null);

  const ask = useCallback(async () => {
    // The hotkey can fire again mid-answer; don't start a second capture.
    if (busy.current) return;
    busy.current = true;

    setAnswer("");
    setStats("");
    setError("");
    setStatus("Capturing…");

    const onEvent = new Channel<StreamEvent>();
    onEvent.onmessage = (message) => {
      if (message.event === "captured") {
        setStatus("Reading your screen…");
      } else if (message.event === "token") {
        setStatus("");
        // Functional update: several tokens can arrive in one React tick.
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

  // The hotkey shows the window in Rust, then tells us to start.
  useEffect(() => {
    const unlisten = listen("warden://summoned", () => void ask());
    return () => void unlisten.then((f) => f());
  }, [ask]);

  // If no hotkey bound, say so — otherwise nothing happening looks like a crash.
  useEffect(() => {
    invoke<string | null>("hotkey_label").then(setHotkey).catch(() => setHotkey(null));
  }, []);

  // Follow the stream rather than making you scroll.
  useEffect(() => {
    const el = answerRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [answer]);

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
      {answer && (
        <pre className="answer" ref={answerRef}>
          {answer}
        </pre>
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
