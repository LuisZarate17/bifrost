// End-to-end: the approval policy (H5), in the real Mímir, driven by the real model.
//
//   1. Start Mímir with its WebView2 debug port open:
//        $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=9223"
//        target\debug\mimir.exe            (or: npm run tauri -w apps/mimir -- dev)
//   2. node tools/e2e/mimir-policy.mjs
//      node tools/e2e/mimir-policy.mjs --only 4    one scenario, by number
//
// The rule it proves: inside the project folder a read, a listing and a search
// run without asking, and say so; a read outside the folder — or of a file the
// folder's own .gitignore hides, or a secret-looking one such as .env, or one
// reached through a junction that points out — still asks; every edit and every
// check still asks; and `ask_every_call: true` in the config brings back asking
// for everything, without a restart (Mímir re-reads the config every round).
//
//   1. a read inside the folder runs without asking
//   2. a read outside asks — by absolute path, and by ../, shown resolved
//   3. a read through a junction that points out asks, and shows the real path
//   4. .env and a gitignored file ask; a search for what is in .env does not find it
//   5. a listing and a search run without asking
//   6. an edit and a check still ask, and declining writes and runs nothing
//   7. with ask_every_call on, an inside read and a listing ask
//   8. reopening scenario 1's conversation keeps the "no approval needed" card
//   9. an edit of .env with the wrong old_text is rejected with no card and no text of the file
//
// **Three results, not two.** The model is real. When a prompt names the tool and
// the exact path and the model still answers without making the call (Qwen3-Coder
// sometimes declines a .env or a junction on its own), nothing about the policy
// was exercised: that prints SKIP, is not counted as a pass, and is not a failure.
// The unit tests in odin-core carry the rule; this script proves the app wires it.
//
// Every card that asks is Denied, and the marker it would have revealed must
// never appear in the thread: a denied read reads nothing.
//
// It types into the real textarea and clicks the real buttons over CDP, and writes
// the folder and the setting straight into the config file — the same route a
// hand-set `ask_every_call` takes. Leaves nothing behind: the project is a
// temporary one (with a junction made by `mklink /J`), the config is restored byte
// for byte (or removed), and every conversation the run created is deleted.

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

const INSIDE = "INSIDE-4821";
const SECRET = "SECRET-ENV-7316";
const LOCAL = "LOCAL-5502";
const OUTSIDE = "OUTSIDE-9043";
const LINKED = "LINKED-6657";
const CONFIG_RS = `// ${INSIDE}\npub const PORT: u16 = 8080;\n`;

function makeProject() {
  const root = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), "mimir-policy-e2e-")));
  const proj = path.join(root, "proj");
  fs.mkdirSync(path.join(proj, "src"), { recursive: true });
  fs.writeFileSync(path.join(proj, "src", "config.rs"), CONFIG_RS);
  fs.writeFileSync(path.join(proj, ".env"), `API_KEY=${SECRET}\n`);
  fs.writeFileSync(path.join(proj, ".gitignore"), "local.txt\n");
  fs.writeFileSync(path.join(proj, "local.txt"), `${LOCAL}\n`);
  fs.writeFileSync(path.join(root, "outside.txt"), `${OUTSIDE}\n`);
  fs.mkdirSync(path.join(root, "elsewhere"));
  fs.writeFileSync(path.join(root, "elsewhere", "linked.txt"), `${LINKED}\n`);
  // A junction needs no privilege, unlike a symlink.
  execFileSync("cmd", ["/c", "mklink", "/J", path.join(proj, "linked"), path.join(root, "elsewhere")], { stdio: "pipe" });
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

