// End-to-end: new folders, delete and move (H9), in the real Mímir, driven by the real model.
//
//   1. Start Mímir with its WebView2 debug port open:
//        $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=9223"
//        target\debug\mimir.exe            (or: npm run tauri -w apps/mimir -- dev)
//   2. node tools/e2e/mimir-ops.mjs
//      node tools/e2e/mimir-ops.mjs --only 2    one scenario, by number
//
// What it proves that no unit test can: that a sentence typed into the composer
// becomes a create, delete or move card, that approving it does exactly what the
// card said on the real disk, and that Undo puts it back — byte for byte for a
// deleted CRLF file, and with the folders a create or a move made taken away.
//
//   1. create a file in folders that do not exist: the card lists them; undo removes the file and them
//   2. delete a CRLF file: a Delete card with its size; undo brings back the exact bytes
//   3. move a file into a new folder: a Move card showing "from → to"; undo moves it back and tidies
//   4. delete and move into .git, or outside the folder: refused before any card, nothing touched
//   5. "Undo all" over an edit plus a move: asks first, then puts the whole project back
//
// **Three results, not two**, as in mimir-policy: when the model does not make the
// call a scenario needs, that prints SKIP — not a pass, not a failure. A WARN line
// is printed too and counted as neither (a card the script denied that is worth a look).
// Exit code: 1 on any FAIL, or when nothing was counted; otherwise 2 if anything
// was skipped; 0 only when everything counted passed and nothing was skipped.
//
// It types into the real textarea and clicks the real buttons over CDP, and writes
// the folder straight into the config file. Leaves nothing behind: the project is
// a temporary one, the config is restored byte for byte (or removed), every
// conversation the run created is deleted, and so is any backup record whose
// file — or whose move destination — lies in this run's project.

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

const crlf = (s) => Buffer.from(s.replace(/\n/g, "\r\n"));
const CONFIG_RS = "pub const PORT: u16 = 8080;\n";
const MAIN_RS = "mod config;\n\nfn main() {\n    println!(\"{}\", config::PORT);\n}\n";
const OLD_NOTES = "first line\nsecond line\nthird line\n";
const NOTES = "remember the milk\n";
const OUTSIDE = "outside-secret\r\n";
const GIT_CONFIG = "[core]\n\tbare = false\n";

function makeProject() {
  const root = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), "mimir-ops-e2e-")));
  const proj = path.join(root, "proj");
  fs.mkdirSync(path.join(proj, "src"), { recursive: true });
  fs.mkdirSync(path.join(proj, ".git"), { recursive: true });
  fs.writeFileSync(path.join(proj, "src", "config.rs"), crlf(CONFIG_RS));
  fs.writeFileSync(path.join(proj, "src", "main.rs"), crlf(MAIN_RS));
  fs.writeFileSync(path.join(proj, "old_notes.txt"), crlf(OLD_NOTES));
  fs.writeFileSync(path.join(proj, "notes.txt"), crlf(NOTES));
  fs.writeFileSync(path.join(proj, ".git", "config"), GIT_CONFIG);
  fs.writeFileSync(path.join(root, "outside.txt"), OUTSIDE);
  // The key Mímir stores a folder under is its canonical form.
  return { root, proj: fs.realpathSync.native(proj) };
}

/** Every file and folder under `dir`, with the bytes of each file — what "back
 *  exactly as it was" is compared against, folders included. */
function snapshot(dir) {
  const out = {};
  const walk = (d) => {
    for (const e of fs.readdirSync(d, { withFileTypes: true })) {
      const p = path.join(d, e.name);
      const rel = path.relative(dir, p).replace(/\\/g, "/");
      if (e.isDirectory()) { out[rel + "/"] = null; walk(p); }
      else out[rel] = fs.readFileSync(p).toString("base64");
    }
  };
  walk(dir);
  return out;
}
const same = (a, b) => JSON.stringify(Object.entries(a).sort()) === JSON.stringify(Object.entries(b).sort());

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

/** What the page's cards look like now, for the checks to read. */
const CARDS = `(() => {
  const one = (c) => ({
    classes: [...c.classList].join(" "),
    name: c.querySelector(".tool-name")?.textContent ?? "",
    shown: c.querySelector(".tool-path")?.textContent ?? "",
    size: c.querySelector(".tool-range")?.textContent ?? "",
    folders: [...c.querySelectorAll(".tool-folders code")].map((e) => e.textContent),
    primary: c.querySelector(".btn.primary")?.textContent ?? "",
    note: [...c.querySelectorAll(".tool-note")].map((e) => e.innerText).join(" | "),
  });
  return {
    cards: [...document.querySelectorAll(".tool.edit")].map(one),
    pending: [...document.querySelectorAll(".tool.edit.pending")].map(one),
    rejected: [...document.querySelectorAll(".tool-rejected summary")].map((e) => e.innerText),
  };
})()`;

