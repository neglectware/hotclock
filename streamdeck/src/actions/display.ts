import type { ActionDef, Ctx } from "../core/defs.js";
import { portOfListen } from "../core/settings.js";
import { COLORS } from "../core/svg.js";
import { str, textFace } from "./common.js";

/** One view the display can be set to. */
export interface ViewChoice {
  view: "single" | "split" | "rotate" | "processing";
  station?: string;
}

export interface StationEntry { id: string; name: string; port: number | null }

/** The views a cycling key steps through: each station on its own, then split, rotate, and processing when wanted. */
export function cycleSequence(stationIds: string[], includeProcessing: boolean): ViewChoice[] {
  const seq: ViewChoice[] = stationIds.map((id) => ({ view: "single", station: id }));
  seq.push({ view: "split" }, { view: "rotate" });
  if (includeProcessing) seq.push({ view: "processing" });
  return seq;
}

/** The choice after the current view in a sequence; the first one when the current view is not in it. */
export function nextChoice(current: { view: string; station: string | null } | null, seq: ViewChoice[]): ViewChoice {
  const i = current
    ? seq.findIndex((c) => c.view === current.view && (c.view !== "single" || c.station === current.station))
    : -1;
  return seq[(i + 1) % seq.length];
}

export function choiceBody(c: ViewChoice): { view: string; station?: string } {
  return c.view === "single" ? { view: "single", station: c.station } : { view: c.view };
}

async function stationList(c: Ctx): Promise<StationEntry[]> {
  const hit = c.extra.stations as StationEntry[] | undefined;
  if (hit) return hit;
  const r = await c.env.http.get(`${c.lib!.base}/api/v1/stations`);
  if (!r.ok) return [];
  const list: StationEntry[] = (r.data?.stations ?? []).map((s: any) => ({ id: String(s.id), name: String(s.display_name ?? s.id), port: portOfListen(s.listen) })); // eslint-disable-line @typescript-eslint/no-explicit-any
  c.extra.stations = list;
  return list;
}

function configured(c: Ctx): ViewChoice | null {
  const v = str(c.s.view) || "cycle";
  if (v === "cycle") return null;
  if (v === "single") return { view: "single", station: str(c.s.displayStation) || undefined };
  return { view: v as ViewChoice["view"] };
}

const LABEL: Record<string, string> = { single: "SINGLE", split: "SPLIT", rotate: "ROTATE", processing: "PROCESSING" };

export const displayView: ActionDef = {
  id: "display-view",
  uses: "library",
  poll: {
    everyMs: 60_000,
    async run(c) {
      c.extra.stations = undefined;
      await stationList(c);
    },
  },
  face(c) {
    const d = c.lib?.display;
    const want = configured(c);
    const cur = d ? LABEL[d.view] ?? d.view.toUpperCase() : "--";
    const stations = (c.extra.stations as StationEntry[] | undefined) ?? [];
    const stName = d?.station ? (stations.find((s) => s.id === d.station)?.name ?? d.station) : null;
    const lit = !!want && !!d && want.view === d.view && (want.view !== "single" || !want.station || want.station === d.station);
    const sub = d?.view === "single" ? (stName ?? "") : d?.view === "rotate" ? `every ${d.rotateS} s` : d?.view === "processing" ? "meters" : d?.view === "split" ? "both stations" : "";
    if (want && !lit) {
      const wl = LABEL[want.view];
      const wst = want.station ? (stations.find((s) => s.id === want.station)?.name ?? want.station) : "";
      return textFace({ tag: "display:set", glyph: "display", top: "SHOW", main: wl, mainSize: want.view === "processing" ? 17 : 24, sub: wst || `now ${cur.toLowerCase()}`, subColor: COLORS.faint });
    }
    return textFace({
      tag: lit ? "display:lit" : "display:cycle",
      bg: lit ? COLORS.bgLit : undefined,
      border: lit ? COLORS.green : null,
      glyph: "display",
      top: want ? "SHOWING" : "DISPLAY",
      main: cur,
      mainSize: cur.length > 7 ? 17 : 24,
      sub,
      subColor: lit ? COLORS.green : COLORS.dim,
    });
  },
  async press(c) {
    let choice = configured(c);
    if (!choice) {
      const list = await stationList(c);
      const seq = cycleSequence(list.map((s) => s.id), c.s.includeProcessing === true);
      choice = nextChoice(c.lib?.display ?? null, seq);
    }
    return c.env.http.put(`${c.lib!.base}/api/v1/display`, choiceBody(choice));
  },
};

export const displayActions: ActionDef[] = [displayView];
