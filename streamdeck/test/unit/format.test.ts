import { describe, expect, it } from "vitest";
import { clock, ellipsize, esc, localHm, remainingText, urgency, wrap } from "../../src/core/format.js";

describe("clock", () => {
  it("formats minutes and seconds, and hours from an hour up", () => {
    expect(clock(0)).toBe("0:00");
    expect(clock(151_000)).toBe("2:31");
    expect(clock(59_499)).toBe("0:59");
    expect(clock(59_500)).toBe("1:00");
    expect(clock(3_600_000)).toBe("1:00:00");
    expect(clock(3_725_000)).toBe("1:02:05");
  });
  it("shows nothing negative", () => {
    expect(clock(-5000)).toBe("0:00");
    expect(clock(NaN)).toBe("0:00");
  });
  it("puts a minus in front of remaining time", () => {
    expect(remainingText(151_000)).toBe("-2:31");
  });
});

describe("urgency", () => {
  it("is amber at 30 s and under, red at 10 s and under", () => {
    expect(urgency(31_000)).toBe("normal");
    expect(urgency(30_000)).toBe("amber");
    expect(urgency(10_001)).toBe("amber");
    expect(urgency(10_000)).toBe("red");
    expect(urgency(0)).toBe("red");
  });
  it("takes other thresholds (the next block: 60 s and 10 s)", () => {
    expect(urgency(61_000, 60_000, 10_000)).toBe("normal");
    expect(urgency(59_000, 60_000, 10_000)).toBe("amber");
    expect(urgency(9_000, 60_000, 10_000)).toBe("red");
  });
});

describe("text fitting", () => {
  it("wraps on words and ends with an ellipsis when it must cut", () => {
    const lines = wrap("A very long song title that cannot possibly fit on two lines of a key", 130, 17, 2);
    expect(lines).toHaveLength(2);
    expect(lines[1].endsWith("…")).toBe(true);
    for (const l of lines) expect(l.length).toBeLessThanOrEqual(14);
  });
  it("breaks a word longer than a line", () => {
    expect(wrap("Supercalifragilisticexpialidocious", 100, 17, 3).length).toBeGreaterThan(1);
  });
  it("leaves short text alone", () => {
    expect(wrap("Short", 130, 17, 2)).toEqual(["Short"]);
    expect(ellipsize("Short", 130, 13)).toBe("Short");
  });
  it("escapes markup", () => {
    expect(esc(`<b>"R&B"</b>`)).toBe("&lt;b&gt;&quot;R&amp;B&quot;&lt;/b&gt;");
  });
  it("reads the wall-clock part of a box time", () => {
    expect(localHm("2026-10-07T18:45:00.000-04:00")).toBe("18:45");
    expect(localHm(null)).toBe("--:--");
  });
});

import { flipDown, flipUp } from "../../src/core/format.js";

describe("flip times (when the digits of a countdown change)", () => {
  it("counting down: the digit changes when the rounded second does", () => {
    // 151.3 s shows 2:31; it shows 2:30 once the remaining time is below 150.5 s: 0.8 s from now.
    expect(flipDown(151_300)).toBe(801);
    // 150.5 s exactly is already 2:31 (half rounds up); 1 ms later it is 2:30.
    expect(flipDown(150_500)).toBeLessThanOrEqual(2);
    expect(flipDown(0)).toBe(1000);
  });
  it("counting up: the digit changes at the next half second", () => {
    expect(flipUp(12_200)).toBe(301);
    expect(flipUp(12_600)).toBe(901);
  });
});
