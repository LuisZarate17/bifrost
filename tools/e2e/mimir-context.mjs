// End-to-end: the context budget inside a tool turn (H7), in the real Mímir,
// driven by the real model.
//
//   1. Start Mímir with its WebView2 debug port open:
//        $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=9223"
//        target\debug\mimir.exe            (or: npm run tauri -w apps/mimir -- dev)
//   2. node tools/e2e/mimir-context.mjs
//      node tools/e2e/mimir-context.mjs --only 2    one scenario, by number
//
// The rule it proves: inside one tool turn, when the prompt would outgrow the
// model's context, Mímir sets old tool results aside (a one-line stub goes to the
// model in their place) instead of letting Ollama silently drop the middle of the
// prompt, which loses the user's request. The thread says so with a muted
// `.dropped` note, or, if even that is not enough, a `.dropped.warn` one. The
// context meter under the composer updates every round of the turn.
//
//   1. at num_ctx 8192, three ~14,000-character files cannot all fit: results are
//      set aside, the request about the giraffe survives, the last file's marker
//      is found, the meter moved, and the saved thread still holds the full files
//   2. at the user's normal num_ctx, three small files set nothing aside
//
// Changing num_ctx makes Ollama reload the model at the new size (~10-20 s on the
// first request). The user's config is restored at the end, which reloads it back
// on next use; that is fine.
//
// **Three results, not two.** The model is real. When it does not make the reads
// (fewer than two read_file calls), nothing about the budget was exercised: that
// prints SKIP, is not counted as a pass, and is not a failure.
//
// Leaves nothing behind: the project is a temporary one, the config is restored
// byte for byte (or removed), and every conversation the run created is deleted.

import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const PORT = Number(process.env.CDP_PORT ?? 9223);
const APPDATA = process.env.APPDATA;
const CONFIG = path.join(APPDATA, "odin", "config.json");

// ── a tiny CDP client ───────────────────────────────────────────────────────

async function connect() {
  let targets;
  try {
    targets = await (await fetch(`http://127.0.0.1:${PORT}/json`)).json();
  } catch {
    throw new Error(`nothing on 127.0.0.1:${PORT} — start Mímir with the debug port (see the top of this file)`);
  }
  const page = targets.find((t) => t.type === "page");
  if (!page) throw new Error("Mímir has no page open");
  const ws = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise((ok, fail) => { ws.onopen = ok; ws.onerror = fail; });
  let id = 0;
  const waiting = new Map();
  ws.onmessage = (m) => {
    const d = JSON.parse(m.data);
    if (waiting.has(d.id)) { waiting.get(d.id)(d); waiting.delete(d.id); }
  };
  const call = (method, params = {}) =>
    new Promise((ok) => { const n = ++id; waiting.set(n, ok); ws.send(JSON.stringify({ id: n, method, params })); });
  const js = async (expr) => {
    const d = await call("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true });
    if (d.result?.exceptionDetails) throw new Error("in page: " + JSON.stringify(d.result.exceptionDetails.exception?.description ?? d.result.exceptionDetails));
    return d.result?.result?.value;
  };
  return { call, js, close: () => ws.close() };
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function until(js, expr, what, ms = 90_000) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    const v = await js(expr);
    if (v) return v;
    await sleep(400);
  }
  throw new Error(`timed out waiting for ${what}`);
}

// ── the project ─────────────────────────────────────────────────────────────

const WORDS = [
  "scheduler", "backlog", "handshake", "timeout", "replica", "checkpoint", "gateway", "retry", "latency",
  "payload", "session", "cursor", "buffer", "manifest", "shard", "quorum", "lease", "token", "digest",
  "upstream", "fallback", "throttle", "snapshot", "dispatch", "ledger", "beacon", "cluster", "region",
  "pipeline", "watchdog", "mirror", "journal", "segment", "window", "anchor", "bridge", "canary", "daemon",
];
const LEVELS = ["INFO", "WARN", "DEBUG", "TRACE", "ERROR"];

