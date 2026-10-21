// End-to-end: Check with Claude (I6) in the real Mimir, against a stand-in `claude`.
//
//   1. Start Mimir with its WebView2 debug port open:
//        $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=9223"
//        target\debug\mimir.exe
//   2. node tools/e2e/mimir-claude.mjs
//      node tools/e2e/mimir-claude.mjs --only 4     one scenario, by number
//
// **Never calls the real `claude`.** The config's `claude_command` points at a
// `claude.cmd` written into a temporary folder, which answers `auth status` from a
// file, and for anything else records what it was given (its arguments, its standard
// input, its working folder, that it was called at all) and prints a canned reply.
// What it proves that no unit test can: that the switch is off until it is turned on,
// that nothing is sent by showing the button or by Cancel, that Send puts the question
// and the turn's diff on the command's standard input, that the answer lands in a card
// labelled Claude and is kept with the thread, that a failure is a card that says so in
// words, and that Stop kills the run and everything it started.
//
// No local model is involved: the turn is seeded — a saved conversation whose two
// changes have undo records written straight into the backups folder, in the format
// `bifrost_core::edit` writes — so the scenarios are about the wiring only.
//
// Leaves nothing behind: the config is restored byte for byte (or removed), the seeded
// conversation, its undo records and the temporary folder are removed.
//
// Exit code: 1 on any FAIL, or when nothing was counted; otherwise 2 if anything was
// skipped; 0 only when everything counted passed.

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const PORT = Number(process.env.CDP_PORT ?? 9223);
const APPDATA = process.env.APPDATA;
const CONFIG = path.join(APPDATA, "bifrost", "config.json");
const BACKUPS = path.join(APPDATA, "bifrost", "backups");

// ── a tiny CDP client ───────────────────────────────────────────────────────

async function connect() {
  let targets;
  try {
    targets = await (await fetch(`http://127.0.0.1:${PORT}/json`)).json();
  } catch {
    throw new Error(`nothing on 127.0.0.1:${PORT} — start Mimir with the debug port (see the top of this file)`);
  }
  const page = targets.find((t) => t.type === "page");
  if (!page) throw new Error("Mimir has no page open");
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

async function until(js, expr, what, ms = 30_000) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    const v = await js(expr);
    if (v) return v;
    await sleep(200);
  }
  throw new Error(`timed out waiting for ${what}`);
}

/** How many of this program are running right now. */
function running(image) {
  const out = execFileSync("tasklist", ["/FI", `IMAGENAME eq ${image}`, "/FO", "CSV", "/NH"], { encoding: "utf8" });
  return out.split("\n").filter((l) => l.toLowerCase().includes(image.toLowerCase())).length;
}

// ── the run ─────────────────────────────────────────────────────────────────

const results = [];
function check(name, ok, detail = "") {
  results.push({ name, ok });
  console.log(`  ${ok ? "PASS" : "FAIL"}  ${name}${detail ? " — " + detail : ""}`);
  return ok;
}

const onlyAt = process.argv.indexOf("--only");
const only = onlyAt > 0 ? Number(process.argv[onlyAt + 1]) : null;
const runs = (n) => only === null || only === n;

// Connect first: a failed connection (the app is not running) leaves no temp folder.
const cdp = await connect();
const { js } = cdp;

const root = fs.mkdtempSync(path.join(os.tmpdir(), "mimir-claude-e2e-"));
const proj = fs.realpathSync.native(root);
const stand = path.join(root, "claude.cmd");
const at = (name) => path.join(root, name);
const read = (name) => (fs.existsSync(at(name)) ? fs.readFileSync(at(name), "utf8") : null);

const QUESTION = "E2E-CLAUDE-QUESTION-6120 make the greeting friendlier";
const ANSWER = "E2E-CLAUDE-ANSWER-8841: the change looks fine, but the greeting is not tested.";
const configBefore = fs.existsSync(CONFIG) ? fs.readFileSync(CONFIG) : null;
const cfgBase = configBefore ? JSON.parse(configBefore) : {};
const convosBefore = new Set((await js(`window.__TAURI_INTERNALS__.invoke("conversations")`)).map((c) => c.id));
const seededIds = [];

