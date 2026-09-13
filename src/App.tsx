import { useState } from "react";
import { invoke, Channel } from "@tauri-apps/api/core";
import "./App.css";

// The image B2 sends. A2 recorded exactly what this model says about it, so a
// wrong answer means the plumbing is wrong rather than the model.
const TEST_IMAGE =
  "C:/Users/you/source/repos/warden/evals/shots/terminal-rust-error.png";

const PROMPT =
  "What am I looking at? If there's an error or something broken, explain it and the fix. Otherwise explain what matters here.";

type StreamEvent =
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
  const [stats, setStats] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  async function ask() {
    setAnswer("");
    setStats("");
    setError("");
    setBusy(true);

    // Measured in the UI, not in Rust — this is what the user actually perceives.
    const t0 = performance.now();
    let firstPaintAt: number | null = null;

    const onEvent = new Channel<StreamEvent>();
    onEvent.onmessage = (message) => {
      if (message.event === "token") {
        if (firstPaintAt === null) firstPaintAt = performance.now() - t0;
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
      await invoke("ask_about_image", {
        imagePath: TEST_IMAGE,
        prompt: PROMPT,
        model: null,
        onEvent,
      });
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <main className="container">
      <h1>Warden</h1>
      <p className="subtitle">B2 — streaming a real answer from a local model</p>

      <button onClick={ask} disabled={busy}>
        {busy ? "Asking…" : "Ask about the test screenshot"}
      </button>

      {error && <pre className="error">{error}</pre>}
      {answer && <pre className="answer">{answer}</pre>}
      {stats && <p className="stats">{stats}</p>}
    </main>
  );
}

export default App;
