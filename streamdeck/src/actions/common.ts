import { wrap } from "../core/format.js";
import { COLORS, stack, type Face, type Glyph, type TextLine } from "../core/svg.js";

export interface TextFaceOptions {
  tag: string;
  bg?: string;
  border?: string | null;
  glyph?: Glyph | null;
  /** A small label above the main text. */
  top?: string;
  topColor?: string;
  /** The main text: wrapped over up to `lines` lines. */
  main: string;
  mainSize?: number;
  mainColor?: string;
  lines?: number;
  /** A small line below. */
  sub?: string;
  subColor?: string;
  subSize?: number;
  bar?: { frac: number; color: string } | null;
}

/** A key with a label, a main text and a small line, laid out in the middle of the key. */
export function textFace(o: TextFaceOptions): Face {
  const mainSize = o.mainSize ?? 24;
  const main = wrap(o.main, 128, mainSize, o.lines ?? 2);
  const sizes: number[] = [];
  if (o.top) sizes.push(12);
  sizes.push(...main.map(() => mainSize));
  if (o.sub) sizes.push(o.subSize ?? 13);
  const ys = stack(sizes, o.glyph ? 22 : 8, o.bar ? 134 : 138, 5);
  const lines: TextLine[] = [];
  let i = 0;
  if (o.top) lines.push({ text: o.top, size: 12, color: o.topColor ?? COLORS.dim, weight: 600, y: ys[i++] });
  for (const m of main) lines.push({ text: m, size: mainSize, color: o.mainColor ?? COLORS.text, weight: 700, y: ys[i++] });
  if (o.sub) lines.push({ text: o.sub, size: o.subSize ?? 13, color: o.subColor ?? COLORS.dim, weight: 500, y: ys[i++] });
  return { tag: o.tag, bg: o.bg, border: o.border, glyph: o.glyph, lines, bar: o.bar };
}

/** The key of an action that has no playlist, item or block picked yet. */
export function unsetFace(tag: string, what: string, glyph?: Glyph): Face {
  return textFace({ tag, glyph, main: "SET UP", mainColor: COLORS.amber, mainSize: 24, sub: what, subColor: COLORS.text });
}

export const str = (v: unknown): string => (typeof v === "string" ? v : "");
export const numOf = (v: unknown): number | null => {
  const n = typeof v === "string" && v.trim() !== "" ? Number(v) : (v as number);
  return typeof n === "number" && Number.isFinite(n) ? n : null;
};
