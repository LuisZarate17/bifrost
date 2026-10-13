// End-to-end: Stage H's own finish line, in the real Mímir, driven by the real model.
//
//   1. Start Mímir with its WebView2 debug port open:
//        $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=9223"
//        target\debug\mimir.exe            (or: npm run tauri -w apps/mimir -- dev)
//   2. node tools/e2e/mimir-acceptance.mjs
//      node tools/e2e/mimir-acceptance.mjs --runs 1    fewer runs
//
// stages/STAGE-H.md says what "done" means: rename a function across files, add a
// function with a unit test, `cargo test` passes, and Undo all puts it all back. This
// does exactly that, three times, against a throwaway crate and the real model, with
// every card approved:
//
//   "Rename greet to welcome everywhere, and add `pub fn shout(name: &str) -> String`
//    that returns the name in capitals, with a unit test. Make sure it builds."
//
// A run passes when **all** of these hold, judged by this script and not by the model:
//   - `cargo test` in the fixture passes, with more tests than it began with
//   - no `greet` function is left: no call, definition or re-export of it in src/
//     (`mod greet;` and `greet::` are the module's name, which the request did not ask
//     to change)
//   - `shout` exists with that signature, and a #[test] calls it
//   - **Undo all then puts the fixture back byte for byte** (the `target/` the check
//     builds aside)
//
// The fixture is a crate with no dependencies, so nothing is fetched and nothing can
// be blocked but the test binary itself: Smart App Control judges each new binary by
// its content hash and refuses some (os error 4551). The script runs `cargo test`
// itself, in a scratch target directory, and on that error touches src/lib.rs and
// tries again, up to three times. The check set in the app is `cargo check`, which
// builds no binary.
//
// **The result is N/3, reported as measured** — the model decides whether a run
// passes, not this script. A run where the model made no edit prints SKIP and is not
// counted; a script where nothing was counted exits 1. Whatever a run did, Undo all
// is asserted to restore the fixture.
//
// It types into the real textarea and clicks the real buttons over CDP, and writes
// the folder and its check straight into the config file. Leaves nothing behind: the
// project and the scratch target are temporary, the config is restored byte for byte
// (or removed), every conversation the run created is deleted, and so is any backup
// record whose file — or whose move destination — lies in this run's project.

import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const PORT = Number(process.env.CDP_PORT ?? 9223);
const APPDATA = process.env.APPDATA;

// The data folder moved to %APPDATA%\bifrost at the I0 rename. Until the new
// Mimir has started once, the real data is still in %APPDATA%\odin, and making a
// bifrost folder here would strand it there. Refuse rather than guess.
if (fs.existsSync(path.join(APPDATA, "odin")) && !fs.existsSync(path.join(APPDATA, "odin", "MOVED.txt"))) {
  console.log("SKIP: %APPDATA%\\odin still holds the data. Start the new Mimir once so it moves to %APPDATA%\\bifrost.");
  process.exit(2);
}
const CONFIG = path.join(APPDATA, "bifrost", "config.json");
const BACKUPS = path.join(APPDATA, "bifrost", "backups");

const RUNS_AT = process.argv.indexOf("--runs");
const RUNS = RUNS_AT > 0 ? Number(process.argv[RUNS_AT + 1]) : 3;

const PROMPT =
  "Rename greet to welcome everywhere, and add `pub fn shout(name: &str) -> String` that returns " +
  "the name in capitals, with a unit test. Make sure it builds.";

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

// ── the fixture ─────────────────────────────────────────────────────────────

const FILES = {
  "Cargo.toml": '[package]\nname = "greetdemo"\nversion = "0.1.0"\nedition = "2021"\n\n[workspace]\n',
  "src/greet.rs":
    'pub fn greet(name: &str) -> String {\n    format!("Hello, {name}!")\n}\n\n' +
    '#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn greets_by_name() {\n' +
    '        assert_eq!(greet("Luis"), "Hello, Luis!");\n    }\n}\n',
  "src/lib.rs":
    "mod greet;\npub use greet::greet;\n\n/// Greets two people at once.\n" +
    'pub fn greet_pair(a: &str, b: &str) -> String {\n    format!("{} {}", greet(a), greet(b))\n}\n',
  "src/main.rs": 'fn main() {\n    println!("{}", greetdemo::greet("world"));\n}\n',
};
const TESTS_AT_START = 1;

