import type { ActionDef, Ctx } from "../core/defs.js";
import { clock, flipDown, urgency } from "../core/format.js";
import { nextClockTime, rfc3339 } from "../core/time.js";
import { COLORS } from "../core/svg.js";
import { numOf, str, textFace, unsetFace } from "./common.js";

const FIRED = new Set(["fired", "late_run"]);

function expectedFor(c: Ctx, id: number | null) {
  const e = c.m.program?.expected;
  return e && id !== null && e.playlistId === id ? e : null;
}

// ---- Start Playlist as Program ----------------------------------------------------------------------------------

export const programStart: ActionDef = {
  id: "program-start",
  uses: "station",
  longPress: "extra",
  face(c) {
    const id = numOf(c.s.playlistId);
    if (id === null) return unsetFace("program:unset", "a playlist", "play");
    const name = str(c.s.playlistName) || `Playlist ${id}`;
    const p = c.m.program;
    if (p?.running && p.playlistId === id) {
      return textFace({ tag: "program:running", bg: COLORS.bgLit, border: COLORS.green, glyph: "play", main: name, mainSize: 19, lines: 3, sub: "RUNNING", subColor: COLORS.green });
    }
    const e = expectedFor(c, id);
    if (e) {
      return textFace({ tag: e.armed ? "program:armed" : "program:expected", glyph: "play", main: name, mainSize: 17, lines: 2, sub: e.label + (e.armed ? " · ARMED" : ""), subColor: e.armed ? COLORS.amber : COLORS.dim, border: e.armed ? COLORS.amber : null });
    }
    return textFace({ tag: "program", glyph: "play", main: name, mainSize: 19, lines: 3, sub: "start program", subColor: COLORS.faint });
  },
  press(c) {
    const id = numOf(c.s.playlistId);
    if (id === null) return null;
    // The key's station goes with the press: another station's port refuses it.
    return c.post("/program/start", { playlist_id: id, t0: "now", source: "deck", ...(c.t.station ? { station: c.t.station } : {}) });
  },
  long(c) {
    const id = numOf(c.s.playlistId);
    if (id === null || !expectedFor(c, id)?.armed) return null;
    return c.post("/program/disarm", { playlist_id: id });
  },
};

export const programArm: ActionDef = {
  id: "program-arm",
  uses: "station",
  face(c) {
    const id = numOf(c.s.playlistId);
    if (id === null) return unsetFace("arm:unset", "a playlist", "play");
    const e = expectedFor(c, id);
    const name = str(c.s.playlistName) || `Playlist ${id}`;
    if (!e) return textFace({ tag: "arm:none", top: "ARM", main: name, mainSize: 17, mainColor: COLORS.faint, sub: "no start expected", subColor: COLORS.faint });
    return e.armed
      ? textFace({ tag: "arm:armed", bg: COLORS.bgAmber, border: COLORS.amber, top: "ARMED", topColor: COLORS.amber, main: name, mainSize: 17, sub: e.label, subColor: COLORS.text })
      : textFace({ tag: "arm:disarmed", top: "ARM", main: name, mainSize: 17, sub: e.label, subColor: COLORS.dim });
  },
  press(c) {
    const id = numOf(c.s.playlistId);
    if (id === null) return null;
    const e = expectedFor(c, id);
    return c.post(e?.armed ? "/program/disarm" : "/program/arm", { playlist_id: id });
  },
};

// ---- Next Block -------------------------------------------------------------------------------------------------

export const nextBlock: ActionDef = {
  id: "next-block",
  uses: "station",
  longPress: "extra",
  face(c) {
    const b = c.m.program?.nextBlock;
    if (!b) return textFace({ tag: "block:none", glyph: "block", main: "NO BLOCK", mainSize: 20, mainColor: COLORS.faint, sub: "nothing due", subColor: COLORS.faint });
    const ms = Math.max(0, b.dueLocal - c.nowMs);
    const u = urgency(ms, 60_000, 10_000);
    const color = u === "red" ? COLORS.red : u === "amber" ? COLORS.amber : COLORS.text;
    const bt = c.m.program?.backTime;
    const f = textFace({
      tag: `block:${u}`,
      glyph: "block",
      top: b.badge || undefined,
      main: clock(ms),
      mainSize: 38,
      mainColor: color,
      lines: 1,
      sub: bt?.label ?? b.name,
      subSize: 13,
      subColor: bt?.label ? COLORS.blue : COLORS.dim,
    });
    return { ...f, repaintMs: Math.min(flipDown(ms), ...[60_000, 10_000].filter((l) => ms > l).map((l) => ms - l + 1)) };
  },
  long(c) {
    const b = c.m.program?.nextBlock;
    if (c.s.fireOnLongPress !== true || !b) return null;
    return c.post(`/program/blocks/${b.blockId}/fire`);
  },
};

