import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { choiceBody, cycleSequence, displayView, nextChoice } from "../../src/actions/display.js";
import { ALL_ACTIONS, PLUGIN_UUID, uuidOf } from "../../src/actions/registry.js";
import type { Env } from "../../src/core/defs.js";
import { Http } from "../../src/core/http.js";
import { cleanHost, portOfListen, resolveTarget } from "../../src/core/settings.js";
import { nextClockTime, nextMinuteOfHour } from "../../src/core/time.js";
import { handlePiRequest } from "../../src/pi.js";
import { allText, ctxFor, FakeClock, faceSvg, liveModel, tagOf } from "./helpers.js";

describe("settings", () => {
  it("takes the key's box over the plugin's, and is unset without either", () => {
    expect(resolveTarget({}, {})).toBeNull();
    expect(resolveTarget({ host: "a.local" }, {})?.host).toBe("a.local");
    expect(resolveTarget({ host: "a.local" }, { host: "b.local", stationPort: 8302, station: "eve" })).toMatchObject({ host: "b.local", stationPort: 8302, libraryPort: 8300, station: "eve" });
    expect(resolveTarget({ host: "a", libraryPort: 9000 }, {})?.libraryPort).toBe(9000);
    expect(resolveTarget({ host: "a" }, {})?.stationPort).toBe(8301);
  });
  it("cleans what a person types", () => {
    expect(cleanHost("  http://hotclock.local/ ")).toBe("hotclock.local");
    expect(cleanHost("hotclock.local:8300")).toBe("hotclock.local");
    expect(cleanHost("192.0.2.30")).toBe("192.0.2.30");
    expect(cleanHost(undefined)).toBe("");
  });
  it("reads a station's port from its listen address", () => {
    expect(portOfListen("0.0.0.0:8302")).toBe(8302);
    expect(portOfListen("nonsense")).toBeNull();
  });
  it("bakes in no host of its own", () => {
    const src = readFileSync(new URL("../../src/core/settings.ts", import.meta.url), "utf8");
    expect(src).not.toMatch(/192\.168|\.ts\.net/i);
  });
});

describe("time", () => {
  it("finds the next minute of the hour", () => {
    const t = Date.parse("2026-10-07T18:43:10.000Z");
    expect(new Date(nextMinuteOfHour(0, t)).toISOString()).toBe("2026-10-07T19:00:00.000Z");
    expect(new Date(nextMinuteOfHour(45, t)).toISOString()).toBe("2026-10-07T18:45:00.000Z");
    // Too close to take: the next hour.
    expect(new Date(nextMinuteOfHour(43, Date.parse("2026-10-07T18:43:00.000Z"))).toISOString()).toBe("2026-10-07T19:43:00.000Z");
  });
  it("finds the next clock time in the box's zone, today or tomorrow", () => {
    const now = Date.parse("2026-10-07T22:00:00.000Z"); // 18:00 in New York
    expect(new Date(nextClockTime("19:30", now, "America/New_York")!).toISOString()).toBe("2026-10-07T23:30:00.000Z");
    expect(new Date(nextClockTime("17:30", now, "America/New_York")!).toISOString()).toBe("2026-10-08T21:30:00.000Z");
    expect(new Date(nextClockTime("19:30", now, "UTC")!).toISOString()).toBe("2026-10-08T19:30:00.000Z");
  });
  it("is right across a daylight-saving change", () => {
    // Clocks in New York go back on 2026-11-01 at 02:00; 20:00 that evening is EST (UTC-5).
    const now = Date.parse("2026-11-01T12:00:00.000Z");
    expect(new Date(nextClockTime("20:00", now, "America/New_York")!).toISOString()).toBe("2026-11-02T01:00:00.000Z");
    // The evening before it is still EDT (UTC-4).
    expect(new Date(nextClockTime("20:00", Date.parse("2026-10-31T12:00:00.000Z"), "America/New_York")!).toISOString()).toBe("2026-11-01T00:00:00.000Z");
  });
  it("refuses what is not a time", () => {
    expect(nextClockTime("25:00", 0, "UTC")).toBeNull();
    expect(nextClockTime("soon", 0, "UTC")).toBeNull();
  });
});

describe("Display View", () => {
  it("cycles through each station, split, rotate, and processing when wanted", () => {
    const seq = cycleSequence(["office", "eve"], true);
    expect(seq.map((s) => `${s.view}${s.station ? ":" + s.station : ""}`)).toEqual(["single:office", "single:eve", "split", "rotate", "processing"]);
    expect(cycleSequence(["office"], false).map((s) => s.view)).toEqual(["single", "split", "rotate"]);
  });
  it("steps to the next view and wraps", () => {
    const seq = cycleSequence(["office", "eve"], true);
    expect(nextChoice({ view: "single", station: "office" }, seq)).toEqual({ view: "single", station: "eve" });
    expect(nextChoice({ view: "single", station: "eve" }, seq)).toEqual({ view: "split" });
    expect(nextChoice({ view: "processing", station: null }, seq)).toEqual({ view: "single", station: "office" });
    expect(nextChoice(null, seq)).toEqual({ view: "single", station: "office" });
    expect(nextChoice({ view: "single", station: "gone" }, seq)).toEqual({ view: "single", station: "office" });
  });
  it("sends the station only for a single view", () => {
    expect(choiceBody({ view: "single", station: "eve" })).toEqual({ view: "single", station: "eve" });
    expect(choiceBody({ view: "processing" })).toEqual({ view: "processing" });
  });
  it("shows the current view, and lights when it is the one the key sets", () => {
    const clock = new FakeClock();
    const lib = { display: { view: "split", station: null, rotateS: 20 }, base: "http://box:8300" };
    const mk = (s: Record<string, string>) => {
      const x = ctxFor(clock, liveModel(clock), s);
      x.c.lib = lib as never;
      return x.c;
    };
    expect(allText(faceSvg(displayView, mk({})).svg)).toContain("SPLIT");
    expect(tagOf(faceSvg(displayView, mk({ view: "split" })).svg)).toBe("display:lit");
    expect(tagOf(faceSvg(displayView, mk({ view: "processing" })).svg)).toBe("display:set");
    expect(allText(faceSvg(displayView, mk({ view: "processing" })).svg)).toContain("PROCESSING");
  });
});