/** A fresh fixture: the files above, and a lockfile made now so that the check the
 *  app runs has nothing to add to the folder but `target/`. */
function makeFixture(proj) {
  fs.rmSync(proj, { recursive: true, force: true });
  for (const [rel, text] of Object.entries(FILES)) {
    fs.mkdirSync(path.dirname(path.join(proj, rel)), { recursive: true });
    fs.writeFileSync(path.join(proj, rel), text);
  }
  const made = spawnSync("cargo", ["generate-lockfile", "--offline"], { cwd: proj, encoding: "utf8" });
  if (made.status !== 0) throw new Error("cargo generate-lockfile failed in the fixture: " + (made.stderr || made.error));
}

/** Every file and folder under `dir`, with the bytes of each file — what "back
 *  exactly as it was" is compared against, folders included. `target/` is the
 *  check's build output, not the model's work, and is left out. */
function snapshot(dir) {
  const out = {};
  const walk = (d) => {
    for (const e of fs.readdirSync(d, { withFileTypes: true })) {
      const p = path.join(d, e.name);
      const rel = path.relative(dir, p).replace(/\\/g, "/");
      if (rel === "target") continue;
      if (e.isDirectory()) { out[rel + "/"] = null; walk(p); }
      else out[rel] = fs.readFileSync(p).toString("base64");
    }
  };
  walk(dir);
  return out;
}
const same = (a, b) => JSON.stringify(Object.entries(a).sort()) === JSON.stringify(Object.entries(b).sort());

/** The text of every .rs file under src/, by path. */
function sources(proj) {
  const out = {};
  const walk = (d) => {
    if (!fs.existsSync(d)) return;
    for (const e of fs.readdirSync(d, { withFileTypes: true })) {
      const p = path.join(d, e.name);
      if (e.isDirectory()) walk(p);
      else if (e.name.endsWith(".rs")) out[path.relative(proj, p).replace(/\\/g, "/")] = fs.readFileSync(p, "utf8");
    }
  };
  walk(path.join(proj, "src"));
  return out;
}

/** `cargo test` in the fixture, in a scratch target directory, retrying where Smart
 *  App Control refuses the fresh test binary (os error 4551) after touching a source
 *  file so that the next build links a different one. */
