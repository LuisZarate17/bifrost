// One picked colour → every accent shade Mímir draws, in both themes.
//
// Worked in OKLCH, where lightness is perceptual: the lightness at which white
// text stops being readable on a fill is nearly the same for every hue (L ≈ 0.55
// to 0.6), so solving for it per hue gives each colour the same legibility. What
// breaks yellow, lime and sky is not lightness but gamut — at L 0.57 a yellow can
// only be mustard — so light seeds keep a light fill and take a dark label
// instead, the way Radix's amber and sky scales do.
//
// Every shade is checked against WCAG 2.1 AA before it is used:
//   label on fill ≥ 4.6:1, and hover/pressed only ever move away from the label;
//   accent-as-text ≥ 4.5:1 on the worst surface it sits on (the user bubble) and
//   on the soft accent wash laid over that bubble.
//
// The result is written as --u-* custom properties on <html>. App.css reads them
// with the default sea as fallback, so nothing stored still draws correctly, and
// public/theme-boot.js replays the stored copy before the first paint.

export type Mode = "dark" | "light";
export type Scheme = "system" | "dark" | "light";
export type Source = "preset" | "custom" | "windows";

export type Shades = {
  accent: string;
  hover: string;
  press: string;
  /** The label drawn on the accent fill: white, or a dark tint of the hue. */
  on: string;
  /** The accent used as text, and as the focus ring. */
  text: string;
};

export type Theme = {
  seed: string;
  source: Source;
  scheme: Scheme;
  /** Precomputed --u-* values, so theme-boot.js replays without doing any maths. */
  vars: Record<string, string>;
};

/** Sea: the color of Mímir's own badge, so the window and its icon match. */
export const DEFAULT_SEED = "#1f8a8a";
export const STORE_KEY = "mimir.theme";

/** In hue order from the default. Every one passes both themes (see derive). */
export const PRESETS: { name: string; seed: string }[] = [
  { name: "Sea", seed: "#1f8a8a" },
  { name: "Blue", seed: "#2275e8" },
  { name: "Violet", seed: "#824fea" },
  { name: "Pink", seed: "#df3798" },
  { name: "Red", seed: "#dc2f3c" },
  { name: "Ember", seed: "#e65909" },
  { name: "Amber", seed: "#fcb52c" },
  { name: "Green", seed: "#1ca045" },
  { name: "Sky", seed: "#5dcdfa" },
  { name: "Graphite", seed: "#71717a" },
];

/**
 * The surfaces the accent must read on. Keep these in step with --bubble in
 * App.css: the bubble is the lightest dark surface and the darkest light one, so
 * a text shade that passes on it passes everywhere else in that theme.
 */
const SURFACES: Record<Mode, { worst: string; soft: number }> = {
  dark: { worst: "#262626", soft: 0.16 },
  light: { worst: "#f0f0f0", soft: 0.1 },
};

// ── colour maths: sRGB ↔ OKLab (Björn Ottosson's matrices), WCAG contrast ──

type RGB = [number, number, number];
type LCH = [number, number, number];

const toLin = (c: number) => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
const toGam = (c: number) => (c <= 0.0031308 ? 12.92 * c : 1.055 * c ** (1 / 2.4) - 0.055);

