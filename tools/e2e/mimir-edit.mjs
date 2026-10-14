// End-to-end: file editing in the real Mimir, driven by the real model.
//
//   1. Start Mimir with its WebView2 debug port open:
//        $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=9223"
//        target\debug\mimir.exe            (or: npm run tauri -w apps/mimir -- dev)
//   2. node tools/e2e/mimir-edit.mjs
//
// What it proves that no unit test can: that a sentence typed into the composer
// becomes an approval card, that the diff on the card is the edit that lands on
// disk — byte for byte, CRLF endings included — and that Undo, Deny, a file
// changed behind the diff's back, and a path outside the folder all behave —
// and, for a turn with two edits in it, that "Undo all" asks first, refuses
// when a file has moved on, and otherwise puts the whole project back byte for
// byte.
//
// It types into the real textarea and clicks the real buttons over CDP. It
// never calls a Rust command to do the thing under test.
//
// Leaves nothing behind: the project folder it edits is a temporary one, the
// config's `workspace` is restored to what it was (or removed), every
// conversation the run created is deleted, and so is any backup record it left.
//
// Exit code: 1 on any FAIL; otherwise 2 if anything was skipped; 0 only when
// everything counted passed and nothing was skipped.
//
// The model is real, so a run can fail for model reasons — the G7 probe
// measured 98/100. A failure prints what the thread showed, so it can be read.

import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const PORT = Number(process.env.CDP_PORT ?? 9223);
const APPDATA = process.env.APPDATA;

// The data folder moved to %APPDATA%\bifrost at the I0 rename. Until the new
// Mimir has started once, the real data is still in %APPDATA%\odin, and making a
// bifrost folder here would strand it there. Refuse rather than guess.
// The same rule as bifrost-core's paths.rs: the old folder is live while it holds
// files and no MOVED.txt. Links count as files, and so does a folder that can't be read.
const holdsFiles = (d) => {
  try {
    return fs.readdirSync(d, { withFileTypes: true }).some((e) => !e.isDirectory() || holdsFiles(path.join(d, e.name)));
  } catch {
    return true;
  }
};
const LEGACY = path.join(APPDATA, "odin");
if (fs.existsSync(LEGACY) && !fs.existsSync(path.join(LEGACY, "MOVED.txt")) && holdsFiles(LEGACY)) {
  console.log("SKIP: %APPDATA%\\odin still holds the data. Start the new Mimir once so it moves to %APPDATA%\\bifrost.");
  process.exit(2);
}
const CONFIG = path.join(APPDATA, "bifrost", "config.json");

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
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "mimir-e2e-"));
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
      const read = document.querySelector(".tool.read.pending, .tool.find.pending");
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

/** Approve every read and edit until the turn ends. */
async function driveAll(js, what) {
  const end = Date.now() + 180_000;
  while (Date.now() < end) {
    const s = await js(`(() => {
      const card = document.querySelector(".tool.read.pending, .tool.find.pending, .tool.edit.pending");
      if (card) { card.querySelector(".btn.primary").click(); return "allowed"; }
      const t = document.querySelector(".composer textarea");
      return t && !t.disabled && !document.querySelector(".send.stop") ? "idle" : null;
    })()`);
    if (s === "idle") return;
    await sleep(400);
  }
  throw new Error(`timed out waiting for ${what}`);
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
      const read = document.querySelector(".tool.read.pending, .tool.find.pending");
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
  results.push({ name, ok, skip: false });
  console.log(`  ${ok ? "PASS" : "FAIL"}  ${name}${detail ? " — " + detail : ""}`);
}
/** The model did not do what the check needs (here: make two edits). Not a
 *  pass and not a failure. */
function skip(name, why) {
  results.push({ name, ok: true, skip: true });
  console.log(`  SKIP  ${name} — ${why}`);
}

const BACKUPS = path.join(APPDATA, "bifrost", "backups");
const backupNames = () => (fs.existsSync(BACKUPS) ? fs.readdirSync(BACKUPS) : []);

// Connect first: a failed connection (the app is not running) then has no temp
// folder to leave behind.
const cdp = await connect();
const { js } = cdp;

const { root, proj } = makeProject();
const configBefore = fs.existsSync(CONFIG) ? fs.readFileSync(CONFIG) : null;
const backupsBefore = new Set(backupNames());

/** Remove the backup records this run made — only those whose file lies in this
 *  run's project, so a Mimir session of the user's own running meanwhile keeps
 *  its undo history. */
function cleanBackups() {
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
  fs.rmSync(root, { recursive: true, force: true });
  process.exit(130);
});
const convosBefore = new Set((await js(`window.__TAURI_INTERNALS__.invoke("conversations")`)).map((c) => c.id));

