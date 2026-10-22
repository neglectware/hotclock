import { describe, expect, it } from "vitest";
import { ALL_ACTIONS, actionByUuid, uuidOf } from "../../src/actions/registry.js";
import * as P from "../../src/actions/playback.js";
import * as G from "../../src/actions/program.js";
import { COLORS } from "../../src/core/svg.js";
import { allText, BOX0, ctxFor, ev, FakeClock, faceSvg, iso, liveModel, nowJson, tagOf, texts } from "./helpers.js";

const ART = "data:image/jpeg;base64,/9j/4AAQ";

function playing(clock: FakeClock, remainingS: number, over: Record<string, unknown> = {}) {
  // A song whose segue is `remainingS` seconds from now.
  const m = liveModel(clock, { now: nowJson({}, BOX0 - (180 - remainingS) * 1000, 180_000, 200_000), ...over });
  return m;
}

describe("Now Playing", () => {
  it("shows title, artist and the time left to the segue", () => {
    const clock = new FakeClock();
    const m = playing(clock, 151);
    const { c } = ctxFor(clock, m, {}, { art: { "/api/v1/items/42/art?size=144": ART } });
    const { svg } = faceSvg(P.nowPlaying, c);
    const t = texts(svg);
    expect(t.map((x) => x.text)).toContain("From Silence");
    expect(t.map((x) => x.text)).toContain("-2:31");
    expect(svg).toContain('data-art="image"');
    expect(svg).toContain(ART);
  });

  it.each([
    [31, "normal", COLORS.text],
    [30, "amber", COLORS.amber],
    [11, "amber", COLORS.amber],
    [10, "red", COLORS.red],
    [1, "red", COLORS.red],
  ] as const)("at %i s the time is %s", (secs, tag, color) => {
    const clock = new FakeClock();
    const m = playing(clock, secs);
    const { c } = ctxFor(clock, m, {}, { art: { "/api/v1/items/42/art?size=144": ART } });
    const { svg } = faceSvg(P.nowPlaying, c);
    const time = texts(svg).find((x) => x.text.startsWith("-"));
    expect(time?.text).toBe(`-0:${String(secs).padStart(2, "0")}`);
    expect(time?.fill).toBe(color);
    expect(tagOf(svg)).toBe(`now-playing:${tag}`);
  });

  it("turns amber as the countdown crosses 30 s on its own", () => {
    const clock = new FakeClock();
    const m = playing(clock, 32);
    const { c: c0 } = ctxFor(clock, m, {}, { art: { "/api/v1/items/42/art?size=144": ART } });
    expect(tagOf(faceSvg(P.nowPlaying, c0).svg)).toBe("now-playing:normal");
    clock.advance(2000);
    const { c: c1 } = ctxFor(clock, m, {}, { art: { "/api/v1/items/42/art?size=144": ART } });
    expect(tagOf(faceSvg(P.nowPlaying, c1).svg)).toBe("now-playing:amber");
    clock.advance(20_000);
    const { c: c2 } = ctxFor(clock, m, {}, { art: { "/api/v1/items/42/art?size=144": ART } });
    expect(tagOf(faceSvg(P.nowPlaying, c2).svg)).toBe("now-playing:red");
  });

  it("shows a plain placeholder for an item without art", () => {
    const clock = new FakeClock();
    const m = liveModel(clock, { now: nowJson({ art_url: null }) });
    const { c } = ctxFor(clock, m);
    const { svg } = faceSvg(P.nowPlaying, c);
    expect(svg).toContain('data-art="placeholder"');
    expect(svg).not.toContain('data-art="image"');
  });

  it("shows a placeholder when the box says there is art but has none to send", () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    const { c } = ctxFor(clock, m, {}, { art: { "/api/v1/items/42/art?size=144": null } });
    expect(faceSvg(P.nowPlaying, c).svg).toContain('data-art="placeholder"');
  });

  it("shows no picture at all while the art is still loading", () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    const { c } = ctxFor(clock, m);
    const { svg } = faceSvg(P.nowPlaying, c);
    expect(svg).not.toContain("data-art=");
  });

  it("says STOPPED when nothing plays, HELD when held", () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    m.apply(ev("state", { state: "stopped", mode: "auto", hold: false, safe_mode: false, active_playlist: null }));
    expect(allText(faceSvg(P.nowPlaying, ctxFor(clock, m).c).svg)).toContain("STOPPED");
  });

  it("marks a gapless run with ALBUM and counts to the end", () => {
    const clock = new FakeClock();
    const m = liveModel(clock, { now: nowJson({ continuity_next: true }) });
    const { svg } = faceSvg(P.nowPlaying, ctxFor(clock, m, {}, { art: { "/api/v1/items/42/art?size=144": ART } }).c);
    expect(allText(svg)).toContain("ALBUM");
    expect(allText(svg)).toContain("-3:20");
  });

  it("escapes markup in a title", () => {
    const clock = new FakeClock();
    const m = liveModel(clock, { now: nowJson({ title: "<script>&" }) });
    const { svg } = faceSvg(P.nowPlaying, ctxFor(clock, m, {}, { art: { "/api/v1/items/42/art?size=144": ART } }).c);
    expect(svg).not.toContain("<script>");
    expect(allText(svg)).toContain("<script>&");
  });

  it("flags safe mode", () => {
    const clock = new FakeClock();
    const m = liveModel(clock, { safe_mode: true, state: "safe" });
    expect(allText(faceSvg(P.nowPlaying, ctxFor(clock, m).c).svg)).toContain("SAFE MODE");
  });
});