export function parseHex(hex: string): RGB | null {
  let h = hex.trim().replace(/^#/, "");
  if (/^[0-9a-f]{3}$/i.test(h)) h = h.replace(/./g, "$&$&");
  if (!/^[0-9a-f]{6}$/i.test(h)) return null;
  return [0, 2, 4].map((i) => parseInt(h.slice(i, i + 2), 16) / 255) as RGB;
}

const toHex = (rgb: RGB) =>
  "#" + rgb.map((v) => Math.round(Math.min(1, Math.max(0, v)) * 255).toString(16).padStart(2, "0")).join("");

function rgbToOklch(rgb: RGB): LCH {
  const [r, g, b] = rgb.map(toLin);
  const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
  const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
  const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
  const L = 0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s;
  const A = 1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s;
  const B = 0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s;
  let h = (Math.atan2(B, A) * 180) / Math.PI;
  if (h < 0) h += 360;
  return [L, Math.hypot(A, B), h];
}

function oklchToLinear([L, C, H]: LCH): RGB {
  const rad = (H * Math.PI) / 180;
  const a = C * Math.cos(rad);
  const b = C * Math.sin(rad);
  const l = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s = (L - 0.0894841775 * a - 1.291485548 * b) ** 3;
  return [
    4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
    -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
    -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s,
  ];
}

const inGamut = (lin: RGB) => lin.every((v) => v >= -1e-4 && v <= 1 + 1e-4);

/** The most chroma this lightness and hue can have and still be an sRGB colour. */
function maxChroma(L: number, H: number): number {
  let lo = 0;
  let hi = 0.4;
  for (let i = 0; i < 24; i++) {
    const mid = (lo + hi) / 2;
    if (inGamut(oklchToLinear([L, mid, H]))) lo = mid;
    else hi = mid;
  }
  return lo;
}

/**
 * OKLCH → sRGB, pulling chroma in rather than clipping channels. Browsers clip,
 * which shifts the hue of an out-of-gamut colour; reducing chroma keeps the hue
 * and the lightness the contrast was solved for.
 */
function lch(L: number, C: number, H: number): RGB {
  const lin = oklchToLinear([L, Math.min(C, maxChroma(L, H)), H]);
  return lin.map((v) => toGam(Math.min(1, Math.max(0, v)))) as RGB;
}

const luminance = (rgb: RGB) => {
  const [r, g, b] = rgb.map(toLin);
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
};

export function contrast(a: RGB, b: RGB): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

/** What the browser paints for an rgba() wash: a blend in gamma-encoded sRGB. */
const over = (fg: RGB, alpha: number, bg: RGB) => fg.map((v, i) => v * alpha + bg[i] * (1 - alpha)) as RGB;

/** Step lightness until `ok` holds, or the range runs out. */
function walk(L: number, dir: 1 | -1, ok: (L: number) => boolean): number {
  for (let n = 0; n < 200 && !ok(L) && L > 0.05 && L < 0.98; n++) L += dir * 0.005;
  return Math.min(0.98, Math.max(0.05, L));
}

const WHITE: RGB = [1, 1, 1];

// ── derivation ──

export function derive(seed: string, mode: Mode): Shades {
  const dark = mode === "dark";
  const surface = SURFACES[mode];
  const worst = parseHex(surface.worst)!;
  let [sl, sc, h] = rgbToOklch(parseHex(seed) ?? parseHex(DEFAULT_SEED)!);

  // Near-grey seeds become a true neutral, rather than a faint tint of noise.
  const grey = sc < 0.03;
  if (grey) sc = 0;

  const lightFill = grey ? dark : sl >= 0.72;
  let fillL: number;
  let label: RGB;
  if (lightFill) {
    label = lch(0.22, Math.min(sc, 0.05), h);
    fillL = walk(Math.min(Math.max(sl, 0.78), 0.92), 1, (L) => contrast(label, lch(L, sc, h)) >= 4.6);
  } else {
    label = WHITE;
    const start = grey ? 0.28 : Math.min(Math.max(sl, dark ? 0.45 : 0.4), dark ? 0.62 : 0.54);
    fillL = walk(start, -1, (L) => contrast(WHITE, lch(L, sc, h)) >= 4.6);
  }

  const fill = lch(fillL, sc, h);
  const away = lightFill ? 1 : -1;
  const wash = over(fill, surface.soft, worst);
  const textL = walk(dark ? Math.max(sl, 0.7) : Math.min(sl, 0.52), dark ? 1 : -1, (L) => {
    const t = lch(L, sc, h);
    return contrast(t, wash) >= 4.5 && contrast(t, worst) >= 4.5;
  });

  return {
    accent: toHex(fill),
    hover: toHex(lch(fillL + away * 0.04, sc, h)),
    press: toHex(lch(fillL + away * 0.08, sc, h)),
    on: toHex(label),
    text: toHex(lch(textL, sc, h)),
  };
}

/** Every --u-* property for one seed, both themes. */
export function themeVars(seed: string): Record<string, string> {
  const vars: Record<string, string> = {};
  for (const mode of ["dark", "light"] as const) {
    const s = derive(seed, mode);
    const x = mode === "dark" ? "d" : "l";
    vars[`--u-accent-${x}`] = s.accent;
    vars[`--u-hover-${x}`] = s.hover;
    vars[`--u-press-${x}`] = s.press;
    vars[`--u-on-${x}`] = s.on;
    vars[`--u-text-${x}`] = s.text;
  }
  return vars;
}

// ── storage and application ──

/**
 * localStorage, not a file: it is synchronous, and only something synchronous
 * can be read before the first paint. A file would need an IPC round trip and the
 * default sea would flash first. The accent is cosmetic and per-machine, so it
 * does not belong in the config Heimdall and the CLI share.
 */
export function loadTheme(): Theme | null {
  try {
    const t = JSON.parse(localStorage.getItem(STORE_KEY) ?? "null");
    return t && typeof t.seed === "string" && parseHex(t.seed) ? (t as Theme) : null;
  } catch {
    return null;
  }
}

export function makeTheme(seed: string, source: Source, scheme: Scheme): Theme {
  return { seed, source, scheme, vars: themeVars(seed) };
}

const systemMode = (): Mode => (matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark");

export function resolvedMode(scheme: Scheme): Mode {
  return scheme === "system" ? systemMode() : scheme;
}

export function applyTheme(t: Theme) {
  const root = document.documentElement;
  for (const [k, v] of Object.entries(t.vars)) root.style.setProperty(k, v);
  root.dataset.theme = resolvedMode(t.scheme);
}

export function saveTheme(t: Theme) {
  applyTheme(t);
  try {
    localStorage.setItem(STORE_KEY, JSON.stringify(t));
  } catch {
    // Private storage can refuse; the theme still applies for this session.
  }
}

/**
 * Keep `data-theme` following Windows while the scheme is "system". theme-boot.js
 * sets it once before the first paint; this keeps it right afterwards.
 */
export function followSystemScheme(current: () => Scheme) {
  matchMedia("(prefers-color-scheme: light)").addEventListener("change", () => {
    if (current() === "system") document.documentElement.dataset.theme = systemMode();
  });
}
