// End-to-end: the check command, and finding code, in the real Mímir, driven by
// the real model.
//
//   1. Start Mímir with its WebView2 debug port open:
//        $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=9223"
//        target\debug\mimir.exe            (or: npm run tauri -w apps/mimir -- dev)
//   2. node tools/e2e/mimir-check.mjs
//      node tools/e2e/mimir-check.mjs --only 6     one scenario, by number
//
// What it proves that no unit test can: that a folder with a Cargo.toml offers
// a check, that the card a check waits on runs nothing until it is allowed, that
// what the command printed reaches the model and the card says it failed, that
// the Stop button kills a running command and everything it started — and that
// with all of that, a change whose first edit breaks the build ends in a build
// that passes. Since H2, also that a search card names what it looks for, that
// what it finds reaches the model, and — with ask_every_call on — that a declined
// listing reads nothing. Since H5 a search inside the folder runs without
// asking, which section 7 now asserts.
//
// It types into the real textarea and clicks the real buttons over CDP. The
// command a scenario needs is written into the config file directly, which is
// the same route a hand-set check takes — the app has no command that accepts
// one, by design.
//
// Leaves nothing behind: the project is a temporary one, the config is restored
// byte for byte (or removed), and every conversation the run created is deleted.
//
// The model is real, so the rename can fail for model reasons. A failure prints
// what the thread showed, so it can be read.

import { execFileSync } from "node:child_process";
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

const crlf = (s) => Buffer.from(s.replace(/\n/g, "\r\n"));
// Its own [workspace], so cargo builds it here and not as part of anything above.
const CARGO_TOML = '[package]\nname = "demo"\nversion = "0.1.0"\nedition = "2021"\n\n[workspace]\n';
// Changing PORT's type alone breaks port(), which returns it — the fix probe's
// f03, where the edit applies cleanly and only the check can say it is wrong.
const CONFIG_RS = "pub const PORT: u16 = 8080;\n\npub fn port() -> u16 {\n    PORT\n}\n";
const MAIN_RS = 'mod config;\n\nfn main() {\n    println!("port {}", config::port());\n}\n';

function makeProject() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "mimir-check-e2e-"));
  const proj = path.join(root, "check-project");
  fs.mkdirSync(path.join(proj, "src"), { recursive: true });
  fs.writeFileSync(path.join(proj, "Cargo.toml"), crlf(CARGO_TOML));
  fs.writeFileSync(path.join(proj, "src", "config.rs"), crlf(CONFIG_RS));
  fs.writeFileSync(path.join(proj, "src", "main.rs"), crlf(MAIN_RS));
  // The key Mímir stores a folder under is its canonical form.
  return { root, proj: fs.realpathSync.native(proj) };
}

/** Does the project build? Run from here, not through the app — into the
 *  project's own target folder, the one the app's check builds into, so the
 *  first call also warms it. */
function builds(proj) {
  const { CARGO_TARGET_DIR: _, ...env } = process.env;
  try {
    execFileSync("cargo", ["check", "--quiet"], {
      cwd: proj,
      env: { ...env, CARGO_NET_OFFLINE: "true", CARGO_TERM_COLOR: "never" },
      stdio: "pipe",
    });
    return true;
  } catch {
    return false;
  }
}

/** How many of this program are running right now. */
function running(image) {
  const out = execFileSync("tasklist", ["/FI", `IMAGENAME eq ${image}`, "/FO", "CSV", "/NH"], { encoding: "utf8" });
  return out.split("\n").filter((l) => l.toLowerCase().includes(image.toLowerCase())).length;
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

/**
 * Answer every card until the turn ends: reads and edits are allowed, checks
 * are answered with `onCheck` ("allow", "deny", or "leave" to stop and return).
 */
async function drive(js, onCheck, what, ms = 180_000) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    const s = await js(`(() => {
      const check = document.querySelector(".tool.check.pending");
      if (check) return "check";
      const other = document.querySelector(".tool.read.pending, .tool.find.pending, .tool.edit.pending");
      if (other) { other.querySelector(".btn.primary").click(); return "allowed"; }
      const t = document.querySelector(".composer textarea");
      return t && !t.disabled && !document.querySelector(".send.stop") ? "idle" : null;
    })()`);
    if (s === "idle") return "idle";
    if (s === "check") {
      if (onCheck === "leave") return "check";
      const label = onCheck === "allow" ? "Run once" : "Deny";
      await js(`[...document.querySelectorAll(".tool.check.pending .btn")].find(b => b.textContent === ${JSON.stringify(label)}).click(), true`);
    }
    await sleep(400);
  }
  throw new Error(`timed out waiting for ${what}`);
}

