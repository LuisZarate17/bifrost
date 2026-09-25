// End-to-end: file editing in the real Citadel, driven by the real model.
//
//   1. Start Citadel with its WebView2 debug port open:
//        $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=9223"
//        target\debug\citadel.exe            (or: npm run tauri -w apps/citadel -- dev)
//   2. node tools/e2e/citadel-edit.mjs
//
// What it proves that no unit test can: that a sentence typed into the composer
// becomes an approval card, that the diff on the card is the edit that lands on
// disk — byte for byte, CRLF endings included — and that Undo, Deny, a file
// changed behind the diff's back, and a path outside the folder all behave.
//
// It types into the real textarea and clicks the real buttons over CDP. It
// never calls a Rust command to do the thing under test.
//
// Leaves nothing behind: the project folder it edits is a temporary one, the
// config's `workspace` is restored to what it was (or removed), and every
// conversation the run created is deleted.
//
// The model is real, so a run can fail for model reasons — the G7 probe
// measured 98/100. A failure prints what the thread showed, so it can be read.

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
    throw new Error(`nothing on 127.0.0.1:${PORT} — start Citadel with the debug port (see the top of this file)`);
  }
  const page = targets.find((t) => t.type === "page");
  if (!page) throw new Error("Citadel has no page open");
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

// ── the project it edits ────────────────────────────────────────────────────

const crlf = (s) => Buffer.from(s.replace(/\n/g, "\r\n"));
const CONFIG_RS = 'pub const PORT: u16 = 8080;\npub const HOST: &str = "127.0.0.1";\n\npub fn address() -> String {\n    format!("{HOST}:{PORT}")\n}\n';
const MAIN_RS = 'mod config;\n\nfn greet(name: &str) -> String {\n    format!("Hello, {name}!")\n}\n\nfn main() {\n    println!("{}", greet("world"));\n}\n';

function makeProject() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "citadel-e2e-"));
  const proj = path.join(root, "demo-project");
  fs.mkdirSync(path.join(proj, "src"), { recursive: true });
  fs.writeFileSync(path.join(proj, "src", "config.rs"), crlf(CONFIG_RS));
  fs.writeFileSync(path.join(proj, "src", "main.rs"), crlf(MAIN_RS));
  fs.writeFileSync(path.join(root, "outside.txt"), "secret\r\n");
  return { root, proj };
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

const idle = `(() => { const t = document.querySelector(".composer textarea"); return t && !t.disabled; })()`;

/** Approve reads as they come; stop at the first edit card and return it. */
async function untilEditCard(js) {
  const end = Date.now() + 120_000;
  while (Date.now() < end) {
    const s = await js(`(() => {
      const edit = document.querySelector(".tool.edit.pending");
      if (edit) return { edit: true, add: [...edit.querySelectorAll(".dl.add .dl-text")].map(e => e.textContent), del: [...edit.querySelectorAll(".dl.del .dl-text")].map(e => e.textContent) };
      const read = document.querySelector(".tool.pending");
      if (read) { read.querySelector(".btn.primary").click(); return { read: true }; }
      const t = document.querySelector(".composer textarea");
      if (t && !t.disabled) return { done: true };
      return null;
    })()`);
    if (s?.edit) return s;
    if (s?.done) return null;
    await sleep(400);
  }
  throw new Error("timed out waiting for an edit card");
}

/** Let the turn finish: approve any read, decline any further edit. The model
 *  may re-read to check its work, or propose another change, after the one
 *  under test — waiting for idle alone would hang on that card. */
async function settle(js, what) {
  const end = Date.now() + 120_000;
  while (Date.now() < end) {
    const s = await js(`(() => {
      const edit = document.querySelector(".tool.edit.pending");
      if (edit) { [...edit.querySelectorAll(".btn")].find(b => b.textContent === "Deny").click(); return "declined another edit"; }
      const read = document.querySelector(".tool.pending");
      if (read) { read.querySelector(".btn.primary").click(); return "allowed a read"; }
      const t = document.querySelector(".composer textarea");
      return t && !t.disabled ? "idle" : null;
    })()`);
    if (s === "idle") return;
    await sleep(400);
  }
  throw new Error(`timed out waiting for ${what}`);
}

const threadText = (js) => js(`document.querySelector(".thread").innerText.slice(-1500)`);

// ── the run ─────────────────────────────────────────────────────────────────

const results = [];
function check(name, ok, detail = "") {
  results.push({ name, ok });
  console.log(`  ${ok ? "PASS" : "FAIL"}  ${name}${detail ? " — " + detail : ""}`);
}

const { root, proj } = makeProject();
const configBefore = fs.existsSync(CONFIG) ? fs.readFileSync(CONFIG) : null;
const cdp = await connect();
const { js } = cdp;
const convosBefore = new Set((await js(`window.__TAURI_INTERNALS__.invoke("conversations")`)).map((c) => c.id));

