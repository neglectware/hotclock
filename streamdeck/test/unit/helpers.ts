import type { ActionDef, Ctx, Env } from "../../src/core/defs.js";
import { Hub, type StationConn } from "../../src/core/hub.js";
import { Http } from "../../src/core/http.js";
import { StationModel, type Envelope } from "../../src/core/model.js";
import type { KeySettings, Target } from "../../src/core/settings.js";
import { renderFace } from "../../src/core/svg.js";
import type { Face } from "../../src/core/svg.js";

/** A clock the test moves by hand. */
export class FakeClock {
  t = 1_000_000;
  now = (): number => this.t;
  advance(ms: number): void {
    this.t += ms;
  }
}

/** The text nodes of a rendered key: `{text, fill}` in drawing order. */
export function texts(svg: string): { text: string; fill: string; size: number }[] {
  const out: { text: string; fill: string; size: number }[] = [];
  const re = /<text [^>]*?font-size="([\d.]+)"[^>]*?fill="([^"]+)"[^>]*>([^<]*)<\/text>/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(svg))) out.push({ size: Number(m[1]), fill: m[2], text: m[3].replace(/&amp;/g, "&").replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&quot;/g, '"') });
  return out;
}

export const allText = (svg: string): string => texts(svg).map((t) => t.text).join(" | ");
export const tagOf = (svg: string): string => /data-face="([^"]*)"/.exec(svg)?.[1] ?? "";

const BOX_ZERO = Date.parse("2026-10-07T18:00:00.000-04:00");

/** An ISO time of the box at `boxMs`. */
export const iso = (boxMs: number): string => new Date(boxMs).toISOString().replace("Z", "+00:00");

export function nowJson(over: Record<string, unknown> = {}, startedBox = BOX_ZERO, segueAfterMs = 180_000, endAfterMs = 200_000): Record<string, unknown> {
  return {
    entry_id: 7, item_id: 42, title: "From Silence", artist: "Demo Artists", album: "Drift", category: "music", kind: "song",
    art_url: "/api/v1/items/42/art?size=144", started_at: iso(startedBox), cue_in_ms: 0,
    segue_at: iso(startedBox + segueAfterMs), ends_at: iso(startedBox + endAfterMs), duration_ms: endAfterMs, origin: "playlist", run_id: null,
    continuity_next: false, emergency: false, ...over,
  };
}

export function snapshot(over: Record<string, unknown> = {}, boxAt = BOX_ZERO): Envelope {
  return {
    v: 1, type: "snapshot", station: "office", seq: 1, at: iso(boxAt),
    data: {
      station: "office", display_name: "Office", state: "playing", mode: "auto", hold: false, safe_mode: false, health: "ok",
      now: nowJson(), next: { item_id: 43, title: "There Is Love", artist: "Demo Artists", album: "Drift", art_url: "/api/v1/items/43/art?size=144", starts_at: iso(BOX_ZERO + 180_000) },
      queue: [], alerts: [], active_playlist: { id: 1, name: "Pre-show" }, at: iso(boxAt), ...over,
    },
  };
}

export const timing = (elapsed: number, toSegue: number | null, toEnd: number, boxAt: number, entry = 7): Envelope => ({
  v: 1, type: "timing", station: "office", seq: 2, at: iso(boxAt),
  data: { entry_id: entry, elapsed_ms: elapsed, remaining_to_segue_ms: toSegue, remaining_to_end_ms: toEnd, next: null },
});

export const ev = (type: string, data: unknown, boxAt = BOX_ZERO): Envelope => ({ v: 1, type, station: "office", seq: 3, at: iso(boxAt), data });

export const BOX0 = BOX_ZERO;

/** A model that is online with a snapshot applied. */
export function liveModel(clock: FakeClock, over: Record<string, unknown> = {}): StationModel {
  const m = new StationModel(clock.now);
  m.setOpen();
  m.apply(snapshot(over));
  return m;
}

export const TARGET: Target = { host: "box", stationPort: 8301, libraryPort: 8300, station: "office", stationChosen: true };

export interface Calls { method: string; path: string; body?: unknown }

/** A context for an action's `face` and `press`, with its station connection stubbed. */
export function ctxFor(
  clock: FakeClock,
  model: StationModel,
  s: KeySettings = {},
  opts: { art?: Record<string, string | null>; extra?: Record<string, unknown>; reply?: (path: string) => { ok: boolean; status?: number; data?: any } } = {}, // eslint-disable-line @typescript-eslint/no-explicit-any
): { c: Ctx; calls: Calls[]; saved: KeySettings[]; opened: string[] } {
  const calls: Calls[] = [];
  const saved: KeySettings[] = [];
  const opened: string[] = [];
  const reply = (path: string) => ({ status: 200, data: {}, ...(opts.reply?.(path) ?? { ok: true }) });
  const conn = {
    base: "http://box:8301",
    model,
    artFor: (u: string | null | undefined) => (u ? (opts.art?.[u] === undefined ? undefined : opts.art[u]) : null),
  } as unknown as StationConn;
  const c: Ctx = {
    s,
    t: TARGET,
    conn,
    m: model,
    lib: null,
    env: { openUrl: (u: string) => opened.push(u), http: new Http(), clock: clock.now, globals: () => ({}), log: () => undefined, hub: null as unknown as Hub } as Env,
    nowMs: clock.now(),
    extra: { ...(opts.extra ?? {}) },
    post: async (path, body) => {
      calls.push({ method: "POST", path, body });
      return reply(path) as never;
    },
    del: async (path) => {
      calls.push({ method: "DELETE", path });
      return reply(path) as never;
    },
    save: (p) => saved.push(p),
    redraw: () => undefined,
  };
  return { c, calls, saved, opened };
}

export function faceSvg(def: ActionDef, c: Ctx): { svg: string; face: Face } {
  const face = def.face(c);
  return { svg: renderFace(face), face };
}
