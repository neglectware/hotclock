import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { Link, type WsLike } from "../../src/core/link.js";

/** A socket the test drives by hand. */
class FakeSocket implements WsLike {
  static all: FakeSocket[] = [];
  handlers: Record<string, ((a?: unknown) => void)[]> = {};
  pings = 0;
  terminated = false;
  sent: string[] = [];
  constructor(public url: string) {
    FakeSocket.all.push(this);
  }
  on(ev: string, cb: (a?: unknown) => void): this {
    (this.handlers[ev] ??= []).push(cb);
    return this;
  }
  emit(ev: string, a?: unknown) {
    for (const h of this.handlers[ev] ?? []) h(a);
  }
  send(d: string) {
    this.sent.push(d);
  }
  ping() {
    this.pings++;
  }
  terminate() {
    this.terminated = true;
  }
}

let nowMs = 0;
const now = () => nowMs;
beforeEach(() => {
  FakeSocket.all = [];
  nowMs = 0;
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval"] });
});
afterEach(() => vi.useRealTimers());

const advance = async (ms: number) => {
  nowMs += ms;
  await vi.advanceTimersByTimeAsync(ms);
};

function mk(extra: Partial<ConstructorParameters<typeof Link>[0]> = {}) {
  const log: string[] = [];
  const link = new Link({
    url: "ws://box/x",
    makeSocket: (u) => new FakeSocket(u) as unknown as WsLike,
    onOpen: () => log.push("open"),
    onMessage: (t) => log.push(`msg:${t}`),
    onDown: () => log.push("down"),
    now,
    ...extra,
  });
  return { link, log };
}

describe("Link", () => {
  it("opens, passes messages on, and reports a close at once", async () => {
    const { link, log } = mk();
    link.start();
    const s = FakeSocket.all[0];
    s.emit("open");
    s.emit("message", "hello");
    expect(log).toEqual(["open", "msg:hello"]);
    s.emit("close");
    expect(log.at(-1)).toBe("down");
  });

  it("retries after 1, 2, 5 and then every 10 seconds, and starts over after a good connection", async () => {
    const { link } = mk();
    link.start();
    const times: number[] = [];
    const startedAt = () => nowMs;
    // Four refused attempts in a row.
    const gaps: number[] = [];
    let last = startedAt();
    for (let i = 0; i < 6; i++) {
      const s = FakeSocket.all.at(-1)!;
      s.emit("close");
      const count = FakeSocket.all.length;
      let waited = 0;
      while (FakeSocket.all.length === count) {
        await advance(100);
        waited += 100;
        expect(waited).toBeLessThan(11_000);
      }
      gaps.push(waited);
      last = nowMs;
      times.push(last);
    }
    expect(gaps).toEqual([1000, 2000, 5000, 10000, 10000, 10000]);
    // A good connection (its first message arrived) resets the schedule.
    const s = FakeSocket.all.at(-1)!;
    s.emit("open");
    s.emit("message", "snapshot");
    s.emit("close");
    const count = FakeSocket.all.length;
    await advance(1000);
    expect(FakeSocket.all.length).toBe(count + 1);
  });

  it("gives up on a connection that never opens", async () => {
    const { link, log } = mk({ connectTimeoutMs: 4000 });
    link.start();
    await advance(4001);
    expect(FakeSocket.all[0].terminated).toBe(true);
    expect(log).toEqual(["down"]);
    await advance(1000);
    expect(FakeSocket.all).toHaveLength(2);
  });

  it("notices a silent connection within 5 seconds and reconnects", async () => {
    const { link, log } = mk({ pingEveryMs: 1000, deadAfterMs: 4000 });
    link.start();
    const s = FakeSocket.all[0];
    s.emit("open");
    await advance(1000);
    await advance(1000);
    expect(s.pings).toBeGreaterThanOrEqual(1);
    await advance(2100);
    // 4.1 s of silence.
    expect(s.terminated).toBe(true);
    expect(log.at(-1)).toBe("down");
    await advance(1000);
    expect(FakeSocket.all).toHaveLength(2);
  });

  it("stays up as long as pongs or messages arrive", async () => {
    const { link } = mk({ pingEveryMs: 1000, deadAfterMs: 4000 });
    link.start();
    const s = FakeSocket.all[0];
    s.emit("open");
    for (let i = 0; i < 20; i++) {
      await advance(1000);
      s.emit("pong");
    }
    expect(s.terminated).toBe(false);
  });

  it("reconnects at once, without waiting, when kicked (the Mac woke up)", async () => {
    const { link, log } = mk();
    link.start();
    FakeSocket.all[0].emit("open");
    link.kick();
    expect(FakeSocket.all[0].terminated).toBe(true);
    expect(FakeSocket.all).toHaveLength(2);
    expect(log.at(-1)).toBe("down");
  });

  it("ignores events of a socket it has dropped", async () => {
    const { link, log } = mk();
    link.start();
    const old = FakeSocket.all[0];
    old.emit("open");
    link.kick();
    log.length = 0;
    old.emit("message", "late");
    old.emit("close");
    expect(log).toEqual([]);
    expect(FakeSocket.all).toHaveLength(2);
  });

  it("takes the defaults for options that are passed as undefined (the hub passes on what it was not given)", async () => {
    const { link } = mk({ backoff: undefined, pingEveryMs: undefined, deadAfterMs: undefined, connectTimeoutMs: undefined });
    link.start();
    FakeSocket.all[0].emit("close");
    await advance(1000);
    expect(FakeSocket.all).toHaveLength(2);
  });

  it("does not send while it is down", () => {
    const { link } = mk();
    link.start();
    expect(link.send("x")).toBe(false);
    FakeSocket.all[0].emit("open");
    expect(link.send("x")).toBe(true);
  });
});
