// Screenshots every `?preview` state of Mímir, in dark and light, at three widths.
//
//   npm run dev:mimir                          first, in another terminal
//   node tools/mimir-shots.mjs                 PNGs into %TEMP%\mimir-shots
//   node tools/mimir-shots.mjs --out <dir>     or into <dir>
//   node tools/mimir-shots.mjs edit light      only shots whose name has every word
//   node tools/mimir-shots.mjs --accent dc2f3c in another accent (not saved)
//
// Headless Edge, because it ships with Windows and draws with the same engine and
// the same system fonts as WebView2 — Segoe UI Variable, Sitka and Cascadia render
// here exactly as they do in the app. The Tauri side is faked by preview.ts, so no
// model, no Ollama and no conversation files are touched.

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const URL = "http://localhost:1430/";

const STATES = {
  thread: "?preview",
  stream: "?preview=stream",
  edit: "?preview=edit",
  check: "?preview=check",
  empty: "?preview=empty",
  appearance: "?preview=appearance",
  models: "?preview=models",
};
// Chromium's preferredColorScheme: 0 is dark, 1 is light.
const THEMES = { dark: 0, light: 1 };
// Wide and laptop keep the sidebar; narrow is under the 820px where it starts closed.
const SIZES = { wide: [1440, 900], laptop: [1100, 760], narrow: [720, 860] };

const EDGE = [
  "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe",
  "C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe",
].find((p) => fs.existsSync(p));

const argv = process.argv.slice(2);
const flag = (name) => {
  const at = argv.indexOf(name);
  if (at < 0) return null;
  const value = argv[at + 1];
  argv.splice(at, 2);
  return value;
};
const outArg = flag("--out");
const accent = flag("--accent");
const out = outArg ? path.resolve(outArg) : path.join(os.tmpdir(), "mimir-shots");
const words = argv;

if (!EDGE) {
  console.error("Microsoft Edge was not found.");
  process.exit(1);
}
try {
  await fetch(URL);
} catch {
  console.error(`Nothing is answering at ${URL}. Start it with: npm run dev:mimir`);
  process.exit(1);
}

fs.mkdirSync(out, { recursive: true });
// Its own profile, emptied each run, so a theme saved while clicking around in a
// preview never leaks into the shots.
const profile = path.join(os.tmpdir(), "mimir-shots-profile");
fs.rmSync(profile, { recursive: true, force: true });

let taken = 0;
for (const [state, query] of Object.entries(STATES)) {
  for (const [theme, scheme] of Object.entries(THEMES)) {
    for (const [size, [w, h]] of Object.entries(SIZES)) {
      const name = `${state}-${theme}-${size}` + (accent ? `-${accent}` : "");
      if (!words.every((word) => name.includes(word))) continue;
      const file = path.join(out, `${name}.png`);
      execFileSync(EDGE, [
        "--headless=new",
        "--disable-gpu",
        "--hide-scrollbars",
        `--user-data-dir=${profile}`,
        `--window-size=${w},${h}`,
        "--virtual-time-budget=4000",
        `--blink-settings=preferredColorScheme=${scheme}`,
        `--screenshot=${file}`,
        URL + query + (accent ? `&accent=${accent}` : ""),
      ], { stdio: "ignore" });
      console.log(file);
      taken++;
    }
  }
}
if (taken === 0) console.error(`No shot names contain all of: ${words.join(" ")}`);
