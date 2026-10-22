/** Wall-clock arithmetic for "at 18:55" and "at :00", done in the box's time zone. */

interface Parts { y: number; mo: number; d: number; h: number; mi: number; s: number }

function partsIn(ms: number, tz: string | null): Parts {
  const f = new Intl.DateTimeFormat("en-CA", {
    timeZone: tz ?? undefined,
    hourCycle: "h23",
    year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", second: "2-digit",
  });
  const o: Record<string, number> = {};
  for (const p of f.formatToParts(new Date(ms))) if (p.type !== "literal") o[p.type] = Number(p.value);
  return { y: o.year, mo: o.month, d: o.day, h: o.hour === 24 ? 0 : o.hour, mi: o.minute, s: o.second };
}

function asUtc(p: Parts): number {
  return Date.UTC(p.y, p.mo - 1, p.d, p.h, p.mi, p.s);
}

/** The instant (epoch ms) of local `h:mi` on the given local date in `tz`. */
function instantOf(y: number, mo: number, d: number, h: number, mi: number, tz: string | null): number {
  const guess = Date.UTC(y, mo - 1, d, h, mi, 0);
  let t = guess - (asUtc(partsIn(guess, tz)) - guess);
  // Once more, in case the guess sat on the other side of a daylight-saving change.
  t = guess - (asUtc(partsIn(t, tz)) - t);
  return t;
}

/** The next time the clock in `tz` reads `HH:MM`, at least `minAheadMs` after `now`. Null when `hhmm` is not a time. */
export function nextClockTime(hhmm: string, now: number, tz: string | null, minAheadMs = 5000): number | null {
  const m = /^(\d{1,2}):(\d{2})$/.exec(hhmm.trim());
  if (!m) return null;
  const h = Number(m[1]);
  const mi = Number(m[2]);
  if (h > 23 || mi > 59) return null;
  const p = partsIn(now, tz);
  let t = instantOf(p.y, p.mo, p.d, h, mi, tz);
  if (t < now + minAheadMs) {
    const next = new Date(Date.UTC(p.y, p.mo - 1, p.d + 1));
    t = instantOf(next.getUTCFullYear(), next.getUTCMonth() + 1, next.getUTCDate(), h, mi, tz);
  }
  return t;
}

/** The next time the minute of the hour is `minute` (on the second 0), at least `minAheadMs` away. */
export function nextMinuteOfHour(minute: number, now: number, minAheadMs = 20_000): number {
  const hour = 3_600_000;
  let t = Math.floor(now / hour) * hour + minute * 60_000;
  while (t < now + minAheadMs) t += hour;
  return t;
}

/** An instant as the RFC 3339 form the API takes. */
export function rfc3339(ms: number): string {
  return new Date(ms).toISOString();
}

/** `HH:MM` of an instant on the wall clock of `tz` (an IANA name); the computer's own zone when it is not known. */
export function zonedHm(ms: number, tz: string | null): string {
  try {
    const f = new Intl.DateTimeFormat("en-GB", { timeZone: tz ?? undefined, hour: "2-digit", minute: "2-digit", hourCycle: "h23" });
    return f.format(new Date(ms));
  } catch {
    return "--:--";
  }
}
