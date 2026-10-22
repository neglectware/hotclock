import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { KeyPainter, PressTracker } from "../../src/core/painter.js";
import { FakeClock } from "./helpers.js";

beforeEach(() => vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] }));
afterEach(() => vi.useRealTimers());

describe("KeyPainter", () => {
  it("sends the first picture at once and skips one that did not change", () => {
    const clock = new FakeClock();
    const sent: string[] = [];
    const p = new KeyPainter((s) => sent.push(s), clock.now);
    p.paint("a");
    p.paint("a");
    expect(sent).toEqual(["a"]);
  });

  it("sends at most two pictures a second, and the latest one in the end", async () => {
    const clock = new FakeClock();
    const sent: { at: number; img: string }[] = [];
    const p = new KeyPainter((img) => sent.push({ at: clock.now(), img }), clock.now);
    for (let i = 0; i < 100; i++) {
      p.paint(`img${i}`);
      clock.advance(50);
      await vi.advanceTimersByTimeAsync(50);
    }
    await vi.advanceTimersByTimeAsync(600);
    for (let i = 1; i < sent.length; i++) expect(sent[i].at - sent[i - 1].at).toBeGreaterThanOrEqual(500);
    expect(sent.length).toBeLessThanOrEqual(11);
    expect(sent.at(-1)!.img).toBe("img99");
  });

  it("shows a change within 500 ms of when it happens", async () => {
    const clock = new FakeClock();
    const sent: number[] = [];
    const p = new KeyPainter(() => sent.push(clock.now()), clock.now);
    p.paint("a");
    clock.advance(100);
    const t0 = clock.now();
    p.paint("b");
    clock.advance(400);
    await vi.advanceTimersByTimeAsync(400);
    expect(sent.at(-1)! - t0).toBeLessThanOrEqual(500);
    expect(sent).toHaveLength(2);
  });

  it("keeps the 500 ms spacing even when its timer fires a millisecond early", async () => {
    const clock = new FakeClock();
    const sent: number[] = [];
    const p = new KeyPainter(() => sent.push(clock.now()), clock.now);
    p.paint("a");
    clock.advance(100);
    p.paint("b");
    // The timer fires when only 499 ms of the clock have passed.
    clock.advance(399);
    await vi.advanceTimersByTimeAsync(400);
    clock.advance(1);
    await vi.advanceTimersByTimeAsync(5);
    expect(sent).toHaveLength(2);
    expect(sent[1] - sent[0]).toBeGreaterThanOrEqual(500);
  });

  it("does not send a picture that was replaced by the old one before its turn", async () => {
    const clock = new FakeClock();
    const sent: string[] = [];
    const p = new KeyPainter((s) => sent.push(s), clock.now);
    p.paint("a");
    clock.advance(10);
    p.paint("b");
    p.paint("a");
    clock.advance(600);
    await vi.advanceTimersByTimeAsync(600);
    expect(sent).toEqual(["a"]);
  });
});

describe("PressTracker", () => {
  it("is short when released before the long-press time, long (once, while down) after it", async () => {
    const events: string[] = [];
    const t = new PressTracker(500, () => events.push("short"), () => events.push("long"));
    t.keyDown();
    await vi.advanceTimersByTimeAsync(499);
    t.keyUp();
    expect(events).toEqual(["short"]);
    t.keyDown();
    await vi.advanceTimersByTimeAsync(501);
    expect(events).toEqual(["short", "long"]);
    t.keyUp();
    expect(events).toEqual(["short", "long"]);
  });
});