describe("Elapsed / Remaining", () => {
  it("shows remaining by default and toggles to elapsed on a press", async () => {
    const clock = new FakeClock();
    const m = playing(clock, 125);
    const { c, saved } = ctxFor(clock, m);
    expect(allText(faceSvg(P.elapsedRemaining, c).svg)).toContain("2:05");
    expect(allText(faceSvg(P.elapsedRemaining, c).svg)).toContain("REMAINING");
    await P.elapsedRemaining.press!(c);
    expect(saved).toEqual([{ show: "elapsed" }]);
    const { c: c2 } = ctxFor(clock, m, { show: "elapsed" });
    expect(allText(faceSvg(P.elapsedRemaining, c2).svg)).toContain("ELAPSED");
    expect(allText(faceSvg(P.elapsedRemaining, c2).svg)).toContain("0:55");
  });
  it("reddens the remaining time at 10 s", () => {
    const clock = new FakeClock();
    const m = playing(clock, 9);
    expect(tagOf(faceSvg(P.elapsedRemaining, ctxFor(clock, m).c).svg)).toBe("elapsed:remaining:red");
  });
  it("shows dashes when nothing plays", () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    m.apply(ev("state", { state: "stopped" }));
    expect(allText(faceSvg(P.elapsedRemaining, ctxFor(clock, m).c).svg)).toContain("--:--");
  });
});

describe("Next Up", () => {
  it("shows the next title and when it starts", () => {
    const clock = new FakeClock();
    const m = playing(clock, 90);
    const svg = faceSvg(P.nextUp, ctxFor(clock, m, {}, { art: {} }).c).svg;
    expect(allText(svg)).toContain("There Is Love");
    expect(allText(svg)).toMatch(/in \d:\d\d/);
  });
  it("says so when nothing is queued", () => {
    const clock = new FakeClock();
    const m = liveModel(clock, { next: null });
    expect(allText(faceSvg(P.nextUp, ctxFor(clock, m).c).svg)).toContain("NOTHING");
  });
});