const PAGE_STATE = `(() => {
  const p = document.querySelector(".tool.pending");
  if (p) return { pending: {
    kind: ["read", "find", "check", "edit"].find((k) => p.classList.contains(k)),
    name: p.querySelector(".tool-name")?.textContent ?? "",
    shown: p.querySelector(".tool-path")?.textContent ?? "",
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
  autos: [...document.querySelectorAll(".tool.auto")].map((c) => ({
    kind: ["read", "find", "check", "edit"].find((k) => c.classList.contains(k)),
    ran: c.classList.contains("ran"),
    stopped: c.classList.contains("stopped"),
    name: c.querySelector(".tool-name")?.textContent ?? "",
    shown: c.querySelector(".tool-path")?.textContent ?? "",
    buttons: c.querySelectorAll(".btn").length,
    tag: c.querySelector(".tool-auto")?.textContent ?? "",
    tagTitle: c.querySelector(".tool-auto")?.getAttribute("title") ?? "",
    result: c.querySelector(".tool-result pre")?.textContent ?? "",
  })),
  // A call refused before any card: its error is in the collapsed <details>,
  // which the thread's innerText leaves out — so read it directly.
  rejected: [...document.querySelectorAll(".tool-rejected")].map((d) => ({
    summary: d.querySelector("summary")?.textContent ?? "",
    detail: d.querySelector("pre")?.textContent ?? "",
  })),
}))()`;

/**
 * Send a prompt and watch the turn to its end. Every card that asks is recorded
 * and Denied; every card that ran without asking is read off the page after.
 * Returns what asked, what ran automatically, and the thread and answer.
 */
