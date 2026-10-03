// End-to-end: reading part of a big file (H8), in the real Mímir, driven by the real model.
//
//   1. Start Mímir with its WebView2 debug port open:
//        $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=9223"
//        target\debug\mimir.exe            (or: npm run tauri -w apps/mimir -- dev)
//   2. node tools/e2e/mimir-bigfile.mjs
//      node tools/e2e/mimir-bigfile.mjs --only 2    one scenario, by number
//
// Why it exists: until H8 a read over 64 KB was refused whole, so Mímir could not
// read five of its own source files, and `edit_file` (which takes files up to
// 1 MiB) had nothing to copy `old_text` from. The benchmark cannot show the gain
// — every file it uses is under 64 KB — so this does, on a ~120 KB CRLF file whose
// one interesting function sits past line 2000:
//
//   1. a question about the file's size: the whole read fails (an auto card,
//      shown failed), the message gives the line count, and the answer uses it
//   2. an edit deep in the file: a ranged auto read card showing its lines, then
//      an edit card; Allow, and the file is changed exactly there and byte for
//      byte everywhere else, CRLF kept
//   3. a read of a gitignored big file with a range asks, and its card shows the
//      range; Allow, and what comes back is exactly that range
//
// **Three results, not two**, as in mimir-policy.mjs. The model is real: when it
// never sends a range, nothing about ranges was exercised, and that prints SKIP —
// not a pass and not a failure.
//
// It types into the real textarea and clicks the real buttons over CDP, and
// writes the folder straight into the config file. Leaves nothing behind: the
// project is a temporary one, the config is restored byte for byte (or removed),
// the backup records its edit made are removed, and every conversation the run
// created is deleted.

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

/** The line the edit scenario changes, and what it becomes. Written so that it
 *  occurs once in the file, and so that a search for the function's name finds
 *  only its signature: the body has to be read to be copied. */
const OLD_LINE = "    let base = 250u64;";
const NEW_LINE = "    let base = 400u64;";
const TARGET = [
  "/// How long to wait before the next attempt, in milliseconds.",
  "pub fn retry_delay_ms(attempt: u32) -> u64 {",
  OLD_LINE,
  "    let capped = attempt.min(6);",
  "    base * (1u64 << capped)",
  "}",
  "",
];

/** One block of plausible Rust, different for every `i`, so nothing repeats. */
function block(i) {
  switch (i % 3) {
    case 0:
      return [
        `/// Whether a request number ${i} is under its limit.`,
        `pub fn check_limit_${i}(count: u32) -> bool {`,
        `    let limit = ${i * 7 + 3}u32;`,
        "    if count > limit {",
        "        return false;",
        "    }",
        `    count % ${(i % 13) + 2} != 0`,
        "}",
        "",
      ];
    case 1:
      return [
        "#[derive(Debug, Clone)]",
        `pub struct Route${i} {`,
        "    pub path: String,",
        "    pub weight: u32,",
        "}",
        "",
        `impl Route${i} {`,
        "    pub fn new(path: &str) -> Self {",
        `        Self { path: path.to_string(), weight: ${(i % 9) + 1} }`,
        "    }",
        "}",
        "",
      ];
    default:
      return [
        `pub const NAME_${i}: &str = "route-${i}";`,
        "",
        `pub fn describe_${i}(code: u16) -> &'static str {`,
        "    match code {",
        `        ${200 + (i % 5)} => "ok",`,
        `        ${400 + (i % 7)} => "bad request",`,
        "        _ => NAME_" + i + ",",
        "    }",
        "}",
        "",
      ];
  }
}

/** Where, 1-based, the target function's signature is in `lines`, and the lines. */
function bigSource() {
  const lines = ["// Generated for mimir-bigfile.mjs: a long file with one function worth finding.", ""];
  let at = 0;
  let size = 0;
  for (let i = 0; size < 120_000; i++) {
    if (!at && lines.length >= 2100) {
      at = lines.length + 2; // the signature is the second line of the target
      lines.push(...TARGET);
    }
    lines.push(...block(i));
    size = lines.reduce((n, l) => n + l.length + 2, 0);
  }
  return { lines, at };
}

