import fs from "node:fs";
import path from "node:path";
import { png } from "./gen.mjs";
import { shield, render, DESIGNS } from "./art.mjs";
import { ico } from "./ico.mjs";

//   node tools/icon/install.mjs <out-dir> [design]
//
// The design defaults to Warden's. Citadel passes `phantom` — the same shield,
// a different palette. PLAN.md decision #5 wants a variant from the same family
// rather than the same image: two identical icons on the Desktop is worse than
// no icon at all, and two *unrelated* ones stop reading as one project.
const out = process.argv[2];
const design = process.argv[3] ?? "sentinel";

if (!out) {
  console.error("usage: node tools/icon/install.mjs <out-dir> [design]");
  process.exit(2);
}
if (!DESIGNS[design]) {
  console.error(`unknown design: ${design}\navailable: ${Object.keys(DESIGNS).join(", ")}`);
  process.exit(2);
}

const grid = shield(DESIGNS[design]);
const at = (s) => ({ size: s, rgba: render(grid, s), png: png(s, s, render(grid, s)) });

// The set tauri.conf.json names, plus the Store logos already in the folder.
const files = {
  "32x32.png": 32, "128x128.png": 128, "128x128@2x.png": 256, "icon.png": 512,
  "Square30x30Logo.png": 30, "Square44x44Logo.png": 44, "Square71x71Logo.png": 71,
  "Square89x89Logo.png": 89, "Square107x107Logo.png": 107, "Square142x142Logo.png": 142,
  "Square150x150Logo.png": 150, "Square284x284Logo.png": 284, "Square310x310Logo.png": 310,
  "StoreLogo.png": 50,
};
for (const [name, size] of Object.entries(files)) {
  fs.writeFileSync(path.join(out, name), png(size, size, render(grid, size)));
}

fs.writeFileSync(path.join(out, "icon.ico"), ico([16, 24, 32, 48, 64, 128, 256].map(at)));
console.log(`wrote ${Object.keys(files).length + 1} ${design} icons into ${out}`);
