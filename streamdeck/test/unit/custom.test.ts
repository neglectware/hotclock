import { describe, expect, it } from "vitest";
import { textFace, unsetFace } from "../../src/actions/common.js";
import { customize, isLit } from "../../src/core/custom.js";
import { COLORS, renderFace } from "../../src/core/svg.js";
import { allText, texts } from "./helpers.js";

const PNG = "data:image/png;base64,iVBORw0KGgo=";
const PNG2 = "data:image/png;base64,AAAA";
const idle = textFace({ tag: "program", glyph: "play", main: "Eve Monday", mainSize: 19, lines: 3, sub: "start program", subColor: COLORS.faint });
const lit = textFace({ tag: "program:running", bg: COLORS.bgLit, border: COLORS.green, glyph: "play", main: "Eve Monday", mainSize: 19, lines: 3, sub: "RUNNING", subColor: COLORS.green });

describe("a key's own face", () => {
  it("without settings the drawn face is the default", () => {
    expect(customize(idle, {})).toBe(idle);
    expect(renderFace(customize(lit, {}))).toBe(renderFace(lit));
  });
  it("a label replaces the main text in its colour; the status line stays", () => {
    const f = customize(lit, { label: "SHOW START", labelColor: "#ff00aa" });
    const t = texts(renderFace(f));
    expect(t.map((x) => x.text)).toEqual(["SHOW START", "RUNNING"]);
    expect(t[0].fill).toBe("#ff00aa");
    expect(t[1].fill).toBe(COLORS.green);
    // a colour alone recolours the action's own text
    expect(texts(renderFace(customize(idle, { labelColor: "#00ff00" })))[0].fill).toBe("#00ff00");
    // an unusable colour is ignored
    expect(customize(idle, { labelColor: "red" })).toBe(idle);
  });
  it("pictures: idle, active, or idle tinted green while lit", () => {
    expect(isLit(idle)).toBe(false);
    expect(isLit(lit)).toBe(true);
    const a = renderFace(customize(idle, { imageIdle: PNG, imageActive: PNG2 }));
    expect(a).toContain(`href="${PNG}"`);
    const b = renderFace(customize(lit, { imageIdle: PNG, imageActive: PNG2 }));
    expect(b).toContain(`href="${PNG2}"`);
    const c = renderFace(customize(lit, { imageIdle: PNG, tintActive: true }));
    expect(c).toContain(`href="${PNG}"`);
    expect(c).toContain(`data-tint="${COLORS.green}"`);
    // idle: not tinted
    expect(renderFace(customize(idle, { imageIdle: PNG, tintActive: true }))).not.toContain("data-tint");
    // something that is not a picture is not drawn
    expect(renderFace(customize(idle, { imageIdle: "javascript:alert(1)" }))).not.toContain("javascript");
  });
  it("an unconfigured key says SET UP in a colour that reads", () => {
    const t = texts(renderFace(unsetFace("program:unset", "a playlist", "play")));
    expect(allText(renderFace(unsetFace("x", "a playlist")))).toContain("SET UP");
    expect(t.find((x) => x.text === "SET UP")!.fill).toBe(COLORS.amber);
    expect(t.find((x) => x.text === "a playlist")!.fill).toBe(COLORS.text);
  });
});