/** A file a `.gitignore` hides, so reading it asks. A marker on a known line. */
const LOCAL_MARK_LINE = 3002;
const LOCAL_MARK = "MARK-7741-LOCAL";
function localSource() {
  const lines = [];
  for (let i = 1; i <= 4000; i++) {
    lines.push(i === LOCAL_MARK_LINE ? `entry ${i}: ${LOCAL_MARK} flushed` : `entry ${i}: worker ${i % 17} idle for ${i * 3} ms`);
  }
  return lines;
}

const crlf = (lines) => Buffer.from(lines.join("\r\n") + "\r\n");

function makeProject() {
  const root = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), "mimir-bigfile-e2e-")));
  const proj = path.join(root, "proj");
  fs.mkdirSync(path.join(proj, "src"), { recursive: true });
  const { lines, at } = bigSource();
  const bytes = crlf(lines);
  if (bytes.length < 100_000) throw new Error(`big.rs is only ${bytes.length} bytes`);
  if (at < 2000) throw new Error(`the target is at line ${at}, not past 2000`);
  const text = bytes.toString("utf8");
  if (text.split(OLD_LINE).length !== 2) throw new Error("the line to change must occur exactly once");
  fs.writeFileSync(path.join(proj, "src", "big.rs"), bytes);
  fs.writeFileSync(path.join(proj, ".gitignore"), "local-big.txt\n");
  const local = localSource();
  fs.writeFileSync(path.join(proj, "local-big.txt"), crlf(local));
  // The key Mímir stores a folder under is its canonical form.
  return { root, proj: fs.realpathSync.native(proj), bigBytes: bytes, bigLines: lines.length, target: at, local };
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

const PAGE_STATE = `(() => {
  const p = document.querySelector(".tool.pending");
  if (p) return { pending: {
    kind: ["read", "find", "check", "edit"].find((k) => p.classList.contains(k)),
    name: p.querySelector(".tool-name")?.textContent ?? "",
    shown: p.querySelector(".tool-path")?.textContent ?? "",
    range: p.querySelector(".tool-range")?.textContent ?? "",
    primary: p.querySelector(".btn.primary")?.textContent ?? "",
  } };
  const t = document.querySelector(".composer textarea");
  const busy = !!document.querySelector(".send.stop");
  return {
    idle: !!t && !t.disabled && !busy,
    seen: busy || document.querySelectorAll(".tool").length > 0 || document.querySelectorAll(".turn.assistant").length > 0,
  };
})()`;

const SNAPSHOT = `(() => ({
  thread: document.querySelector(".thread")?.innerText ?? "",
  answer: [...document.querySelectorAll(".turn.assistant")].map((e) => e.innerText).pop() ?? "",
  cards: [...document.querySelectorAll(".tool")].map((c) => ({
    kind: ["read", "find", "check", "edit"].find((k) => c.classList.contains(k)),
    ran: c.classList.contains("ran"),
    failed: c.classList.contains("failed"),
    auto: c.classList.contains("auto"),
    name: c.querySelector(".tool-name")?.textContent ?? "",
    shown: c.querySelector(".tool-path")?.textContent ?? "",
    range: c.querySelector(".tool-range")?.textContent ?? "",
    result: c.querySelector(".tool-result pre")?.textContent ?? "",
  })),
}))()`;

/**
 * Send a prompt and watch the turn to its end. `decide(card)` answers each card
 * that asks: "allow" or "deny" (the default). Returns what asked, every card on
 * the page afterwards, and the thread and answer.
 */
async function turn(cdp, prompt, decide = () => "deny", ms = 300_000) {
  const { js } = cdp;
  await say(cdp, prompt);
  const asked = [];
  const end = Date.now() + ms;
  const began = Date.now();
  while (Date.now() < end) {
    const s = await js(PAGE_STATE);
    if (s.pending) {
      asked.push(s.pending);
      const label = decide(s.pending) === "allow" ? "Allow|Apply edit" : "Deny";
      await js(`(() => { const re = new RegExp("^(" + ${JSON.stringify(label)} + ")"); const b = [...document.querySelectorAll(".tool.pending .btn")].find((x) => re.test(x.textContent.trim())); b?.click(); return true; })()`);
      await until(js, `!document.querySelector(".tool.pending")`, "the card to be answered", 30_000).catch(() => {});
      continue;
    }
    // A turn that has not started yet also looks idle; give it a few seconds.
    if (s.idle && (s.seen || Date.now() - began > 8_000)) break;
    await sleep(400);
  }
  if (Date.now() >= end) throw new Error("timed out waiting for the turn to end");
  await sleep(400);
  return { asked, ...(await js(SNAPSHOT)) };
}