/**
 * Let a turn run to its end. Reads and searches are allowed; a change card is
 * approved when `approve(card)` says so and Denied otherwise. Every change card
 * that asked is recorded, so a scenario can say what was offered.
 */
async function drive(cdp, approve, ms = 240_000) {
  const { js } = cdp;
  const asked = [];
  const readAsked = [];
  const end = Date.now() + ms;
  const began = Date.now();
  while (Date.now() < end) {
    const s = await js(`(() => {
      const edit = document.querySelector(".tool.edit.pending");
      if (edit) return { edit: true };
      const read = document.querySelector(".tool.read.pending, .tool.find.pending");
      if (read) {
        const seen = { classes: [...read.classList].join(" "), shown: read.querySelector(".tool-path")?.textContent ?? read.innerText };
        read.querySelector(".btn.primary").click();
        return { read: true, seen };
      }
      const t = document.querySelector(".composer textarea");
      const busy = !!document.querySelector(".send.stop");
      return { idle: !!t && !t.disabled && !busy, seen: busy || document.querySelectorAll(".tool").length > 0 };
    })()`);
    if (s.read) readAsked.push(s.seen);
    if (s.edit) {
      const card = (await js(CARDS)).pending[0];
      asked.push(card);
      const label = approve(card) ? card.primary : "Deny";
      await js(`[...document.querySelectorAll(".tool.edit.pending .btn")].find((b) => b.textContent === ${JSON.stringify(label)})?.click(), true`);
      await until(js, `!document.querySelector(".tool.edit.pending")`, "the card to be decided", 30_000).catch(() => {});
      continue;
    }
    // A turn that has not started yet also looks idle; give it a few seconds.
    if (s.idle && (s.seen || Date.now() - began > 8_000)) break;
    await sleep(400);
  }
  if (Date.now() >= end) throw new Error("timed out waiting for the turn to end");
  await sleep(400);
  return { asked, readAsked, ...(await js(CARDS)) };
}

async function clickUndo(js, selector) {
  await js(`[...document.querySelectorAll(${JSON.stringify(selector + " .btn")})].find((b) => b.textContent === "Undo")?.click(), true`);
  await until(js, `!!document.querySelector(${JSON.stringify(selector.replace(".ran", ".undone"))})`, "the undo");
}

// ── results: PASS, FAIL, and SKIP (the model did not make the call) ────────

const results = [];
let warns = 0;
function warn(name, detail) {
  warns++;
  console.log(`  WARN  ${name} — ${detail}`);
}
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

/** Remove the backup records this run made — only those whose file, or whose move
 *  destination, lies in this run's project, so a Mímir session of the user's own
 *  running meanwhile keeps its undo history. Called while `proj` is still set. */
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
const START = snapshot(proj);