/** Deterministic, so a run is repeatable; varied, so it does not compress to few tokens. */
function rng(seed) {
  let s = seed >>> 0;
  return () => ((s = (Math.imul(s, 1664525) + 1013904223) >>> 0) / 2 ** 32);
}

/** Numbered log-like lines to about `size` characters, with one marker line near the end. */
function fileText(seed, size, marker) {
  const r = rng(seed);
  const pick = (a) => a[Math.floor(r() * a.length)];
  const line = (n) =>
    `${String(n).padStart(4, "0")} ${pick(LEVELS)} ${pick(WORDS)}-${Math.floor(r() * 9000 + 1000)} ` +
    `${pick(WORDS)} ${pick(WORDS)} took ${Math.floor(r() * 900 + 10)}ms on ${pick(WORDS)}${Math.floor(r() * 90 + 10)}`;
  const lines = [];
  let n = 1;
  let len = 0;
  while (len < size) {
    const l = line(n++);
    lines.push(l);
    len += l.length + 1;
  }
  lines.splice(lines.length - 3, 0, marker);
  return lines.join("\n") + "\n";
}

const MARKERS = { a: "MARKER-A-5521", b: "MARKER-B-8830", c: "MARKER-C-3417" };

function writeFiles(proj, size) {
  [["a", 11], ["b", 22], ["c", 33]].forEach(([k, seed]) => {
    fs.writeFileSync(path.join(proj, `${k}.txt`), fileText(seed, size, MARKERS[k]));
  });
}

function makeProject() {
  const root = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), "mimir-context-e2e-")));
  const proj = path.join(root, "proj");
  fs.mkdirSync(proj);
  // The key Mímir stores a folder under is its canonical form.
  return { root, proj: fs.realpathSync.native(proj) };
}

// ── driving the page ────────────────────────────────────────────────────────

async function newChat(js) {
  await js(`(() => { const b = document.querySelector(".new-chat") ?? document.querySelector('[aria-label="New chat"]'); b.click(); return true; })()`);
  await until(js, `document.querySelector(".main.empty") !== null`, "an empty chat");
}

async function toolsOn(js) {
  await js(`(() => { const t = document.querySelector(".composer-bar .toggle"); if (t.getAttribute("aria-pressed") !== "true") t.click(); return true; })()`);
  await until(js, `document.querySelector(".composer-bar .toggle").getAttribute("aria-pressed") === "true"`, "Tools on");
}

async function say({ call, js }, text) {
  await js(`(() => { const t = document.querySelector(".composer textarea"); t.focus(); return true; })()`);
  await call("Input.insertText", { text });
  await sleep(150);
  await call("Input.dispatchKeyEvent", { type: "keyDown", key: "Enter", code: "Enter", windowsVirtualKeyCode: 13 });
  await call("Input.dispatchKeyEvent", { type: "keyUp", key: "Enter", code: "Enter", windowsVirtualKeyCode: 13 });
}

/** Approve anything pending until the turn is idle. Since H5 a read inside the
 *  folder does not ask, so usually nothing is. */
async function drive(js, what, ms = 300_000) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    const s = await js(`(() => {
      const p = document.querySelector(".tool.pending");
      if (p) { p.querySelector(".btn.primary")?.click(); return "allowed"; }
      const t = document.querySelector(".composer textarea");
      const idle = !!t && !t.disabled && !document.querySelector(".send.stop");
      // A turn that has not started yet also looks idle.
      const seen = document.querySelectorAll(".tool").length > 0 || document.querySelectorAll(".turn.assistant").length > 0;
      return idle && seen ? "idle" : null;
    })()`);
    if (s === "idle") { await sleep(600); return; }
    await sleep(400);
  }
  throw new Error(`timed out waiting for ${what}`);
}

const SNAPSHOT = `(() => {
  const ctx = document.querySelector(".ctx");
  return {
    thread: document.querySelector(".thread")?.innerText ?? "",
    answer: [...document.querySelectorAll(".turn.assistant")].map((e) => e.innerText).pop() ?? "",
    reads: document.querySelectorAll(".tool.read").length,
    notes: [...document.querySelectorAll(".dropped")].map((e) => ({ text: e.innerText, warn: e.classList.contains("warn") })),
    meter: ctx ? { title: ctx.getAttribute("title") ?? "", text: ctx.innerText } : null,
  };
})()`;

