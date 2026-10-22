import { wrap } from "./format.js";
import { COLORS, stack, type Face, type TextLine } from "./svg.js";
import type { KeySettings } from "./settings.js";

/** What a person can set on any key's face (the property inspector's "Key face" section). */
export interface FaceSettings {
  /** Text drawn in place of the action's own main text (the playlist name, the key's name). */
  label?: string;
  /** `#rrggbb` of the label. */
  labelColor?: string;
  /** A picture for the key (a data URI, made 144 px square by the inspector). */
  imageIdle?: string;
  /** The picture while the key is lit (a program running, a playlist on air, a hold engaged...). */
  imageActive?: string;
  /** With no active picture: the idle one, tinted green, while lit. */
  tintActive?: boolean;
}

const HEX = /^#[0-9a-f]{6}$/i;

export function faceSettings(s: KeySettings): FaceSettings {
  const str = (v: unknown) => (typeof v === "string" && v.trim() ? v.trim() : undefined);
  const img = (v: unknown) => (typeof v === "string" && /^data:image\/(png|jpeg|gif|webp|svg\+xml)[;,]/.test(v) ? v : undefined);
  return {
    label: str(s.label),
    labelColor: typeof s.labelColor === "string" && HEX.test(s.labelColor) ? s.labelColor : undefined,
    imageIdle: img(s.imageIdle),
    imageActive: img(s.imageActive),
    tintActive: s.tintActive === true,
  };
}

/** A key is lit when its face says so: a green or amber background, or a frame. */
export function isLit(face: Face): boolean {
  return face.lit ?? (face.bg === COLORS.bgLit || face.bg === COLORS.bgAmber || !!face.border);
}

/** The face an action drew, with the person's label and pictures over it. Without any setting it is unchanged. */
export function customize(face: Face, s: KeySettings): Face {
  const f = faceSettings(s);
  if (!f.label && !f.imageIdle && !f.imageActive && !f.labelColor) return face;
  const lit = isLit(face);
  const out: Face = { ...face };
  const pic = lit ? (f.imageActive ?? f.imageIdle) : f.imageIdle;
  if (pic) {
    const tint = lit && !f.imageActive && f.tintActive ? COLORS.green : undefined;
    out.art = { kind: "image", href: pic, tint };
    // the person's own picture is shown as it is (album art is dimmed under text, this is not)
    out.artDim = f.label || face.lines.length ? 0.25 : 0;
  }
  if (f.label || f.labelColor) {
    // the small lines (status, countdowns of 15 px and under) stay; the main text is the label
    const small = face.lines.filter((l) => l.size <= 15);
    const big = face.lines.filter((l) => l.size > 15);
    const text = f.label ?? big.map((l) => l.text).join(" ");
    const size = text.length > 12 ? 18 : 22;
    const main = wrap(text, 128, size, 3);
    const color = f.labelColor ?? COLORS.text;
    const sizes = [...main.map(() => size), ...small.map((l) => l.size)];
    const ys = stack(sizes, face.glyph ? 22 : 8, face.bar ? 134 : 138, 5);
    const lines: TextLine[] = main.map((t, i) => ({ text: t, size, color, weight: 700, y: ys[i] }));
    small.forEach((l, i) => lines.push({ ...l, y: ys[main.length + i] }));
    out.lines = lines;
  }
  return out;
}
