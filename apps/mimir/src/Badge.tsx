/**
 * Mimir's badge: a round shield, drawn as SVG.
 *
 * The same geometry as tools/icon/badge.mjs, which draws the taskbar and file
 * icons: a face, an ink ring, a thin rim and a boss at the centre. The file icons
 * keep Mimir's fixed Deep Well color; in the window the badge takes the accent the
 * user picked, so it changes with Appearance like everything else the accent marks.
 *
 * In the light theme it also shows a black outline at its edge. The outline is
 * always in the SVG and App.css decides when it is visible, so a change of theme
 * needs no re-render.
 *
 * At 24px and below no stroke is drawn thinner than 1.4px, or the rim would
 * disappear.
 */

/** At or below this, a badge is drawn with a minimum stroke. */
const SMALL = 24;
const MIN_STROKE_PX = 1.4;

export function Badge({ size, className }: { size: number; className?: string }) {
  // The box is 100 units wide, so one pixel is 100 / size units.
  const stroke = (units: number) => (size <= SMALL ? Math.max(units, (MIN_STROKE_PX * 100) / size) : units);
  return (
    <svg
      className={"badge" + (className ? " " + className : "")}
      viewBox="0 0 100 100"
      width={size}
      height={size}
      aria-hidden="true"
    >
      <circle cx="50" cy="50" r="48" className="badge-disc" />
      <circle cx="50" cy="50" r="25" className="badge-mark" fill="none" strokeWidth={stroke(8)} />
      <circle cx="50" cy="50" r="41.5" className="badge-mark" fill="none" strokeWidth={stroke(4)} />
      <circle cx="50" cy="50" r="10.5" className="badge-boss" />
      <circle cx="50" cy="50" r="5" className="badge-disc" />
      <circle cx="50" cy="50" r="46.5" className="badge-outline" fill="none" strokeWidth={stroke(3)} />
    </svg>
  );
}