function restore() {
  if (configBefore) fs.writeFileSync(CONFIG, configBefore);
  else fs.rmSync(CONFIG, { force: true });
}
process.on("SIGINT", () => {
  restore();
  for (const id of seededIds) for (const ext of ["json", "bak"]) fs.rmSync(path.join(BACKUPS, `${id}.${ext}`), { force: true });
  fs.rmSync(root, { recursive: true, force: true });
  process.exit(130);
});

/** Write the config: the user's own, with the Claude settings this scenario needs.
 *  `checks` undefined leaves the field out, which is how a config from before I6 reads. */
function useConfig({ checks, command = stand }) {
  const cfg = { ...cfgBase, claude_command: command };
  delete cfg.claude_checks;
  if (checks !== undefined) cfg.claude_checks = checks;
  fs.mkdirSync(path.dirname(CONFIG), { recursive: true });
  fs.writeFileSync(CONFIG, JSON.stringify(cfg, null, 2));
}

const auth = (loggedIn) =>
  fs.writeFileSync(at("auth.json"), JSON.stringify(loggedIn ? { loggedIn: true, authMethod: "claude.ai", email: "e2e@example.com" } : { loggedIn: false }));
const reply = (obj) => fs.writeFileSync(at("reply.json"), JSON.stringify(obj));
const forget = () => ["calls.txt", "auth-calls.txt", "stdin.txt", "args.txt", "cwd.txt", "slow.flag"].forEach((f) => fs.rmSync(at(f), { force: true }));

// The stand-in. `%~dp0` is its own folder. It answers `auth status` from auth.json and
// otherwise records everything and prints reply.json — slowly, when slow.flag is there.
fs.writeFileSync(
  stand,
  [
    "@echo off",
    'if "%1"=="auth" (',
    '  echo auth>> "%~dp0auth-calls.txt"',
    '  type "%~dp0auth.json"',
    "  exit /b 0",
    ")",
    'echo called>> "%~dp0calls.txt"',
    'findstr "^" > "%~dp0stdin.txt"',
    'echo %*> "%~dp0args.txt"',
    'cd > "%~dp0cwd.txt"',
    'if exist "%~dp0slow.flag" ping -n 60 127.0.0.1 >nul',
    'type "%~dp0reply.json"',
    "",
  ].join("\r\n"),
);
auth(true);
reply({ type: "result", subtype: "success", is_error: false, result: ANSWER });

/** The seeded turn: a saved conversation whose edit and create have undo records. */
function seed() {
  fs.mkdirSync(path.join(root, "src"), { recursive: true });
  const a = path.join(root, "src", "a.txt");
  const b = path.join(root, "src", "b.txt");
  fs.writeFileSync(a, "hello there\nsecond line\n");
  fs.writeFileSync(b, "a new file\n");
  const base = BigInt(Date.now()) * 1_000_000n;
  const idEdit = base.toString(16);
  const idCreate = (base + 1n).toString(16);
  seededIds.push(idEdit, idCreate);
  fs.mkdirSync(BACKUPS, { recursive: true });
  fs.writeFileSync(path.join(BACKUPS, `${idEdit}.bak`), "hello\nsecond line\n");
  fs.writeFileSync(
    path.join(BACKUPS, `${idEdit}.json`),
    JSON.stringify({ path: a, display: "src/a.txt", create: false, after: 0, before: null, kind: "edit" }),
  );
  fs.writeFileSync(
    path.join(BACKUPS, `${idCreate}.json`),
    JSON.stringify({ path: b, display: "src/b.txt", create: true, after: 0, kind: "create", made_dirs: [] }),
  );
  const diff = { hunks: [], added: 1, removed: 1, create: false };
  const tool = (name, p, undo) => ({
    role: "tool",
    content: `${name} ${p}`,
    tool_calls: [{ function: { name, arguments: { path: p, diff, undo } } }],
  });
  return [
    { role: "user", content: QUESTION },
    tool("edit_file", "src/a.txt", idEdit),
    tool("create_file", "src/b.txt", idCreate),
    { role: "assistant", content: "Done: I made the greeting friendlier and added src/b.txt." },
  ];
}

const saved = async (messages) =>
  js(`window.__TAURI_INTERNALS__.invoke("save_conversation", { conversation: {
    id: "", title: "", created_at: "", updated_at: "", messages: ${JSON.stringify(messages)} } })`);
