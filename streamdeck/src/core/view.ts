import { COLORS, renderFace, toDataUri, type Face } from "./svg.js";

/**
 * What a key shows, as a standard Stream Deck key: its state (0 a plain grey square, 1 a plain green one, or the
 * pictures the person set for the two states in the app), its title, and, for Now Playing only, a picture of the
 * album art. The person's own title, font, size, colour and position come from the app.
 */
export interface KeyView {
  state: 0 | 1;
  /** null: the person's own title (the app's Title field); a string: the plugin's text. */
  title: string | null;
  /** null: the state's picture (the plugin's default or the person's); a data URI: the plugin's picture. */
  image: string | null;
  /** What the face says, for the tests and for debugging: the face's tag and all its text. */
  tag: string;
  text: string;
}

/** How an action's face becomes a key. */
export interface ViewKind {
  /** An info key: its title is live text (the time left, a countdown), whatever the Title field says. */
  live?: boolean;
  /** It draws its picture (the album art of Now Playing). */
  art?: boolean;
}

/** A key is lit when its face says so: a green or amber background, or a frame. */
export function isLit(face: Face): boolean {
  return face.lit ?? (face.bg === COLORS.bgLit || face.bg === COLORS.bgAmber || !!face.border);
}

const byY = (face: Face) => [...face.lines].sort((a, b) => a.y - b.y);

/** The face's text as title lines. */
export function faceText(face: Face, sep = "\n"): string {
  return byY(face)
    .map((l) => l.text.trim())
    .filter(Boolean)
    .join(sep);
}

/** The key's own label when the person set no title: the face's main text (its large lines), else all of it. */
export function labelOf(face: Face): string {
  const big = byY(face).filter((l) => l.size > 15);
  return big.length ? big.map((l) => l.text.trim()).join("\n") : faceText(face);
}

/** The key for a face, given the person's own title (empty: none). */
export function viewOf(face: Face, kind: ViewKind, userTitle: string): KeyView {
  let title: string | null;
  if (face.say !== undefined) title = face.say;
  else if (kind.live || face.live) title = face.title ?? faceText(face);
  else title = userTitle.trim() ? null : labelOf(face);
  let image: string | null = null;
  if (kind.art && face.art?.kind === "image") {
    // the art (and the warning colour and bar of the time left), without text: the title is drawn by the app
    image = toDataUri(renderFace({ ...face, lines: [], glyph: null, border: null }));
  }
  return { state: isLit(face) ? 1 : 0, title, image, tag: face.tag, text: faceText(face, " | ") };
}

/** What Stream Deck is told for a key. */
export interface KeyOut {
  setState(state: 0 | 1): unknown;
  /** No title: the person's own (or none). */
  setTitle(title?: string): unknown;
  /** No image: the state's picture. */
  setImage(image?: string): unknown;
}

/** Tells Stream Deck what changed from `prev` to `next` (all of it when `prev` is null). */
export function applyView(prev: KeyView | null, next: KeyView, out: KeyOut): void {
  const quiet = (r: unknown) => {
    if (r && typeof (r as Promise<void>).catch === "function") (r as Promise<void>).catch(() => undefined);
  };
  if (!prev || prev.state !== next.state) quiet(out.setState(next.state));
  if (!prev || prev.title !== next.title) quiet(out.setTitle(next.title ?? undefined));
  if (!prev || prev.image !== next.image) quiet(out.setImage(next.image ?? undefined));
}

/** The part of a view Stream Deck sees: two views that differ only in tag or text need no update. */
export const viewSig = (v: KeyView): string => `${v.state}\u0000${v.title ?? "\u0001"}\u0000${v.image ?? ""}`;
