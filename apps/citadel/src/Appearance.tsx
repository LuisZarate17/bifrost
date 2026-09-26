import { useEffect, useRef, useState, type CSSProperties } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  DEFAULT_SEED, PRESETS, loadTheme, makeTheme, saveTheme,
  type Scheme, type Source, type Theme,
} from "./accent";
import { Check, Windows, X } from "./Icons";

const SCHEMES: { value: Scheme; label: string }[] = [
  { value: "system", label: "System" },
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark" },
];

const current = (): Theme => loadTheme() ?? makeTheme(DEFAULT_SEED, "preset", "system");

/**
 * Re-read the Windows accent if the user chose to follow it.
 *
 * Called at startup and whenever the window regains focus — the moment after
 * someone changes the colour in Windows Settings and comes back. There is no
 * change notification to listen to without pulling in WinRT, and a registry read
 * on focus costs nothing.
 */
export async function syncWindowsAccent() {
  const t = loadTheme();
  if (t?.source !== "windows") return;
  try {
    const hex = await invoke<string | null>("system_accent");
    if (hex && hex !== t.seed) saveTheme(makeTheme(hex, "windows", t.scheme));
  } catch {
    // Keep the colour already applied.
  }
}

/**
 * Theme and accent. Every change applies at once — the app behind the dialog is
 * the preview — and is saved as it is made, so there is no Save button to miss.
 */
export function Appearance({ onClose }: { onClose: () => void }) {
  const [theme, setTheme] = useState<Theme>(current);
  const [windows, setWindows] = useState<string | null>(null);
  const panel = useRef<HTMLDivElement>(null);

  useEffect(() => {
    panel.current?.focus();
    invoke<string | null>("system_accent").then(setWindows).catch(() => {});
  }, []);

  const update = (seed: string, source: Source, scheme: Scheme = theme.scheme) => {
    const next = makeTheme(seed, source, scheme);
    saveTheme(next);
    setTheme(next);
  };

  const custom = theme.source === "custom";

  return (
    <div className="scrim" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div
        className="dialog appearance"
        role="dialog"
        aria-modal="true"
        aria-labelledby="appearance-title"
        tabIndex={-1}
        ref={panel}
        onKeyDown={(e) => {
          if (e.key === "Escape") onClose();
        }}
      >
        <div className="dialog-head">
          <div>
            <h2 id="appearance-title">Appearance</h2>
            <p className="dialog-sub">Changes apply straight away and are kept on this machine.</p>
          </div>
          <button className="icon-btn" onClick={onClose} aria-label="Close" title="Close (Esc)">
            <X />
          </button>
        </div>

        <div className="settings">
          <section className="setting">
            <div className="setting-text">
              <h3>Theme</h3>
              <p>System follows your Windows setting.</p>
            </div>
            <div className="seg" role="radiogroup" aria-label="Theme">
              {SCHEMES.map((s) => (
                <button
                  key={s.value}
                  role="radio"
                  aria-checked={theme.scheme === s.value}
                  className={theme.scheme === s.value ? "on" : ""}
                  onClick={() => update(theme.seed, theme.source, s.value)}
                >
                  {s.label}
                </button>
              ))}
            </div>
          </section>

          <section className="setting stacked">
            <div className="setting-text">
              <h3>Accent colour</h3>
              <p>Marks sending, focus, the tower and anything waiting on you. Every shade is checked for contrast in both themes.</p>
            </div>
            <div className="swatches" role="radiogroup" aria-label="Accent colour">
              {PRESETS.map((p) => {
                const on = theme.source === "preset" && theme.seed === p.seed;
                return (
                  <button
                    key={p.seed}
                    role="radio"
                    aria-checked={on}
                    aria-label={p.name}
                    title={p.name}
                    className={"swatch" + (on ? " on" : "")}
                    style={{ "--c": p.seed } as CSSProperties}
                    onClick={() => update(p.seed, "preset")}
                  />
                );
              })}
              <label
                className={"swatch custom" + (custom ? " on" : "")}
                title="Custom colour"
                style={custom ? ({ "--c": theme.seed } as CSSProperties) : undefined}
              >
                <input
                  type="color"
                  aria-label="Custom colour"
                  value={custom ? theme.seed : DEFAULT_SEED}
                  onChange={(e) => update(e.target.value, "custom")}
                />
              </label>
            </div>
            <button
              className={"match-windows" + (theme.source === "windows" ? " on" : "")}
              onClick={() => windows && update(windows, "windows")}
              disabled={!windows}
              aria-pressed={theme.source === "windows"}
            >
              <Windows />
              <span className="mw-text">
                <span className="mw-title">
                  Match Windows
                  {windows && <span className="mw-chip" style={{ background: windows }} aria-hidden="true" />}
                </span>
                <span className="muted">
                  {windows ? "Follows the accent colour in Windows Settings" : "Windows did not report an accent colour"}
                </span>
              </span>
              {theme.source === "windows" && <Check className="mw-check" />}
            </button>
          </section>
        </div>
      </div>
    </div>
  );
}
