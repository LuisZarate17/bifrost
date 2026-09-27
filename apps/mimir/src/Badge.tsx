/**
 * Mímir's badge: its half of the Helm of Awe, drawn as SVG.
 *
 * The same geometry as tools/icon/badge.mjs, which draws the taskbar and file
 * icons: a disc with four tridents on the compass points and the eye at the
 * centre. The file icons keep Mímir's fixed sea color; in the window the badge
 * takes the accent the user picked, so it changes with Appearance like
 * everything else the accent marks.
 *
 * At 24px and below it draws the simplified arm (a shaft and one crossbar),
 * because the trident's prongs merge into a blot at that size.
 */

const COMPASS = [0, 90, 180, 270];
const TRIDENT = "M15 0H36M26 -9V9M26 -9H33M26 9H33";
const TRIDENT_SMALL = "M15 0H38M27 -10V10";

export function Badge({ size, className }: { size: number; className?: string }) {
  const small = size <= 24;
  return (
    <svg
      className={"badge" + (className ? " " + className : "")}
      viewBox="0 0 100 100"
      width={size}
      height={size}
      aria-hidden="true"
    >
      <circle cx="50" cy="50" r="48" className="badge-disc" />
      {COMPASS.map((a) => (
        <path
          key={a}
          d={small ? TRIDENT_SMALL : TRIDENT}
          transform={`translate(50 50) rotate(${a})`}
          className="badge-mark"
          fill="none"
          strokeWidth={8}
          strokeLinecap="round"
          strokeLinejoin="round"
        />
      ))}
      <circle cx="50" cy="50" r={small ? 7.5 : 5.5} className="badge-eye" />
    </svg>
  );
}
