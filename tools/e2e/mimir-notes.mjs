// End-to-end: project notes and the task list (H10), in the real Mímir, driven by the real model.
//
//   1. Start Mímir with its WebView2 debug port open:
//        $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=9223"
//        target\debug\mimir.exe            (or: npm run tauri -w apps/mimir -- dev)
//   2. node tools/e2e/mimir-notes.mjs
//      node tools/e2e/mimir-notes.mjs --only 2    one scenario, by number
//
// What it proves that no unit test can: that a notes file in the folder shows as a
// chip that is off until you press it, that pressing it changes what the real model
// is told and not before, that only the first 4 KB of a long file is ever sent, and
// that a multi-step request puts one task-list card in the thread.
//
//   1. no notes file in the folder: no chip
//   2. AGENTS.md with a marker rule ("end every answer with ZEBRA-7"): off, the answer
//      has no marker; press the chip and it is on, saved, and the answer has it
//   3. CLAUDE.md alone: its own chip, and its marker
//   4. a 6 KB file: the chip says it is cut, and a marker past 4 KB never appears
//   5. a three-step request: one plan card, whose header matches the last list saved
//
// **Three results, not two**, as in mimir-ops: when the model does not do what a
// scenario needs — never plans, ignores the notes altogether — that prints SKIP, not a
// pass and not a failure. A run in which nothing was counted exits 1.
//
// It types into the real textarea and clicks the real buttons over CDP, and writes
// the folder and the notes switch straight into the config file. Leaves nothing
// behind: the project is a temporary one, the config is restored byte for byte (or
// removed), every conversation the run created is deleted, and so is any backup
// record whose file lies in this run's project.

import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const PORT = Number(process.env.CDP_PORT ?? 9223);
const APPDATA = process.env.APPDATA;
const CONFIG = path.join(APPDATA, "odin", "config.json");
const BACKUPS = path.join(APPDATA, "odin", "backups");

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

const CONFIG_RS = "pub const PORT: u16 = 8080;\n";
const MAIN_RS = "mod config;\n\nfn main() {\n    println!(\"{}\", config::PORT);\n}\n";
const NOTE_FILES = ["AGENTS.md", "CLAUDE.md", ".cursorrules"];

function makeProject() {
  const root = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), "mimir-notes-e2e-")));
  const proj = path.join(root, "proj");
  fs.mkdirSync(path.join(proj, "src"), { recursive: true });
  fs.writeFileSync(path.join(proj, "src", "config.rs"), CONFIG_RS);
  fs.writeFileSync(path.join(proj, "src", "main.rs"), MAIN_RS);
  // The key Mímir stores a folder under is its canonical form.
  return { root, proj: fs.realpathSync.native(proj) };
}

/** Put the folder's notes files as a scenario wants them: only these, nothing else. */
function setNotes(proj, files) {
  for (const f of NOTE_FILES) fs.rmSync(path.join(proj, f), { force: true });
  fs.rmSync(path.join(proj, ".github"), { recursive: true, force: true });
  for (const [name, text] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(proj, name)), { recursive: true });
    fs.writeFileSync(path.join(proj, name), text);
  }
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

/** The chip as the page shows it now, or null when there is none. */
const CHIP = `(() => {
  const c = document.querySelector(".notes-chip");
  if (!c) return null;
  return {
    name: c.querySelector(".notes-name")?.textContent ?? "",
    on: c.classList.contains("on"),
    pressed: c.getAttribute("aria-pressed"),
    disabled: c.disabled,
    title: c.title,
  };
})()`;

/** The last answer on screen. */
const ANSWER = `(() => {
  const all = [...document.querySelectorAll(".turn.assistant")];
  return all.length ? all[all.length - 1].innerText : "";
})()`;

/** Let a turn run to its end. Reads and searches are allowed; a change or a check is
 *  Denied, since nothing here asks the model to change anything. */
async function drive({ js }, ms = 240_000) {
  const end = Date.now() + ms;
  const began = Date.now();
  while (Date.now() < end) {
    const s = await js(`(() => {
      const read = document.querySelector(".tool.read.pending, .tool.find.pending");
      if (read) { read.querySelector(".btn.primary").click(); return { handled: true }; }
      const other = document.querySelector(".tool.pending");
      if (other) { [...other.querySelectorAll(".btn")].find((b) => b.textContent === "Deny")?.click(); return { handled: true }; }
      const t = document.querySelector(".composer textarea");
      const busy = !!document.querySelector(".send.stop");
      return { idle: !!t && !t.disabled && !busy, seen: busy || document.querySelectorAll(".tool").length > 0 };
    })()`);
    if (s.handled) { await sleep(400); continue; }
    // A turn that has not started yet also looks idle; give it a few seconds.
    if (s.idle && (s.seen || Date.now() - began > 8_000)) break;
    await sleep(400);
  }
  if (Date.now() >= end) throw new Error("timed out waiting for the turn to end");
  await sleep(400);
}