try {
  // Point Mimir at the throwaway project. Written to the config file directly
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
  const shown = await js(`[...document.querySelectorAll(".tool-rejected summary")].map(e => e.innerText).join(" | ")`);
  const cards = await js(`document.querySelectorAll(".tool.edit").length`);
  // With no attempt and no card the model declined on its own: both checks
  // below would pass without the guard ever having been asked.
  if (!shown && cards === 0) skip("an edit outside the folder", "the model declined on its own and made no attempt");
  else {
    check("the file outside the folder is untouched", outside === "secret\r\n", JSON.stringify(outside));
    check("no approval card was offered for it", cards === 0, `${cards} edit cards`);
    console.log(`        (the thread showed: ${shown})`);
  }

  // 5 ── undo a whole turn ─────────────────────────────────────────────────
  console.log("\n5. undo a whole turn");
  fs.writeFileSync(configPath, crlf(CONFIG_RS));
  fs.writeFileSync(mainPath, crlf(MAIN_RS));
  const convosBefore5 = new Set((await js(`window.__TAURI_INTERNALS__.invoke("conversations")`)).map((c) => c.id));
  await newChat(js);
  await say(cdp, 'Change PORT to 3000 in src/config.rs and "world" to "Luis" in src/main.rs.');
  await driveAll(js, "the two-file turn");
  const edited = { config: fs.readFileSync(configPath), main: fs.readFileSync(mainPath) };
  const applied = await js(`document.querySelectorAll(".tool.edit.ran").length`);
  const both = !edited.config.equals(crlf(CONFIG_RS)) && !edited.main.equals(crlf(MAIN_RS));
  if (applied < 2 || !both) {
    skip("Undo all", `the model made ${applied} edit(s), and changed ${both ? "both files" : "fewer than two files"}`);
  } else {
    const line = () => js(`document.querySelector(".turn-undo .btn")?.innerText ?? ""`);
    const count = () => js(`document.querySelector(".turn-undo-count")?.innerText ?? ""`);
    const click = () => js(`document.querySelector(".turn-undo .btn").click(), true`);
    const unchanged = () => fs.readFileSync(configPath).equals(edited.config) && fs.readFileSync(mainPath).equals(edited.main);
    const resting = "Undo all";

    check("the turn shows how many changes it made", (await count()) === `${applied} changes in this turn`, await count());
    check("and offers to undo them all", (await line()) === resting, await line());

    await click();
    await sleep(150);
    check("the first click only asks", (await line()) === `Undo ${applied} changes?` && unchanged(), await line());
    await sleep(4500);
    check("the question lapses on its own", (await line()) === resting && unchanged(), await line());

    // A file that moved on since: the whole turn is refused, nothing written.
    const theirs = Buffer.concat([edited.config, crlf("// my own line\n")]);
    fs.writeFileSync(configPath, theirs);
    await click();
    await sleep(600);
    await click();
    await until(js, `!!document.querySelector(".banner")`, "the refusal", 30_000);
    const banner = await js(`document.querySelector(".banner-text").innerText`);
    check("a changed file refuses the whole turn", banner.includes("Nothing was undone") && banner.includes("config.rs"), banner);
    check("a refused undo writes nothing", fs.readFileSync(configPath).equals(theirs) && fs.readFileSync(mainPath).equals(edited.main));

    // Put the hand edit back out, and it goes through.
    fs.writeFileSync(configPath, edited.config);
    await js(`document.querySelector(".banner .icon-btn").click(), true`);
    await click();
    await sleep(600);
    await click();
    await until(js, `!document.querySelector(".tool.edit.ran") && !!document.querySelector(".tool.edit.undone")`, "the undo", 30_000);
    check("the whole project is back byte for byte, CRLF kept",
      fs.readFileSync(configPath).equals(crlf(CONFIG_RS)) && fs.readFileSync(mainPath).equals(crlf(MAIN_RS)));
    check("every card reads Undone and the line is gone",
      (await js(`document.querySelectorAll(".tool.edit.undone").length`)) === applied && (await line()) === "");

    // Reopened from the sidebar: the cards are still Undone.
    const mine = (await js(`window.__TAURI_INTERNALS__.invoke("conversations")`)).find((c) => !convosBefore5.has(c.id));
    // The cards flip before the save lands; reopening too soon would read the
    // file from before it. Wait until the saved thread says every edit is undone.
    await until(
      js,
      `window.__TAURI_INTERNALS__.invoke("open_conversation", { id: ${JSON.stringify(mine?.id ?? "")} }).then((c) => {
        const undos = c.messages.flatMap((m) => m.tool_calls ?? []).map((t) => t.function?.arguments ?? {}).filter((a) => a.undo);
        return undos.length > 0 && undos.every((a) => a.undone === true);
      })`,
      "the undo to be saved",
      15_000,
    );
    await newChat(js);
    // By id: an earlier section asked about the same files, so titles can repeat.
    await js(`document.querySelector(${JSON.stringify(`.cv-open[data-id=${JSON.stringify(mine?.id ?? "")}]`)}).click(), true`);
    await until(js, `!!document.querySelector(".tool.edit")`, "the thread to reopen", 30_000);
    check("reopened, the cards are still Undone and there is no line",
      (await js(`document.querySelectorAll(".tool.edit.undone").length`)) === applied && (await line()) === "");
  }
} catch (e) {
  check("the run completed", false, String(e.message ?? e));
  console.log("\n  last of the thread:\n" + (await threadText(js).catch(() => "(unavailable)")));
} finally {
  // Put everything back the way it was.
  if (configBefore) fs.writeFileSync(CONFIG, configBefore);
  else fs.rmSync(CONFIG, { force: true });
  // Guarded: if the app died, this must not throw before the project and the
  // undo records below are removed.
  for (const c of await js(`window.__TAURI_INTERNALS__.invoke("conversations")`).catch(() => [])) {
    if (!convosBefore.has(c.id)) await js(`window.__TAURI_INTERNALS__.invoke("delete_conversation", { id: ${JSON.stringify(c.id)} })`).catch(() => {});
  }
  await js(`(location.href = location.origin + "/"), true`).catch(() => {});
  cdp.close();
  cleanBackups();
  fs.rmSync(root, { recursive: true, force: true });
}

const skipped = results.filter((r) => r.skip).length;
const counted = results.filter((r) => !r.skip);
const failed = counted.filter((r) => !r.ok).length;
console.log(`\n${counted.length - failed}/${counted.length} checks passed (${skipped} skipped)`);
// A skipped section proved nothing about what it covers: not a pass.
if (skipped) console.log("A section was skipped (the model did not make the call it needed). Not a pass.");
process.exit(failed ? 1 : skipped ? 2 : 0);