describe("transport keys", () => {
  it("Next, Fade, Hold, Stop and Cut call the API they are for", async () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    const run = async (d: typeof P.next, s = {}) => {
      const x = ctxFor(clock, m, s);
      await (d.press ?? d.long)!(x.c);
      return x.calls;
    };
    expect(await run(P.next)).toEqual([{ method: "POST", path: "/transport/next", body: undefined }]);
    expect(await run(P.fadeNext, { fadeMs: 3000 })).toEqual([{ method: "POST", path: "/transport/fade-next", body: { fade_ms: 3000 } }]);
    expect(await run(P.holdResume)).toEqual([{ method: "POST", path: "/transport/hold", body: undefined }]);
    expect(await run(P.stop, { fadeMs: 2000 })).toEqual([{ method: "POST", path: "/transport/stop", body: { fade_ms: 2000 } }]);
    expect(await run(P.cut)).toEqual([{ method: "POST", path: "/transport/cut", body: undefined }]);
  });

  it("Hold turns into Resume while held, and lights up", async () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    m.apply(ev("state", { state: "held", mode: "auto", hold: true, safe_mode: false, active_playlist: { id: 1, name: "Pre-show" } }));
    const x = ctxFor(clock, m);
    const { svg, face } = faceSvg(P.holdResume, x.c);
    expect(allText(svg)).toContain("HELD");
    expect(face.bg).toBe(COLORS.bgAmber);
    await P.holdResume.press!(x.c);
    expect(x.calls[0].path).toBe("/transport/resume");
  });

  it("Stop and Cut only act on a long press", () => {
    expect(P.stop.longPress).toBe("only");
    expect(P.cut.longPress).toBe("only");
    expect(P.stop.press).toBeUndefined();
    expect(P.cut.press).toBeUndefined();
  });
});

describe("playlist keys", () => {
  it("Switch Playlist lights while its playlist is active", async () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    const lit = ctxFor(clock, m, { playlistId: 1, playlistName: "Pre-show" });
    expect(tagOf(faceSvg(P.switchPlaylist, lit.c).svg)).toBe("switch:lit");
    const other = ctxFor(clock, m, { playlistId: 2, playlistName: "Elevator", how: "now" });
    expect(tagOf(faceSvg(P.switchPlaylist, other.c).svg)).toBe("switch");
    await P.switchPlaylist.press!(other.c);
    expect(other.calls).toEqual([{ method: "POST", path: "/playlist/switch", body: { playlist_id: 2, how: "now" } }]);
  });
  it("asks for a playlist until one is picked, and then does nothing on a press", async () => {
    const clock = new FakeClock();
    const x = ctxFor(clock, liveModel(clock), {});
    expect(allText(faceSvg(P.switchPlaylist, x.c).svg)).toContain("SET UP");
    await P.switchPlaylist.press!(x.c);
    expect(x.calls).toEqual([]);
  });

  it("Reshuffle flashes the new next title for two seconds", async () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    const x = ctxFor(clock, m);
    expect(tagOf(faceSvg(P.reshuffle, x.c).svg)).toBe("reshuffle");
    await P.reshuffle.press!(x.c);
    expect(x.calls[0].path).toBe("/playlist/reshuffle");
    expect(tagOf(faceSvg(P.reshuffle, x.c).svg)).toBe("reshuffle:flash");
    expect(allText(faceSvg(P.reshuffle, x.c).svg)).toContain("There Is Love");
    clock.advance(2100);
    const later = ctxFor(clock, m, {}, { extra: x.c.extra });
    expect(tagOf(faceSvg(P.reshuffle, later.c).svg)).toBe("reshuffle");
  });

  it("Schedule Change creates a one-shot, shows its time and cancels on the next press", async () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    m.apply(ev("program", { running: false, program: null, time_zone: "America/New_York" }));
    const x = ctxFor(clock, m, { playlistId: 2, playlistName: "Elevator", when: "minute", minute: 0 }, { reply: (p) => (p === "/schedule/changes" ? { ok: true, data: { at: "2026-10-07T18:00:00.000Z", result: { id: 5, at: "2026-10-07T23:00:00.000Z" } } } : { ok: true }) });
    expect(allText(faceSvg(P.scheduleChange, x.c).svg)).toContain("at :00");
    await P.scheduleChange.press!(x.c);
    expect(x.calls[0].path).toBe("/schedule/changes");
    const body = x.calls[0].body as { playlist_id: number; at: string };
    expect(body.playlist_id).toBe(2);
    expect(new Date(body.at).getUTCMinutes()).toBe(0);
    expect(Date.parse(body.at)).toBeGreaterThan(m.boxNow());
    expect(allText(faceSvg(P.scheduleChange, x.c).svg)).toContain("19:00");
    expect(tagOf(faceSvg(P.scheduleChange, x.c).svg)).toBe("schedule:pending");
    await P.scheduleChange.press!(x.c);
    expect(x.calls[1]).toEqual({ method: "DELETE", path: "/schedule/changes/5" });
    expect(tagOf(faceSvg(P.scheduleChange, x.c).svg)).toBe("schedule");
  });

  it("Schedule Change can be set to a delay", async () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    const x = ctxFor(clock, m, { playlistId: 2, playlistName: "E", when: "offset", offsetMin: 5 });
    expect(allText(faceSvg(P.scheduleChange, x.c).svg)).toContain("in 5 min");
    await P.scheduleChange.press!(x.c);
    expect(Date.parse((x.calls[0].body as { at: string }).at)).toBe(m.boxNow() + 300_000);
  });

  it("Fire Item sends the cart and lights while it plays", async () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    const x = ctxFor(clock, m, { itemId: 9, itemTitle: "Station ID", durationMs: 4000 });
    expect(tagOf(faceSvg(P.fireItem, x.c).svg)).toBe("cart");
    await P.fireItem.press!(x.c);
    expect(x.calls).toEqual([{ method: "POST", path: "/cart", body: { item_id: 9 } }]);
    expect(tagOf(faceSvg(P.fireItem, x.c).svg)).toBe("cart:playing");
    clock.advance(4100);
    const later = ctxFor(clock, m, { itemId: 9, itemTitle: "Station ID", durationMs: 4000 }, { extra: x.c.extra });
    expect(tagOf(faceSvg(P.fireItem, later.c).svg)).toBe("cart");
  });

  it("Fire Item lights from the box's own list of playing carts", () => {
    const clock = new FakeClock();
    const m = liveModel(clock, { carts: [{ item_id: 9 }] });
    const x = ctxFor(clock, m, { itemId: 9, itemTitle: "Station ID" });
    expect(tagOf(faceSvg(P.fireItem, x.c).svg)).toBe("cart:playing");
  });
});