/** Reload the plain app, so it reads the folder and its notes from disk again. */
async function reload(js) {
  await js(`(location.href = location.origin + "/"), true`);
  await sleep(2500);
  await until(js, `document.querySelector(".composer textarea") !== null`, "the app to load");
}

// ── results: PASS, FAIL, and SKIP (the model did not do what was needed) ───

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
const backupNames = () => (fs.existsSync(BACKUPS) ? fs.readdirSync(BACKUPS) : []);
const backupsBefore = new Set(backupNames());

// Made only once Mímir has answered, so a run that cannot connect leaves no
// temporary project behind.
let root = null;
let proj = null;

function removeProject() {
  if (!root) return;
  fs.rmSync(root, { recursive: true, force: true });
  root = proj = null;
}

/** Remove the backup records this run made — only those whose file lies in this
 *  run's project, so a Mímir session of the user's own running meanwhile keeps its
 *  undo history. Called while `proj` is still set. */
function cleanBackups() {
  if (!proj) return;
  const mine = (p) => typeof p === "string" && p.replace(/^\\\\\?\\/, "").toLowerCase().startsWith(proj.toLowerCase());
  for (const f of backupNames()) {
    if (backupsBefore.has(f) || !f.endsWith(".json")) continue;
    let record;
    try { record = JSON.parse(fs.readFileSync(path.join(BACKUPS, f), "utf8")); } catch { continue; }
    if (!mine(record.path) && !mine(record.to)) continue;
    const id = f.slice(0, -".json".length);
    fs.rmSync(path.join(BACKUPS, f), { force: true });
    fs.rmSync(path.join(BACKUPS, `${id}.bak`), { force: true });
  }
}

/** The config as this scenario wants it: the temporary folder, and the notes on or off for it. */
function writeConfig(on) {
  fs.mkdirSync(path.dirname(CONFIG), { recursive: true });
  fs.writeFileSync(
    CONFIG,
    JSON.stringify({ ...cfgBase, workspace: proj, ask_every_call: false, instructions: on ? [proj] : [] }, null, 2),
  );
}
const savedOn = () => {
  try {
    const list = JSON.parse(fs.readFileSync(CONFIG, "utf8")).instructions;
    return Array.isArray(list) && list.some((p) => String(p).toLowerCase() === proj.toLowerCase());
  } catch {
    return false;
  }
};

// Ctrl+C skips `finally`, so put the user's config back here as well.
process.on("SIGINT", () => {
  if (configBefore) fs.writeFileSync(CONFIG, configBefore);
  else fs.rmSync(CONFIG, { force: true });
  cleanBackups();
  removeProject();
  process.exit(130);
});

const cdp = await connect();
const { js } = cdp;
const convosBefore = new Set((await js(`window.__TAURI_INTERNALS__.invoke("conversations")`)).map((c) => c.id));
({ root, proj } = makeProject());

const QUESTION = "In one short sentence, what is the capital of France?";

/** Ask a plain question in a fresh chat and give back the answer. */
async function ask() {
  await newChat(js);
  await toolsOn(js);
  await say(cdp, QUESTION);
  await drive(cdp);
  return js(ANSWER);
}