// ---- Fire Block -------------------------------------------------------------------------------------------------

export const fireBlock: ActionDef = {
  id: "fire-block",
  uses: "station",
  face(c) {
    const id = numOf(c.s.blockId);
    if (id === null) return unsetFace("fire:unset", "a block", "block");
    const st = c.m.program?.blocks.find((b) => b.blockId === id)?.status;
    const fired = !!st && FIRED.has(st);
    return textFace({
      tag: fired ? "fire:fired" : "fire",
      bg: fired ? COLORS.bgLit : undefined,
      border: fired ? COLORS.green : null,
      glyph: "block",
      main: str(c.s.blockName) || `Block ${id}`,
      mainSize: 18,
      lines: 3,
      sub: fired ? "FIRED" : "fire now",
      subColor: fired ? COLORS.green : COLORS.faint,
    });
  },
  press(c) {
    const id = numOf(c.s.blockId);
    return id === null ? null : c.post(`/program/blocks/${id}/fire`);
  },
};

export const continueKey: ActionDef = {
  id: "continue",
  uses: "station",
  face(c) {
    const w = c.m.program?.waiting;
    const waiting = !!w && (w.kind === "operator" || c.m.program?.state === "waiting");
    return waiting
      ? textFace({ tag: "continue:waiting", bg: COLORS.bgAmber, border: COLORS.amber, glyph: "continue", main: "CONTINUE", mainSize: 21, mainColor: COLORS.amber, sub: w?.name, subColor: COLORS.text })
      : textFace({ tag: "continue", glyph: "continue", main: "CONTINUE", mainSize: 21, mainColor: COLORS.faint, sub: "nothing waiting", subColor: COLORS.faint });
  },
  press: (c) => c.post("/program/continue"),
};

export const skipBlock: ActionDef = {
  id: "skip-block",
  uses: "station",
  face(c) {
    const b = c.m.program?.nextBlock;
    return textFace({ tag: "skip", glyph: "skip", main: "SKIP", mainSize: 28, mainColor: b ? COLORS.text : COLORS.faint, sub: b?.name ?? "no block due", subColor: COLORS.faint });
  },
  press: (c) => c.post("/program/blocks/next/skip"),
};

export const abortProgram: ActionDef = {
  id: "abort-program",
  uses: "station",
  longPress: "only",
  hint: "HOLD",
  face(c) {
    const running = !!c.m.program?.running;
    return textFace({ tag: "abort", glyph: "abort", main: "ABORT", mainSize: 26, mainColor: running ? COLORS.red : COLORS.faint, sub: "hold to end program", subColor: COLORS.faint });
  },
  long: (c) => c.post("/program/abort", { stop_audio: c.s.stopAudio === true }),
};

// ---- Back-time End ----------------------------------------------------------------------------------------------

export const endAt: ActionDef = {
  id: "end-at",
  uses: "station",
  face(c) {
    const bt = c.m.program?.backTime;
    if (bt && bt.targetLocal !== null) {
      const ms = Math.max(0, bt.targetLocal - c.nowMs);
      const f = textFace({ tag: "end:active", bg: COLORS.bgAmber, border: COLORS.amber, glyph: "clock", top: "ENDS IN", main: clock(ms), mainSize: 36, mainColor: COLORS.amber, lines: 1, sub: bt.label ?? undefined, subColor: COLORS.text });
      return { ...f, repaintMs: flipDown(ms) };
    }
    const id = numOf(c.s.playlistId);
    if (id === null) return unsetFace("end:unset", "a playlist", "clock");
    const when = c.s.endMode === "in" ? `in ${numOf(c.s.endIn) ?? 30} min` : `at ${str(c.s.endTime) || "--:--"}`;
    return textFace({ tag: "end", glyph: "clock", top: "END", main: str(c.s.playlistName) || "Playlist", mainSize: 17, lines: 2, sub: when, subSize: 18, subColor: COLORS.text });
  },
  press(c) {
    const id = numOf(c.s.playlistId);
    if (id === null) return null;
    const boxNow = c.m.boxNow();
    let at: number | null;
    if (c.s.endMode === "in") at = boxNow + (numOf(c.s.endIn) ?? 30) * 60_000;
    else at = nextClockTime(str(c.s.endTime), boxNow, c.m.program?.timeZone ?? null);
    if (at === null) return { ok: false, status: 0, data: null, error: "the end time is not set" };
    return c.post("/end-at", { playlist_id: id, at: rfc3339(at), then: "end" });
  },
};

export const programActions: ActionDef[] = [programStart, programArm, nextBlock, fireBlock, continueKey, skipBlock, abortProgram, endAt];
