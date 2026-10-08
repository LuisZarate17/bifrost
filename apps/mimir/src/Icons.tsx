// Outlined, monochrome, drawn on a 16px grid and coloured by `currentColor`, so
// every icon takes its colour from whatever it sits in. Inline rather than an
// icon package: a couple of dozen glyphs do not justify a dependency, and an app
// whose rule is that nothing leaves the machine should not be loading them from one.

import type { SVGProps } from "react";

type P = SVGProps<SVGSVGElement>;

const base = (p: P) => ({
  width: 16,
  height: 16,
  viewBox: "0 0 16 16",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 1.4,
  strokeLinecap: "round" as const,
  strokeLinejoin: "round" as const,
  "aria-hidden": true,
  ...p,
});

export const Plus = (p: P) => (
  <svg {...base(p)}><path d="M8 3v10M3 8h10" /></svg>
);

export const ArrowUp = (p: P) => (
  <svg {...base(p)}><path d="M8 13V3M3.5 7.5 8 3l4.5 4.5" /></svg>
);

export const Stop = (p: P) => (
  <svg {...base(p)}><rect x="4" y="4" width="8" height="8" rx="1.5" fill="currentColor" stroke="none" /></svg>
);

export const Sidebar = (p: P) => (
  <svg {...base(p)}>
    <rect x="2" y="2.75" width="12" height="10.5" rx="2" />
    <path d="M6 2.75v10.5" />
  </svg>
);

export const Chevron = (p: P) => (
  <svg {...base(p)}><path d="m4.5 6.25 3.5 3.5 3.5-3.5" /></svg>
);

export const Wrench = (p: P) => (
  <svg {...base(p)}>
    <path d="M9.9 2.6a3.2 3.2 0 0 0-3.8 4.2L2.6 10.3a1.3 1.3 0 0 0 1.8 1.8l3.5-3.5a3.2 3.2 0 0 0 4.2-3.8l-1.9 1.9-1.6-.4-.4-1.6Z" />
  </svg>
);

/** A lens: looking for something in the project — a search or a listing. */
export const Search = (p: P) => (
  <svg {...base(p)}>
    <circle cx="7" cy="7" r="4.25" />
    <path d="m10.25 10.25 3.25 3.25" />
  </svg>
);

/** A prompt in a window: a command that runs. For the check, which is a command
 *  and not a verdict — a tick would read as "passed" on a check that failed. */
export const Terminal = (p: P) => (
  <svg {...base(p)}>
    <rect x="1.75" y="2.75" width="12.5" height="10.5" rx="2" />
    <path d="m4.75 6.25 2 1.75-2 1.75M8.5 10h2.75" />
  </svg>
);

export const File = (p: P) => (
  <svg {...base(p)}>
    <path d="M9 2H4.75A1.25 1.25 0 0 0 3.5 3.25v9.5A1.25 1.25 0 0 0 4.75 14h6.5a1.25 1.25 0 0 0 1.25-1.25V5.5L9 2Z" />
    <path d="M9 2v3.5h3.5" />
  </svg>
);

export const Image = (p: P) => (
  <svg {...base(p)}>
    <rect x="2.5" y="3" width="11" height="10" rx="1.75" />
    <circle cx="6" cy="6.5" r="1.1" />
    <path d="m13.5 10.5-3-3-6 5.5" />
  </svg>
);

export const Copy = (p: P) => (
  <svg {...base(p)}>
    <rect x="5.5" y="5.5" width="8" height="8" rx="1.5" />
    <path d="M10.5 5.5V3.75A1.25 1.25 0 0 0 9.25 2.5h-5.5A1.25 1.25 0 0 0 2.5 3.75v5.5A1.25 1.25 0 0 0 3.75 10.5H5.5" />
  </svg>
);

export const Check = (p: P) => (
  <svg {...base(p)}><path d="m3.5 8.5 3 3 6-7" /></svg>
);

export const X = (p: P) => (
  <svg {...base(p)}><path d="m4 4 8 8M12 4l-8 8" /></svg>
);

