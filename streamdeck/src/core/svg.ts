import { esc } from "./format.js";

/** Key size the plugin draws at; Stream Deck scales it for smaller keys. */
export const SIZE = 144;

export const COLORS = {
  bg: "#15171b",
  bgLit: "#1f6f43",
  bgAmber: "#7a4b00",
  bgRed: "#7a1f22",
  bgOffline: "#2b2e34",
  text: "#f4f5f7",
  dim: "#a3a9b3",
  faint: "#6b717b",
  green: "#3ddc84",
  amber: "#ffb020",
  red: "#ff5459",
  blue: "#5aaaff",
} as const;

export interface TextLine {
  text: string;
  size: number;
  color?: string;
  weight?: number;
  /** Baseline, in key pixels. */
  y: number;
  /** Tabular digits for countdowns, so the digits do not jitter. */
  mono?: boolean;
}

export type Art =
  /** A data URI of the album art (144 px JPEG), or the person's own picture; `tint` colours it (a lit key). */
  | { kind: "image"; href: string; tint?: string }
  /** The plain stand-in for an item without art. */
  | { kind: "placeholder" };

export interface Face {
  bg?: string;
  art?: Art | null;
  /** 0 to 1: how far the art is dimmed toward the background. */
  artDim?: number;
  lines: TextLine[];
  /** A progress bar along the bottom edge. */
  bar?: { frac: number; color: string } | null;
  /** A bright frame, for keys that are lit. */
  border?: string | null;
  /** Shown as `data-face` on the root: what the key says, for tests and for debugging. */
  tag: string;
  /** Redraw this long from now even if nothing else changed: when the digits of a countdown will next change. */
  repaintMs?: number;
  /** A small glyph in a corner: `hold`, `stop`, `next`, and so on. */
  glyph?: Glyph | null;
  /** The key is lit (active); when unset it is read from the background and the frame. */
  lit?: boolean;
}

export type Glyph = "next" | "fade" | "pause" | "stop" | "cut" | "play" | "shuffle" | "cart" | "clock" | "block" | "loud" | "display" | "status" | "continue" | "skip" | "abort" | "note" | "list";

const FONT = "-apple-system, 'Helvetica Neue', Helvetica, Arial, sans-serif";

export function glyphPath(g: Glyph, color: string): string {
  // 24 x 24 icons drawn at the top-left corner, scaled by the caller.
  const f = `fill="${color}"`;
  const s = `fill="none" stroke="${color}" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"`;
  switch (g) {
    case "next": return `<path d="M5 4 L15 12 L5 20 Z" ${f}/><rect x="16" y="4" width="3.4" height="16" ${f}/>`;
    case "fade": return `<path d="M3 12 L21 5 L21 19 Z" ${f} opacity="0.85"/>`;
    case "pause": return `<rect x="5" y="4" width="5" height="16" ${f}/><rect x="14" y="4" width="5" height="16" ${f}/>`;
    case "stop": return `<rect x="5" y="5" width="14" height="14" rx="1.5" ${f}/>`;
    case "cut": return `<path d="M5 5 L19 19 M19 5 L5 19" ${s}/>`;
    case "play": return `<path d="M7 4 L19 12 L7 20 Z" ${f}/>`;
    case "shuffle": return `<path d="M3 7 H8 C14 7 10 17 16 17 H20 M3 17 H8 C10 17 11 15 12 13 M13 11 C14 9 15 7 17 7 H20" ${s}/>`;
    case "cart": return `<rect x="3" y="7" width="18" height="12" rx="2" ${s}/><path d="M7 7 V4 H17 V7" ${s}/>`;
    case "clock": return `<circle cx="12" cy="12" r="8.5" ${s}/><path d="M12 7 V12 L15.5 14" ${s}/>`;
    case "block": return `<rect x="4" y="4" width="16" height="16" rx="2" ${s}/><path d="M8 9 H16 M8 13 H13" ${s}/>`;
    case "loud": return `<path d="M3 14 H6 L11 18 V6 L6 10 H3 Z" ${f}/><path d="M15 9 C17 11 17 13 15 15 M18 6 C22 10 22 14 18 18" ${s}/>`;
    case "display": return `<rect x="3" y="5" width="18" height="12" rx="1.5" ${s}/><path d="M9 21 H15 M12 17 V21" ${s}/>`;
    case "status": return `<circle cx="12" cy="12" r="7" ${f}/>`;
    case "continue": return `<path d="M4 5 L12 12 L4 19 Z M12 5 L20 12 L12 19 Z" ${f}/>`;
    case "skip": return `<path d="M4 5 L12 12 L4 19 Z" ${f}/><path d="M14 5 L20 12 L14 19" ${s}/>`;
    case "note": return `<path d="M9 18 V5 L20 3 V16" ${s}/><circle cx="6.5" cy="18" r="3" ${f}/><circle cx="17.5" cy="16" r="3" ${f}/>`;
    case "list": return `<path d="M8 6 H21 M8 12 H21 M8 18 H21" ${s}/><circle cx="3.5" cy="6" r="1.6" ${f}/><circle cx="3.5" cy="12" r="1.6" ${f}/><circle cx="3.5" cy="18" r="1.6" ${f}/>`;
    case "abort": return `<path d="M12 3 V13 M7 6 A8 8 0 1 0 17 6" ${s}/>`;
  }
}