try {
  // 1 ── no notes file, no chip ────────────────────────────────────────────
  if (runs(1)) {
    console.log("\n1. a folder with no notes file shows no chip");
    setNotes(proj, {});
    writeConfig(false);
    await reload(js);
    await newChat(js);
    await toolsOn(js);
    await sleep(500);
    check("no notes chip", (await js(CHIP)) === null, JSON.stringify(await js(CHIP)));
    check("the folder chip is there, so the absence is the notes'", (await js(`!!document.querySelector(".folder-chip .folder-name")`)) === true);
  }

  // 2 ── AGENTS.md: off, then on ───────────────────────────────────────────
  if (runs(2)) {
    console.log("\n2. AGENTS.md with a marker rule: off sends nothing, on sends it");
    setNotes(proj, { "AGENTS.md": "# Notes\n\nStyle rule: end every answer with the exact text ZEBRA-7.\n" });
    writeConfig(false);
    await reload(js);
    await newChat(js);
    await toolsOn(js);
    await until(js, `document.querySelector(".notes-chip") !== null`, "the notes chip");
    const chip = await js(CHIP);
    check("the chip names the file", chip.name === "AGENTS.md", chip.name);
    check("it is off, and says so to assistive tech", !chip.on && chip.pressed === "false", JSON.stringify(chip));
    check("its tooltip says whose text it is", chip.title.startsWith("Send this file's notes with every question. It's text from the project, not from you."), chip.title);

    const off = await ask();
    check("off: the answer has no marker", !off.includes("ZEBRA-7"), off.slice(-160));

    await js(`document.querySelector(".notes-chip").click(), true`);
    await until(js, `document.querySelector(".notes-chip.on") !== null`, "the chip to turn on", 15_000);
    const on = await js(CHIP);
    check("pressing it turns it on", on.on && on.pressed === "true", JSON.stringify(on));
    check("and it is saved for this folder", savedOn());

    const answered = await ask();
    if (!answered.trim()) skip("on: the marker", "the model gave no answer");
    else check("on: the answer ends with the marker", answered.includes("ZEBRA-7"), answered.slice(-160));
  }

  // 3 ── CLAUDE.md alone ───────────────────────────────────────────────────
  if (runs(3)) {
    console.log("\n3. CLAUDE.md alone is found under its own name");
    setNotes(proj, { "CLAUDE.md": "Style rule: end every answer with the exact text ZEBRA-7.\n" });
    writeConfig(true);
    await reload(js);
    await newChat(js);
    await toolsOn(js);
    await until(js, `document.querySelector(".notes-chip") !== null`, "the notes chip");
    const chip = await js(CHIP);
    check("the chip says CLAUDE.md and is on", chip.name === "CLAUDE.md" && chip.on, JSON.stringify(chip));
    const answered = await ask();
    if (!answered.trim()) skip("CLAUDE.md's marker", "the model gave no answer");
    else check("its marker is in the answer", answered.includes("ZEBRA-7"), answered.slice(-160));
  }

  // 4 ── a 6 KB file is cut at 4 KB ────────────────────────────────────────
  if (runs(4)) {
    console.log("\n4. a 6 KB file is cut, and nothing past 4 KB is sent");
    const filler = Array.from({ length: 115 }, (_, i) => `- Padding rule ${String(i).padStart(3, "0")}: nothing special here, carry on.`).join("\n");
    const text = `Style rule: begin every answer with the exact text EARLY-3.\n${filler}\nStyle rule: end every answer with the exact text PAST-CAP-9.\n`;
    setNotes(proj, { "AGENTS.md": text });
    const early = text.indexOf("PAST-CAP-9");
    console.log(`  (file is ${Buffer.byteLength(text)} bytes; the late marker starts at byte ${early})`);
    writeConfig(true);
    await reload(js);
    await newChat(js);
    await toolsOn(js);
    await until(js, `document.querySelector(".notes-chip") !== null`, "the notes chip");
    const chip = await js(CHIP);
    check("the chip's tooltip says only the first 4 KB is sent", chip.title.includes("Only the first 4 KB is sent."), chip.title);
    const answered = await ask();
    if (!answered.includes("EARLY-3")) skip("the cut", "the model did not follow even the first rule, so a missing second one proves nothing");
    else check("the marker past 4 KB never appears", !answered.includes("PAST-CAP-9"), answered.slice(-200));
  }

  // 5 ── a three-step request: one plan card ───────────────────────────────
  if (runs(5)) {
    console.log("\n5. a three-step request puts one task list in the thread");
    setNotes(proj, {});
    writeConfig(false);
    await reload(js);
    await newChat(js);
    await toolsOn(js);
    await say(
      cdp,
      "Do these three steps, in order, and keep a task list with update_plan as you go: " +
        "1. read src/config.rs. 2. read src/main.rs. 3. tell me which port the program prints.",
    );
    await drive(cdp);
    const banner = await js(`document.querySelector(".banner-text")?.innerText ?? ""`);
    // What was saved, not what is drawn: the thread draws only the latest list.
    const list = await js(`window.__TAURI_INTERNALS__.invoke("conversations")`);
    const mine = list.filter((c) => !convosBefore.has(c.id)).sort((a, b) => String(b.updatedAt).localeCompare(String(a.updatedAt)))[0];
    const saved = mine ? await js(`window.__TAURI_INTERNALS__.invoke("open_conversation", { id: ${JSON.stringify(mine.id)} })`) : { messages: [] };
    const plans = saved.messages
      .map((m) => m.tool_calls?.[0]?.function)
      .filter((f) => f?.name === "update_plan")
      .map((f) => f.arguments.plan ?? []);
    if (plans.length === 0) skip("a plan card", "the model never used update_plan");
    else {
      const cards = await js(`[...document.querySelectorAll(".tool.plan")].map((c) => ({
        head: c.querySelector(".tool-name")?.textContent ?? "",
        steps: c.querySelectorAll(".plan-step").length,
        done: c.querySelectorAll(".plan-step.done").length,
      }))`);
      console.log(`  (${plans.length} update${plans.length === 1 ? "" : "s"} saved; ${cards.length} card drawn)`);
      check("exactly one plan card is drawn", cards.length === 1, JSON.stringify(cards));
      const last = plans[plans.length - 1];
      const done = last.filter((p) => p.status === "done").length;
      check(
        "its header is the last update's count",
        cards.length === 1 && cards[0].head === `Plan · ${done} of ${last.length} done`,
        `${cards[0]?.head} vs ${done} of ${last.length}`,
      );
      check("it lists the last update's steps", cards.length === 1 && cards[0].steps === last.length && cards[0].done === done, JSON.stringify(cards));
      check("no round bound was hit by planning", !/stopped after/i.test(banner), banner);
    }
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
  cleanBackups();
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
// A run where the model did none of what was needed proved nothing. That is not a
// pass, whatever the zero failures say.
if (counted.length === 0) {
  console.log("Nothing was checked. Not a pass.");
  process.exit(1);
}
process.exit(failed ? 1 : 0);