const stored = async (id) => (await js(`window.__TAURI_INTERNALS__.invoke("open_conversation", { id: ${JSON.stringify(id)} })`)).messages;

/** Reload the page (so it re-reads the config) and open the seeded conversation. */
async function openSeeded(id) {
  await js(`(location.href = location.origin + "/"), true`);
  await sleep(2500);
  await until(js, `document.querySelector(".composer textarea") !== null`, "the app to load");
  await until(js, `[...document.querySelectorAll(".cv-open")].some(b => b.textContent.includes("E2E-CLAUDE-QUESTION-6120"))`, "the seeded thread in the sidebar", 15_000);
  await js(`[...document.querySelectorAll(".cv-open")].find(b => b.textContent.includes("E2E-CLAUDE-QUESTION-6120")).click(), true`);
  await until(js, `document.querySelector(".turn.assistant") !== null`, "the thread to open");
}

const lineText = (j) => j(`document.querySelector(".turn-undo")?.innerText ?? ""`);
const buttons = (j) => j(`[...document.querySelectorAll(".turn-undo .btn")].map(b => b.textContent)`);
const click = (j, sel, text) =>
  j(`(() => { const b = [...document.querySelectorAll(${JSON.stringify(sel)})].find(b => ${text === undefined ? "true" : `b.textContent === ${JSON.stringify(text)}`}); if (!b) return false; b.click(); return true; })()`);

async function openSettings() {
  await js(`document.querySelector(".sb-link").click(), true`);
  await until(js, `document.querySelector(".dialog.appearance") !== null`, "the settings");
}
const closeSettings = () => js(`document.querySelector('.dialog.appearance [aria-label="Close"]').click(), true`);
const statusLine = () => until(js, `(() => { const t = document.querySelector(".claude-status")?.innerText ?? ""; return t.startsWith("Looking") ? "" : t; })()`, "the status line", 20_000);