/**
 * Wait for a finding card named `name` ("Search" or "List files"), pending or —
 * since H5, for a call inside the folder — already ran. Allows pending reads
 * and pending finding cards of the other name on the way. Resolves with
 * `{ shown, pending }`, or null if the turn ended without one.
 */
async function untilFind(js, name, ms = 120_000) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    const s = await js(`(() => {
      const want = [...document.querySelectorAll(".tool.find.pending, .tool.find.ran")]
        .find((c) => c.querySelector(".tool-name").textContent === ${JSON.stringify(name)});
      if (want) return { shown: want.querySelector(".tool-path").textContent, pending: want.classList.contains("pending") };
      const f = document.querySelector(".tool.find.pending");
      if (f) { f.querySelector(".btn.primary").click(); return "allowed"; }
      const r = document.querySelector(".tool.read.pending");
      if (r) { r.querySelector(".btn.primary").click(); return "allowed"; }
      const t = document.querySelector(".composer textarea");
      return t && !t.disabled && !document.querySelector(".send.stop") ? "idle" : null;
    })()`);
    if (s?.shown !== undefined) return s;
    if (s === "idle") return null;
    await sleep(400);
  }
  throw new Error(`timed out waiting for a ${name} card`);
}

const threadText = (js) => js(`document.querySelector(".thread").innerText.slice(-1500)`);
const lastAnswer = (js) =>
  js(`[...document.querySelectorAll(".turn.assistant")].map(e => e.innerText).pop() ?? ""`);

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

const { root, proj } = makeProject();
const configPath = path.join(proj, "src", "config.rs");
const configBefore = fs.existsSync(CONFIG) ? fs.readFileSync(CONFIG) : null;

// The fix scenario's edits leave undo records in the user's backups folder.
// Only this run's — records whose file lies in this run's project — are removed
// afterwards, so a Mímir session of the user's own keeps its undo history.
const BACKUPS = path.join(APPDATA, "odin", "backups");
const backupNames = () => (fs.existsSync(BACKUPS) ? fs.readdirSync(BACKUPS) : []);
const backupsBefore = new Set(backupNames());
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
  process.exit(130);
});
const cfgBase = configBefore ? JSON.parse(configBefore) : {};
const cdp = await connect();
const { js } = cdp;
const convosBefore = new Set((await js(`window.__TAURI_INTERNALS__.invoke("conversations")`)).map((c) => c.id));

/** Point Mímir at the project, with this check command set for it. Since H5 a
 *  read, listing or search inside the folder runs without asking unless
 *  `ask_every_call` is on; it is written explicitly, so the user's own setting
 *  cannot change what a scenario sees. Mímir re-reads the config every round. */
function useCheck(argv, askEveryCall = false) {
  const checks = { ...(cfgBase.checks ?? {}), [proj]: argv };
  fs.mkdirSync(path.dirname(CONFIG), { recursive: true });
  fs.writeFileSync(CONFIG, JSON.stringify({ ...cfgBase, workspace: proj, checks, ask_every_call: askEveryCall }, null, 2));
}