try {
  fs.mkdirSync(path.dirname(CONFIG), { recursive: true });
  fs.writeFileSync(CONFIG, JSON.stringify({ ...cfgBase, workspace: proj, ask_every_call: false }, null, 2));
  // The plain URL, not a reload of a ?preview: a preview fakes its own folder.
  await js(`(location.href = location.origin + "/"), true`);
  await sleep(2500);
  await until(js, `document.querySelector(".composer textarea") !== null`, "the app to load");

  // 1 ── create in new folders ─────────────────────────────────────────────
  if (runs(1)) {
    console.log("\n1. create a file in folders that do not exist, then undo");
    await newChat(js);
    await toolsOn(js);
    await say(cdp, "Use create_file to create src/net/http/client.rs containing exactly: pub fn get() {}. Do not use any other tool first.");
    const t = await drive(cdp, () => true);
    const card = t.asked.find((c) => c.classes.includes("create"));
    if (!card) skip("a create in new folders", "the model did not call create_file");
    else {
      check("the card lists the folders it will make", card.folders.join(",") === "src/net/,src/net/http/", JSON.stringify(card.folders));
      check("the file exists after approval", fs.existsSync(path.join(proj, "src", "net", "http", "client.rs")));
      check("the card says Applied, with the folders it made",
        t.cards.some((c) => c.classes.includes("ran") && c.folders.length === 2), JSON.stringify(t.cards.map((c) => c.classes)));
      await clickUndo(js, ".tool.edit.create.ran");
      check("undo removes the file and the folders it made", !fs.existsSync(path.join(proj, "src", "net")));
      check("the card says the file is removed", (await js(CARDS)).cards.some((c) => c.note.includes("the file is removed")));
      check("the project is exactly as it started", same(snapshot(proj), START));
    }
  }

  // 2 ── delete a CRLF file ────────────────────────────────────────────────
  if (runs(2)) {
    console.log("\n2. delete a CRLF file, then undo");
    await newChat(js);
    await say(cdp, "Use delete_file to delete old_notes.txt. Do not use any other tool first.");
    const t = await drive(cdp, () => true);
    const card = t.asked.find((c) => c.classes.includes("delete"));
    if (!card) skip("a delete", "the model did not call delete_file");
    else {
      check("it is a Delete card with its size", card.name === "Delete file" && card.size === `${crlf(OLD_NOTES).length} B`, `${card.name} / ${card.size}`);
      check("the button says Delete file", card.primary === "Delete file", card.primary);
      check("the file is gone after approval", !fs.existsSync(path.join(proj, "old_notes.txt")));
      check("the card says Deleted", t.cards.some((c) => c.classes.includes("ran") && c.classes.includes("delete")));
      await clickUndo(js, ".tool.edit.delete.ran");
      check("undo brings back the exact bytes, CRLF kept",
        fs.existsSync(path.join(proj, "old_notes.txt")) && fs.readFileSync(path.join(proj, "old_notes.txt")).equals(crlf(OLD_NOTES)));
      check("the card says the file is back", (await js(CARDS)).cards.some((c) => c.note.includes("the file is back")));
      check("the project is exactly as it started", same(snapshot(proj), START));
    }
  }

  // 3 ── move into a new folder ────────────────────────────────────────────
  if (runs(3)) {
    console.log("\n3. move a file into a new folder, then undo");
    await newChat(js);
    await say(cdp, "Use move_file to move notes.txt to docs/archive/notes.txt. Do not use any other tool first.");
    const t = await drive(cdp, () => true);
    const card = t.asked.find((c) => c.classes.includes("move"));
    if (!card) skip("a move", "the model did not call move_file");
    else {
      check("it is a Move card", card.name === "Move file", card.name);
      check('it shows "from → to"', card.shown.replace(/\s+/g, " ").includes("notes.txt → docs/archive/notes.txt"), card.shown);
      check("it lists the folders it will make", card.folders.join(",") === "docs/,docs/archive/", JSON.stringify(card.folders));
      check("the file moved, bytes kept",
        !fs.existsSync(path.join(proj, "notes.txt")) &&
          fs.readFileSync(path.join(proj, "docs", "archive", "notes.txt")).equals(crlf(NOTES)));
      await clickUndo(js, ".tool.edit.move.ran");
      check("undo moves it back and takes the folders away", fs.readFileSync(path.join(proj, "notes.txt")).equals(crlf(NOTES)) && !fs.existsSync(path.join(proj, "docs")));
      check("the project is exactly as it started", same(snapshot(proj), START));
    }
  }

  // 4 ── refused places ────────────────────────────────────────────────────
  if (runs(4)) {
    console.log("\n4. delete and move into .git, or outside the folder, are refused");
    const prompts = [
      ["a delete of .git/config", "Use delete_file to delete .git/config. Do not use any other tool first.", "delete_file", "inside .git"],
      ["a delete outside the folder", "Use delete_file to delete ../outside.txt. Do not use any other tool first.", "delete_file", "outside the project folder"],
      ["a move into .git", "Use move_file to move notes.txt to .git/notes.txt. Do not use any other tool first.", "move_file", "inside .git"],
      ["a move outside the folder", "Use move_file to move notes.txt to ../moved.txt. Do not use any other tool first.", "move_file", "outside the project folder"],
    ];
    for (const [what, prompt, tool, reason] of prompts) {
      await newChat(js);
      await say(cdp, prompt);
      const t = await drive(cdp, () => false);
      const called = t.rejected.length > 0 || t.asked.length > 0;
      // Measured 2026-10-09: asked for "../outside.txt", the model sent
      // "outside.txt" — a file inside the folder, refused as missing. That is not
      // the call this check is about, so it is a SKIP, like no call at all; the
      // refusal of the real path is what the check below is for, and "nothing was
      // touched" still proves no harm.
      const sentAnotherPath =
        !t.rejected.some((r) => r.includes(reason)) && t.rejected.some((r) => r.includes("doesn't exist")) && t.asked.length === 0;
      if (!called) skip(what, "the model answered without making the call");
      else if (sentAnotherPath) skip(what, `the model sent another path, refused as missing — ${JSON.stringify(t.rejected)}`);
      else {
        // The refused call itself never reaches a card. What the model tries
        // next is its own call — measured 2026-10-03: after a refused move into
        // .git it proposed a create elsewhere — and that one asks, and is denied
        // here; "nothing was touched" below is what proves no harm.
        // A card of ANY kind (read, edit, create, delete, move, find) whose shown
        // path or destination is the refused place would be the hole: into .git,
        // or outside the folder. A card elsewhere is the model trying something
        // else, which asks like any other — measured 2026-10-08: after a refused
        // move, a delete of the same file; denied here, and shown as a WARN.
        const verb = tool === "move_file" ? "Couldn't move" : "Couldn't delete";
        const refusedPlace = (c) => /(^|[\\/ ])\.git([\\/]|$)|\.\.[\\/]|outside|moved\.txt/.test(c.shown);
        const every = [...t.asked, ...t.readAsked];
        const own = every.filter(refusedPlace);
        check(
          `${what}: no card of any kind was offered for the refused place`,
          own.length === 0,
          JSON.stringify(every.map((c) => `${c.classes}: ${c.shown}`)),
        );
        check(
          `${what}: the thread says "${verb}" and "${reason}"`,
          t.rejected.some((r) => r.includes(verb) && r.includes(reason)),
          JSON.stringify(t.rejected),
        );
        if (tool === "move_file") {
          const other = every.filter((c) => !c.classes.includes("move") && c.shown.includes("notes.txt"));
          if (other.length) warn(`${what}: after the refusal the model offered a card of another kind on the same file`, `${JSON.stringify(other.map((c) => `${c.classes}: ${c.shown}`))} — asked and denied, so not unsafe`);
        }
      }
    }
    check("nothing was touched", same(snapshot(proj), START) && fs.readFileSync(path.join(root, "outside.txt")).toString() === OUTSIDE);
    check("no file appeared outside the folder", !fs.existsSync(path.join(root, "moved.txt")));
  }

  // 5 ── Undo all over an edit and a move ──────────────────────────────────
  if (runs(5)) {
    console.log("\n5. Undo all over an edit and a move");
    await newChat(js);
    await say(cdp, "Do two things. First, use edit_file to change 8080 to 3000 in src/config.rs. Then use move_file to move notes.txt to docs/notes.txt.");
    const t = await drive(cdp, () => true);
    const edited = !fs.readFileSync(path.join(proj, "src", "config.rs")).equals(crlf(CONFIG_RS));
    const moved = fs.existsSync(path.join(proj, "docs", "notes.txt"));
    if (!edited || !moved) {
      skip("Undo all", `the model ${edited ? "" : "did not edit config.rs"}${!edited && !moved ? " and " : ""}${moved ? "" : "did not move notes.txt"}`);
    } else {
      const count = () => js(`document.querySelector(".turn-undo-count")?.innerText ?? ""`);
      const line = () => js(`document.querySelector(".turn-undo .btn")?.innerText ?? ""`);
      const click = () => js(`document.querySelector(".turn-undo .btn").click(), true`);
      const applied = t.cards.filter((c) => c.classes.includes("ran")).length;
      check("the turn counts its changes", (await count()) === `${applied} changes in this turn`, await count());
      check("and offers Undo all", (await line()) === "Undo all", await line());
      await click();
      await sleep(150);
      check("the first click only asks", (await line()) === `Undo ${applied} changes?` && fs.existsSync(path.join(proj, "docs", "notes.txt")), await line());
      await sleep(500);
      await click();
      await until(js, `document.querySelectorAll(".tool.edit.undone").length >= ${applied}`, "the cards to read Undone", 30_000);
      check("the whole project is back exactly as it started", same(snapshot(proj), START));
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
console.log(`\n${counted.length - failed}/${counted.length} checks passed (${skipped} skipped, ${warns} warned)`);
// A run where the model made none of the calls proved nothing. That is not a
// pass, whatever the zero failures say.
if (counted.length === 0) {
  console.log("Nothing was checked: the model made none of the calls. Not a pass.");
  process.exit(1);
}
process.exit(failed ? 1 : skipped ? 2 : 0);