try {
  // Point Citadel at the throwaway project. Written to the config file directly
  // — the app has no command that takes a folder path, by design, so this is
  // the same route the native picker's choice takes.
  const cfg = configBefore ? JSON.parse(configBefore) : {};
  fs.mkdirSync(path.dirname(CONFIG), { recursive: true });
  fs.writeFileSync(CONFIG, JSON.stringify({ ...cfg, workspace: proj }, null, 2));
  // The plain URL, not a reload: a dev build left on ?preview=edit would seed
  // its own fake folder, and the test would be looking at the fixture.
  await js(`(location.href = location.origin + "/"), true`);
  await sleep(2500);
  await until(js, `document.querySelector(".composer textarea") !== null`, "the app to load");

  const configPath = path.join(proj, "src", "config.rs");
  const mainPath = path.join(proj, "src", "main.rs");

  // 1 ── apply, then undo ──────────────────────────────────────────────────
  console.log("\n1. apply an edit, then undo it");
  await newChat(js);
  await toolsOn(js);
  const chip = await js(`document.querySelector(".folder-chip")?.innerText ?? ""`);
  check("the folder chip names the project", chip.includes("demo-project"), chip);

  await say(cdp, "In src/config.rs, change the port from 8080 to 3000.");
  const card = await untilEditCard(js);
  check("an edit card appeared for approval", !!card, card ? "" : await threadText(js));
  if (card) {
    check("the diff removes the 8080 line", card.del.some((l) => l.includes("8080")), JSON.stringify(card.del));
    check("the diff adds the 3000 line", card.add.some((l) => l.includes("3000")), JSON.stringify(card.add));
    check("nothing is written before approval", fs.readFileSync(configPath).equals(crlf(CONFIG_RS)));

    await js(`document.querySelector(".tool.edit.pending .btn.primary").click(), true`);
    await settle(js, "the answer");
    const want = crlf(CONFIG_RS.replace("8080", "3000"));
    check("the file is exactly the approved edit, CRLF kept", fs.readFileSync(configPath).equals(want));
    check("the card says Applied", await js(`!!document.querySelector(".tool.edit.ran")`));

    await js(`[...document.querySelectorAll(".tool.edit.ran .btn")].find(b => b.textContent === "Undo").click(), true`);
    await until(js, `!!document.querySelector(".tool.edit.undone")`, "the undo");
    check("Undo restores the original bytes", fs.readFileSync(configPath).equals(crlf(CONFIG_RS)));
  }

  // 2 ── deny ──────────────────────────────────────────────────────────────
  console.log("\n2. deny an edit");
  await newChat(js);
  await say(cdp, "In src/main.rs, make main greet \"Luis\" instead of \"world\".");
  const card2 = await untilEditCard(js);
  check("an edit card appeared", !!card2, card2 ? "" : await threadText(js));
  if (card2) {
    check("the diff is the requested change", card2.add.some((l) => l.includes('"Luis"')), JSON.stringify(card2.add));
    await js(`[...document.querySelectorAll(".tool.edit.pending .btn")].find(b => b.textContent === "Deny").click(), true`);
    await until(js, `!!document.querySelector(".tool.edit.denied")`, "the denial");
    await settle(js, "the answer after denying");
    check("a denied edit writes nothing", fs.readFileSync(mainPath).equals(crlf(MAIN_RS)));
  }

  // 3 ── the file changes while the diff is on screen ──────────────────────
  console.log("\n3. the file changes before the edit is approved");
  await newChat(js);
  await say(cdp, "In src/config.rs, change the host from 127.0.0.1 to 0.0.0.0.");
  const card3 = await untilEditCard(js);
  check("an edit card appeared", !!card3, card3 ? "" : await threadText(js));
  if (card3) {
    const theirs = crlf(CONFIG_RS + "// someone else's line\n");
    fs.writeFileSync(configPath, theirs);
    await js(`document.querySelector(".tool.edit.pending .btn.primary").click(), true`);
    await until(js, `!!document.querySelector(".tool.edit.failed")`, "the refusal", 30_000);
    check("the stale edit is refused, not written", fs.readFileSync(configPath).equals(theirs));
    const note = await js(`document.querySelector(".tool.edit.failed .tool-note")?.innerText ?? ""`);
    check("the card says why", note.includes("changed after"), note);
    await settle(js, "the answer after the refusal");
    fs.writeFileSync(configPath, crlf(CONFIG_RS));
  }

  // 4 ── outside the folder ────────────────────────────────────────────────
  console.log("\n4. an edit outside the folder");
  await newChat(js);
  await say(cdp, "Use edit_file on ../outside.txt to replace the text secret with public. Do not read it first.");
  await settle(js, "the answer");
  const outside = fs.readFileSync(path.join(root, "outside.txt"), "utf8");
  check("the file outside the folder is untouched", outside === "secret\r\n", JSON.stringify(outside));
  const shown = await js(`[...document.querySelectorAll(".tool-rejected summary")].map(e => e.innerText).join(" | ")`);
  const cards = await js(`document.querySelectorAll(".tool.edit").length`);
  check("no approval card was offered for it", cards === 0, `${cards} edit cards`);
  console.log(`        (the thread showed: ${shown || "no attempt — the model declined on its own"})`);
} catch (e) {
  check("the run completed", false, String(e.message ?? e));
  console.log("\n  last of the thread:\n" + (await threadText(js).catch(() => "(unavailable)")));
} finally {
  // Put everything back the way it was.
  if (configBefore) fs.writeFileSync(CONFIG, configBefore);
  else fs.rmSync(CONFIG, { force: true });
  for (const c of await js(`window.__TAURI_INTERNALS__.invoke("conversations")`)) {
    if (!convosBefore.has(c.id)) await js(`window.__TAURI_INTERNALS__.invoke("delete_conversation", { id: ${JSON.stringify(c.id)} })`);
  }
  await js(`(location.href = location.origin + "/"), true`).catch(() => {});
  cdp.close();
  fs.rmSync(root, { recursive: true, force: true });
}

const failed = results.filter((r) => !r.ok).length;
console.log(`\n${results.length - failed}/${results.length} checks passed`);
process.exit(failed ? 1 : 0);