/** Meter tooltip "N of LIMIT tokens in the last request" -> { spent, limit }. */
function meterNumbers(m) {
  const hit = /^(\S+) of (\S+) tokens/.exec(m?.title ?? "");
  if (!hit) return null;
  const n = (s) => Number(s.replace(/\D/g, ""));
  return { spent: n(hit[1]), limit: n(hit[2]) };
}

const PROMPT =
  "Read a.txt, then b.txt, then c.txt, one at a time with read_file, and tell me the marker line from each. " +
  "Also: what colour is the giraffe I mentioned? (It is purple.)";

async function runTurn(cdp, what) {
  await newChat(cdp.js);
  await toolsOn(cdp.js);
  await say(cdp, PROMPT);
  await drive(cdp.js, what);
  return cdp.js(SNAPSHOT);
}

// ── results: PASS, FAIL, and SKIP (the model did not make the calls) ───────

const results = [];
function check(name, ok, detail = "") {
  results.push({ name, ok, skip: false });
  console.log(`  ${ok ? "PASS" : "FAIL"}  ${name}${detail ? " — " + detail : ""}`);
  return ok;
}
function skip(name, why) {
  results.push({ name, ok: true, skip: true });
  console.log(`  SKIP  ${name} — ${why}`);
}

const onlyAt = process.argv.indexOf("--only");
const only = onlyAt > 0 ? Number(process.argv[onlyAt + 1]) : null;
const runs = (n) => only === null || only === n;

const configBefore = fs.existsSync(CONFIG) ? fs.readFileSync(CONFIG) : null;
const cfgBase = configBefore ? JSON.parse(configBefore) : {};

// Made only once Mímir has answered, so a run that cannot connect leaves no
// temporary project behind.
let root = null;
let proj = null;

function removeProject() {
  if (!root) return;
  fs.rmSync(root, { recursive: true, force: true });
  root = proj = null;
}

// Ctrl+C skips `finally`, so put the user's config back here as well.
process.on("SIGINT", () => {
  if (configBefore) fs.writeFileSync(CONFIG, configBefore);
  else fs.rmSync(CONFIG, { force: true });
  removeProject();
  process.exit(130);
});

const cdp = await connect();
const { js } = cdp;
const convosBefore = new Set((await js(`window.__TAURI_INTERNALS__.invoke("conversations")`)).map((c) => c.id));
({ root, proj } = makeProject());

/** Point Mímir at the project with this context size. `ask_every_call` is written
 *  explicitly, so the user's own setting cannot change what a scenario sees. */
function configure(numCtx) {
  fs.mkdirSync(path.dirname(CONFIG), { recursive: true });
  fs.writeFileSync(
    CONFIG,
    JSON.stringify({ ...cfgBase, workspace: proj, num_ctx: numCtx, ask_every_call: false }, null, 2),
  );
}

async function reload() {
  // The plain URL, not a reload of a ?preview: a preview fakes its own folder.
  await js(`(location.href = location.origin + "/"), true`);
  await sleep(2500);
  await until(js, `document.querySelector(".composer textarea") !== null`, "the app to load");
}

/** The conversation this run just made, if it is not one already seen. */
const seenConvos = new Set();
async function newestConvo() {
  await sleep(1500);
  const all = await js(`window.__TAURI_INTERNALS__.invoke("conversations")`);
  const c = all.find((x) => !convosBefore.has(x.id) && !seenConvos.has(x.id)) ?? null;
  if (c) seenConvos.add(c.id);
  return c;
}

