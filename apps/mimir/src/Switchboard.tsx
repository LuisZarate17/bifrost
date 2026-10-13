import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Check, Eye, Refresh, Wrench, X } from "./Icons";

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

export type CardState = {
  usedMib: number;
  totalMib: number;
  name: string;
  model: string;
  numCtx: number;
};

/**
 * `qwen3-vl:8b-instruct-q4_K_M` → `{ family: "qwen3-vl", size: "8b", tag: "8b-instruct-q4_K_M" }`.
 *
 * For the compact places — the composer pill, the sidebar — where the whole tag
 * does not fit and the family plus parameter count is what tells two apart. The
 * full name is always one hover away.
 */
export function splitName(name: string): { family: string; size: string; tag: string } {
  const [family, tag = ""] = name.split(":");
  const size = tag.split("-")[0] ?? "";
  return { family, size, tag };
}

/**
 * Which capabilities are worth a badge.
 *
 * `completion` is on everything, so showing it would be noise — the two that
 * change what a model is *for* are whether it can see an image and whether it
 * can call a tool.
 */
const BADGES = [
  { key: "vision", label: "Vision", Icon: Eye, title: "Can be sent images. Vidar needs this for screenshots." },
  { key: "tools", label: "Tools", Icon: Wrench, title: "Can emit structured tool calls (see docs/G0-results.md)." },
] as const;

/** A thin bar for "how full is the card". Shared by the dialog and the sidebar. */
export function GpuMeter({ card, compact }: { card: CardState | null; compact?: boolean }) {
  if (!card) return null;
  const pct = Math.min(100, (card.usedMib / Math.max(card.totalMib, 1)) * 100);
  const gb = (mib: number) => (mib / 1024).toFixed(1);
  return (
    <div className={compact ? "meter compact" : "meter"} title={`${card.name}\n${card.usedMib.toLocaleString()} of ${card.totalMib.toLocaleString()} MiB`}>
      <div className="meter-row">
        <span className="meter-label">GPU memory</span>
        <span className="meter-value">
          {gb(card.usedMib)} / {gb(card.totalMib)} GB
        </span>
      </div>
      <div className="meter-track">
        <div className={"meter-fill" + (pct > 85 ? " hot" : "")} style={{ width: `${pct}%` }} />
      </div>
    </div>
  );
}

/**
 * The switchboard: what is installed, what it can do, and what is loaded.
 *
 * This is what makes Mimir a hub rather than a chat box — it is the one place
 * the resident model is chosen, and the choice is written to the shared config
 * so the CLI and Vidar follow it without being told separately.
 */
export function Switchboard({
  models,
  current,
  card,
  onPick,
  onRefresh,
  onClose,
  busy,
}: {
  models: ModelInfo[];
  current: string | null;
  card: CardState | null;
  onPick: (tag: string) => void;
  onRefresh: () => void;
  onClose: () => void;
  busy: boolean;
}) {
  const [working, setWorking] = useState<string | null>(null);
  const [note, setNote] = useState("");
  const panel = useRef<HTMLDivElement>(null);

  // Focus moves into the dialog so Escape and Tab work without reaching for the
  // mouse first.
  useEffect(() => {
    panel.current?.focus();
  }, []);

  const run = useCallback(
    async (tag: string, what: "load" | "unload") => {
      setWorking(tag);
      setNote("");
      try {
        if (what === "load") {
          // Loads at the config's num_ctx — the same value the conversation asks
          // with, which is what stops the next request silently reloading.
          const secs = await invoke<number>("load_model", { tag });
          setNote(`Loaded ${tag} in ${secs.toFixed(1)}s`);
        } else {
          const released = await invoke<boolean>("unload_model", { tag });
          setNote(released ? `Released ${tag}` : `${tag} was not loaded`);
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
    <div
      className="scrim"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        className="dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="models-title"
        tabIndex={-1}
        ref={panel}
        onKeyDown={(e) => {
          if (e.key === "Escape") onClose();
        }}
      >
        <div className="dialog-head">
          <div>
            <h2 id="models-title">Models</h2>
            <p className="dialog-sub">
              The model you choose is shared — Vidar and the <code>bifrost</code> CLI follow it.
            </p>
          </div>
          <div className="dialog-tools">
            <button className="icon-btn" onClick={onRefresh} disabled={busy} title="Refresh" aria-label="Refresh">
              <Refresh />
            </button>
            <button className="icon-btn" onClick={onClose} title="Close" aria-label="Close">
              <X />
            </button>
          </div>
        </div>

        <ul className="models" role="radiogroup" aria-label="Installed models">
          {models.map((m) => {
            const picked = m.name === current;
            return (
              <li key={m.name} className={"model" + (picked ? " picked" : "")}>
                <button
                  className="model-pick"
                  role="radio"
                  aria-checked={picked}
                  onClick={() => void pick(m.name)}
                  disabled={busy}
                >
                  <span className="radio">{picked && <Check />}</span>
                  <span className="model-text">
                    <span className="model-name">{m.name}</span>
                    <span className="model-meta">
                      {m.parameterSize} · {m.quantization} · {(m.size / 1e9).toFixed(1)} GB
                    </span>
                  </span>
                </button>

                <span className="caps">
                  {BADGES.filter((b) => m.capabilities.includes(b.key)).map(({ key, label, Icon, title }) => (
                    <span key={key} className="cap" title={title}>
                      <Icon width={12} height={12} />
                      {label}
                    </span>
                  ))}
                  {m.error && (
                    <span className="cap warn" title={m.error}>
                      unknown
                    </span>
                  )}
                </span>

                <span className="residency">
                  {m.isResident ? (
                    <span className="loaded" title="Ollama has a runner for this in memory">
                      <span className="dot on" />
                      {(m.residentMib / 1024).toFixed(1)} GB
                    </span>
                  ) : (
                    <span className="unloaded">
                      <span className="dot" />
                      Not loaded
                    </span>
                  )}
                </span>

                <button
                  className="btn ghost sm model-act"
                  onClick={() => void run(m.name, m.isResident ? "unload" : "load")}
                  disabled={busy || working !== null}
                >
                  {working === m.name ? "Working…" : m.isResident ? "Unload" : "Load"}
                </button>
              </li>
            );
          })}
        </ul>

        {note && <p className="dialog-note">{note}</p>}

        <div className="dialog-foot">
          <GpuMeter card={card} />
          <p className="dialog-fine">
            16 GB holds one 8B model at a time — loading another makes them take turns. This
            figure is read from the card, not from <code>ollama ps</code>, which under-reports
            by about 2.5 GB.
          </p>
        </div>
      </div>
    </div>
  );
}
