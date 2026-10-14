import fs from "node:fs";
import path from "node:path";
import { png } from "./png.mjs";
import { BADGES, renderBadge } from "./badge.mjs";
import { ico, icns } from "./ico.mjs";

//   node tools/icon/install.mjs <out-dir> <vidar|mimir>
//
// Writes one app's whole icon set: the PNGs tauri.conf.json names, the Store
// logos Tauri scaffolded beside them, a multi-size .ico and an .icns.
//
// The two apps share one mark split in half — the Helm of Awe, see badge.mjs —
// so they read as one project, while their silhouettes inside the disc (arms on
// the diagonals, arms on the compass points) and their colors keep them apart at
// 16px, where shape carries identity and hue alone does not.
const [out, which] = process.argv.slice(2);

if (!out || !BADGES[which]) {
  console.error(`usage: node tools/icon/install.mjs <out-dir> <${Object.keys(BADGES).join("|")}>`);
  process.exit(2);
}

const badge = BADGES[which];
const pngAt = (size) => png(size, size, renderBadge(badge, size));
const image = (size) => ({ size, rgba: renderBadge(badge, size), png: pngAt(size) });

const files = {
  "32x32.png": 32, "128x128.png": 128, "128x128@2x.png": 256, "icon.png": 512,
  "Square30x30Logo.png": 30, "Square44x44Logo.png": 44, "Square71x71Logo.png": 71,
  "Square89x89Logo.png": 89, "Square107x107Logo.png": 107, "Square142x142Logo.png": 142,
  "Square150x150Logo.png": 150, "Square284x284Logo.png": 284, "Square310x310Logo.png": 310,
  "StoreLogo.png": 50,
};
for (const [name, size] of Object.entries(files)) fs.writeFileSync(path.join(out, name), pngAt(size));

fs.writeFileSync(path.join(out, "icon.ico"), ico([16, 20, 24, 32, 40, 48, 64, 128, 256].map(image)));
fs.writeFileSync(path.join(out, "icon.icns"), icns([32, 64, 128, 256, 512, 1024].map(image)));
console.log(`wrote ${Object.keys(files).length + 2} ${which} icons into ${out}`);