try {
  // 1 ── a squeezed turn keeps the request ─────────────────────────────────
  if (runs(1)) {
    console.log("\n1. at num_ctx 8192, old results are set aside and the request survives");
    writeFiles(proj, 14_000);
    configure(8192);
    await reload();
    const t = await runTurn(cdp, "the turn to end (the first request reloads the model at the new size)");
    const convo = await newestConvo();
    const before = results.length;
    if (t.reads < 2) skip("the budget", `the model made ${t.reads} read_file calls, need at least 2`);
    else {
      const warn = t.notes.find((n) => n.warn);
      const aside = t.notes.find((n) => /Set aside \d+ earlier result/.test(n.text));
      if (warn) check("the thread says some results were set aside", false, warn.text);
      else check("the thread says some results were set aside", !!aside, aside?.text ?? JSON.stringify(t.notes));
      check("the request survived: the answer mentions the giraffe", /purple|giraffe/i.test(t.answer), t.answer.slice(0, 200));
      check("the answer has the last file's marker", t.answer.includes(MARKERS.c), t.answer.slice(0, 200));
      const m = meterNumbers(t.meter);
      check("the context meter was updated, and is within the context", !!m && m.spent > 0 && m.spent <= 8192, JSON.stringify(t.meter));
      if (convo) {
        const saved = (await js(`window.__TAURI_INTERNALS__.invoke("open_conversation", { id: ${JSON.stringify(convo.id)} })`)).messages;
        check(
          "the saved tool result is the full file",
          saved.some((x) => x.role === "tool" && JSON.stringify(x).includes("MARKER-A-")),
          `${saved.length} messages`,
        );
      } else skip("the saved tool result is the full file", "the conversation was not found");
    }
    if (results.slice(before).some((r) => !r.ok)) console.log("\n  last of the thread:\n" + t.thread.slice(-1500));
  }

  // 2 ── no squeeze when everything fits ───────────────────────────────────
  if (runs(2)) {
    const normal = cfgBase.num_ctx ?? 32768;
    console.log(`\n2. at num_ctx ${normal}, small files set nothing aside`);
    writeFiles(proj, 2_000);
    configure(normal);
    await reload();
    const t = await runTurn(cdp, "the turn to end");
    await newestConvo();
    const before = results.length;
    if (t.reads < 2) skip("nothing set aside", `the model made ${t.reads} read_file calls, need at least 2`);
    else {
      check("no note about set-aside results or the context", t.notes.length === 0, JSON.stringify(t.notes));
      check(
        "the answer has all three markers",
        Object.values(MARKERS).every((x) => t.answer.includes(x)),
        t.answer.slice(0, 200),
      );
      const m = meterNumbers(t.meter);
      check("the context meter shows", !!m && m.spent > 0, JSON.stringify(t.meter));
    }
    if (results.slice(before).some((r) => !r.ok)) console.log("\n  last of the thread:\n" + t.thread.slice(-1500));
  }
} catch (e) {
  check("the run completed", false, String(e.message ?? e));
  const t = await js(`document.querySelector(".thread")?.innerText.slice(-1500) ?? ""`).catch(() => "(unavailable)");
  console.log("\n  last of the thread:\n" + t);
} finally {
  // Put everything back the way it was — the files first, since nothing below
  // can fail them, and each conversation on its own, so one that will not
  // delete does not keep the rest.
  if (configBefore) fs.writeFileSync(CONFIG, configBefore);
  else fs.rmSync(CONFIG, { force: true });
  removeProject();
  for (const c of await js(`window.__TAURI_INTERNALS__.invoke("conversations")`).catch(() => [])) {
    if (!convosBefore.has(c.id)) {
      await js(`window.__TAURI_INTERNALS__.invoke("delete_conversation", { id: ${JSON.stringify(c.id)} })`).catch(() => {});
    }
  }
  await js(`(location.href = location.origin + "/"), true`).catch(() => {});
  cdp.close();
}

const skipped = results.filter((r) => r.skip).length;
const counted = results.filter((r) => !r.skip);
const failed = counted.filter((r) => !r.ok).length;
console.log(`\n${counted.length - failed}/${counted.length} checks passed (${skipped} skipped)`);
// A run where the model made too few calls proved nothing about the budget.
// That is not a pass, whatever the zero failures say.
if (counted.length === 0) {
  console.log("Nothing was checked: the model made too few calls. Not a pass.");
  process.exit(1);
}
// 1 for a fail, 2 when a scenario was skipped: a skip proved nothing, so it must not read as a pass.
process.exit(failed ? 1 : skipped ? 2 : 0);