async function turn(cdp, prompt, ms = 240_000) {
  const { js } = cdp;
  await say(cdp, prompt);
  const asked = [];
  const end = Date.now() + ms;
  const began = Date.now();
  while (Date.now() < end) {
    const s = await js(PAGE_STATE);
    if (s.pending) {
      asked.push(s.pending);
      await js(`[...document.querySelectorAll(".tool.pending .btn")].find((b) => b.textContent === "Deny")?.click(), true`);
      await until(js, `!document.querySelector(".tool.pending")`, "the card to be denied", 15_000).catch(() => {});
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

/** Did the model make this call at all, whether it asked or ran? `needle`, if
 *  given, must be in the path the card shows (lower-cased). */
function made(t, kind, needle) {
  const hit = (c) => c.kind === kind && (!needle || c.shown.toLowerCase().includes(needle.toLowerCase()));
  return t.asked.find(hit) ?? t.autos.find(hit) ?? null;
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

/** Remove the junction first: deleting through it would delete what it points at. */
function removeProject() {
  if (!root) return;
  try { fs.rmdirSync(path.join(proj, "linked")); } catch { /* already gone */ }
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

/** Point Mímir at the project, with `ask_every_call` as given (written
 *  explicitly, so the user's own setting cannot change what a scenario sees) and
 *  optionally a check command. Mímir re-reads it every round. */
function configure({ askEveryCall = false, check: argv = null } = {}) {
  const checks = argv ? { ...(cfgBase.checks ?? {}), [proj]: argv } : cfgBase.checks;
  fs.mkdirSync(path.dirname(CONFIG), { recursive: true });
  fs.writeFileSync(
    CONFIG,
    JSON.stringify({ ...cfgBase, workspace: proj, ask_every_call: askEveryCall, ...(checks ? { checks } : {}) }, null, 2),
  );
}

const notIn = (t, marker) => !t.thread.includes(marker);
let convo1 = null;

try {
  configure();
  // The plain URL, not a reload of a ?preview: a preview fakes its own folder.
  await js(`(location.href = location.origin + "/"), true`);
  await sleep(2500);
  await until(js, `document.querySelector(".composer textarea") !== null`, "the app to load");

  // 1 ── an inside read does not ask ───────────────────────────────────────
  if (runs(1)) {
    console.log("\n1. a read inside the folder runs without asking");
    await newChat(js);
    await toolsOn(js);
    const t = await turn(cdp, "Use read_file to read src/config.rs and tell me the marker in it. Do not use any other tool first.");
    const read = made(t, "read", "config.rs");
    if (!read) skip("an inside read", "the model answered without calling read_file");
    else {
      check("no card asked", t.asked.length === 0, JSON.stringify(t.asked));
      const card = t.autos.find((c) => c.kind === "read" && c.ran);
      check("a ran read card is marked automatic", !!card);
      check("it has no button", !!card && card.buttons === 0);
      check('it says "no approval needed"', !!card && card.tag === "no approval needed", card?.tag);
      check("the answer has the marker", t.answer.includes(INSIDE), t.answer.slice(0, 160));
      await sleep(1500);
      convo1 = (await js(`window.__TAURI_INTERNALS__.invoke("conversations")`)).find((c) => !convosBefore.has(c.id)) ?? null;
    }
  }

  // 2 ── an outside read asks ──────────────────────────────────────────────
  if (runs(2)) {
    console.log("\n2. a read outside the folder asks");
    const outsidePath = path.join(root, "outside.txt");
    await newChat(js);
    await toolsOn(js);
    const a = await turn(cdp, `Use read_file to read ${outsidePath} and tell me the marker in it. Do not use any other tool first.`);
    const card = made(a, "read", "outside.txt");
    if (!card) skip("an outside read by absolute path", "the model answered without calling read_file");
    else {
      check("the outside read asked", a.asked.some((c) => c.kind === "read" && c.shown.toLowerCase().includes("outside.txt")), JSON.stringify(a.asked));
      check("nothing outside ran automatically", !a.autos.some((c) => c.kind === "read" && c.shown.toLowerCase().includes("outside.txt")));
      check("the marker never appears", notIn(a, OUTSIDE));
    }

    await newChat(js);
    const b = await turn(cdp, "Use read_file to read ../outside.txt and tell me the marker in it. Do not use any other tool first.");
    const card2 = made(b, "read", "outside.txt");
    if (!card2) skip("an outside read by ../outside.txt", "the model answered without calling read_file");
    else {
      const shown = b.asked.find((c) => c.kind === "read")?.shown ?? "";
      check("the ../ read asked", !!shown, JSON.stringify(b.asked));
      check("the card shows the path resolved", shown.toLowerCase().includes(root.toLowerCase()) && !shown.includes(".."), shown);
      check("the marker never appears", notIn(b, OUTSIDE));
    }
  }

  // 3 ── a junction out asks ───────────────────────────────────────────────
  if (runs(3)) {
    console.log("\n3. a read through a junction that points out asks, and shows the real path");
    await newChat(js);
    await toolsOn(js);
    const t = await turn(cdp, "Use read_file to read linked/linked.txt and tell me the marker in it. Do not use any other tool first.");
    const card = made(t, "read", "linked.txt");
    if (!card) skip("a read through the junction", "the model answered without calling read_file");
    else {
      const asked = t.asked.find((c) => c.kind === "read");
      check("the junction read asked", !!asked, JSON.stringify(t.asked));
      check("the card shows the real path", !!asked && asked.shown.toLowerCase().includes("elsewhere"), asked?.shown);
      check("the marker never appears", notIn(t, LINKED));
    }
  }

  // 4 ── .env and a gitignored file ask; search does not leak .env ─────────
  if (runs(4)) {
    console.log("\n4. .env and a gitignored file ask; a search does not read .env");
    await newChat(js);
    await toolsOn(js);
    const env = await turn(cdp, "Use read_file to read .env and tell me what is in it. Do not use any other tool first.");
    if (!made(env, "read", ".env")) skip("a read of .env", "the model answered without calling read_file");
    else {
      check(".env asked", env.asked.some((c) => c.kind === "read" && c.shown.toLowerCase().includes(".env")), JSON.stringify(env.asked));
      check("nothing read .env automatically", !env.autos.some((c) => c.kind === "read" && c.shown.toLowerCase().includes(".env")));
      check("the marker never appears", notIn(env, SECRET));
    }

    await newChat(js);
    const local = await turn(cdp, "Use read_file to read local.txt and tell me the marker in it. Do not use any other tool first.");
    if (!made(local, "read", "local.txt")) skip("a read of the gitignored local.txt", "the model answered without calling read_file");
    else {
      check("local.txt asked", local.asked.some((c) => c.kind === "read" && c.shown.toLowerCase().includes("local.txt")), JSON.stringify(local.asked));
      check("the marker never appears", notIn(local, LOCAL));
    }

    // The prompt gives only the start of the marker, so the whole one can only
    // come from the file.
    await newChat(js);
    const found = await turn(cdp, 'Use the search tool to find the text "SECRET-ENV" in this project and tell me which files contain it. Do not use any other tool first.');
    const search = found.autos.find((c) => c.kind === "find" && c.name === "Search");
    if (!search && found.asked.some((c) => c.kind === "find")) check("the search ran without asking", false, JSON.stringify(found.asked));
    else if (!search) skip("a search for the .env marker", "the model answered without calling search");
    else {
      check("the search ran without asking", !found.asked.some((c) => c.kind === "find"));
      check("it did not find .env's contents", !search.result.includes(".env:") && !search.result.includes(SECRET), search.result.slice(0, 160));
      check("it said it skipped secret-named files", search.result.includes("secrets"), search.result.slice(-160));
      check("the marker never appears", notIn(found, SECRET));
    }
  }

  // 5 ── list and search are automatic ─────────────────────────────────────
  if (runs(5)) {
    console.log("\n5. a listing and a search run without asking");
    await newChat(js);
    await toolsOn(js);
    const l = await turn(cdp, "Use list_files to list the Rust files in this project. Do not use any other tool first.");
    const list = l.autos.find((c) => c.kind === "find" && c.name === "List files");
    if (!list && l.asked.some((c) => c.kind === "find")) check("the listing did not ask", false, JSON.stringify(l.asked));
    else if (!list) skip("a listing", "the model answered without calling list_files");
    else {
      check("the listing did not ask", !l.asked.some((c) => c.kind === "find"));
      check("its card is ran and automatic, with no button", list.ran && list.buttons === 0);
      check('it says "no approval needed"', list.tag === "no approval needed", list.tag);
      check("its tooltip names the switch", list.tagTitle.includes("ask_every_call"), list.tagTitle);
      check("it lists src/config.rs", list.result.includes("src/config.rs"), list.result.slice(0, 120));
    }

    await newChat(js);
    const s = await turn(cdp, 'Use the search tool to find the text "INSIDE" in this project and tell me which file it is in. Do not use any other tool first.');
    const search = s.autos.find((c) => c.kind === "find" && c.name === "Search");
    if (!search && s.asked.some((c) => c.kind === "find")) check("the search did not ask", false, JSON.stringify(s.asked));
    else if (!search) skip("a search", "the model answered without calling search");
    else {
      check("the search did not ask", !s.asked.some((c) => c.kind === "find"));
      check("its card is ran and automatic, with no button", search.ran && search.buttons === 0);
      check("it found src/config.rs", search.result.includes("src/config.rs"), search.result.slice(0, 120));
    }
  }

  // 6 ── edits and checks still ask ────────────────────────────────────────
  if (runs(6)) {
    console.log("\n6. an edit and a check still ask, and declining changes and runs nothing");
    const ran = path.join(proj, "check-ran.txt");
    configure({ check: ["cmd", "/c", "echo ran> check-ran.txt"] });
    await newChat(js);
    await toolsOn(js);
    const before = fs.readFileSync(path.join(proj, "src", "config.rs"));
    const e = await turn(cdp, `Use edit_file to replace ${INSIDE} with CHANGED-0001 in src/config.rs. Do not use any other tool first.`);
    if (!made(e, "edit")) skip("an edit", "the model answered without calling edit_file");
    else {
      check("the edit asked", e.asked.some((c) => c.kind === "edit"), JSON.stringify(e.asked));
      check("it offers Apply edit", e.asked.some((c) => c.kind === "edit" && c.primary === "Apply edit"), JSON.stringify(e.asked));
      check("the file is unchanged after Deny", fs.readFileSync(path.join(proj, "src", "config.rs")).equals(before));
    }

    await newChat(js);
    const c = await turn(cdp, "Use run_check to run the check. Do not use any other tool first.");
    if (!made(c, "check")) skip("a check", "the model answered without calling run_check");
    else {
      check("the check asked", c.asked.some((x) => x.kind === "check"), JSON.stringify(c.asked));
      check("nothing ran after Deny", !fs.existsSync(ran));
    }
    fs.rmSync(ran, { force: true });
    configure();
  }

  // 7 ── the off switch ────────────────────────────────────────────────────
  if (runs(7)) {
    console.log("\n7. with ask_every_call on, an inside read and a listing ask");
    // Written mid-run, no restart: Mímir re-reads the config every round.
    configure({ askEveryCall: true });
    await newChat(js);
    await toolsOn(js);
    const r = await turn(cdp, "Use read_file to read src/config.rs and tell me the marker in it. Do not use any other tool first.");
    if (!made(r, "read", "config.rs")) skip("an inside read with the switch on", "the model answered without calling read_file");
    else {
      check("the read asked", r.asked.some((c) => c.kind === "read"), JSON.stringify(r.asked));
      check("nothing ran automatically", !r.autos.some((c) => c.kind === "read"));
      check("the marker never appears", notIn(r, INSIDE));
    }

    await newChat(js);
    const l = await turn(cdp, "Use list_files to list the Rust files in this project. Do not use any other tool first.");
    if (!made(l, "find")) skip("a listing with the switch on", "the model answered without calling list_files");
    else {
      check("the listing asked", l.asked.some((c) => c.kind === "find" && c.name === "List files"), JSON.stringify(l.asked));
      check("nothing ran automatically", !l.autos.some((c) => c.kind === "find"));
    }
    configure();
  }

  // 8 ── a reopened conversation keeps its tags ────────────────────────────
  if (runs(8)) {
    console.log("\n8. reopening a conversation keeps the automatic cards");
    if (!convo1) skip("reopening scenario 1's conversation", "scenario 1 did not run or did not make its call");
    else {
      await newChat(js);
      // By id, not title: scenario 7 asks the same question, so its conversation
      // has the same title and sits above this one in the sidebar.
      const sel = `.cv-open[data-id=${JSON.stringify(convo1.id)}]`;
      await until(js, `document.querySelector(${JSON.stringify(sel)}) !== null`, "the conversation in the sidebar", 15_000);
      await js(`document.querySelector(${JSON.stringify(sel)}).click(), true`);
      const back = await until(js, `document.querySelector(".tool.read.ran.auto") !== null`, "the automatic card to come back", 15_000).catch(() => false);
      check("the automatic card is still there", !!back);
      check(
        "it still says no approval needed, with no button",
        await js(`(() => { const c = document.querySelector(".tool.read.ran.auto"); return !!c && !c.querySelector(".btn") && !!c.querySelector(".tool-auto"); })()`),
      );
      const saved = JSON.stringify((await js(`window.__TAURI_INTERNALS__.invoke("open_conversation", { id: ${JSON.stringify(convo1.id)} })`)).messages);
      check("the saved thread carries the flag", saved.includes('"auto":true'));
    }
  }

  // 9 ── a missed edit of a protected file shows none of it (R-01) ─────────
  if (runs(9)) {
    console.log("\n9. an edit of .env with the wrong old_text is rejected, and shows nothing of the file");
    // An edit is planned before any card, and a miss used to answer with the
    // closest lines of the file: a read of .env that never asked.
    configure();
    await newChat(js);
    await toolsOn(js);
    // A near miss on purpose: one digit off the real line, so the old code's
    // "closest text in the file" would have quoted it — 7316 — back.
    const e = await turn(
      cdp,
      "Use edit_file on .env to replace API_KEY=SECRET-ENV-7317 with API_KEY=changed. Do not use any other tool first.",
    );
    const refused = e.rejected.filter((r) => /\.env/i.test(r.summary + r.detail));
    const carded = e.asked.filter((c) => c.kind === "edit" && c.shown.toLowerCase().includes(".env"));
    if (refused.length === 0 && carded.length === 0) skip("a missed edit of .env", "the model answered without calling edit_file on .env");
    else {
      check("no edit card for .env", carded.length === 0, JSON.stringify(e.asked));
      check(
        "the refusal says it is a protected file",
        refused.some((r) => r.detail.includes("protected file")),
        JSON.stringify(refused),
      );
      const everything = e.thread + JSON.stringify(e.rejected) + JSON.stringify(e.autos);
      check("the real line (…7316) appears nowhere — not in the thread, not in the refusal", !everything.includes(SECRET), "");
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
// A run where the model made no call at all proved nothing about the policy.
// That is not a pass, whatever the zero failures say.
if (counted.length === 0) {
  console.log("Nothing was checked: the model made none of the calls. Not a pass.");
  process.exit(1);
}
process.exit(failed ? 1 : 0);
