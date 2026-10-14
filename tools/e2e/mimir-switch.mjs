// End-to-end: choosing a project folder offers the coding model, and clearing
// it offers to switch back — against the real Ollama, loading the real models.
//
//   1. Start Mimir with its WebView2 debug port open:
//        $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=9223"
//        target\debug\mimir.exe            (or: npm run tauri -w apps/mimir -- dev)
//   2. node tools/e2e/mimir-switch.mjs
//      node tools/e2e/mimir-switch.mjs --only 2     one scenario, by number
//
// What it proves that no unit test can: that the offer bar actually appears
// when a folder is chosen, that "Not now" dismisses it without touching the
// config, that "Switch" really loads `qwen3-coder:30b` and the config and
// `/api/ps` agree about it, that the coder answers a real question with tools
// on, and that clearing the folder offers — and performs — the switch back.
//
// Two practice folders stand in for the native picker, which cannot be driven
// from here: they are seeded straight into `recent_workspaces`, the same list
// `choose_project_folder` writes to, and picked by clicking the recent list.
//
// Leaves nothing behind: the config is restored byte for byte (or removed),
// and the vision model is left loaded, the way the machine is normally found.

// Exit code: 1 on any FAIL, or when nothing was counted (a --only that matches no
// scenario); otherwise 2 if anything was skipped; 0 only when everything counted
// passed and nothing was skipped.
//
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
const OLLAMA = "http://localhost:11434";

const VISION = "qwen3-vl:8b-instruct-q4_K_M";
const CODER = "qwen3-coder:30b";

// ── a tiny CDP client, the same shape as mimir-check.mjs's ──────────────────

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

// ── the practice folders ─────────────────────────────────────────────────────

function makeFolder(name, file) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), `mimir-switch-${name}-`));
  fs.writeFileSync(path.join(root, file), `this is ${name}\n`);
  return fs.realpathSync.native(root);
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

/** Pick one of the two seeded folders from the new-chat screen's recent list. */
async function pickRecent(js, index) {
  await js(`(() => { document.querySelectorAll(".recent-folder")[${index}].click(); return true; })()`);
}

async function clickOffer(js, label) {
  await js(`(() => {
    const b = [...document.querySelectorAll(".model-offer button")].find(b => b.textContent.trim().startsWith(${JSON.stringify(label)}));
    if (!b) return false;
    b.click();
    return true;
  })()`);
}

const modelPillText = (js) =>
  js(`(() => { const f = document.querySelector(".mp-family"); const s = document.querySelector(".mp-size"); return f ? f.textContent + " " + s.textContent : ""; })()`);

async function psHas(tag) {
  const r = await fetch(`${OLLAMA}/api/ps`);
  const { models = [] } = await r.json();
  return models.some((m) => (m.name ?? m.model) === tag);
}

const threadText = (js) => js(`document.querySelector(".thread")?.innerText.slice(-1500) ?? ""`);

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

// Connect first: a failed connection (the app is not running) then has no temp
// folders to leave behind.
const cdp = await connect();
const { js } = cdp;

const A = makeFolder("a", "a.txt");
const B = makeFolder("b", "b.txt");
const configBefore = fs.existsSync(CONFIG) ? fs.readFileSync(CONFIG) : null;

/** The two models this script moves in and out of memory, loaded or not, as
 *  Ollama had them when the run began; `null` if Ollama could not be asked. */
async function loadedNow() {
  const { models = [] } = await (await fetch(`${OLLAMA}/api/ps`)).json();
  const tags = models.map((m) => m.name ?? m.model);
  return { [VISION]: tags.includes(VISION), [CODER]: tags.includes(CODER) };
}
const loadedBefore = await loadedNow().catch(() => null);

/** The scenarios switch the app's model, and with it what Ollama has loaded.
 *  Restoring the config alone would leave the two out of step: the config says
 *  one model, Ollama holds the other. So put each model back the way it was
 *  found, loaded or not. Best effort — a failure here is only a note. */
