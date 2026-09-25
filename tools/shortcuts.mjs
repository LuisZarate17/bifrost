// Create or repoint the Desktop and Start Menu shortcuts.
//
//   node tools/shortcuts.mjs           # write them
//   node tools/shortcuts.mjs --check   # report where they currently point, change nothing
//
// Why this is a script and not a one-off. The shortcuts were made by hand and
// pointed at `src-tauri/target/debug/warden.exe`, which stopped existing the
// moment Before-G0 step 1 moved Warden into apps/ and consolidated every crate
// into one root `target/`. A shortcut that silently does nothing is a worse
// failure than one that is obviously broken, so the fix has to be repeatable —
// and PLAN.md decision #5 gives Citadel its own shortcut beside Warden's, so a
// second caller already exists.
//
// The Desktop is resolved through the shell, never by joining %USERPROFILE% and
// "Desktop": on this machine it is OneDrive-redirected to
// C:\Users\<user>\OneDrive\Desktop, and assembling the path writes a shortcut
// into a folder nobody ever looks at.

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
 * Their icons come from `tools/icon/`: Warden is the `sentinel` shield, Citadel
 * the `bastion` tower. Same family, different silhouette — two identical icons
 * on the Desktop is worse than no icon at all, and a recoloured shield turned out
 * to be too close to tell apart at taskbar size.
 */
const APPS = [
  {
    name: "Warden",
    exe: path.join(repoRoot, "target", "debug", "warden.exe"),
    description: "Press a hotkey, get an answer about whatever is on screen.",
  },
  {
    name: "Citadel",
    exe: path.join(repoRoot, "target", "debug", "citadel.exe"),
    description: "A local chat app. Every token stays on this machine.",
  },
];

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
    console.log(`!  ${app.name}: ${app.exe} does not exist — build it first (npm run app)`);
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

if (missingExe && !check) {
  console.log("\nShortcuts were written, but at least one target does not exist yet.");
  process.exitCode = 1;
}