// ── results: PASS, FAIL, and SKIP (the model did not make the call) ────────

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

/** Remove the backup records this run's edit made — only those whose file lies
 *  in this run's project, so a Mímir session of the user's own keeps its history. */
function cleanBackups() {
  if (!proj) return;
  const mine = (p) => typeof p === "string" && p.replace(/^\\\\\?\\/, "").toLowerCase().startsWith(proj.toLowerCase());
  for (const f of backupNames()) {
    if (backupsBefore.has(f) || !f.endsWith(".json")) continue;
    let record;
    try { record = JSON.parse(fs.readFileSync(path.join(BACKUPS, f), "utf8")); } catch { continue; }
    if (!mine(record.path)) continue;
    const id = f.slice(0, -".json".length);
    fs.rmSync(path.join(BACKUPS, f), { force: true });
    fs.rmSync(path.join(BACKUPS, `${id}.bak`), { force: true });
  }
}

function removeProject() {
  if (!root) return;
  fs.rmSync(root, { recursive: true, force: true });
  root = proj = null;
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
let made;
({ root, proj, ...made } = makeProject());
const { bigBytes, bigLines, target, local } = made;
const bigPath = path.join(proj, "src", "big.rs");

/** Point Mímir at the project, with `ask_every_call` off written explicitly, so
 *  the user's own setting cannot change what a scenario sees. */
function configure() {
  fs.mkdirSync(path.dirname(CONFIG), { recursive: true });
  fs.writeFileSync(CONFIG, JSON.stringify({ ...cfgBase, workspace: proj, ask_every_call: false }, null, 2));
}

/** The text a read of lines `a`..`b` of `lines` gives, as Rust words it. */
const ranged = (lines, a, b) => `[lines ${a}–${Math.min(b, lines.length)} of ${lines.length}]\n` + lines.slice(a - 1, b).join("\n") + "\n";
const rangeOf = (text) => {
  const m = /(\d+)\s*[–-]\s*(\d+)/.exec(text);
  return m ? [Number(m[1]), Number(m[2])] : null;
};

try {
  configure();
  // The plain URL, not a reload of a ?preview: a preview fakes its own folder.
  await js(`(location.href = location.origin + "/"), true`);
  await sleep(2500);
  await until(js, `document.querySelector(".composer textarea") !== null`, "the app to load");

  // 1 ── the whole read fails and says how long the file is ───────────────
  if (runs(1)) {
    console.log("\n1. a read of the whole big file fails with the line count, and the answer uses it");
    await newChat(js);
    await toolsOn(js);
    const t = await turn(cdp, "How many lines does src/big.rs have? Use read_file on it first; if it cannot be read whole, say what the error tells you.");
    const reads = t.cards.filter((c) => c.kind === "read");
    const whole = reads.find((c) => c.auto && c.failed && !c.range);
    if (!reads.length) skip("a read of big.rs", "the model answered without calling read_file");
    else if (!whole) skip("the whole read failing", "the model's first read was already a ranged one: " + JSON.stringify(reads.map((c) => c.range)));
    else {
      check("no card asked", t.asked.length === 0, JSON.stringify(t.asked));
      check("the failed read ran without asking", whole.auto);
      check("its card has no range", whole.range === "");
      check("the error gives the line count", whole.result.includes(`${bigLines} lines`), whole.result.slice(0, 200));
      check("and says to read in parts", whole.result.includes("start_line"), whole.result.slice(0, 260));
      check("the answer has the line count", t.answer.replace(/[,\s]/g, "").includes(String(bigLines)), `${bigLines}: ${t.answer.slice(0, 160)}`);
    }
  }

  // 2 ── an edit deep in the file ──────────────────────────────────────────
  if (runs(2)) {
    console.log(`\n2. an edit past line 2000 (retry_delay_ms is at line ${target}): a ranged read, then the edit`);
    await newChat(js);
    await toolsOn(js);
    let allowed = 0;
    const t = await turn(
      cdp,
      "In src/big.rs, change the base delay in retry_delay_ms from 250 to 400. Find the function with search, then read just those lines of the file with read_file using start_line and end_line, then use edit_file. Do not read the whole file.",
      (card) => (card.kind === "edit" && allowed++ === 0 ? "allow" : "deny"),
    );
    const ranges = t.cards.filter((c) => c.kind === "read" && c.range);
    if (!ranges.length) skip("a ranged read of big.rs", "the model never sent a range");
    else {
      const card = ranges.find((c) => c.result.startsWith("[lines "));
      check("a ranged read ran without asking", !!card && card.auto && card.ran, JSON.stringify(ranges.map((c) => c.range)));
      const span = card ? rangeOf(card.range) : null;
      check("its card shows its lines", !!card && /^lines \d+–\d+$/.test(card.range), card?.range);
      check("they include the function", !!span && span[0] <= target && target <= span[1], `${card?.range}, signature at ${target}`);
      check("its result opens with the header", !!card && card.result.startsWith(`[lines ${span?.[0]}–${span?.[1]} of ${bigLines}]`), card?.result.slice(0, 60));
      const edit = t.asked.find((c) => c.kind === "edit");
      if (!edit) skip("the edit", "the model read the lines but made no edit");
      else {
        check("an edit card asked", true);
        check("the edit card names big.rs", edit.shown.toLowerCase().includes("big.rs"), edit.shown);
        const want = Buffer.from(bigBytes.toString("utf8").replace(OLD_LINE, NEW_LINE));
        const got = fs.readFileSync(bigPath);
        check(
          "the file is changed exactly there and byte for byte elsewhere, CRLF kept",
          got.equals(want),
          got.equals(bigBytes) ? "unchanged" : `${got.length} bytes, expected ${want.length}`,
        );
        check("every line ending is still CRLF", !/[^\r]\n/.test(got.toString("latin1")));
      }
    }
  }

  // 3 ── a read of a hidden big file asks, with its range ───────────────────
  if (runs(3)) {
    console.log("\n3. a gitignored big file read with a range asks, shows the range, and reads exactly it");
    await newChat(js);
    await toolsOn(js);
    const a = LOCAL_MARK_LINE - 3;
    const b = LOCAL_MARK_LINE + 2;
    const t = await turn(
      cdp,
      `Use read_file on local-big.txt with start_line ${a} and end_line ${b}, and tell me what those lines say. Do not read the whole file.`,
      () => "allow",
    );
    const asked = t.asked.find((c) => c.kind === "read" && c.shown.toLowerCase().includes("local-big.txt"));
    if (!asked) skip("a read of local-big.txt", "the model answered without calling read_file");
    else if (!asked.range) skip("a ranged read of local-big.txt", "the model sent no range");
    else {
      check("the pending card showed a range", /^lines \d+–\d+$/.test(asked.range), asked.range);
      const span = rangeOf(asked.range);
      const card = t.cards.find((c) => c.kind === "read" && c.ran && c.range === asked.range);
      check("it is not marked automatic", !!card && !card.auto);
      const want = span ? ranged(local, span[0], span[1]) : "";
      check("what was read is exactly that range", !!card && card.result === want, card?.result.slice(0, 120));
      if (span && span[0] <= LOCAL_MARK_LINE && LOCAL_MARK_LINE <= span[1]) {
        check("the answer quotes the marker", t.answer.includes(LOCAL_MARK), t.answer.slice(0, 160));
      }
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
// A run where the model never sent a range proved nothing about ranges.
// That is not a pass, whatever the zero failures say.
if (counted.length === 0) {
  console.log("Nothing was checked: the model made none of the calls. Not a pass.");
  process.exit(1);
}
process.exit(failed ? 1 : 0);
