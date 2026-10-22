import type { ActionDef, Ctx } from "../core/defs.js";
import { clock, ellipsize, flipDown, flipUp, parseTime, remainingText, urgency, wrap, type Urgency } from "../core/format.js";
import { nextMinuteOfHour, rfc3339, zonedHm } from "../core/time.js";
import { COLORS, stack, type Art, type Face, type TextLine } from "../core/svg.js";
import { numOf, str, textFace, unsetFace } from "./common.js";

const URGENCY_COLOR: Record<Urgency, string> = { normal: COLORS.text, amber: COLORS.amber, red: COLORS.red };

function fadeBody(c: Ctx): { fade_ms?: number } {
  const ms = numOf(c.s.fadeMs);
  return ms && ms > 0 ? { fade_ms: ms } : {};
}

// ---- Now Playing ------------------------------------------------------------------------------------------------

export const nowPlaying: ActionDef = {
  id: "now-playing",
  uses: "station",
  face(c) {
    const m = c.m;
    if (m.safeMode) return textFace({ tag: "now-playing:safe", bg: COLORS.bgRed, main: "SAFE MODE", mainSize: 22, sub: "station is protecting itself", subSize: 11 });
    const now = m.now;
    if (!now) {
      const label = m.state === "held" ? "HELD" : "STOPPED";
      return textFace({ tag: "now-playing:idle", glyph: "stop", main: label, mainColor: COLORS.dim, mainSize: 24, sub: m.activePlaylist?.name ?? "nothing playing", subColor: COLORS.faint });
    }
    const art = nowArt(c, now.artUrl);
    const rem = m.remainingMs(c.nowMs);
    const u = rem ? urgency(rem.ms) : "normal";
    const remColor = URGENCY_COLOR[u];
    const title = wrap(now.title, 130, 17, 2);
    const lines: TextLine[] = [
      ...title.map((t, i) => ({ text: t, size: 17, weight: 700, y: 24 + i * 19 })),
      { text: ellipsize(now.artist, 130, 13), size: 13, weight: 500, color: COLORS.dim, y: 24 + title.length * 19 + 2 },
    ];
    if (rem && now.continuityNext) lines.push({ text: "ALBUM", size: 11, weight: 700, color: COLORS.blue, y: 94 });
    if (rem) lines.push({ text: remainingText(rem.ms), size: 34, weight: 800, color: remColor, y: 122, mono: true });
    // The bar moves only when the digits do (it is worked out from the whole seconds shown), so it never costs an extra repaint.
    const end = rem ? (rem.to === "segue" ? now.segueMs : now.endsMs) : null;
    const totalS = end === null ? 0 : Math.max(1, Math.round((end - now.startedMs) / 1000));
    const remS = rem ? Math.min(totalS, Math.round(rem.ms / 1000)) : 0;
    return {
      tag: `now-playing:${u}`,
      repaintMs: rem ? Math.min(flipDown(rem.ms), nextThreshold(rem.ms)) : undefined,
      art,
      artDim: 0.58,
      lines,
      bar: rem && totalS > 0 ? { frac: (totalS - remS) / totalS, color: remColor === COLORS.text ? COLORS.green : remColor } : null,
    };
  },
};

/** Time until a countdown crosses its amber or red line, so the key changes color on the dot. */
function nextThreshold(ms: number, lines = [30_000, 10_000]): number {
  const ahead = lines.filter((l) => ms > l).map((l) => ms - l + 1);
  return ahead.length ? Math.min(...ahead) : 60_000;
}

/** The art of a playing item: the picture, a plain placeholder when it has none, or nothing while it loads. */
function nowArt(c: Ctx, artUrl: string | null): Art | null {
  if (!artUrl) return { kind: "placeholder" };
  const a = c.conn?.artFor(artUrl);
  if (a === undefined) return null;
  return a ? { kind: "image", href: a } : { kind: "placeholder" };
}

// ---- Elapsed / Remaining ----------------------------------------------------------------------------------------

export const elapsedRemaining: ActionDef = {
  id: "elapsed-remaining",
  uses: "station",
  face(c) {
    const m = c.m;
    const showElapsed = c.s.show === "elapsed";
    const rem = m.remainingMs(c.nowMs);
    const el = m.elapsedMs(c.nowMs);
    if (!m.now || (showElapsed ? el === null : rem === null)) {
      return textFace({ tag: "elapsed:idle", main: "--:--", mainColor: COLORS.faint, mainSize: 40, sub: showElapsed ? "ELAPSED" : "REMAINING", subColor: COLORS.faint });
    }
    const ms = showElapsed ? (el as number) : (rem as { ms: number }).ms;
    const text = clock(ms);
    const u = showElapsed ? "normal" : urgency(ms);
    const size = text.length > 5 ? 34 : 46;
    const ys = stack([12, size], 14, 134, 8);
    return {
      tag: `elapsed:${showElapsed ? "elapsed" : "remaining"}:${u}`,
      repaintMs: showElapsed ? flipUp(ms) : Math.min(flipDown(ms), nextThreshold(ms)),
      lines: [
        { text: showElapsed ? "ELAPSED" : "REMAINING", size: 12, color: COLORS.dim, weight: 600, y: ys[0] },
        { text, size, weight: 800, color: URGENCY_COLOR[u], y: ys[1], mono: true },
      ],
    };
  },
  press(c) {
    c.save({ show: c.s.show === "elapsed" ? "remaining" : "elapsed" });
  },
};

