/**
 * The bastion tower, drawn as SVG so it can follow the accent.
 *
 * The same 16×16 art as Citadel's taskbar icon (src-tauri/icons), cell for cell.
 * The taskbar icon stays red — Windows needs a fixed file — but in the window the
 * three reds and the two outline inks are re-derived from the accent by accent.ts
 * and arrive as --tw-* properties. The torch glow and its cream core are the
 * light inside the tower, so they keep their colour whatever the walls are.
 *
 * Shown at 2× or 3× so every cell lands on whole device pixels.
 */

// a outline · b wall highlight · c wall · d inner outline · e wall shadow · f glow · g core
const GRID = [
  ".aaa..aaaa..aaa.",
  ".aba..abba..aba.",
  ".abbaabccbaabba.",
  ".abcbbccccbbcba.",
  ".abccccccccccba.",
  ".abccccccccccba.",
  ".abccccccccccba.",
  ".abccccddccccba.",
  ".abeeedffdeeeba.",
  ".abeedffffdeeba.",
  ".abeedfggfdeeba.",
  ".abeedfggfdeeba.",
  ".abeedfggfdeeba.",
  ".abeedfggfdeeba.",
  "abbbbbbbbbbbbbba",
  "aaaaaaaaaaaaaaaa",
];

const CELLS = GRID.flatMap((row, y) =>
  [...row].flatMap((ch, x) => (ch === "." ? [] : [{ x, y, ch }])),
);

export function Tower({ size, className }: { size: number; className?: string }) {
  return (
    <svg
      className={"tower" + (className ? " " + className : "")}
      viewBox="0 0 16 16"
      width={size}
      height={size}
      shapeRendering="crispEdges"
      aria-hidden="true"
    >
      {CELLS.map(({ x, y, ch }) => (
        <rect key={`${x}-${y}`} className={"tw-" + ch} x={x} y={y} width={1} height={1} />
      ))}
    </svg>
  );
}