export const Pencil = (p: P) => (
  <svg {...base(p)}>
    <path d="M10.5 2.75a1.4 1.4 0 0 1 2 2L5.25 12l-2.75.75.75-2.75 7.25-7.25Z" />
  </svg>
);

export const Trash = (p: P) => (
  <svg {...base(p)}>
    <path d="M2.75 4.5h10.5M6.25 4.5V3a.75.75 0 0 1 .75-.75h2a.75.75 0 0 1 .75.75v1.5M4 4.5l.6 8.2a1.25 1.25 0 0 0 1.25 1.05h4.3a1.25 1.25 0 0 0 1.25-1.05L12 4.5" />
  </svg>
);

export const Move = (p: P) => (
  <svg {...base(p)}>
    <path d="M2.5 8h10.5M9 4l4 4-4 4" />
  </svg>
);

export const Refresh = (p: P) => (
  <svg {...base(p)}>
    <path d="M13 8a5 5 0 1 1-1.46-3.54M13 2.75v3h-3" />
  </svg>
);

export const Chip = (p: P) => (
  <svg {...base(p)}>
    <rect x="4" y="4" width="8" height="8" rx="1.25" />
    <path d="M6.5 1.75V4M9.5 1.75V4M6.5 12v2.25M9.5 12v2.25M1.75 6.5H4M1.75 9.5H4M12 6.5h2.25M12 9.5h2.25" />
  </svg>
);

export const Eye = (p: P) => (
  <svg {...base(p)}>
    <path d="M1.75 8S4 3.5 8 3.5 14.25 8 14.25 8 12 12.5 8 12.5 1.75 8 1.75 8Z" />
    <circle cx="8" cy="8" r="1.9" />
  </svg>
);

export const Alert = (p: P) => (
  <svg {...base(p)}>
    <path d="M8 2.25 1.75 13.25h12.5L8 2.25Z" />
    <path d="M8 6.5v3M8 11.4v.1" />
  </svg>
);

export const Folder = (p: P) => (
  <svg {...base(p)}>
    <path d="M2 4.25A1.25 1.25 0 0 1 3.25 3h3.1l1.4 1.5h5A1.25 1.25 0 0 1 14 5.75v6A1.25 1.25 0 0 1 12.75 13h-9.5A1.25 1.25 0 0 1 2 11.75v-7.5Z" />
  </svg>
);

export const Shield = (p: P) => (
  <svg {...base(p)}>
    <path d="M8 1.75 2.75 3.75v4c0 3.1 2.2 5.4 5.25 6.5 3.05-1.1 5.25-3.4 5.25-6.5v-4L8 1.75Z" />
  </svg>
);

export const Palette = (p: P) => (
  <svg {...base(p)}>
    <path d="M8 2.25a5.75 5.75 0 1 0 0 11.5c.85 0 1.35-.55 1.35-1.25 0-.4-.17-.7-.4-.98a1.3 1.3 0 0 1 1-2.12h1.3a2.4 2.4 0 0 0 2.4-2.4c0-2.6-2.5-4.75-5.65-4.75Z" />
    <circle cx="5" cy="7.25" r=".85" fill="currentColor" stroke="none" />
    <circle cx="6.75" cy="4.75" r=".85" fill="currentColor" stroke="none" />
    <circle cx="9.75" cy="4.9" r=".85" fill="currentColor" stroke="none" />
  </svg>
);

/** Four panes, for "Match Windows". Drawn in the same line as the rest, not the logo. */
export const Windows = (p: P) => (
  <svg {...base(p)}>
    <rect x="2.5" y="2.5" width="4.75" height="4.75" rx=".75" />
    <rect x="8.75" y="2.5" width="4.75" height="4.75" rx=".75" />
    <rect x="2.5" y="8.75" width="4.75" height="4.75" rx=".75" />
    <rect x="8.75" y="8.75" width="4.75" height="4.75" rx=".75" />
  </svg>
);

/** A checklist, for the model's task list. */
export const Checklist = (p: P) => (
  <svg {...base(p)}>
    <path d="m2.25 4.25 1.25 1.25 2-2.5M2.25 10.25l1.25 1.25 2-2.5M8.5 4.5h5M8.5 10.5h5" />
  </svg>
);