// ---- Next Up ----------------------------------------------------------------------------------------------------

export const nextUp: ActionDef = {
  id: "next-up",
  uses: "station",
  face(c) {
    const n = c.m.next;
    if (!n) return textFace({ tag: "next-up:none", top: "NEXT UP", main: "NOTHING", mainColor: COLORS.faint, mainSize: 22, sub: "queued" });
    const rem = c.m.remainingMs(c.nowMs);
    const inMs = n.startsMs !== null ? n.startsMs - c.m.boxNow() : rem ? rem.ms : null;
    const art = c.conn?.artFor(n.artUrl);
    const f = textFace({
      tag: "next-up",
      top: "NEXT UP",
      main: n.title,
      mainSize: 16,
      lines: 3,
      sub: inMs !== null ? `in ${clock(Math.max(0, inMs))}` : n.artist,
      subSize: 16,
      subColor: COLORS.text,
    });
    return { ...f, art: art ? { kind: "image", href: art } : null, artDim: 0.72, repaintMs: inMs !== null ? flipDown(Math.max(0, inMs)) : undefined };
  },
};

// ---- Transport keys ---------------------------------------------------------------------------------------------

export const next: ActionDef = {
  id: "next",
  uses: "station",
  face(c) {
    const idle = c.m.state === "stopped";
    return textFace({ tag: "next", glyph: "next", main: "NEXT", mainSize: 30, mainColor: idle ? COLORS.faint : COLORS.text, sub: "segue now", subColor: COLORS.faint });
  },
  press: (c) => c.post("/transport/next"),
};

export const fadeNext: ActionDef = {
  id: "fade-next",
  uses: "station",
  face(c) {
    const idle = c.m.state === "stopped";
    return textFace({ tag: "fade-next", glyph: "fade", main: "FADE", mainSize: 30, mainColor: idle ? COLORS.faint : COLORS.text, sub: "to next", subColor: COLORS.faint });
  },
  press: (c) => c.post("/transport/fade-next", fadeBody(c)),
};

export const holdResume: ActionDef = {
  id: "hold-resume",
  uses: "station",
  face(c) {
    const held = c.m.hold || c.m.state === "held";
    return held
      ? textFace({ tag: "hold:held", bg: COLORS.bgAmber, border: COLORS.amber, glyph: "pause", main: "HELD", mainSize: 30, mainColor: COLORS.amber, sub: "tap to resume", subColor: COLORS.text })
      : textFace({ tag: "hold:free", glyph: "pause", main: "HOLD", mainSize: 30, sub: "after this song", subColor: COLORS.faint });
  },
  press: (c) => (c.m.hold || c.m.state === "held" ? c.post("/transport/resume") : c.post("/transport/hold")),
};

export const stop: ActionDef = {
  id: "stop",
  uses: "station",
  longPress: "only",
  hint: "HOLD",
  face(c) {
    return textFace({ tag: "stop", glyph: "stop", main: "STOP", mainSize: 30, mainColor: c.m.state === "stopped" ? COLORS.faint : COLORS.red, sub: "hold to stop", subColor: COLORS.faint });
  },
  long: (c) => c.post("/transport/stop", fadeBody(c)),
};

export const cut: ActionDef = {
  id: "cut",
  uses: "station",
  longPress: "only",
  hint: "HOLD",
  face(c) {
    return textFace({ tag: "cut", glyph: "cut", main: "CUT", mainSize: 30, mainColor: c.m.state === "stopped" ? COLORS.faint : COLORS.red, sub: "hold to cut", subColor: COLORS.faint });
  },
  long: (c) => c.post("/transport/cut"),
};

// ---- Playlists --------------------------------------------------------------------------------------------------