try {
  const seeded = await saved(seed());
  const id = seeded.id;

  // 1 ── off by default ────────────────────────────────────────────────────
  if (runs(1)) {
    console.log("\n1. with the switch off there is no button, and nothing can be sent");
    forget();
    useConfig({}); // no claude_checks field: a config from before I6
    await openSeeded(id);
    check("the turn's Undo all is there", (await buttons(js)).includes("Undo all"), JSON.stringify(await buttons(js)));
    check("there is no Check with Claude button", !(await buttons(js)).includes("Check with Claude"));
    const refused = await js(`window.__TAURI_INTERNALS__.invoke("claude_check", { question: "q", undoIds: [], hash: "", page: "e2e", request: 1 }).then(() => "sent", (e) => String(e))`);
    check("the command itself refuses while the switch is off", /off/i.test(refused), refused);
    check("the stand-in was never called", read("calls.txt") === null);
  }

  // 2 ── the settings ──────────────────────────────────────────────────────
  if (runs(2)) {
    console.log("\n2. the settings: the status line and the switch");
    forget();
    useConfig({});
    await openSeeded(id);
    await openSettings();
    check("the note says nothing is sent until you press the button",
      (await js(`document.querySelector(".dialog.appearance").innerText`)).includes("only when you press Check with Claude"));
    const sw = `document.querySelector('.dialog.appearance [role="switch"]')`;
    check("the switch is off", (await js(`${sw}.getAttribute("aria-checked")`)) === "false");
    const off = await statusLine();
    check("with the switch off the status is not checked", off === "Not checked while Claude checks are off", off);
    await sleep(800);
    check("and claude was not run to find out", read("auth-calls.txt") === null);
    await js(`${sw}.click(), true`);
    await until(js, `${sw}.getAttribute("aria-checked") === "true"`, "the switch to turn on", 10_000);
    const signedIn = await until(js, `(() => { const t = document.querySelector(".claude-status")?.innerText ?? ""; return t.startsWith("Signed in") ? t : ""; })()`, "the status after turning on", 20_000);
    check("turning it on looks for claude, and a signed-in one is named", signedIn === "Signed in as e2e@example.com", signedIn);
    check("turning it on is saved in the config", JSON.parse(fs.readFileSync(CONFIG, "utf8")).claude_checks === true);
    await closeSettings();
    await until(js, `[...document.querySelectorAll(".turn-undo .btn")].some(b => b.textContent === "Check with Claude")`, "the button", 10_000);
    check("the button appears next to Undo all", (await buttons(js)).join("|").startsWith("Undo all|Check with Claude"), JSON.stringify(await buttons(js)));

    auth(false);
    await openSettings();
    const notIn = await statusLine();
    check("an unsigned claude says how to sign in", notIn === "Not signed in — run claude in a terminal to sign in", notIn);
    await closeSettings();
    auth(true);

    useConfig({ checks: true, command: at("no-such-claude.cmd") });
    await openSettings();
    const none = await statusLine();
    check("a missing claude is said so", none === "claude command not found", none);
    await closeSettings();
    check("the stand-in was never asked a question", read("calls.txt") === null);
  }

  // 3 ── the preview, and Cancel ───────────────────────────────────────────
  if (runs(3)) {
    console.log("\n3. the button shows what would go; Cancel sends nothing");
    forget();
    useConfig({ checks: true });
    await openSeeded(id);
    check("the button is there", (await buttons(js)).includes("Check with Claude"));
    await click(js, ".turn-undo .btn", "Check with Claude");
    await until(js, `document.querySelector(".claude-confirm-text") !== null`, "the confirm row", 10_000);
    const row = await lineText(js);
    check("it says what it sends", /Sends your question and 2 files' changes \(\d+ (B|KB)\) to Claude/.test(row.replace(/\s+/g, " ")), JSON.stringify(row));
    check("it offers Send and Cancel", (await buttons(js)).includes("Send") && (await buttons(js)).includes("Cancel"));
    check("showing it ran nothing", read("calls.txt") === null);
    await click(js, ".turn-undo .btn", "Cancel");
    await until(js, `document.querySelector(".claude-confirm-text") === null`, "the row to go", 5_000);
    check("Cancel brings the button back", (await buttons(js)).includes("Check with Claude"));
    await sleep(800);
    check("Cancel sent nothing", read("calls.txt") === null);
    check("no card appeared", (await js(`document.querySelectorAll(".claude-card").length`)) === 0);
  }

  // 4 ── Send ──────────────────────────────────────────────────────────────
  if (runs(4)) {
    console.log("\n4. Send puts the question and the diff to Claude, and the answer lands in a Claude card");
    forget();
    reply({ type: "result", subtype: "success", is_error: false, result: ANSWER });
    useConfig({ checks: true });
    await openSeeded(id);
    await click(js, ".turn-undo .btn", "Check with Claude");
    await until(js, `[...document.querySelectorAll(".turn-undo .btn")].some(b => b.textContent === "Send")`, "Send", 10_000);
    await click(js, ".turn-undo .btn", "Send");
    await until(js, `document.querySelector(".claude-card") !== null`, "the Claude card", 30_000);
    const card = await js(`document.querySelector(".claude-card").innerText`);
    check("the card is labelled Claude", /^Claude\b/.test(card), JSON.stringify(card.slice(0, 40)));
    check("it holds the answer", card.includes("E2E-CLAUDE-ANSWER-8841"), card.slice(0, 120));
    check("it is not an error card", !(await js(`document.querySelector(".claude-card").classList.contains("error")`)));
    check("it comes after the turn's answer",
      await js(`(() => { const a = [...document.querySelectorAll(".turn.assistant")].pop(); const c = document.querySelector(".claude-card");
        return !!(a.compareDocumentPosition(c) & Node.DOCUMENT_POSITION_FOLLOWING); })()`));

    const stdin = read("stdin.txt") ?? "";
    check("the question went on standard input", stdin.includes(QUESTION), stdin.slice(0, 120));
    check("so did the edit's diff", stdin.includes("--- a/src/a.txt") && stdin.includes("-hello") && stdin.includes("+hello there"));
    check("and the created file's", stdin.includes("--- /dev/null") && stdin.includes("+++ b/src/b.txt") && stdin.includes("+a new file"));
    const args = read("args.txt") ?? "";
    check("it was run with -p, json output and no tools",
      /(^|\s)-p(\s|$)/.test(args) && args.includes("--output-format json") && args.includes('--tools ""') && args.includes("--no-session-persistence"), args.trim());
    check("with the user's hooks, notes and MCP servers kept out", args.includes("--safe-mode") && args.includes("--strict-mcp-config"), args.trim().slice(0, 140));
    const cwd = (read("cwd.txt") ?? "").trim();
    check("it ran in a folder of its own, not the project", cwd.toLowerCase().endsWith("bifrost-claude-check") && !cwd.toLowerCase().startsWith(proj.toLowerCase()), cwd);

    await sleep(500);
    const msgs = await stored(id);
    const kept = msgs.filter((m) => m.role === "claude");
    check("the answer is saved with the thread", kept.length === 1 && kept[0].content.includes("E2E-CLAUDE-ANSWER-8841"), JSON.stringify(msgs.map((m) => m.role)));
    check("the line still offers a check", (await buttons(js)).includes("Check with Claude"));

    await openSeeded(id);
    check("reopened, the card is still there", (await js(`document.querySelectorAll(".claude-card").length`)) === 1);
    check("and still after the turn's line",
      await js(`(() => { const l = document.querySelector(".turn-undo"); const c = document.querySelector(".claude-card");
        return !!(l.compareDocumentPosition(c) & Node.DOCUMENT_POSITION_FOLLOWING); })()`));
  }

  // 5 ── a failure ─────────────────────────────────────────────────────────
  if (runs(5)) {
    console.log("\n5. a failed check is a card that says so in words, and is not saved");
    forget();
    reply({ type: "result", is_error: true, result: "Not logged in - Please run /login" });
    useConfig({ checks: true });
    await openSeeded(id);
    const before = (await stored(id)).length;
    await click(js, ".turn-undo .btn", "Check with Claude");
    await until(js, `[...document.querySelectorAll(".turn-undo .btn")].some(b => b.textContent === "Send")`, "Send", 10_000);
    await click(js, ".turn-undo .btn", "Send");
    await until(js, `document.querySelector(".claude-card.error") !== null`, "the error card", 30_000);
    const card = await js(`document.querySelector(".claude-card.error").innerText`);
    check("it is labelled Claude and says Check failed", /Claude/.test(card) && card.includes("Check failed"), JSON.stringify(card.slice(0, 80)));
    check("it says what to do", card.includes("not signed in"), card.slice(0, 160));
    await sleep(500);
    check("it is not saved", (await stored(id)).length === before);
  }

  // 6 ── Stop ──────────────────────────────────────────────────────────────
  if (runs(6)) {
    console.log("\n6. Stop ends a running check and everything it started");
    forget();
    fs.writeFileSync(at("slow.flag"), "slow");
    reply({ type: "result", is_error: false, result: "too late" });
    useConfig({ checks: true });
    await openSeeded(id);
    const pingsBefore = running("PING.EXE");
    const cardsBefore = await js(`document.querySelectorAll(".claude-card").length`);
    await click(js, ".turn-undo .btn", "Check with Claude");
    await until(js, `[...document.querySelectorAll(".turn-undo .btn")].some(b => b.textContent === "Send")`, "Send", 10_000);
    await click(js, ".turn-undo .btn", "Send");
    await until(js, `(document.querySelector(".turn-undo")?.innerText ?? "").includes("Claude is checking")`, "the wait", 10_000);
    await sleep(1500);
    check("the stand-in is running", read("calls.txt") !== null && running("PING.EXE") > pingsBefore);
    check("there is a Stop button", (await buttons(js)).includes("Stop"));

    // The turns' Stop (what Stop and Esc on an answer send) is not the check's.
    await js(`window.__TAURI_INTERNALS__.invoke("stop", {})`);
    await sleep(1500);
    check("a turn's Stop does not end the check",
      (await lineText(js)).includes("Claude is checking") && running("PING.EXE") > pingsBefore);

    // And the composer waits: nothing can be sent while the check runs.
    const turnsBefore = await js(`document.querySelectorAll(".turn.user").length`);
    await js(`document.querySelector(".composer textarea").focus(), true`);
    await cdp.call("Input.insertText", { text: "E2E-SENT-WHILE-CHECKING" });
    await sleep(200);
    check("Send is disabled while a check runs", await js(`document.querySelector(".composer .send")?.disabled === true`));
    await cdp.call("Input.dispatchKeyEvent", { type: "keyDown", key: "Enter", code: "Enter", windowsVirtualKeyCode: 13 });
    await cdp.call("Input.dispatchKeyEvent", { type: "keyUp", key: "Enter", code: "Enter", windowsVirtualKeyCode: 13 });
    await sleep(500);
    check("and Enter sends nothing", (await js(`document.querySelectorAll(".turn.user").length`)) === turnsBefore);

    const started = Date.now();
    await click(js, ".turn-undo .btn", "Stop");
    await until(js, `[...document.querySelectorAll(".turn-undo .btn")].some(b => b.textContent === "Check with Claude")`, "the button to come back", 15_000);
    check("it stopped quickly", Date.now() - started < 10_000, `${Date.now() - started} ms`);
    await sleep(500);
    check("no ping is left running", running("PING.EXE") <= pingsBefore);
    check("a stopped check leaves no card", (await js(`document.querySelectorAll(".claude-card").length`)) === cardsBefore);
  }

  // 7 ── the preview is what is sent ───────────────────────────────────────
  if (runs(7)) {
    console.log("\n7. a file that changes between the preview and Send stops the send");
    forget();
    reply({ type: "result", is_error: false, result: ANSWER });
    useConfig({ checks: true });
    await openSeeded(id);
    const a = path.join(root, "src", "a.txt");
    const original = fs.readFileSync(a, "utf8");
    await click(js, ".turn-undo .btn", "Check with Claude");
    await until(js, `[...document.querySelectorAll(".turn-undo .btn")].some(b => b.textContent === "Send")`, "Send", 10_000);
    fs.writeFileSync(a, original + "changed after the preview\n");
    await click(js, ".turn-undo .btn", "Send");
    await until(js, `document.querySelector(".claude-card.error") !== null`, "the refusal", 30_000);
    const card = await js(`document.querySelector(".claude-card.error").innerText`);
    check("it says the files changed since the preview", card.includes("The files changed since the preview"), card.slice(0, 120));
    check("and sent nothing", read("calls.txt") === null);
    fs.writeFileSync(a, original);
  }

  // 8 ── a Stop that comes first ───────────────────────────────────────────
  if (runs(8)) {
    console.log("\n8. a check's Stop that arrives before the check still stops it");
    forget();
    useConfig({ checks: true });
    await openSeeded(id);
    const call = (cmd, args) => js(`window.__TAURI_INTERNALS__.invoke(${JSON.stringify(cmd)}, ${JSON.stringify(args)}).then((v) => ({ ok: v }), (e) => ({ err: String(e) }))`);
    await call("claude_stop", { page: "e2e-page", request: 7 });
    const started = Date.now();
    const got = await call("claude_check", { question: "q", undoIds: seededIds, hash: "x", page: "e2e-page", request: 7 });
    check("the check comes back stopped, at once", got.ok === null && Date.now() - started < 3000, JSON.stringify(got));
    check("and ran nothing", read("calls.txt") === null);
    const next = await call("claude_check", { question: "q", undoIds: seededIds, hash: "x", page: "e2e-page", request: 8 });
    check("the next check is not caught by it", typeof next.err === "string" && next.err.includes("changed since the preview"), JSON.stringify(next));
  }
} catch (e) {
  check("the run completed", false, String(e.message ?? e));
  console.log("\n  last of the thread:\n" + (await js(`document.querySelector(".thread")?.innerText.slice(-1200)`).catch(() => "(unavailable)")));
} finally {
  restore();
  for (const c of await js(`window.__TAURI_INTERNALS__.invoke("conversations")`).catch(() => [])) {
    if (!convosBefore.has(c.id)) await js(`window.__TAURI_INTERNALS__.invoke("delete_conversation", { id: ${JSON.stringify(c.id)} })`).catch(() => {});
  }
  await js(`(location.href = location.origin + "/"), true`).catch(() => {});
  cdp.close();
  for (const sid of seededIds) for (const ext of ["json", "bak"]) fs.rmSync(path.join(BACKUPS, `${sid}.${ext}`), { force: true });
  fs.rmSync(root, { recursive: true, force: true });
}

const skipped = results.filter((r) => r.skip).length;
const counted = results.filter((r) => !r.skip);
const failed = counted.filter((r) => !r.ok).length;
console.log(`\n${counted.length - failed}/${counted.length} checks passed (${skipped} skipped)`);
if (counted.length === 0) {
  console.log("Nothing was checked: no scenario ran. Not a pass.");
  process.exit(1);
}
process.exit(failed ? 1 : skipped ? 2 : 0);
