import { afterEach, describe, expect, it } from "vitest";
import WebSocket from "ws";
import { Hub } from "../../src/core/hub.js";
import { Http } from "../../src/core/http.js";
import { FakeBox } from "./fakebox.js";
import { ev, snapshot } from "./helpers.js";

const clock = () => performance.now();
const until = async (f: () => boolean, ms = 3000, what = "condition") => {
  const t = Date.now();
  while (!f()) {
    if (Date.now() - t > ms) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 10));
  }
};

const made: { hub: Hub; box: FakeBox }[] = [];
afterEach(async () => {
  for (const m of made.splice(0)) {
    m.hub.dispose();
    await m.box.stop().catch(() => undefined);
  }
});

async function setup(opts: { backoff?: number[]; deadAfterMs?: number; pingEveryMs?: number } = {}) {
  const box = await new FakeBox().start();
  const hub = new Hub(
    {
      http: new Http(),
      clock,
      makeSocket: (u) => new WebSocket(u) as never,
      backoff: opts.backoff ?? [50, 100, 200],
      pingEveryMs: opts.pingEveryMs ?? 100,
      deadAfterMs: opts.deadAfterMs ?? 450,
      connectTimeoutMs: 500,
    },
    50,
  );
  made.push({ hub, box });
  return { box, hub };
}

describe("Hub with a fake box", () => {
  it("connects, takes the snapshot and the events, and shares one socket among keys", async () => {
    const { box, hub } = await setup();
    const a = hub.station("127.0.0.1", box.port);
    const b = hub.station("127.0.0.1", box.port);
    expect(a).toBe(b);
    await until(() => a.model.ready, 3000, "snapshot");
    expect(a.model.now?.title).toBe("From Silence");
    expect(box.connections).toBe(1);
    box.send(ev("state", { state: "playing", mode: "auto", hold: true, safe_mode: false, active_playlist: { id: 1, name: "Pre-show" } }));
    await until(() => a.model.hold, 1000, "state event");
  });

  it("fetches the art by its URL and caches it", async () => {
    const { box, hub } = await setup();
    const c = hub.station("127.0.0.1", box.port);
    await until(() => c.model.ready);
    await until(() => c.artFor(c.model.now?.artUrl) !== undefined, 2000, "art");
    const uri = c.artFor(c.model.now?.artUrl);
    expect(uri).toMatch(/^data:image\/jpeg;base64,/);
    expect(Buffer.from(uri!.split(",")[1], "base64").toString()).toBe("fakejpeg");
    const fetches = box.requests.filter((r) => r.url.includes("/art")).length;
    c.artFor(c.model.now?.artUrl);
    c.artFor(c.model.now?.artUrl);
    expect(box.requests.filter((r) => r.url.includes("/art")).length).toBe(fetches);
  });

  it("asks for the program state after each snapshot", async () => {
    const { box, hub } = await setup();
    box.program = { running: true, program: { run_id: 1, playlist_id: 3, playlist: "Eve", state: "running" }, next_block: null, expected: null, back_time: { state: "off" }, blocks: [] };
    const c = hub.station("127.0.0.1", box.port);
    await until(() => c.model.program?.running === true, 2000, "program state");
    expect(c.model.program?.playlist).toBe("Eve");
  });

  it("goes OFFLINE when the box stops and comes back by itself, with no stale now-playing in between", async () => {
    const { box, hub } = await setup({ backoff: [50, 100] });
    const port = box.port;
    const c = hub.station("127.0.0.1", port);
    await until(() => c.model.ready);
    expect(c.model.now).not.toBeNull();
    await box.stop();
    await until(() => !c.model.online, 2000, "offline");
    expect(c.model.now).toBeNull();
    expect(c.model.ready).toBe(false);
    // The station restarts on the same port, playing something else.
    const box2 = new FakeBox();
    box2.snapshotData = { now: { ...snapshot().data.now, title: "After Restart" } };
    await box2.start(port);
    made.push({ hub: new Hub({ http: new Http(), clock, makeSocket: (u) => new WebSocket(u) as never }), box: box2 });
    await until(() => c.model.ready, 3000, "reconnect");
    expect(c.model.now?.title).toBe("After Restart");
  });

  it("notices a box that goes silent (network gone) and shows OFFLINE", async () => {
    const { box, hub } = await setup({ pingEveryMs: 100, deadAfterMs: 400 });
    const c = hub.station("127.0.0.1", box.port);
    await until(() => c.model.ready);
    box.goSilent();
    await until(() => !c.model.online, 2000, "silent offline");
    expect(c.model.now).toBeNull();
  });

  it("starts over on wake: nothing from before the sleep stays on a key", async () => {
    const { box, hub } = await setup();
    const c = hub.station("127.0.0.1", box.port);
    await until(() => c.model.ready);
    // While the Mac slept the box moved on to another song, which this connection never heard of.
    box.snapshotData = { now: { ...snapshot().data.now, title: "Slept Through" } };
    hub.wake();
    expect(c.model.now).toBeNull();
    expect(c.model.ready).toBe(false);
    await until(() => c.model.ready, 2000, "reconnect after wake");
    expect(c.model.now?.title).toBe("Slept Through");
  });

  it("closes a connection a little after its last key leaves, and not before", async () => {
    const { box, hub } = await setup();
    const c = hub.station("127.0.0.1", box.port);
    await until(() => c.model.ready);
    hub.releaseStation(c);
    await new Promise((r) => setTimeout(r, 20));
    expect(box.sockets.size).toBe(1);
    const again = hub.station("127.0.0.1", box.port);
    expect(again).toBe(c);
    hub.releaseStation(again);
    await until(() => box.sockets.size === 0, 2000, "closed");
    expect(hub.stationCount).toBe(0);
  });
});
