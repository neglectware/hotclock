import { describe, expect, it } from "vitest";
import { COLORS, noStationFace, offlineFace, type Face } from "../../src/core/svg.js";
import { applyView, labelOf, viewOf, type KeyView } from "../../src/core/view.js";

const face = (over: Partial<Face> = {}): Face => ({
  tag: "t",
  lines: [
    { text: "PRE-SHOW", size: 12, y: 30 },
    { text: "Night", size: 19, y: 60 },
    { text: "rotation", size: 19, y: 82 },
    { text: "at next segue", size: 13, y: 110 },
  ],
  ...over,
});

describe("a face as a standard Stream Deck key", () => {
  it("its label is the main text; the person's title replaces it", () => {
    expect(labelOf(face())).toBe("Night\nrotation");
    expect(viewOf(face(), {}, "").title).toBe("Night\nrotation");
    expect(viewOf(face(), {}, "SHOW").title).toBeNull();
  });

  it("state 1 while lit (green, amber, a frame, or said so), else 0; never a picture of its own", () => {
    expect(viewOf(face(), {}, "").state).toBe(0);
    expect(viewOf(face({ bg: COLORS.bgLit }), {}, "").state).toBe(1);
    expect(viewOf(face({ border: COLORS.green }), {}, "").state).toBe(1);
    expect(viewOf(face({ border: COLORS.green, lit: false }), {}, "").state).toBe(0);
    expect(viewOf(face({ bg: COLORS.bgLit, art: { kind: "image", href: "data:image/png;base64,AA" } }), {}, "").image).toBeNull();
  });

  it("a problem shows over the person's title, on state 0", () => {
    for (const f of [noStationFace(), offlineFace("Office")]) {
      const v = viewOf(f, {}, "MY KEY");
      expect(v.state).toBe(0);
      expect(v.title).toMatch(/STATION|OFFLINE/);
    }
  });

  it("an info key's title is its live text; only a key that draws art sends a picture", () => {
    expect(viewOf(face({ title: "-2:31\nFrom Silence" }), { live: true }, "MINE").title).toBe("-2:31\nFrom Silence");
    expect(viewOf(face(), { live: true }, "MINE").title).toBe("PRE-SHOW\nNight\nrotation\nat next segue");
    const art = viewOf(face({ art: { kind: "image", href: "data:image/png;base64,AA" } }), { live: true, art: true }, "");
    expect(decodeURIComponent(art.image ?? "")).toContain("data:image/png;base64,AA");
    expect(decodeURIComponent(art.image ?? "")).not.toContain("Night");
    expect(viewOf(face({ art: { kind: "placeholder" } }), { live: true, art: true }, "").image).toBeNull();
  });
});

describe("telling Stream Deck", () => {
  it("sends everything first, then only what changed; no title or picture means the person's own", () => {
    const calls: string[] = [];
    const out = {
      setState: (n: 0 | 1) => void calls.push(`state ${n}`),
      setTitle: (t?: string) => void calls.push(`title ${t ?? "(own)"}`),
      setImage: (i?: string) => void calls.push(`image ${i ?? "(state)"}`),
    };
    const a: KeyView = { state: 0, title: "NEXT", image: null, tag: "", text: "" };
    applyView(null, a, out);
    expect(calls).toEqual(["state 0", "title NEXT", "image (state)"]);
    calls.length = 0;
    applyView(a, { ...a, state: 1 }, out);
    applyView({ ...a, state: 1 }, { ...a, state: 1, title: null }, out);
    expect(calls).toEqual(["state 1", "title (own)"]);
  });
});
