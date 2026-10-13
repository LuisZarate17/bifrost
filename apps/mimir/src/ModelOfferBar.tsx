import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { splitName } from "./Switchboard";
import { Refresh, X } from "./Icons";

/** What `model_offer` (and `accept_model_offer`'s recomputation of it in Rust)
 *  can hand back — mirrors `ModelOffer` in `apps/mimir/src-tauri/src/lib.rs`. */
export type ModelOffer = { kind: "coding" | "back"; from: string; to: string };

/** The model pill's own trick — family plus size — so the offer names a model
 *  the way the rest of the window already does, rather than a bare tag. */
function nice(tag: string): string {
  const { family, size } = splitName(tag);
  return size ? `${family} ${size}` : tag;
}

/** How long a success note stays up before the bar hides itself. */
const SUCCESS_LINGER_MS = 4_000;

/**
 * The bar that offers to switch models once a project folder changes.
 *
 * Shown by `App.tsx` right above the composer, after `chooseFolder`,
 * `reopenFolder` or `clearFolder` — never on app start, which is why the
 * fetch that drives it lives in the caller, not here. This component only
 * acts on an offer it is already holding.
 */
export function ModelOfferBar({
  offer,
  busy,
  onDismiss,
  onHide,
  onSwitched,
  onSwitching,
  onRefresh,
}: {
  offer: ModelOffer;
  /** An answer is running — the same flag the check picker disables on. */
  busy: boolean;
  /** Declined: hide the bar, and remember not to show this offer again this
   *  session. */
  onDismiss: () => void;
  /** Hide the bar without remembering anything — after a switch landed, or
   *  failed. Neither is the user saying no to the offer. */
  onHide: () => void;
  /** The switch landed: update the model pill immediately, without waiting
   *  for a refresh that would not override it anyway (the pill only follows
   *  the config once, the first time it loads). */
  onSwitched: (tag: string) => void;
  /** A switch started (`true`) or ended (`false`, whatever came of it). The window
   *  holds the composer's send while it runs: a question sent in between would go
   *  out with the old tag, and Ollama would load that model beside the new one. */
  onSwitching: (on: boolean) => void;
  /** Refresh everything else the Switchboard keeps — residency, the GPU meter. */
  onRefresh: () => void;
}) {
  const [state, setState] = useState<"idle" | "working" | "done" | "error">("idle");
  const [note, setNote] = useState("");
  const hideTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);

  useEffect(() => () => clearTimeout(hideTimer.current), []);

  const accept = async () => {
    setState("working");
    onSwitching(true);
    try {
      // The offer is recomputed in Rust from the config and what is
      // installed — only the direction is sent, never a model tag.
      const secs = await invoke<number>("accept_model_offer", { kind: offer.kind });
      setState("done");
      setNote(`${nice(offer.to)} is ready (${secs.toFixed(0)} s).`);
      onSwitched(offer.to);
      onRefresh();
      // Safe to fire late: App keys this bar by its offer, so a newer offer
      // is a different bar and unmounting this one clears the timer.
      hideTimer.current = setTimeout(onHide, SUCCESS_LINGER_MS);
    } catch (e) {
      setState("error");
      setNote(String(e));
      // The config was already switched when only the load failed (Rust says so
      // with this wording, and the model loads with the next question). The window's
      // model has to follow the config, or the next question goes out with the old tag.
      if (String(e).startsWith(`Switched to ${offer.to}, but loading it failed`)) {
        onSwitched(offer.to);
        onRefresh();
      }
    } finally {
      onSwitching(false);
    }
  };

  const toName = nice(offer.to);
  const fromName = nice(offer.from);

  return (
    <div className="model-offer" role="status">
      <Refresh width={14} height={14} />
      <span className="model-offer-text">
        {state === "working" && `Loading ${toName}…`}
        {(state === "done" || state === "error") && note}
        {state === "idle" && offer.kind === "coding" && (
          <>
            {/* About ten seconds: 10.5 and 10.8 s measured 2026-10-02 at the app's num_ctx, and 11 s
                straight after a reboot. */}
            <strong>{toName}</strong> is the model for working in code. Switch to it? Loading takes about
            ten seconds, and Vidar can't see your screen until you switch back.
          </>
        )}
        {state === "idle" && offer.kind === "back" && (
          <>
            Folder closed. Switch back to <strong>{toName}</strong>? Vidar needs it to see your screen.
          </>
        )}
      </span>
      {state === "error" ? (
        <button className="btn ghost sm" onClick={onHide}>
          <X width={12} height={12} />
          Dismiss
        </button>
      ) : state === "idle" ? (
        <>
          <button className="btn ghost sm" onClick={onDismiss} disabled={busy}>
            {offer.kind === "coding" ? "Not now" : `Keep ${fromName}`}
          </button>
          <button className="btn accent sm" onClick={() => void accept()} disabled={busy}>
            {offer.kind === "coding" ? "Switch" : "Switch back"}
          </button>
        </>
      ) : null}
    </div>
  );
}
