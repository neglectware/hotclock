/** Time and text helpers shared by every key renderer. Pure functions, no I/O. */

/** `m:ss` for under an hour, `h:mm:ss` from an hour up. Negative and NaN input shows as zero. */
export function clock(ms: number): string {
  const total = Number.isFinite(ms) ? Math.max(0, Math.round(ms / 1000)) : 0;
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const ss = String(s).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
}

/** Remaining time as the Now Playing key shows it: `-2:31`. */
export function remainingText(ms: number): string {
  return `-${clock(ms)}`;
}

/** Colors of a countdown: red at 10 s and under, amber at 30 s and under (architecture 8). */
export type Urgency = "normal" | "amber" | "red";

export function urgency(ms: number, amberMs = 30_000, redMs = 10_000): Urgency {
  if (ms <= redMs) return "red";
  if (ms <= amberMs) return "amber";
  return "normal";
}

/** Escape text for an SVG text node or attribute. */
export function esc(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

/** Average glyph width of the key font, as a fraction of the font size (bold sans). */
export const GLYPH = 0.56;

/** Cut text to what fits a width at a font size, with an ellipsis. */
export function ellipsize(text: string, width: number, size: number): string {
  const max = Math.max(1, Math.floor(width / (size * GLYPH)));
  const t = text.trim();
  if (t.length <= max) return t;
  return t.slice(0, Math.max(1, max - 1)).trimEnd() + "…";
}

/** Wrap text on word breaks into at most `lines` lines that fit `width`; the last line is ellipsized. */
export function wrap(text: string, width: number, size: number, lines: number): string[] {
  const max = Math.max(1, Math.floor(width / (size * GLYPH)));
  const words = text.trim().split(/\s+/).filter(Boolean);
  const out: string[] = [];
  let cur = "";
  for (const word of words) {
    let w = word;
    while (w.length > max) {
      // One word longer than a line: break it.
      if (cur) out.push(cur);
      cur = "";
      out.push(w.slice(0, max));
      w = w.slice(max);
    }
    const next = cur ? `${cur} ${w}` : w;
    if (next.length <= max) {
      cur = next;
    } else {
      out.push(cur);
      cur = w;
    }
  }
  if (cur) out.push(cur);
  if (out.length > lines) {
    const rest = out.slice(lines - 1).join(" ");
    out.length = lines - 1;
    out.push(rest);
  }
  const last = out.length - 1;
  if (last >= 0) out[last] = ellipsize(out[last], width, size);
  return out;
}

/** The wall-clock part `HH:MM` of an RFC 3339 time, in the box's own zone (the offset it carries). */
export function localHm(iso: string | null | undefined): string {
  if (!iso) return "--:--";
  const m = /T(\d{2}):(\d{2})/.exec(iso);
  return m ? `${m[1]}:${m[2]}` : "--:--";
}

/** Parse an RFC 3339 time to epoch ms; null when it is not one. */
export function parseTime(iso: unknown): number | null {
  if (typeof iso !== "string") return null;
  const t = Date.parse(iso);
  return Number.isFinite(t) ? t : null;
}

/** How long until a counting-down time, shown in whole seconds (rounded), shows a different second. At least 1 ms. */
export function flipDown(ms: number): number {
  if (!Number.isFinite(ms) || ms <= 0) return 1000;
  const shown = Math.round(ms / 1000);
  return Math.max(1, Math.ceil(ms - (shown - 0.5) * 1000) + 1);
}

/** The same for a time counting up. */
export function flipUp(ms: number): number {
  if (!Number.isFinite(ms) || ms < 0) return 1000;
  const shown = Math.round(ms / 1000);
  return Math.max(1, Math.ceil((shown + 0.5) * 1000 - ms) + 1);
}
