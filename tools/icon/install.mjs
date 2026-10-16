import fs from "node:fs";
import path from "node:path";
import { png } from "./png.mjs";
import { BADGES, renderBadge } from "./badge.mjs";
import { ico, icns } from "./ico.mjs";

//   node tools/icon/install.mjs <out-dir> <vidar|mimir|hugin|bifrost>
//   node tools/icon/install.mjs --tray <out-dir>
//   node tools/icon/install.mjs --preview <dir>
//
// The `--tray` form writes the Bifrost mark at the tray sizes (tray-16.png ... tray-32.png)
// that Vidar's tray embeds; it is the suite's tray, so the mark and not Vidar's shield.
//
// The first form writes one icon set: the PNGs tauri.conf.json names, the Store
// logos Tauri scaffolded beside them, a multi-size .ico and an .icns. Vidar's and
// Mimir's go into apps/<app>/src-tauri/icons; the build embeds icon.ico in the
// exe, so the taskbar, the shortcuts and Vidar's tray all show it after a rebuild.
// Hugin has no window to carry an icon yet, and the Bifrost mark is not an app.
//
// The second writes every badge at 16, 32, 64 and 256 px, and sheet.png: all of
// them on a dark and a light ground, with the 16px row enlarged six times so its
// pixels can be judged. Nothing in the repo changes.
const [first, second] = process.argv.slice(2);

const SIZES = [16, 32, 64, 256];

const TRAY_SIZES = [16, 20, 24, 32];

if (first === "--tray" && second) {
  fs.mkdirSync(second, { recursive: true });
  for (const size of TRAY_SIZES) {
    fs.writeFileSync(path.join(second, `tray-${size}.png`), png(size, size, renderBadge(BADGES.bifrost, size)));
  }
  console.log(`wrote ${TRAY_SIZES.length} tray icons into ${second}`);
  process.exit(0);
}

if (first === "--preview" && second) {
  fs.mkdirSync(second, { recursive: true });
  for (const [name, badge] of Object.entries(BADGES)) {
    for (const size of SIZES) fs.writeFileSync(path.join(second, `${name}-${size}.png`), png(size, size, renderBadge(badge, size)));
  }
  fs.writeFileSync(path.join(second, "sheet.png"), sheet());
  console.log(`wrote ${Object.keys(BADGES).length * SIZES.length} previews and sheet.png into ${second}`);
  process.exit(0);
}

if (!first || !BADGES[second]) {
  console.error(`usage: node tools/icon/install.mjs <out-dir> <${Object.keys(BADGES).join("|")}>`);
  console.error("       node tools/icon/install.mjs --tray <out-dir>");
  console.error("       node tools/icon/install.mjs --preview <dir>");
  process.exit(2);
}

const badge = BADGES[second];
const pngAt = (size) => png(size, size, renderBadge(badge, size));
const image = (size) => ({ size, rgba: renderBadge(badge, size), png: pngAt(size) });

const files = {
  "32x32.png": 32, "128x128.png": 128, "128x128@2x.png": 256, "icon.png": 512,
  "Square30x30Logo.png": 30, "Square44x44Logo.png": 44, "Square71x71Logo.png": 71,
  "Square89x89Logo.png": 89, "Square107x107Logo.png": 107, "Square142x142Logo.png": 142,
  "Square150x150Logo.png": 150, "Square284x284Logo.png": 284, "Square310x310Logo.png": 310,
  "StoreLogo.png": 50,
};
for (const [name, size] of Object.entries(files)) fs.writeFileSync(path.join(first, name), pngAt(size));

fs.writeFileSync(path.join(first, "icon.ico"), ico([16, 20, 24, 32, 40, 48, 64, 128, 256].map(image)));
fs.writeFileSync(path.join(first, "icon.icns"), icns([32, 64, 128, 256, 512, 1024].map(image)));
console.log(`wrote ${Object.keys(files).length + 2} ${second} icons into ${first}`);

/**
 * One PNG with every badge, on a dark band and a light one: 256, 64, 32 and 16 px
 * as drawn, then the 16px drawing enlarged six times, pixel for pixel.
 */
function sheet() {
  const names = Object.keys(BADGES);
  const ZOOM = 6, GAP = 16, PAD = 20;
  const cells = [256, 64, 32, 16, 16 * ZOOM];
  const colWidth = cells.reduce((a, b) => a + b + GAP, 0) - GAP;
  const width = PAD * 2 + colWidth;
  const band = PAD * 2 + names.length * (256 + GAP) - GAP;
  const height = band * 2;
  const rgba = Buffer.alloc(width * height * 4);
  const grounds = [[0x1c, 0x1c, 0x1f], [0xec, 0xee, 0xf0]];

  grounds.forEach(([r, g, b], gi) => {
    for (let y = gi * band; y < (gi + 1) * band; y++) {
      for (let x = 0; x < width; x++) {
        const i = (y * width + x) * 4;
        rgba[i] = r; rgba[i + 1] = g; rgba[i + 2] = b; rgba[i + 3] = 255;
      }
    }
  });

  /** Lay one drawing over the ground at (ox, oy), each pixel repeated `zoom` times. */
  const place = (pixels, size, ox, oy, zoom = 1) => {
    for (let y = 0; y < size * zoom; y++) {
      for (let x = 0; x < size * zoom; x++) {
        const s = (Math.floor(y / zoom) * size + Math.floor(x / zoom)) * 4;
        const a = pixels[s + 3] / 255;
        if (a === 0) continue;
        const d = ((oy + y) * width + (ox + x)) * 4;
        for (let k = 0; k < 3; k++) rgba[d + k] = Math.round(pixels[s + k] * a + rgba[d + k] * (1 - a));
      }
    }
  };

  grounds.forEach((_, gi) => {
    names.forEach((name, ni) => {
      const top = gi * band + PAD + ni * (256 + GAP);
      let left = PAD;
      cells.forEach((cell, ci) => {
        const size = ci === cells.length - 1 ? 16 : cell;
        const zoom = ci === cells.length - 1 ? ZOOM : 1;
        place(renderBadge(BADGES[name], size), size, left, top + Math.floor((256 - cell) / 2), zoom);
        left += cell + GAP;
      });
    });
  });
  return png(width, height, rgba);
}