describe("property inspector requests", () => {
  const env = (routes: Record<string, unknown>, globals = { host: "box.local" } as Record<string, unknown>): { env: Env; urls: string[] } => {
    const urls: string[] = [];
    const http = new Http(async (url) => {
      urls.push(url);
      const hit = Object.entries(routes).find(([k]) => url.includes(k));
      return hit ? new Response(JSON.stringify(hit[1]), { status: 200 }) : new Response("{}", { status: 404 });
    });
    return { env: { http, globals: () => globals } as unknown as Env, urls };
  };

  it("lists the box's stations with their ports", async () => {
    const { env: e, urls } = env({ "/api/v1/stations": { stations: [{ id: "office", display_name: "Office", listen: "0.0.0.0:8301" }, { id: "eve", display_name: "Eve", listen: "0.0.0.0:8302" }] } });
    const r = await handlePiRequest(e, { cmd: "stations", tag: "1" });
    expect(r).toMatchObject({ ok: true, tag: "1", stations: [{ id: "office", name: "Office", port: 8301 }, { id: "eve", name: "Eve", port: 8302 }] });
    expect(urls[0]).toBe("http://box.local:8300/api/v1/stations");
  });
  it("uses the key's own host when it has one", async () => {
    const { env: e, urls } = env({ "/health": {} });
    await handlePiRequest(e, { cmd: "test", host: "other.local" });
    expect(urls[0]).toBe("http://other.local:8300/api/v1/stations");
  });
  it("Test connection asks the library for its stations, and names a station port for what it is", async () => {
    const ok = env({ "/api/v1/stations": { stations: [{ id: "office" }, { id: "eve" }] } });
    expect(await handlePiRequest(ok.env, { cmd: "test" })).toMatchObject({ ok: true, message: "Connected to the library: 2 stations (office, eve)." });
    const st = env({ "/health": { status: "ok", station: "office" } }, { host: "box.local", libraryPort: 8301 });
    const r = await handlePiRequest(st.env, { cmd: "test" });
    expect(r.ok).toBe(false);
    expect(r.error).toBe("8301 is a station port (station office), not the library. The library is usually 8300.");
    const none = env({});
    expect((await handlePiRequest(none.env, { cmd: "test" })).error).toMatch(/^No answer from box\.local:8300/);
  });
  it("lists playlists, items and a playlist's blocks", async () => {
    const { env: e } = env({
      "/api/v1/playlists/3": { entries: [{ type: "item", item: {} }, { type: "block", block: { id: 21, name: "Top ID", kind: "insert" } }] },
      "/api/v1/playlists": { playlists: [{ id: 3, name: "Eve", station_id: null }] },
      "/api/v1/items": { items: [{ id: 9, title: "Station ID", artist: "", duration_ms: 4200, category: "id" }] },
    });
    expect(await handlePiRequest(e, { cmd: "playlists" })).toMatchObject({ ok: true, playlists: [{ id: 3, name: "Eve" }] });
    expect(await handlePiRequest(e, { cmd: "items", q: "station" })).toMatchObject({ ok: true, items: [{ id: 9, title: "Station ID", durationMs: 4200 }] });
    expect(await handlePiRequest(e, { cmd: "blocks", playlistId: 3 })).toMatchObject({ ok: true, blocks: [{ id: 21, name: "Top ID" }] });
  });
  it("says so when no box address is set or the box does not answer", async () => {
    const none = await handlePiRequest(env({}, {}).env, { cmd: "stations" });
    expect(none.ok).toBe(false);
    expect(none.error).toMatch(/box address/i);
    const down = await handlePiRequest(env({}).env, { cmd: "stations" });
    expect(down.ok).toBe(false);
  });
});

describe("manifest", () => {
  const manifest = JSON.parse(readFileSync(new URL("../../com.neglectware.hotclock.sdPlugin/manifest.json", import.meta.url), "utf8"));
  it("is the Neglectware plugin and has the id the task asks for", () => {
    expect(manifest.UUID).toBe("com.neglectware.hotclock");
    expect(PLUGIN_UUID).toBe("com.neglectware.hotclock");
    expect(manifest.Author).toBe("Neglectware");
    expect(JSON.stringify(manifest)).not.toMatch(/org\.example/i);
  });
  it("lists exactly the actions the plugin registers", () => {
    expect(manifest.Actions.map((a: { UUID: string }) => a.UUID).sort()).toEqual(ALL_ACTIONS.map(uuidOf).sort());
  });
  it("gives every action a property inspector, a keypad controller and its icons", () => {
    for (const a of manifest.Actions) {
      expect(a.PropertyInspectorPath).toBe("ui/pi.html");
      expect(a.Controllers).toEqual(["Keypad"]);
      expect(a.Name.length).toBeGreaterThan(2);
    }
  });
});