export const switchPlaylist: ActionDef = {
  id: "switch-playlist",
  uses: "station",
  face(c) {
    const id = numOf(c.s.playlistId);
    if (id === null) return unsetFace("switch:unset", "a playlist", "play");
    const lit = c.m.activePlaylist?.id === id && c.m.state !== "stopped";
    const how = str(c.s.how) || "segue";
    return textFace({
      tag: lit ? "switch:lit" : "switch",
      bg: lit ? COLORS.bgLit : undefined,
      border: lit ? COLORS.green : null,
      glyph: "play",
      main: str(c.s.playlistName) || `Playlist ${id}`,
      mainSize: 19,
      lines: 3,
      sub: lit ? "ON AIR" : how === "now" ? "start now" : how === "end" ? "after this one" : "at next segue",
      subColor: lit ? COLORS.green : COLORS.faint,
    });
  },
  press(c) {
    const id = numOf(c.s.playlistId);
    if (id === null) return null;
    return c.post("/playlist/switch", { playlist_id: id, how: str(c.s.how) || "segue" });
  },
};

export const reshuffle: ActionDef = {
  id: "reshuffle",
  uses: "station",
  face(c) {
    if ((c.extra.flashUntil as number | undefined) && (c.extra.flashUntil as number) > c.nowMs) {
      const n = c.m.next;
      return textFace({ tag: "reshuffle:flash", bg: COLORS.bgLit, glyph: "shuffle", top: "NEXT", main: n?.title ?? "(nothing)", mainSize: 17, lines: 3, sub: n?.artist, subColor: COLORS.text });
    }
    return textFace({ tag: "reshuffle", glyph: "shuffle", main: "RESHUFFLE", mainSize: 19, sub: c.m.activePlaylist?.name, subColor: COLORS.faint });
  },
  async press(c) {
    const r = await c.post("/playlist/reshuffle");
    if (r.ok) c.extra.flashUntil = c.nowMs + 2000;
    return r;
  },
};

// ---- Schedule change --------------------------------------------------------------------------------------------

interface Pending { id: number; at: string; name: string }

export const scheduleChange: ActionDef = {
  id: "schedule-change",
  uses: "station",
  poll: {
    everyMs: 5000,
    async run(c) {
      const r = await c.env.http.get(`${c.conn!.base}/api/v1/schedule/upcoming?hours=24`);
      if (!r.ok) return;
      const ev = (r.data?.events ?? []).filter((e: any) => e.origin === "oneshot"); // eslint-disable-line @typescript-eslint/no-explicit-any
      ev.sort((a: any, b: any) => String(a.predicted_at).localeCompare(String(b.predicted_at))); // eslint-disable-line @typescript-eslint/no-explicit-any
      const e = ev[0];
      c.extra.pending = e ? ({ id: -e.block_id, at: e.target ?? e.predicted_at, name: e.name } satisfies Pending) : null;
    },
  },
  face(c) {
    const p = c.extra.pending as Pending | null | undefined;
    if (p) {
      // The box sends UTC times; the key shows its wall clock, in the box's time zone.
      const at = parseTime(p.at);
      const hm = at === null ? "--:--" : zonedHm(at, c.m.program?.timeZone ?? null);
      return textFace({ tag: "schedule:pending", bg: COLORS.bgAmber, border: COLORS.amber, glyph: "clock", top: "SCHEDULED", main: hm, mainSize: 38, mainColor: COLORS.amber, sub: "tap to cancel", subColor: COLORS.text });
    }
    if (numOf(c.s.playlistId) === null) return unsetFace("schedule:unset", "a playlist", "clock");
    const when = c.s.when === "offset" ? `in ${numOf(c.s.offsetMin) ?? 5} min` : `at :${String(numOf(c.s.minute) ?? 0).padStart(2, "0")}`;
    return textFace({ tag: "schedule", glyph: "clock", top: "SCHEDULE", main: str(c.s.playlistName) || "Playlist", mainSize: 17, lines: 2, sub: when, subSize: 18, subColor: COLORS.text });
  },
  async press(c) {
    const p = c.extra.pending as Pending | null | undefined;
    if (p) {
      const r = await c.del(`/schedule/changes/${p.id}`);
      if (r.ok) c.extra.pending = null;
      return r;
    }
    const id = numOf(c.s.playlistId);
    if (id === null) return null;
    const boxNow = c.m.boxNow();
    const at = c.s.when === "offset" ? boxNow + (numOf(c.s.offsetMin) ?? 5) * 60_000 : nextMinuteOfHour(numOf(c.s.minute) ?? 0, boxNow);
    const r = await c.post("/schedule/changes", { playlist_id: id, at: rfc3339(at), how: str(c.s.how) || "segue" });
    // An action's answer is the station's state with the result in `result` (the state's own `at` is the time now).
    const made = r.data?.result ?? r.data;
    if (r.ok && made && typeof made.id === "number") c.extra.pending = { id: made.id, at: made.at ?? rfc3339(at), name: str(c.s.playlistName) } satisfies Pending;
    return r;
  },
};

// ---- Cart -------------------------------------------------------------------------------------------------------

