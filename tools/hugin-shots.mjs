// Screenshots Hugin for the README: the start screen, a session at an approval, and the reports
// of `hugin -p` and `hugin --ask`.
//
//   node tools/hugin-shots.mjs                 PNGs into docs/images
//   node tools/hugin-shots.mjs --out <dir>     or into <dir>
//
// The screens are Hugin's own: the test `shots_tests::readme_screens_render` draws them with the
// real drawing code from scripted data (no Ollama, no model) and, when HUGIN_SHOTS_DIR is set,
// writes each as a page that is a terminal window. This script sets it, runs the test, and
// photographs the pages with headless Edge, which draws with the same system fonts as the app's
// WebView2 — Cascadia Mono is the terminal's font here too. Each page writes its own size into
// itself, so each shot is as large as its window and no larger.

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";

const ROOT = path.resolve(import.meta.dirname, "..");
const SHOTS = ["hugin-start", "hugin-session", "hugin-run", "hugin-ask"];
// Twice the pixels, so the text stays crisp when the README scales the picture down.
const SCALE = 2;

const EDGE = [
  "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe",
  "C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe",
].find((p) => fs.existsSync(p));

const argv = process.argv.slice(2);
const at = argv.indexOf("--out");
const out = path.resolve(at < 0 ? path.join(ROOT, "docs", "images") : argv[at + 1] ?? "");

if (!EDGE) {
  console.error("Microsoft Edge was not found.");
  process.exit(1);
}

const pages = fs.mkdtempSync(path.join(os.tmpdir(), "hugin-shots-"));
// Its own profile, emptied each run, so nothing of the user's browser is in the shots.
const profile = path.join(pages, "profile");
try {
  execFileSync(
    "cargo",
    ["test", "-p", "hugin", "--", "--exact", "shots_tests::readme_screens_render"],
    { cwd: ROOT, env: { ...process.env, HUGIN_SHOTS_DIR: pages }, stdio: "inherit" },
  );

  fs.mkdirSync(out, { recursive: true });
  const edge = (...args) =>
    execFileSync(EDGE, ["--headless=new", "--disable-gpu", "--hide-scrollbars", `--user-data-dir=${profile}`, ...args], {
      encoding: "utf8",
      maxBuffer: 64 * 1024 * 1024,
      stdio: ["ignore", "pipe", "ignore"],
    });

  for (const name of SHOTS) {
    const url = pathToFileURL(path.join(pages, `${name}.html`)).href;
    const size = /data-size="(\d+),(\d+)"/.exec(edge("--virtual-time-budget=4000", "--dump-dom", url));
    if (!size) throw new Error(`${name}: the page did not report its size`);
    const [w, h] = [Number(size[1]), Number(size[2])];
    const file = path.join(out, `${name}.png`);
    edge(
      `--window-size=${w},${h}`,
      `--force-device-scale-factor=${SCALE}`,
      "--virtual-time-budget=4000",
      `--screenshot=${file}`,
      url,
    );
    console.log(`${file}  ${w * SCALE}x${h * SCALE}`);
  }
} finally {
  fs.rmSync(pages, { recursive: true, force: true });
}