function cargoTest(proj, target) {
  let last = null;
  for (let attempt = 1; attempt <= 3; attempt++) {
    const r = spawnSync("cargo", ["test", "--offline"], {
      cwd: proj,
      encoding: "utf8",
      env: { ...process.env, CARGO_TARGET_DIR: target, CARGO_NET_OFFLINE: "true" },
      timeout: 300_000,
    });
    const text = `${r.stdout ?? ""}\n${r.stderr ?? ""}`;
    const passed = [...text.matchAll(/test result: ok\. (\d+) passed/g)].reduce((n, m) => n + Number(m[1]), 0);
    last = { ok: r.status === 0, passed, text, attempts: attempt };
    if (r.status === 0 || !text.includes("os error 4551")) break;
    for (const f of ["src/lib.rs", "src/main.rs"]) {
      const p = path.join(proj, f);
      if (fs.existsSync(p)) { const now = new Date(); fs.utimesSync(p, now, now); break; }
    }
  }
  return last;
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

/** Let a turn run to its end, approving every card that asks — an edit, a read, a
 *  check. Counts what it approved, by kind. */
async function drive({ js }, ms = 600_000) {
  const approved = { edit: 0, check: 0, other: 0 };
  const end = Date.now() + ms;
  const began = Date.now();
  while (Date.now() < end) {
    const s = await js(`(() => {
      const card = document.querySelector(".tool.pending");
      if (card) {
        const kind = card.classList.contains("edit") ? "edit" : card.classList.contains("check") ? "check" : "other";
        card.querySelector(".btn.primary").click();
        return { kind };
      }
      const t = document.querySelector(".composer textarea");
      const busy = !!document.querySelector(".send.stop");
      return { idle: !!t && !t.disabled && !busy, seen: busy || document.querySelectorAll(".tool").length > 0 };
    })()`);
    if (s.kind) {
      approved[s.kind]++;
      await sleep(500);
      continue;
    }
    // A turn that has not started yet also looks idle; give it a few seconds.
    if (s.idle && (s.seen || Date.now() - began > 8_000)) break;
    await sleep(400);
  }
  if (Date.now() >= end) throw new Error("timed out waiting for the turn to end");
  await sleep(500);
  return approved;
}

/** Undo the whole turn: the Undo-all line (two clicks — the first only asks), or, if
 *  the turn has no such line, each card's own Undo, newest first. */
async function undoEverything(js) {
  if (await js(`!!document.querySelector(".turn-undo .btn")`)) {
    await js(`document.querySelector(".turn-undo .btn").click(), true`);
    await sleep(600);
    await js(`document.querySelector(".turn-undo .btn")?.click(), true`);
  } else {
    for (let i = 0; i < 40; i++) {
      const clicked = await js(`(() => {
        const cards = [...document.querySelectorAll(".tool.edit.ran")].filter((c) => [...c.querySelectorAll(".btn")].some((b) => b.textContent === "Undo"));
        const b = cards.length ? [...cards[cards.length - 1].querySelectorAll(".btn")].find((b) => b.textContent === "Undo") : null;
        if (b) b.click();
        return !!b;
      })()`);
      if (!clicked) break;
      await sleep(900);
    }
  }
  await until(js, `document.querySelectorAll(".tool.edit.ran").length === 0`, "every change to be undone", 60_000).catch(() => {});
  await sleep(400);
}

// ── results: PASS, FAIL, and SKIP (the model made no edit) ──────────────────

const results = [];
function check(name, ok, detail = "") {
  results.push({ name, ok, skip: false });
  console.log(`  ${ok ? "PASS" : "FAIL"}  ${name}${detail ? " — " + detail : ""}`);
  return ok;
}
/** Whether some `#[test]` function's body calls `name(`. For every `#[test]` attribute
 *  the `fn` after it is found and its body taken by matching braces from its first
 *  `{` — a test attribute followed by a body that never calls it is not enough. */
function someTestCalls(text, name) {
  const call = new RegExp(String.raw`\b${name}\s*\(`);
  for (const m of text.matchAll(/#\[test\]/g)) {
    const fn = /\bfn\s+\w+/.exec(text.slice(m.index));
    if (!fn) continue;
    const open = text.indexOf("{", m.index + fn.index);
    if (open < 0) continue;
    let depth = 0;
    let end = text.length;
    for (let i = open; i < text.length; i++) {
      if (text[i] === "{") depth++;
      else if (text[i] === "}" && --depth === 0) {
        end = i;
        break;
      }
    }
    if (call.test(text.slice(open, end))) return true;
  }
  return false;
}

function skip(name, why) {
  results.push({ name, ok: true, skip: true });
  console.log(`  SKIP  ${name} — ${why}`);
}

const configBefore = fs.existsSync(CONFIG) ? fs.readFileSync(CONFIG) : null;
const cfgBase = configBefore ? JSON.parse(configBefore) : {};
// odin_core::config::DEFAULT_MODEL, which is what the app uses when the config names none.
console.log(`acceptance: ${RUNS} run(s), model ${cfgBase.model ?? "qwen3-vl:8b-instruct-q4_K_M (default)"}`);
const backupNames = () => (fs.existsSync(BACKUPS) ? fs.readdirSync(BACKUPS) : []);
const backupsBefore = new Set(backupNames());

// Made only once Mímir has answered, so a run that cannot connect leaves no
// temporary project behind.
let root = null;
let proj = null;
let target = null;

function removeProject() {
  if (root) fs.rmSync(root, { recursive: true, force: true });
  if (target) fs.rmSync(target, { recursive: true, force: true });
  root = proj = target = null;
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
root = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), "mimir-acceptance-e2e-")));
fs.mkdirSync(path.join(root, "proj"));
// The key Mímir stores a folder under is its canonical form.
proj = fs.realpathSync.native(path.join(root, "proj"));
target = path.join(os.tmpdir(), `mimir-acceptance-target-${process.pid}`);

