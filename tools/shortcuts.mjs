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
// Hugin is different: a terminal program. It is copied to %LOCALAPPDATA%\bifrost\bin (so
// rebuilding it never breaks a shortcut or a running copy), that folder is put on the user
// PATH so `hugin` works in any terminal, and Hugin.lnk starts hugin-launch.exe, a windowless
// program that opens Windows Terminal in the folder Hugin was last used in. Neither copy
// carries a shortcut key: Bifrost's tray (Vidar's process) registers Ctrl+Alt+H, which a
// running process answers at once where Explorer takes ~3 s.
//
// Taskbar pins are not touched: Windows keeps them out of reach of scripts on
// purpose. After a rename, unpin the old app and pin the new one by hand.

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { BADGES, renderBadge } from "./icon/badge.mjs";
import { ico } from "./icon/ico.mjs";
import { png } from "./icon/png.mjs";

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

/** A value inside a PowerShell single-quoted string. PowerShell also ends such a string at
 *  a typographic single quote (‘ ’ ‚ ‛), so each of those is doubled as well — a profile
 *  folder named O’Brien must stay a path, not become code. */
const q = (text) => String(text).replace(/['‘’‚‛]/g, (c) => c + c);

/** Shell folders, asked for rather than assembled. */
function shellFolder(name) {
  return ps(`[Environment]::GetFolderPath('${name}')`);
}

function readShortcut(lnk) {
  if (!fs.existsSync(lnk)) return null;
  const out = ps(
    `$s=(New-Object -ComObject WScript.Shell).CreateShortcut('${q(lnk)}');` +
      `Write-Output $s.TargetPath; Write-Output $s.WorkingDirectory; Write-Output $s.Hotkey`,
  ).split(/\r?\n/);
  return { target: out[0] ?? "", workingDir: out[1] ?? "", hotkey: out[2] ?? "" };
}

function writeShortcut(lnk, { exe, description }) {
  ps(
    `$s=(New-Object -ComObject WScript.Shell).CreateShortcut('${q(lnk)}');` +
      `$s.TargetPath='${q(exe)}';` +
      `$s.WorkingDirectory='${q(repoRoot)}';` +
      `$s.IconLocation='${q(exe)},0';` +
      `$s.Description='${q(description)}';` +
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

// ── Hugin ───────────────────────────────────────────────────────────────────


/** The PATH entries compared as Windows does: ignoring case and a trailing backslash. */
const normalized = (entry) => entry.trim().replace(/[\\/]+$/, "").toLowerCase();
const onPath = (pathValue, dir) => pathValue.split(";").some((e) => e.trim() && normalized(e) === normalized(dir));

/** Copy `from` to `to` when the build is newer or the copy is missing; say what happened. */
function copyIfNewer(from, to, check) {
  const name = path.basename(to);
  if (!fs.existsSync(from)) {
    console.log(`!  ${name}: ${from} does not exist — build it first (cargo build -p hugin)`);
    return;
  }
  const have = fs.existsSync(to);
  const upToDate = have && fs.statSync(to).mtimeMs >= fs.statSync(from).mtimeMs;
  if (check) {
    console.log(`  ${name}: ${!have ? "missing" : upToDate ? "up to date" : "older than the build"}`);
    return;
  }
  if (upToDate) {
    console.log(`  same   ${name}`);
    return;
  }
  try {
    fs.copyFileSync(from, to);
    console.log(`copied   ${name}`);
  } catch (e) {
    const busy = ["EBUSY", "EPERM", "EACCES"].includes(e.code);
    console.log(`!  ${name}: not copied — ${busy ? "it is running; close it and run this again" : e.message}`);
  }
}

function hugin() {
  const localAppData = process.env.LOCALAPPDATA;
  if (!localAppData) {
    console.log("!  Hugin: LOCALAPPDATA is not set");
    return;
  }
  const binDir = path.join(localAppData, "bifrost", "bin");
  const debugDir = path.join(repoRoot, "target", "debug");
  const launcher = path.join(binDir, "hugin-launch.exe");
  const icon = path.join(binDir, "hugin.ico");
  const description = "A coding agent in the terminal, on Bifrost's engine.";
  console.log(`
Hugin  (${binDir})`);

  if (!check) fs.mkdirSync(binDir, { recursive: true });
  for (const exe of ["hugin.exe", "hugin-launch.exe"]) copyIfNewer(path.join(debugDir, exe), path.join(binDir, exe), check);

  // The user PATH only, through tools/user-path.ps1: it keeps every entry as it is (unexpanded,
  // in order, the same registry type) and only ever appends.
  const pathScript = path.join(repoRoot, "tools", "user-path.ps1");
  const said = JSON.parse(
    execFileSync(
      "powershell.exe",
      ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", pathScript, "-Folder", binDir, ...(check ? ["-Check"] : [])],
      { encoding: "utf8" },
    ).trim(),
  );
  if (check) {
    console.log(`  user PATH ${said.there ? "has" : "does not have"} ${binDir}`);
  } else if (said.changed) {
    console.log(`PATH     was: ${said.before ?? "(none)"}`);
    console.log(`         now: ${said.after}`);
    console.log("         (terminals already open keep the old one; new ones find hugin)");
  } else {
    console.log("  same   user PATH already has it");
  }

  // The same ico the apps embed: 16 to 256 px entries.
  const badge = BADGES.hugin;
  const bytes = ico([16, 20, 24, 32, 40, 48, 64, 128, 256].map((size) => {
    const rgba = renderBadge(badge, size);
    return { size, rgba, png: png(size, size, rgba) };
  }));
  const iconSame = fs.existsSync(icon) && fs.readFileSync(icon).equals(bytes);
  if (check) console.log(`  hugin.ico: ${fs.existsSync(icon) ? (iconSame ? "up to date" : "differs from the badge") : "missing"}`);
  else if (!iconSame) {
    fs.writeFileSync(icon, bytes);
    console.log("wrote    hugin.ico");
  }

  for (const [label, dir] of locations) {
    const lnk = path.join(dir, "Hugin.lnk");
    // No key on either copy: Bifrost's tray registers Ctrl+Alt+H (a running process answers
    // at once, Explorer takes ~3 s), and Explorer holding it would block the tray's.
    const hotkey = "";
    const before = readShortcut(lnk);
    if (check) {
      console.log(`${label.padEnd(11)} Hugin.lnk`);
      console.log(`  ${before ? `${before.target}  key: ${before.hotkey || "(none)"}` : "(does not exist)"}`);
      continue;
    }
    ps(
      `$s=(New-Object -ComObject WScript.Shell).CreateShortcut('${q(lnk)}');` +
        `$s.TargetPath='${q(launcher)}';` +
        `$s.WorkingDirectory='${q(binDir)}';` +
        `$s.IconLocation='${q(icon)},0';` +
        `$s.Description='${q(description)}';` +
        `$s.Hotkey='${hotkey}';` +
        `$s.Save()`,
    );
    const after = readShortcut(lnk);
    const changed = !before || before.target !== after.target || before.hotkey !== after.hotkey;
    console.log(`${changed ? "updated" : "  same "}  ${label.padEnd(11)} Hugin.lnk`);
    if (before && changed) console.log(`           was: ${before.target}  key: ${before.hotkey || "(none)"}`);
    console.log(`           now: ${after.target}  key: ${after.hotkey || "(none)"}`);
  }
}

hugin();
