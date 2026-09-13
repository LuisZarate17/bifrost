import { useState } from "react";
import { invoke, Channel } from "@tauri-apps/api/core";
import "./App.css";

// Lives here for now. PLAN.md section 2 wants it in a config file — capture/prompt.txt
// already does that for the CLI. E1 owns making it tunable without a rebuild.
const PROMPT =
  "What am I looking at? If there's an error or something broken, explain it and the fix. Otherwise explain what matters here.";

type StreamEvent =
  | { event: "captured"; data: { width: number; height: number; pngBytes: number } }
  | { event: "token"; data: { text: string } }
  | {
      event: "done";
      data: {
        promptEvalCount: number;
        evalCount: number;
        totalMs: number;
        ttftMs: number;
      };
    };

function App() {
  const [answer, setAnswer] = useState("");
  const [status, setStatus] = useState("");
  const [stats, setStats] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  async function ask() {
    setAnswer("");
    setStats("");
    setError("");
    setStatus("Capturing…");
    setBusy(true);

    // Measured in the UI, not in Rust — this is what the user actually perceives.
    const t0 = performance.now();
    let firstPaintAt: number | null = null;

    const onEvent = new Channel<StreamEvent>();
    onEvent.onmessage = (message) => {
      if (message.event === "captured") {
        const d = message.data;
        setStatus(
          `Captured ${d.width}×${d.height} (${Math.round(d.pngBytes / 1024)} KB) — asking…`
        );
      } else if (message.event === "token") {
        if (firstPaintAt === null) {
          firstPaintAt = performance.now() - t0;
          setStatus("");
        }
        // Functional update: several tokens can land in one React tick, and this
        // way none of them is lost when that happens.
        setAnswer((prev) => prev + message.data.text);
      } else {
        const d = message.data;
        setStats(
          `${d.promptEvalCount} tokens in · ${d.evalCount} out · ` +
            `first token ${(d.ttftMs / 1000).toFixed(2)}s ` +
            `(UI saw it at ${((firstPaintAt ?? 0) / 1000).toFixed(2)}s) · ` +
            `total ${(d.totalMs / 1000).toFixed(1)}s`
        );
      }
    };

    try {
      await invoke("ask_about_screen", { prompt: PROMPT, model: null, onEvent });
    } catch (e) {
      setError(String(e));
      setStatus("");
    } finally {
      setBusy(false);
    }
  }

  return (
    <main className="container">
      <h1>Warden</h1>
      <p className="subtitle">B3 — capture the screen, ask about it</p>

      <button onClick={ask} disabled={busy}>
        {busy ? "Working…" : "What's on my screen?"}
      </button>

      {status && <p className="stats">{status}</p>}
      {error && <pre className="error">{error}</pre>}
      {answer && <pre className="answer">{answer}</pre>}
      {stats && <p className="stats">{stats}</p>}
    </main>
  );
}

export default App;