let passed = 0;

try {
  makeFixture(proj);
  fs.mkdirSync(path.dirname(CONFIG), { recursive: true });
  fs.writeFileSync(
    CONFIG,
    JSON.stringify(
      { ...cfgBase, workspace: proj, ask_every_call: false, checks: { ...(cfgBase.checks ?? {}), [proj]: ["cargo", "check"] } },
      null,
      2,
    ),
  );

  for (let run = 1; run <= RUNS; run++) {
    console.log(`\nrun ${run} of ${RUNS}`);
    try {
      makeFixture(proj);
      const START = snapshot(proj);
      // The plain URL, not a reload of a ?preview: a preview fakes its own folder.
      await js(`(location.href = location.origin + "/"), true`);
      await sleep(2500);
      await until(js, `document.querySelector(".composer textarea") !== null`, "the app to load");
      await newChat(js);
      await toolsOn(js);

      await say(cdp, PROMPT);
      const began = Date.now();
      const approved = await drive(cdp);
      const banner = await js(`document.querySelector(".banner-text")?.innerText ?? ""`);
      console.log(`  (${Math.round((Date.now() - began) / 1000)} s; approved ${approved.edit} edit cards, ${approved.check} checks, ${approved.other} reads${banner ? "; banner: " + banner : ""})`);

      if (same(snapshot(proj), START)) {
        skip(`run ${run}`, "the model made no edit");
        continue;
      }

      // 1 ── what the model left behind, judged here ──
      const test = cargoTest(proj, target);
      const src = sources(proj);
      const all = Object.values(src).join("\n");
      const offenders = Object.entries(src)
        .filter(([, text]) => /\bgreet\b/.test(text.replace(/\bmod\s+greet\b/g, "").replace(/\bgreet::/g, "")))
        .map(([file]) => file);
      const shoutDef = /\bfn\s+shout\s*\(\s*name\s*:\s*&str\s*\)\s*->\s*String/.test(all);
      const shoutTest = someTestCalls(all, "shout");
      const ok = [
        check(`run ${run}: the turn ended in the model's answer, not at the cap`, !/stopped after/i.test(banner), banner),
        check(`run ${run}: cargo test passes${test.attempts > 1 ? ` (after ${test.attempts} tries; Smart App Control)` : ""}`, test.ok, test.ok ? "" : test.text.slice(-400)),
        check(`run ${run}: it has more tests than the fixture began with`, test.passed > TESTS_AT_START, `${test.passed} passed`),
        check(`run ${run}: no greet function is left`, offenders.length === 0, offenders.join(", ")),
        check(`run ${run}: shout exists with that signature`, shoutDef),
        check(`run ${run}: a test calls shout`, shoutTest),
      ];

      // 2 ── Undo all, whatever the verdict ──
      await undoEverything(js);
      const back = same(snapshot(proj), START);
      ok.push(check(`run ${run}: Undo all puts the fixture back byte for byte`, back, back ? "" : "differs: " + JSON.stringify(Object.keys(snapshot(proj)).filter((k) => snapshot(proj)[k] !== START[k]))));
      if (ok.every(Boolean)) passed++;
    } catch (e) {
      check(`run ${run}: completed`, false, String(e.message ?? e));
      const t = await js(`document.querySelector(".thread")?.innerText.slice(-1200) ?? ""`).catch(() => "(unavailable)");
      console.log("\n  last of the thread:\n" + t);
    }
  }
} catch (e) {
  check("the script completed", false, String(e.message ?? e));
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
console.log(`\nacceptance: ${passed}/${RUNS} runs passed${skipped ? ` (${skipped} skipped: no edit)` : ""}; ${counted.length - failed}/${counted.length} checks passed`);
// A script where the model made no edit in any run proved nothing. That is not a
// pass, whatever the zero failures say.
if (counted.length === 0) {
  console.log("Nothing was checked: the model made no edit in any run. Not a pass.");
  process.exit(1);
}
// 1 for a fail, 2 when a scenario was skipped: a skip proved nothing, so it must not read as a pass.
process.exit(failed ? 1 : skipped ? 2 : 0);