async function restoreLoadedModels() {
  // The context the app loads with is in the config this script restores.
  let numCtx = 32768;
  try {
    const n = configBefore ? JSON.parse(configBefore).num_ctx : undefined;
    if (Number.isInteger(n) && n > 0) numCtx = n;
  } catch {}
  if (!loadedBefore) {
    console.log("  note: Ollama could not be asked at the start, so its loaded model was not put back.");
    return;
  }
  try {
    const now = await loadedNow();
    for (const tag of [CODER, VISION]) {
      if (now[tag] && !loadedBefore[tag]) {
        await fetch(`${OLLAMA}/api/generate`, { method: "POST", body: JSON.stringify({ model: tag, keep_alive: 0 }) });
      } else if (!now[tag] && loadedBefore[tag]) {
        // As the app loads it (bifrost-core's ollama::load): its num_ctx and keep_alive -1.
        // Without them the model would come back at Ollama's default context and
        // expire after five minutes, not the way it was found.
        await fetch(`${OLLAMA}/api/generate`, {
          method: "POST",
          body: JSON.stringify({ model: tag, prompt: "", stream: false, keep_alive: -1, options: { num_ctx: numCtx } }),
        });
      }
    }
  } catch (e) {
    console.log(`  note: Ollama's loaded model may differ from the restored config (${e.message ?? e}).`);
  }
}

// Ctrl+C skips `finally`, so put the user's config back here as well.
process.on("SIGINT", () => {
  if (configBefore) fs.writeFileSync(CONFIG, configBefore);
  else fs.rmSync(CONFIG, { force: true });
  fs.rmSync(A, { recursive: true, force: true });
  fs.rmSync(B, { recursive: true, force: true });
  console.log("  note: interrupted — Ollama's loaded model was not put back.");
  process.exit(130);
});
const cfgBase = configBefore ? JSON.parse(configBefore) : {};

function writeConfig(extra) {
  fs.mkdirSync(path.dirname(CONFIG), { recursive: true });
  fs.writeFileSync(
    CONFIG,
    JSON.stringify(
      { ...cfgBase, model: VISION, workspace: null, model_before_coding: undefined, recent_workspaces: [A, B], ...extra },
      null,
      2,
    ),
  );
}

function readConfig() {
  return JSON.parse(fs.readFileSync(CONFIG, "utf8"));
}

const convosBefore = new Set((await js(`window.__TAURI_INTERNALS__.invoke("conversations")`)).map((c) => c.id));