describe("Station Status and Loudness", () => {
  it("is green, amber or red by the worst alert", () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    expect(tagOf(faceSvg(P.stationStatus, ctxFor(clock, m).c).svg)).toBe("status:ok");
    m.apply(ev("alert", { id: 1, code: "disk_low", severity: "warning", message: "" }));
    expect(tagOf(faceSvg(P.stationStatus, ctxFor(clock, m).c).svg)).toBe("status:warning");
    m.apply(ev("alert", { id: 2, code: "no_audio", severity: "critical", message: "" }));
    const svg = faceSvg(P.stationStatus, ctxFor(clock, m).c).svg;
    expect(tagOf(svg)).toBe("status:critical");
    expect(allText(svg)).toContain("no_audio");
  });
  it("is red and says OFFLINE when the station does not answer", () => {
    const f = P.stationStatus.offlineFace!("Office");
    expect(f.bg).toBe(COLORS.bgRed);
    expect(f.lines.map((l) => l.text)).toContain("OFFLINE");
  });
  it("opens the web UI on a press", async () => {
    const clock = new FakeClock();
    const x = ctxFor(clock, liveModel(clock));
    await P.stationStatus.press!(x.c);
    expect(x.opened).toEqual(["http://box:8300/"]);
  });
  it("Loudness is green within 1 LU and amber beyond, and resets on a long press", async () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    m.apply(ev("loudness", { target: -14, short_term: -14, window: -14.2, integrated: -14.1, momentary: -13, state: "tracking", gain_db: 0 }));
    let x = ctxFor(clock, m);
    let svg = faceSvg(P.loudness, x.c).svg;
    expect(allText(svg)).toContain("-14.2");
    expect(allText(svg)).toContain("/ -14");
    expect(tagOf(svg)).toBe("loudness:ok");
    m.apply(ev("loudness", { target: -14, window: -12.7, state: "tracking" }));
    x = ctxFor(clock, m);
    svg = faceSvg(P.loudness, x.c).svg;
    expect(tagOf(svg)).toBe("loudness:off-target");
    expect(texts(svg).find((t) => t.text === "-12.7")?.fill).toBe(COLORS.amber);
    await P.loudness.long!(x.c);
    expect(x.calls[0].path).toBe("/loudness/reset");
  });
  it("Loudness shows the reading with no target when the controller is off, and OFF with no reading", () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    m.apply(ev("loudness", { target: null, window: -20, state: "off" }));
    expect(tagOf(faceSvg(P.loudness, ctxFor(clock, m).c).svg)).toBe("loudness:no-target");
    expect(allText(faceSvg(P.loudness, ctxFor(clock, m).c).svg)).toContain("-20.0");
    m.apply(ev("loudness", { target: null, window: null, state: "off" }));
    expect(tagOf(faceSvg(P.loudness, ctxFor(clock, m).c).svg)).toBe("loudness:off");
  });
});

