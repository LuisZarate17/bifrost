import { useCallback, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

export type ModelInfo = {
  name: string;
  size: number;
  parameterSize: string;
  quantization: string;
  isCurrent: boolean;
  isResident: boolean;
  residentMib: number;
  capabilities: string[];
  maxContext: number | null;
  error: string | null;
};

/**
 * Which capabilities are worth a badge.
 *
 * `completion` is on everything, so showing it would be noise — the two that
 * change what a model is *for* are whether it can see an image and whether it
 * can call a tool.
 */
const BADGES = [
  { key: "vision", label: "vision", title: "Can be sent images. Warden needs this for screenshots." },
  { key: "tools", label: "tools", title: "Can emit structured tool calls (see docs/G0-results.md)." },
] as const;

/**
 * The switchboard: what is installed, what it can do, and what is loaded.
 *
 * This is what makes Citadel a hub rather than a chat box — it is the one place
 * the resident model is chosen, and the choice is written to the shared config
 * so the CLI and Warden follow it without being told separately.
 */
export function Switchboard({
  models,
  current,
  onPick,
  onRefresh,
  busy,
}: {
  models: ModelInfo[];
  current: string | null;
  onPick: (tag: string) => void;
  onRefresh: () => void;
  busy: boolean;
}) {
  const [working, setWorking] = useState<string | null>(null);
  const [note, setNote] = useState("");

  const run = useCallback(
    async (tag: string, what: "load" | "unload") => {
      setWorking(tag);
      setNote("");
      try {
        if (what === "load") {
          // Loads at the config's num_ctx — the same value the conversation asks
          // with, which is what stops the next request silently reloading.
          const secs = await invoke<number>("load_model", { tag });
          setNote(`loaded ${tag} in ${secs.toFixed(1)}s`);
        } else {
          const released = await invoke<boolean>("unload_model", { tag });
          setNote(released ? `released ${tag}` : `${tag} was not loaded`);
        }
        onRefresh();
      } catch (e) {
        setNote(String(e));
      } finally {
        setWorking(null);
      }
    },
    [onRefresh],
  );

  const pick = useCallback(
    async (tag: string) => {
      onPick(tag);
      try {
        // The switchboard part: one source of truth for "current model".
        await invoke("set_model", { tag });
        onRefresh();
      } catch (e) {
        setNote(String(e));
      }
    },
    [onPick, onRefresh],
  );

  return (
    <div className="switchboard">
      <div className="sb-head">
        <strong>Models</strong>
        <span className="sb-hint">
          The current model is shared — the CLI and Warden follow this choice
        </span>
        <button className="ghost" onClick={onRefresh} disabled={busy}>
          Refresh
        </button>
      </div>

      <ul className="sb-list">
        {models.map((m) => (
          <li key={m.name} className={m.name === current ? "picked" : ""}>
            <label className="sb-pick">
              <input
                type="radio"
                name="model"
                checked={m.name === current}
                onChange={() => void pick(m.name)}
                disabled={busy}
              />
              <span className="sb-name">{m.name}</span>
            </label>

            <span className="sb-meta">
              {m.parameterSize} {m.quantization} · {(m.size / 1e9).toFixed(1)} GB
              {m.maxContext ? ` · up to ${m.maxContext.toLocaleString()} ctx` : ""}
            </span>

            <span className="sb-caps">
              {BADGES.filter((b) => m.capabilities.includes(b.key)).map((b) => (
                <span key={b.key} className="cap" title={b.title}>
                  {b.label}
                </span>
              ))}
              {m.error && (
                <span className="cap warn" title={m.error}>
                  capabilities unknown
                </span>
              )}
            </span>

            <span className="sb-state">
              {m.isResident ? (
                <span className="resident" title="Ollama has a runner for this in memory">
                  ● {m.residentMib.toLocaleString()} MiB
                </span>
              ) : (
                <span className="cold">○ not loaded</span>
              )}
            </span>

            <span className="sb-actions">
              <button
                className="ghost"
                onClick={() => void run(m.name, m.isResident ? "unload" : "load")}
                disabled={busy || working !== null}
              >
                {working === m.name ? "…" : m.isResident ? "Unload" : "Load"}
              </button>
            </span>
          </li>
        ))}
      </ul>

      {note && <p className="sb-note">{note}</p>}

      <p className="sb-foot">
        <strong>16 GB will not hold two 8B models.</strong> Loading one while another is
        resident makes them take turns — the card figure below is the truth, not{" "}
        <code>ollama ps</code>; the two disagreed by ~2,500 MiB at C4a.
      </p>
    </div>
  );
}