try {
  writeConfig({});
  await js(`(location.href = location.origin + "/"), true`);
  await sleep(2500);
  await until(js, `document.querySelector(".composer textarea") !== null`, "the app to load");

  await newChat(js);
  await toolsOn(js);

  // 1 ── picking A offers the coding model, and "Not now" leaves it alone ───
  if (runs(1)) {
    console.log("\n1. choosing a folder offers the coding model, and declining changes nothing");
    await pickRecent(js, 0);
    await until(js, `document.querySelector(".model-offer-text")?.textContent.includes("working in code") ?? false`, "the coding offer");
    const text = await js(`document.querySelector(".model-offer-text")?.textContent ?? ""`);
    check("the offer names the coding model", text.includes("qwen3-coder"), text);
    await clickOffer(js, "Not now");
    await until(js, `document.querySelector(".model-offer") === null`, "the bar to close");
    check("the config's model is unchanged", readConfig().model === VISION, readConfig().model);
  }

  // 2 ── clearing, then picking B, and switching ────────────────────────────
  if (runs(2)) {
    console.log("\n2. picking B and switching loads the coding model for real");
    await js(`(() => { document.querySelector(".folder-x")?.click(); return true; })()`);
    // Wait for the clear to land first. Clearing and picking both rewrite the
    // config; fired back to back, the clear could be saved last and leave no
    // folder — a race no person clicks fast enough to hit, but a script does.
    await until(js, `document.querySelector(".folder-x") === null`, "the folder to clear");
    await sleep(300);
    await pickRecent(js, 1);
    await until(js, `document.querySelector(".model-offer-text")?.textContent.includes("working in code") ?? false`, "the coding offer");
    await clickOffer(js, "Switch");
    await until(js, `document.querySelector(".model-offer-text")?.textContent.includes("is ready") ?? false`, "the switch to finish", 240_000);
    const cfg = readConfig();
    check("the config now names the coding model", cfg.model === CODER, cfg.model);
    check("the previous model is remembered", cfg.model_before_coding === VISION, cfg.model_before_coding);
    check("Ollama has the coder loaded", await psHas(CODER));
    check("Ollama does not have the vision model loaded", !(await psHas(VISION)));
    const pill = await modelPillText(js);
    check("the model pill names the coder", pill.toLowerCase().includes("qwen3-coder"), pill);
  }

  // 3 ── the coder actually works in the app ────────────────────────────────
  if (runs(3)) {
    console.log("\n3. the coding model answers a tool question in the app");
    await newChat(js);
    await toolsOn(js);
    await say(cdp, "List the files in this folder.");
    // Since H5 a listing inside the folder runs without asking, so the card is
    // usually already "ran"; click Allow only if it is waiting.
    await until(js, `!!document.querySelector(".tool.find.ran, .tool.find.pending")`, "a list_files card", 60_000);
    await js(`document.querySelector(".tool.find.pending .btn.primary")?.click(), true`);
    await until(
      js,
      `(() => { const t = document.querySelector(".composer textarea"); return t && !t.disabled && !document.querySelector(".send.stop"); })()`,
      "an answer",
      240_000,
    );
    const said = await js(`[...document.querySelectorAll(".turn.assistant")].map(e => e.innerText).pop() ?? ""`);
    check("the model answered after the tool ran", said.length > 0, said.slice(0, 160));
  }

  // 4 ── clearing offers, and performs, the switch back ─────────────────────
  if (runs(4)) {
    console.log("\n4. clearing the folder offers, and performs, the switch back");
    await newChat(js);
    await toolsOn(js);
    await js(`(() => { document.querySelector(".folder-x")?.click(); return true; })()`);
    // By its own text: scenario 2's "is ready" bar can still be up when this
    // starts, and an empty-handed wait for any bar would take that one.
    await until(js, `document.querySelector(".model-offer-text")?.textContent.includes("Folder closed") ?? false`, "the switch-back offer");
    const text = await js(`document.querySelector(".model-offer-text")?.textContent ?? ""`);
    check("the offer names the vision model", text.includes("qwen3-vl"), text);
    // Measured 2026-10-02: the earlier bar's 4 s hide timer outlived it and
    // closed this offer. A new offer is now a new bar, so it stays.
    await new Promise((r) => setTimeout(r, 5_000));
    const still = await js(`document.querySelector(".model-offer-text")?.textContent ?? ""`);
    check("the offer is still up 5 s later", still.includes("Folder closed"), still || "(gone)");
    await clickOffer(js, "Switch back");
    await until(js, `document.querySelector(".model-offer-text")?.textContent.includes("is ready") ?? false`, "the switch back to finish", 240_000);
    const cfg = readConfig();
    check("the config is back on the vision model", cfg.model === VISION, cfg.model);
    check("there is nothing left to switch back to", cfg.model_before_coding == null, cfg.model_before_coding);
    check("Ollama has the vision model loaded", await psHas(VISION));
    check("Ollama does not have the coder loaded", !(await psHas(CODER)));
  }
} catch (e) {
  check("the run completed", false, String(e.message ?? e));
  console.log("\n  last of the thread:\n" + (await threadText(js).catch(() => "(unavailable)")));
} finally {
  if (configBefore) fs.writeFileSync(CONFIG, configBefore);
  else fs.rmSync(CONFIG, { force: true });
  // Scenario 3's conversation is a test artifact, not the user's.
  // Guarded: if the app died, this must not throw before the folders below are
  // removed.
  for (const c of await js(`window.__TAURI_INTERNALS__.invoke("conversations")`).catch(() => [])) {
    if (!convosBefore.has(c.id)) await js(`window.__TAURI_INTERNALS__.invoke("delete_conversation", { id: ${JSON.stringify(c.id)} })`).catch(() => {});
  }
  await js(`(location.href = location.origin + "/"), true`).catch(() => {});
  cdp.close();
  fs.rmSync(A, { recursive: true, force: true });
  fs.rmSync(B, { recursive: true, force: true });
  // Last: loading a model can take a while, and the cleanup above must not wait.
  await restoreLoadedModels();
}

const skipped = results.filter((r) => r.skip).length;
const counted = results.filter((r) => !r.skip);
const failed = counted.filter((r) => !r.ok).length;
console.log(`\n${counted.length - failed}/${counted.length} checks passed (${skipped} skipped)`);
// A run in which nothing was counted proved nothing — `--only` with a number no
// scenario has, for one. That is not a pass, whatever the zero failures say.
if (counted.length === 0) {
  console.log("Nothing was checked: no scenario ran. Not a pass.");
  process.exit(1);
}
process.exit(failed ? 1 : skipped ? 2 : 0);