try {
  useCheck(["cargo", "check"]);
  // Warm the build once, so the model's first check is not a cold compile.
  builds(proj);
  await js(`(location.href = location.origin + "/"), true`);
  await sleep(2500);
  await until(js, `document.querySelector(".composer textarea") !== null`, "the app to load");

  await newChat(js);
  await toolsOn(js);

  // 1 ── the folder offers a check ─────────────────────────────────────────
  if (runs(1)) {
  console.log("\n1. a Cargo project offers a check");
  const offered = await js(`window.__TAURI_INTERNALS__.invoke("check_options")`);
  check("cargo check and cargo test are offered", JSON.stringify(offered) === '["cargo check","cargo test"]', JSON.stringify(offered));
  const chip = await until(js, `document.querySelector(".check-chip select")?.value ?? ""`, "the check chip", 10_000).catch(() => "");
  check("the chip shows the folder's check", chip === "cargo check", JSON.stringify(chip));
  }

  // 2 ── deny, then allow ──────────────────────────────────────────────────
  if (runs(2)) {
  console.log("\n2. a check runs nothing until it is allowed");
  const marker = path.join(proj, "check-ran.txt");
  // Leaves a file behind when it runs, so "nothing ran" is a fact on disk.
  useCheck(["cmd", "/c", "echo CHECK-OUTPUT-7731& echo ran> check-ran.txt"]);
  await newChat(js);
  await say(cdp, "Run the check with run_check.");
  const card = await drive(js, "leave", "a check card");
  check("a check card waits for approval", card === "check", card === "check" ? "" : await threadText(js));
  if (card === "check") {
    const shown = await js(`document.querySelector(".tool.check.pending .tool-path")?.textContent ?? ""`);
    check("the card names the command", shown.includes("CHECK-OUTPUT-7731"), shown);
    check("nothing has run before approval", !fs.existsSync(marker));
    await js(`[...document.querySelectorAll(".tool.check.pending .btn")].find(b => b.textContent === "Deny").click(), true`);
    await until(js, `!!document.querySelector(".tool.check.denied")`, "the denial");
    await drive(js, "deny", "the answer after denying");
    check("a denied check runs nothing", !fs.existsSync(marker));
  }

  await newChat(js);
  await say(cdp, "Run the check with run_check, then tell me what it printed.");
  await drive(js, "allow", "the answer after allowing");
  check("an allowed check runs", fs.existsSync(marker));
  const out = await js(`document.querySelector(".tool.check.ran pre")?.textContent ?? ""`);
  check("its output is on the card", out.includes("CHECK-OUTPUT-7731"), out.slice(0, 120));
  fs.rmSync(marker, { force: true });
  }

  // 3 ── a failing check reaches the model ─────────────────────────────────
  if (runs(3)) {
  console.log("\n3. a failing check says so, and the model sees why");
  useCheck(["cmd", "/c", "echo error: THE-BUILD-IS-BROKEN-5521& exit 1"]);
  await newChat(js);
  await say(cdp, "Run the check with run_check and tell me exactly what error it reports.");
  await drive(js, "allow", "the answer");
  check("the card says it failed", await js(`!!document.querySelector(".tool.check.ran.fail")`));
  const said = await lastAnswer(js);
  check("the model quotes the error it was given", said.includes("5521"), said.slice(0, 160));
  }

  // 4 ── Stop kills the tree ───────────────────────────────────────────────
  if (runs(4)) {
  console.log("\n4. Stop ends a running check and everything it started");
  const pingsBefore = running("PING.EXE");
  // cmd starts ping as a child. Killing only cmd would leave ping running.
  useCheck(["cmd", "/c", "ping -n 60 127.0.0.1"]);
  await newChat(js);
  await say(cdp, "Run the check with run_check.");
  const card4 = await drive(js, "leave", "a check card");
  if (card4 === "check") {
    await js(`document.querySelector(".tool.check.pending .btn.primary").click(), true`);
    await until(js, `!!document.querySelector(".tool.check.running")`, "the check to start", 10_000);
    await sleep(1500);
    check("ping is running", running("PING.EXE") > pingsBefore);
    const started = Date.now();
    await js(`document.querySelector(".send.stop").click(), true`);
    await until(js, `!!document.querySelector(".tool.check.stopped")`, "the card to say stopped", 15_000);
    check("the card says stopped, quickly", Date.now() - started < 10_000, `${Date.now() - started} ms`);
    await sleep(500);
    check("no ping is left running", running("PING.EXE") <= pingsBefore);
  } else {
    check("a check card appeared", false, await threadText(js));
  }
  }

  // 5 ── a late result lands in its own thread ─────────────────────────────
  if (runs(5)) {
  console.log("\n5. a check stopped just before switching threads leaves the other thread alone");
  // Another conversation, whose second entry is a tool card — the same place
  // the stopped check's card will have in its own thread.
  const other = await js(`window.__TAURI_INTERNALS__.invoke("save_conversation", { conversation: {
    id: "", title: "", created_at: "", updated_at: "",
    messages: [
      { role: "user", content: "E2E-OTHER-THREAD-4410" },
      { role: "tool", content: "the other thread's file", tool_calls: [{ function: { name: "read_file", arguments: { path: "C:/other.txt" } } }] },
    ] } })`);
  const read = async () =>
    JSON.stringify((await js(`window.__TAURI_INTERNALS__.invoke("open_conversation", { id: ${JSON.stringify(other.id)} })`)).messages);
  const before = await read();
  useCheck(["cmd", "/c", "ping -n 60 127.0.0.1"]);
  // Saved behind the app's back, so the sidebar has not heard of it: reload.
  await js(`(location.href = location.origin + "/"), true`);
  await sleep(2500);
  await until(js, `[...document.querySelectorAll(".cv-open")].some(b => b.textContent.includes("E2E-OTHER-THREAD-4410"))`, "the other thread in the sidebar", 15_000);
  await newChat(js);
  await toolsOn(js);
  await say(cdp, "Run the check with run_check.");
  if ((await drive(js, "leave", "a check card")) === "check") {
    await js(`document.querySelector(".tool.check.pending .btn.primary").click(), true`);
    await until(js, `!!document.querySelector(".tool.check.running")`, "the check to start", 10_000);
    // Stop, and open the other conversation the moment the sidebar is usable
    // again — one render later, long before the killed check's result is back.
    await js(`new Promise((done) => {
      document.querySelector(".send.stop").click();
      const open = () => {
        const b = [...document.querySelectorAll(".cv-open")].find(b => b.textContent.includes("E2E-OTHER-THREAD-4410"));
        if (b && !b.disabled && !document.querySelector(".send.stop")) { b.click(); done(true); }
        else requestAnimationFrame(open);
      };
      requestAnimationFrame(open);
    })`);
    await sleep(3000);
    const shown = await js(`[...document.querySelectorAll(".tool")].map(t => t.className + " " + t.innerText.slice(0, 60)).join(" | ")`);
    // Before the fix, the late result patched whatever card sat at its index in
    // the thread now open — this one — turning it "stopped", and the save that
    // followed dropped it from this conversation's file.
    check(
      "the open thread's card is untouched",
      shown.startsWith("tool read ran") && !shown.includes("stopped") && !shown.includes("check"),
      shown,
    );
    check("the other thread's file is untouched", (await read()) === before);
  } else {
    check("a check card appeared", false, await threadText(js));
  }
  }

  // 6 ── the loop the check exists for ─────────────────────────────────────
  if (runs(6)) {
  console.log("\n6. a change whose first edit breaks the build ends in a build that passes");
  useCheck(["cargo", "check"]);
  await newChat(js);
  await say(cdp, "In src/config.rs, change the type of PORT from u16 to u32.");
  await drive(js, "allow", "the change", 300_000);
  const config = fs.readFileSync(configPath, "utf8");
  const checksRun = await js(`document.querySelectorAll(".tool.check.ran").length`);
  const ok = [
    check("the model ran the check", checksRun > 0, `${checksRun} checks`),
    check("PORT is a u32", config.includes("PORT: u32"), JSON.stringify(config)),
    check("the project builds", builds(proj)),
  ].every(Boolean);
  // The model is real; when it goes wrong, what it did is the finding.
  if (!ok) console.log("\n  the thread:\n" + (await threadText(js)));
  }

  // 7 ── a search, allowed ─────────────────────────────────────────────────
  if (runs(7)) {
  console.log("\n7. a search card shows what it looks for, and what it finds reaches the model");
  fs.writeFileSync(configPath, crlf(CONFIG_RS));
  await newChat(js);
  await say(cdp, 'Use the search tool to find the text "PORT" in this project, then tell me which files and lines it is on.');
  const hit = await untilFind(js, "Search");
  if (check("a search card appeared", hit !== null, hit === null ? await threadText(js) : "")) {
    check("the card shows what it looks for", hit.shown.includes("PORT"), hit.shown);
    // H5: a search inside the folder runs at once, marked as needing no approval.
    check("it ran without asking", !hit.pending);
    check(
      "the card is marked automatic, with no button",
      await js(`(() => { const c = document.querySelector(".tool.find.ran.auto"); return !!c && !c.querySelector(".btn") && !!c.querySelector(".tool-auto"); })()`),
    );
    await drive(js, "allow", "the answer");
    const found = await js(`document.querySelector(".tool.find.ran pre")?.textContent ?? ""`);
    check("what it found is on the card", found.includes("src/config.rs:1:"), found.slice(0, 120));
    const said = await lastAnswer(js);
    check("the model answers from it", said.includes("config.rs"), said.slice(0, 160));
  }
  }

  // 8 ── a listing, declined ───────────────────────────────────────────────
  if (runs(8)) {
  console.log("\n8. with ask_every_call on, a listing asks, can be declined, and the model is told");
  // Since H5 a listing inside the folder runs without asking, so this proves the
  // off switch: with `ask_every_call` on, the card waits and Deny reads nothing.
  useCheck(["cargo", "check"], true);
  await newChat(js);
  await say(cdp, "Use list_files to list the Rust files in this project.");
  const hit = await untilFind(js, "List files");
  if (check("a list card waits for approval", hit !== null && hit.pending, hit === null ? await threadText(js) : "")) {
    await js(`[...document.querySelectorAll(".tool.find.pending .btn")].find(b => b.textContent === "Deny").click(), true`);
    await until(js, `!!document.querySelector(".tool.find.denied")`, "the denial");
    const note = await js(`document.querySelector(".tool.find.denied .tool-note")?.innerText ?? ""`);
    check("the card says nothing was read", note.includes("Nothing was read"), note);
    await drive(js, "deny", "the answer after declining");
  }
  useCheck(["cargo", "check"], false);
  }
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
  cleanBackups();
  fs.rmSync(root, { recursive: true, force: true });
}

const failed = results.filter((r) => !r.ok).length;
console.log(`\n${results.length - failed}/${results.length} checks passed`);
process.exit(failed ? 1 : 0);