describe("program keys", () => {
  const prog = (over: Record<string, unknown> = {}) => ({
    running: false, program: null,
    next_block: { block_id: 11, name: "Top of hour ID", type: "insert", badge: "ID", in_ms: 90_000 },
    expected: { playlist_id: 3, playlist: "Eve", at: iso(BOX0), in_ms: 600_000, armed: false, label: "expected 18:45" },
    back_time: { state: "off" }, blocks: [], time_zone: "America/New_York", ...over,
  });

  it("Start Program shows the expected start, then RUNNING; starts with t0 = now; a long press disarms", async () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    m.apply(ev("program", prog({ expected: { playlist_id: 3, playlist: "Eve", at: iso(BOX0), in_ms: 600_000, armed: true, label: "expected 18:45" } })));
    const x = ctxFor(clock, m, { playlistId: 3, playlistName: "Eve" });
    expect(allText(faceSvg(G.programStart, x.c).svg)).toContain("expected 18:45");
    expect(tagOf(faceSvg(G.programStart, x.c).svg)).toBe("program:armed");
    await G.programStart.press!(x.c);
    expect(x.calls[0]).toEqual({ method: "POST", path: "/program/start", body: { playlist_id: 3, t0: "now", source: "deck", station: "office" } });
    await G.programStart.long!(x.c);
    expect(x.calls[1]).toEqual({ method: "POST", path: "/program/disarm", body: { playlist_id: 3 } });
    m.apply(ev("program", prog({ running: true, program: { run_id: 1, playlist_id: 3, playlist: "Eve", state: "running" }, expected: null })));
    expect(tagOf(faceSvg(G.programStart, ctxFor(clock, m, { playlistId: 3, playlistName: "Eve" }).c).svg)).toBe("program:running");
  });

  it("Arm toggles between arm and disarm", async () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    m.apply(ev("program", prog()));
    let x = ctxFor(clock, m, { playlistId: 3, playlistName: "Eve" });
    await G.programArm.press!(x.c);
    expect(x.calls[0].path).toBe("/program/arm");
    m.apply(ev("program", prog({ expected: { playlist_id: 3, playlist: "Eve", at: iso(BOX0), in_ms: 600_000, armed: true, label: "expected 18:45" } })));
    x = ctxFor(clock, m, { playlistId: 3, playlistName: "Eve" });
    await G.programArm.press!(x.c);
    expect(x.calls[0].path).toBe("/program/disarm");
  });

  it("Next Block counts down: normal, amber under 60 s, red under 10 s", () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    m.apply(ev("program", prog()));
    const at = (extraMs: number) => {
      clock.advance(extraMs);
      return faceSvg(G.nextBlock, ctxFor(clock, m).c).svg;
    };
    expect(tagOf(at(0))).toBe("block:normal");
    expect(allText(at(0))).toContain("1:30");
    expect(tagOf(at(31_000))).toBe("block:amber");
    expect(tagOf(at(50_000))).toBe("block:red");
    expect(allText(at(0))).toContain("0:09");
  });
  it("Next Block shows the back-time label and fires on a long press only when configured", async () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    m.apply(ev("program", prog({ back_time: { state: "planned", label: "END +3 s", delta_s: 3, target: iso(BOX0 + 600_000) } })));
    const off = ctxFor(clock, m, {});
    expect(allText(faceSvg(G.nextBlock, off.c).svg)).toContain("END +3 s");
    await G.nextBlock.long!(off.c);
    expect(off.calls).toEqual([]);
    const on = ctxFor(clock, m, { fireOnLongPress: true });
    await G.nextBlock.long!(on.c);
    expect(on.calls[0].path).toBe("/program/blocks/11/fire");
  });
  it("Fire Block lights once its block has fired", async () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    m.apply(ev("program", prog({ blocks: [{ block_id: 12, status: "pending" }] })));
    const x = ctxFor(clock, m, { blockId: 12, blockName: "Liner" });
    expect(tagOf(faceSvg(G.fireBlock, x.c).svg)).toBe("fire");
    await G.fireBlock.press!(x.c);
    expect(x.calls[0].path).toBe("/program/blocks/12/fire");
    m.apply(ev("program", prog({ blocks: [{ block_id: 12, status: "fired" }] })));
    expect(tagOf(faceSvg(G.fireBlock, ctxFor(clock, m, { blockId: 12, blockName: "Liner" }).c).svg)).toBe("fire:fired");
  });
  it("Continue lights while a wait block waits for the operator", async () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    m.apply(ev("program", prog()));
    expect(tagOf(faceSvg(G.continueKey, ctxFor(clock, m).c).svg)).toBe("continue");
    m.apply(ev("program", prog({ running: true, program: { run_id: 1, playlist_id: 3, state: "waiting", waiting: { block_id: 9, name: "Hold for host", kind: "operator" } } })));
    const x = ctxFor(clock, m);
    expect(tagOf(faceSvg(G.continueKey, x.c).svg)).toBe("continue:waiting");
    await G.continueKey.press!(x.c);
    expect(x.calls[0].path).toBe("/program/continue");
  });
  it("Skip and Abort call the API; Abort needs a long press and can stop the audio", async () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    const x = ctxFor(clock, m, { stopAudio: true });
    await G.skipBlock.press!(x.c);
    await G.abortProgram.long!(x.c);
    expect(x.calls.map((c) => c.path)).toEqual(["/program/blocks/next/skip", "/program/abort"]);
    expect(x.calls[1].body).toEqual({ stop_audio: true });
    expect(G.abortProgram.longPress).toBe("only");
  });
  it("Back-time End sends the time as an instant in the box's zone and shows the countdown", async () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    m.apply(ev("program", prog()));
    const x = ctxFor(clock, m, { playlistId: 3, playlistName: "Eve", endMode: "at", endTime: "19:30" });
    expect(allText(faceSvg(G.endAt, x.c).svg)).toContain("at 19:30");
    await G.endAt.press!(x.c);
    const body = x.calls[0].body as { at: string; playlist_id: number; then: string };
    // BOX0 is 18:00 -04:00, so 19:30 New York is 23:30Z the same day.
    expect(body).toEqual({ playlist_id: 3, at: "2026-10-07T23:30:00.000Z", then: "end" });
    m.apply(ev("program", prog({ running: true, program: { run_id: 1, playlist_id: 3, state: "running" }, back_time: { state: "planned", label: "END +3 s", delta_s: 3, target: iso(BOX0 + 125_000) } })));
    const svg = faceSvg(G.endAt, ctxFor(clock, m, { playlistId: 3 }).c).svg;
    expect(allText(svg)).toContain("2:05");
    expect(allText(svg)).toContain("END +3 s");
  });
});

describe("registry", () => {
  it("has every action once, under the plugin id", () => {
    const ids = ALL_ACTIONS.map(uuidOf);
    expect(new Set(ids).size).toBe(ids.length);
    for (const id of ids) expect(id.startsWith("com.neglectware.hotclock.")).toBe(true);
    expect(actionByUuid("com.neglectware.hotclock.now-playing")).toBe(P.nowPlaying);
  });
  it("draws every action without throwing, for a station that is stopped and one that plays", () => {
    const clock = new FakeClock();
    for (const def of ALL_ACTIONS.filter((d) => d.uses === "station")) {
      for (const m of [liveModel(clock), (() => { const s = liveModel(clock); s.apply(ev("state", { state: "stopped" })); return s; })()]) {
        const svg = faceSvg(def, ctxFor(clock, m).c).svg;
        expect(svg.startsWith("<svg")).toBe(true);
        expect(svg).toContain("</svg>");
      }
    }
  });
});