/** Draw a face as a 144 x 144 SVG document. */
export function renderFace(face: Face): string {
  const bg = face.bg ?? COLORS.bg;
  const parts: string[] = [];
  parts.push(`<rect width="${SIZE}" height="${SIZE}" fill="${bg}"/>`);
  if (face.art?.kind === "image") {
    let filter = "";
    if (face.art.tint && /^#[0-9a-f]{6}$/i.test(face.art.tint)) {
      // the picture's brightness in the tint colour
      const c = face.art.tint;
      const [r, g, b] = [1, 3, 5].map((i) => parseInt(c.slice(i, i + 2), 16) / 255);
      const row = (k: number) => `${(0.2126 * k).toFixed(3)} ${(0.7152 * k).toFixed(3)} ${(0.0722 * k).toFixed(3)} 0 0`;
      parts.push(`<filter id="tint"><feColorMatrix type="matrix" values="${row(r)} ${row(g)} ${row(b)} 0 0 0 1 0"/></filter>`);
      filter = ` filter="url(#tint)" data-tint="${c}"`;
    }
    parts.push(
      `<image data-art="image" x="0" y="0" width="${SIZE}" height="${SIZE}" preserveAspectRatio="xMidYMid slice" href="${esc(face.art.href)}"${filter}/>`,
    );
    const dim = Math.min(1, Math.max(0, face.artDim ?? 0.62));
    parts.push(`<rect width="${SIZE}" height="${SIZE}" fill="${bg}" opacity="${dim}"/>`);
  } else if (face.art?.kind === "placeholder") {
    parts.push(
      `<g data-art="placeholder" opacity="0.5"><circle cx="72" cy="58" r="40" fill="none" stroke="#3a3f48" stroke-width="3"/>` +
        `<circle cx="72" cy="58" r="24" fill="none" stroke="#2f343c" stroke-width="2"/><circle cx="72" cy="58" r="5" fill="#3a3f48"/></g>`,
    );
  }
  if (face.glyph) {
    parts.push(`<g transform="translate(6 6) scale(0.75)" opacity="0.8">${glyphPath(face.glyph, COLORS.dim)}</g>`);
  }
  for (const l of face.lines) {
    const color = l.color ?? COLORS.text;
    const weight = l.weight ?? 600;
    const mono = l.mono ? ` font-variant-numeric="tabular-nums"` : "";
    // A faint shadow keeps text legible over album art.
    parts.push(
      `<text x="72" y="${l.y}" text-anchor="middle" font-family="${FONT}" font-size="${l.size}" font-weight="${weight}"${mono} ` +
        `fill="${color}" stroke="${bg}" stroke-opacity="0.55" stroke-width="3" paint-order="stroke" stroke-linejoin="round">${esc(l.text)}</text>`,
    );
  }
  if (face.bar) {
    const w = Math.max(0, Math.min(1, face.bar.frac)) * SIZE;
    parts.push(`<rect y="${SIZE - 6}" width="${SIZE}" height="6" fill="#000" opacity="0.45"/>`);
    parts.push(`<rect data-bar="1" y="${SIZE - 6}" width="${w.toFixed(1)}" height="6" fill="${face.bar.color}"/>`);
  }
  if (face.border) {
    parts.push(`<rect x="3" y="3" width="${SIZE - 6}" height="${SIZE - 6}" rx="10" fill="none" stroke="${face.border}" stroke-width="5"/>`);
  }
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${SIZE}" height="${SIZE}" viewBox="0 0 ${SIZE} ${SIZE}" data-face="${esc(face.tag)}">${parts.join("")}</svg>`;
}

/** An SVG string as the data URI `setImage` takes. */
export function toDataUri(svg: string): string {
  return `data:image/svg+xml;charset=utf8,${encodeURIComponent(svg)}`;
}

/** Y baselines for a stack of lines, centered vertically in `[top, bottom]`. */
export function stack(sizes: number[], top = 8, bottom = 136, gap = 4): number[] {
  const heights = sizes.map((s) => s * 1.05);
  const total = heights.reduce((a, b) => a + b, 0) + gap * (sizes.length - 1);
  let y = top + (bottom - top - total) / 2;
  return sizes.map((s, i) => {
    const base = y + s * 0.82;
    y += heights[i] + gap;
    return Math.round(base * 10) / 10;
  });
}

/** The grey key every action shows while its box does not answer. */
export function offlineFace(station?: string): Face {
  const lines: TextLine[] = [{ text: "OFFLINE", size: 25, color: COLORS.dim, weight: 700, y: 78 }];
  if (station) lines.push({ text: station.slice(0, 14), size: 14, color: COLORS.faint, weight: 500, y: 104 });
  return { bg: COLORS.bgOffline, lines, tag: "offline" };
}

/** A station key whose station was never chosen: it sends nothing (it must never fall back to the first station). */
export function noStationFace(): Face {
  return {
    bg: COLORS.bgOffline,
    border: COLORS.red,
    lines: [
      { text: "NO", size: 22, color: COLORS.red, weight: 700, y: 58 },
      { text: "STATION", size: 22, color: COLORS.red, weight: 700, y: 82 },
      { text: "pick one", size: 14, color: COLORS.text, weight: 500, y: 106 },
    ],
    tag: "no-station",
  };
}

/** The key shown before the box address is set. */
export function setupFace(): Face {
  return {
    bg: COLORS.bgOffline,
    lines: [
      { text: "SET UP", size: 24, color: COLORS.amber, weight: 700, y: 70 },
      { text: "box address", size: 14, color: COLORS.text, weight: 500, y: 94 },
    ],
    tag: "setup",
  };
}
