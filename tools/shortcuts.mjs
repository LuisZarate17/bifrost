// Create or repoint the Desktop and Start Menu shortcuts, and remove the ones the
// renames left behind.
//
//   node tools/shortcuts.mjs           # write them, remove retired ones
//   node tools/shortcuts.mjs --check   # report where they currently point, change nothing
//
// Why this is a script and not a one-off. The shortcuts were made by hand and
// pointed at `src-tauri/target/debug/heimdall.exe`, which stopped existing the
// moment Before-G0 step 1 moved the overlay into apps/ and consolidated every
// crate into one root `target/`. A shortcut that silently does nothing is a worse
// failure than one that is obviously broken, so the fix has to be repeatable —
// and PLAN.md decision #5 gives Mimir its own shortcut beside Vidar's, so a
// second caller already exists.
//
// The Desktop is resolved through the shell, never by joining %USERPROFILE% and
// "Desktop": on this machine it is OneDrive-redirected to
// C:\Users\<user>\OneDrive\Desktop, and assembling the path writes a shortcut
// into a folder nobody ever looks at.
//
// Taskbar pins are not touched: Windows keeps them out of reach of scripts on
// purpose. After a rename, unpin the old app and pin the new one by hand.

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

/**
 * Every app that gets a shortcut.
 *
 * **Launching one must never launch the other** (PLAN.md decision #5). They are
 * two processes that happen to share a model, and either alone is fully usable —
 * so they get two shortcuts rather than one launcher with a menu.
 *
 * Each shortcut shows its exe's own icon, which the build embeds from the app's
 * `icon.ico`, drawn by `tools/icon/`: one shield per app. Two identical icons on
 * the Desktop is worse than no icon at all, and color alone is too little to tell
 * two icons apart at taskbar size, so the shields differ in pattern too.
 */
const APPS = [
  {
    name: "Vidar",
    exe: path.join(repoRoot, "target", "debug", "vidar.exe"),
    build: "npm run app:vidar",
    description: "Press a hotkey, get an answer about whatever is on screen.",
  },
  {
    name: "Mimir",
    exe: path.join(repoRoot, "target", "debug", "mimir.exe"),
    build: "npm run app:mimir",
    description: "A local chat app. Every token stays on this machine.",
  },
];

/**
 * Shortcut names from before a rename. Left in place they open an exe that is
 * gone or out of date, so they are removed. Heimdall and Mímir became Vidar and
 * Mimir at Stage I's I0; Warden and Citadel were their names before that.
 *
 * Only when they point at one of this repo's exes, though: a shortcut that
 * merely shares one of these names is somebody else's, and stays.
 */
const RETIRED = ["Heimdall", "Mímir", "Warden", "Citadel"];
const RETIRED_EXES = new Set(["heimdall.exe", "mimir.exe", "warden.exe", "citadel.exe"]);

const ps = (script) =>
  execFileSync(
    "powershell.exe",
    ["-NoProfile", "-NonInteractive", "-Command", script],
    { encoding: "utf8" },
  ).trim();

/** Shell folders, asked for rather than assembled. */
function shellFolder(name) {
  return ps(`[Environment]::GetFolderPath('${name}')`);
}

function readShortcut(lnk) {
  if (!fs.existsSync(lnk)) return null;
  const out = ps(
    `$s=(New-Object -ComObject WScript.Shell).CreateShortcut('${lnk}');` +
      `Write-Output $s.TargetPath; Write-Output $s.WorkingDirectory`,
  ).split(/\r?\n/);
  return { target: out[0] ?? "", workingDir: out[1] ?? "" };
}

function writeShortcut(lnk, { exe, description }) {
  ps(
    `$s=(New-Object -ComObject WScript.Shell).CreateShortcut('${lnk}');` +
      `$s.TargetPath='${exe}';` +
      `$s.WorkingDirectory='${repoRoot}';` +
      `$s.IconLocation='${exe},0';` +
      `$s.Description='${description.replace(/'/g, "''")}';` +
      `$s.Save()`,
  );
}

const check = process.argv.includes("--check");
const locations = [
  ["Desktop", shellFolder("Desktop")],
  ["Start Menu", shellFolder("Programs")],
];

let missingExe = false;
for (const app of APPS) {
  if (!fs.existsSync(app.exe)) {
    console.log(`!  ${app.name}: ${app.exe} does not exist — build it first (${app.build})`);
    missingExe = true;
  }
  for (const [label, dir] of locations) {
    const lnk = path.join(dir, `${app.name}.lnk`);
    const before = readShortcut(lnk);

    if (check) {
      console.log(`${label.padEnd(11)} ${app.name}.lnk`);
      console.log(`  ${before ? before.target : "(does not exist)"}`);
      continue;
    }

    writeShortcut(lnk, app);
    const after = readShortcut(lnk);
    const changed = !before || before.target !== after.target;
    console.log(`${changed ? "updated" : "  same "}  ${label.padEnd(11)} ${app.name}.lnk`);
    if (before && changed) console.log(`           was: ${before.target}`);
    console.log(`           now: ${after.target}`);
  }
}

for (const name of RETIRED) {
  for (const [label, dir] of locations) {
    const lnk = path.join(dir, `${name}.lnk`);
    const was = readShortcut(lnk);
    if (!was) continue;
    const ours = RETIRED_EXES.has(path.basename(was.target).toLowerCase());
    if (check) {
      const what = ours ? "retired — the next write removes it" : `not ours, left alone: ${was.target || "no target"}`;
      console.log(`${label.padEnd(11)} ${name}.lnk  (${what})`);
      continue;
    }
    if (!ours) {
      console.log(`left     ${label.padEnd(11)} ${name}.lnk — it points at ${was.target || "nothing"}, not one of ours`);
      continue;
    }
    fs.rmSync(lnk);
    console.log(`removed  ${label.padEnd(11)} ${name}.lnk`);
  }
}

if (missingExe && !check) {
  console.log("\nShortcuts were written, but at least one target does not exist yet.");
  process.exitCode = 1;
}
