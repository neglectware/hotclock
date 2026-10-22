import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import * as P from "../../src/actions/playback.js";
import * as G from "../../src/actions/program.js";
import { KeyController, LONG_PRESS_MS, type KeyHandle } from "../../src/core/controller.js";
import type { Env } from "../../src/core/defs.js";
import { Http } from "../../src/core/http.js";
import { StationModel } from "../../src/core/model.js";
import { allText, FakeClock, liveModel, nowJson, snapshot, texts } from "./helpers.js";

/** A station connection that is only a model and a list of listeners. */
class FakeConn {
  base = "http://box:8301";
  refs = 0;
  listeners = new Set<() => void>();
  constructor(public model: StationModel) {}
  subscribe(l: () => void) {
    this.listeners.add(l);
    return () => this.listeners.delete(l);
  }
  changed() {
    for (const l of [...this.listeners]) l();
  }
  artFor() {
    return null;
  }
}

class FakeHub {
  conns = new Map<string, FakeConn>();
  constructor(private make: () => FakeConn) {}
  station(host: string, port: number) {
    const k = `${host}:${port}`;
    if (!this.conns.has(k)) this.conns.set(k, this.make());
    this.conns.get(k)!.refs++;
    return this.conns.get(k)!;
  }
  releaseStation(c: FakeConn) {
    c.refs--;
  }
  library() {
    throw new Error("not used");
  }
  releaseLibrary() {}
}

interface Rig {
  clock: FakeClock;
  conn: FakeConn;
  fetched: { method: string; url: string; body?: string; headers?: Record<string, string> }[];
  images: string[];
  oks: number;
  alerts: number;
  key: KeyHandle;
  env: Env;
}

function rig(opts: { online?: boolean } = {}): Rig {
  const clock = new FakeClock();
  const model = new StationModel(clock.now);
  if (opts.online !== false) {
    model.setOpen();
    model.apply(snapshot());
  }
  const conn = new FakeConn(model);
  const fetched: Rig["fetched"] = [];
  const http = new Http(async (url, init) => {
    fetched.push({ method: init?.method ?? "GET", url, body: init?.body as string | undefined, headers: init?.headers as Record<string, string> });
    return new Response("{}", { status: 202, headers: { "content-type": "application/json" } });
  });
  const r = { clock, conn, fetched, images: [] as string[], oks: 0, alerts: 0 } as Rig;
  r.key = { setImage: (u) => r.images.push(u as string), showOk: () => void r.oks++, showAlert: () => void r.alerts++, setSettings: () => undefined };
  r.env = { hub: new FakeHub(() => conn) as never, http, clock: clock.now, globals: () => ({ host: "box" }), openUrl: () => undefined, log: () => undefined };
  return r;
}