export const fireItem: ActionDef = {
  id: "fire-item",
  uses: "station",
  face(c) {
    const id = numOf(c.s.itemId);
    if (id === null) return unsetFace("cart:unset", "an item", "cart");
    // The box reports the carts it is playing in `state.carts`; the key's own timer covers boxes that do not.
    const playing = c.m.carts.some((x) => x.itemId === id) || ((c.extra.litUntil as number | undefined) ?? 0) > c.nowMs;
    return textFace({
      tag: playing ? "cart:playing" : "cart",
      bg: playing ? COLORS.bgLit : undefined,
      border: playing ? COLORS.green : null,
      glyph: "cart",
      main: str(c.s.itemTitle) || `Item ${id}`,
      mainSize: 18,
      lines: 3,
      sub: playing ? "PLAYING" : "fire over audio",
      subColor: playing ? COLORS.green : COLORS.faint,
    });
  },
  async press(c) {
    const id = numOf(c.s.itemId);
    if (id === null) return null;
    const r = await c.post("/cart", { item_id: id });
    if (r.ok) c.extra.litUntil = c.nowMs + (numOf(c.s.durationMs) ?? 5000);
    return r;
  },
};

// ---- Station status ---------------------------------------------------------------------------------------------

export const stationStatus: ActionDef = {
  id: "station-status",
  uses: "station",
  offlineFace: (label) => textFace({ tag: "status:offline", bg: COLORS.bgRed, border: COLORS.red, glyph: "status", main: "OFFLINE", mainSize: 24, mainColor: COLORS.red, sub: label || undefined, subColor: COLORS.text }),
  face(c) {
    const st = c.m.status();
    const name = c.m.displayName || c.t.station || "station";
    if (st === "critical") {
      const a = c.m.alerts.find((x) => x.severity === "critical");
      return textFace({ tag: "status:critical", bg: COLORS.bgRed, border: COLORS.red, glyph: "status", main: "ALERT", mainSize: 28, mainColor: COLORS.red, sub: a?.code ?? name, subColor: COLORS.text });
    }
    if (st === "warning") {
      const a = c.m.alerts.find((x) => x.severity === "warning");
      return textFace({ tag: "status:warning", bg: COLORS.bgAmber, border: COLORS.amber, glyph: "status", main: "WARNING", mainSize: 24, mainColor: COLORS.amber, sub: a?.code ?? name, subColor: COLORS.text });
    }
    return textFace({ tag: "status:ok", glyph: "status", main: "OK", mainSize: 36, mainColor: COLORS.green, sub: name, subColor: COLORS.dim });
  },
  press(c) {
    c.env.openUrl(`http://${c.t.host}:${c.t.libraryPort}/`);
  },
};

// ---- Loudness ---------------------------------------------------------------------------------------------------

export const loudness: ActionDef = {
  id: "loudness",
  uses: "station",
  longPress: "extra",
  face(c) {
    const l = c.m.loudness;
    if (!l || (l.target === null && l.window === null)) {
      return textFace({ tag: "loudness:off", glyph: "loud", main: "OFF", mainSize: 26, mainColor: COLORS.faint, sub: "no reading", subColor: COLORS.faint });
    }
    if (l.target === null) {
      // The controller is off: the reading is still the box's.
      return textFace({ tag: "loudness:no-target", glyph: "loud", top: "LUFS", main: (l.window as number).toFixed(1), mainSize: 38, mainColor: COLORS.dim, sub: "no target set", subSize: 13, subColor: COLORS.faint });
    }
    if (l.window === null) {
      return textFace({ tag: "loudness:wait", glyph: "loud", top: "LUFS", main: "--.-", mainSize: 38, mainColor: COLORS.faint, sub: `/ ${fmtLu(l.target)}`, subSize: 16 });
    }
    const off = Math.abs(l.window - l.target);
    const ok = off <= 1;
    return textFace({
      tag: ok ? "loudness:ok" : "loudness:off-target",
      glyph: "loud",
      top: "LUFS",
      main: l.window.toFixed(1),
      mainSize: 38,
      mainColor: ok ? COLORS.green : COLORS.amber,
      sub: `/ ${fmtLu(l.target)}`,
      subSize: 17,
      subColor: COLORS.text,
    });
  },
  long: (c) => c.post("/loudness/reset"),
};

function fmtLu(v: number): string {
  return Number.isInteger(v) ? String(v) : v.toFixed(1);
}

export const playbackActions: ActionDef[] = [
  nowPlaying,
  elapsedRemaining,
  nextUp,
  next,
  fadeNext,
  holdResume,
  stop,
  cut,
  switchPlaylist,
  scheduleChange,
  reshuffle,
  fireItem,
  loudness,
  stationStatus,
];

export type { Face };