beforeEach(() => vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval"] }));
afterEach(() => vi.useRealTimers());

const settle = async () => {
  await vi.advanceTimersByTimeAsync(0);
};

describe("a key's presses", () => {
  it("sends a short press to the API, with a JSON content type", async () => {
    const r = rig();
    const k = new KeyController(P.next, r.key, r.env);
    k.setSettings({ station: "office" });
    k.keyDown();
    k.keyUp();
    await settle();
    expect(r.fetched).toHaveLength(1);
    expect(r.fetched[0].method).toBe("POST");
    expect(r.fetched[0].url).toBe("http://box:8301/api/v1/transport/next");
    expect(r.fetched[0].headers?.["Content-Type"]).toBe("application/json");
    expect(r.oks).toBe(1);
  });

  it("a station key with no station chosen shows NO STATION and sends nothing (never the first station)", async () => {
    for (const def of [P.next, G.programStart]) {
      const r = rig();
      const k = new KeyController(def, r.key, r.env);
      k.setSettings({ playlistId: 3 });
      const text = allText(decode(r.images.at(-1)!));
      expect(text).toContain("NO");
      expect(text).toContain("STATION");
      k.keyDown();
      k.keyUp();
      await settle();
      expect(r.fetched).toEqual([]);
      expect(r.alerts).toBe(1);
      // a station picked: it works
      k.setSettings({ playlistId: 3, station: "eve", stationPort: 8302 });
      r.clock.advance(1500);
      await vi.advanceTimersByTimeAsync(1500);
      expect(allText(decode(r.images.at(-1)!))).not.toContain("STATION");
    }
  });

  it("ignores a press while the station is offline, and never fires it later", async () => {
    const r = rig({ online: false });
    const k = new KeyController(P.next, r.key, r.env);
    k.setSettings({ station: "office" });
    expect(allText(decode(r.images.at(-1)!))).toContain("OFFLINE");
    k.keyDown();
    k.keyUp();
    await settle();
    expect(r.fetched).toEqual([]);
    expect(r.alerts).toBe(1);
    // The station comes back: nothing was queued.
    r.conn.model.setOpen();
    r.conn.model.apply(snapshot());
    r.conn.changed();
    await vi.advanceTimersByTimeAsync(5000);
    expect(r.fetched).toEqual([]);
  });

  it("ignores a long press that completes after the station went offline", async () => {
    const r = rig();
    const k = new KeyController(P.stop, r.key, r.env);
    k.setSettings({ station: "office" });
    k.keyDown();
    await vi.advanceTimersByTimeAsync(200);
    r.conn.model.reset();
    await vi.advanceTimersByTimeAsync(LONG_PRESS_MS);
    k.keyUp();
    await settle();
    expect(r.fetched).toEqual([]);
  });

  it("Stop does nothing on a short press (it shows HOLD) and stops on a long one", async () => {
    const r = rig();
    const k = new KeyController(P.stop, r.key, r.env);
    k.setSettings({ station: "office" });
    k.keyDown();
    await vi.advanceTimersByTimeAsync(100);
    k.keyUp();
    r.clock.advance(600);
    await vi.advanceTimersByTimeAsync(600);
    expect(r.fetched).toEqual([]);
    expect(allText(decode(r.images.at(-1)!))).toContain("HOLD");

    k.keyDown();
    await vi.advanceTimersByTimeAsync(LONG_PRESS_MS + 10);
    // It fires while the key is still down.
    await settle();
    expect(r.fetched).toHaveLength(1);
    expect(r.fetched[0].url).toBe("http://box:8301/api/v1/transport/stop");
    k.keyUp();
    await settle();
    expect(r.fetched).toHaveLength(1);
  });

  it("an extra long press (Loudness) does its own thing, and a short press does nothing for a key with none", async () => {
    const r = rig();
    const k = new KeyController(P.loudness, r.key, r.env);
    k.setSettings({ station: "office" });
    k.keyDown();
    await vi.advanceTimersByTimeAsync(LONG_PRESS_MS + 10);
    k.keyUp();
    await settle();
    expect(r.fetched.map((f) => f.url)).toEqual(["http://box:8301/api/v1/loudness/reset"]);
  });

  it("shows the alert on the key when the box refuses", async () => {
    const r = rig();
    r.env.http = new Http(async () => new Response(JSON.stringify({ error: { code: "refused", message: "nothing to skip" } }), { status: 409 }));
    const k = new KeyController(G.skipBlock, r.key, r.env);
    k.setSettings({ station: "office" });
    k.keyDown();
    k.keyUp();
    await settle();
    expect(r.alerts).toBe(1);
    expect(r.oks).toBe(0);
  });

  it("asks for the box address when none is set", () => {
    const r = rig();
    r.env.globals = () => ({});
    const k = new KeyController(P.next, r.key, r.env);
    k.setSettings({ station: "office" });
    expect(allText(decode(r.images.at(-1)!))).toContain("SET UP");
    k.keyDown();
    k.keyUp();
    expect(r.fetched).toEqual([]);
  });

  it("a key's own host overrides the plugin's", () => {
    const r = rig();
    const k = new KeyController(P.next, r.key, r.env);
    k.setSettings({ host: "other.local", stationPort: 8302 });
    expect([...(r.env.hub as unknown as FakeHub).conns.keys()]).toEqual(["other.local:8302"]);
  });
});

describe("the countdown on a Now Playing key", () => {
  it("agrees with the box's timing events within 0.5 s over a whole track and repaints at most twice a second", async () => {
    const r = rig();
    // A 60 s song starting now.
    const BOX0 = Date.parse("2026-10-07T18:00:00.000-04:00");
    const m = r.conn.model;
    m.apply(snapshot({ now: nowJson({ art_url: null }, BOX0, 55_000, 60_000), next: null }, BOX0));
    const k = new KeyController(P.nowPlaying, r.key, r.env);
    k.setSettings({ station: "office" });
    let worst = 0;
    const shown = (): number | null => {
      const t = texts(decode(r.images.at(-1)!)).find((x) => /^-\d+:\d\d$/.test(x.text));
      if (!t) return null;
      const [mm, ss] = t.text.slice(1).split(":").map(Number);
      return (mm * 60 + ss) * 1000;
    };
    let tick = 0;
    for (let ms = 0; ms < 55_000; ms += 250) {
      r.clock.advance(250);
      await vi.advanceTimersByTimeAsync(250);
      tick++;
      // The box's 1 Hz timing event, sent at a slightly jittery moment: 40 ms late.
      if (tick % 4 === 0) {
        const elapsed = tick * 250;
        m.apply({ type: "timing", at: new Date(BOX0 + elapsed).toISOString(), data: { entry_id: 7, elapsed_ms: elapsed, remaining_to_segue_ms: 55_000 - elapsed, remaining_to_end_ms: 60_000 - elapsed, next: null } });
      }
      k.refresh();
      const truth = 55_000 - tick * 250;
      const s = shown();
      if (s !== null) worst = Math.max(worst, Math.abs(s - truth));
    }
    expect(worst).toBeLessThanOrEqual(500);
    // 55 s of drawing at 4 Hz: about one new picture a second, never more than two.
    expect(k.painter.sent).toBeLessThanOrEqual(55 * 2);
    expect(k.painter.sent).toBeGreaterThan(40);
  });
});

function decode(uri: string): string {
  return decodeURIComponent(uri.replace("data:image/svg+xml;charset=utf8,", ""));
}
