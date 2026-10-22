import type { Env } from "./core/defs.js";
import { cleanHost, DEFAULT_LIBRARY_PORT, portOfListen } from "./core/settings.js";

/** A request from a property inspector. The inspector cannot reach the box itself (the box answers browsers only on its own origins), so the plugin asks. */
export interface PiRequest {
  cmd: "stations" | "playlists" | "items" | "blocks" | "test" | string;
  /** The box the key points at (its own override, or the plugin's). */
  host?: string;
  q?: string;
  playlistId?: number;
  /** Echoed back so the inspector can match an answer to its question. */
  tag?: string;
}

export interface PiReply {
  reply: string;
  ok: boolean;
  error?: string;
  tag?: string;
  [k: string]: unknown;
}

export async function handlePiRequest(env: Env, req: PiRequest): Promise<PiReply> {
  const g = env.globals();
  const host = cleanHost(req.host) || cleanHost(g.host);
  const reply = (r: Partial<PiReply> & { ok: boolean }): PiReply => ({ reply: req.cmd, tag: req.tag, ...r });
  if (!host) return reply({ ok: false, error: "Set the box address first." });
  const base = `http://${host}:${g.libraryPort ?? DEFAULT_LIBRARY_PORT}`;
  switch (req.cmd) {
    case "test": {
      // The library's station list, not only "something answers": a station's own port answers /health too.
      const port = g.libraryPort ?? DEFAULT_LIBRARY_PORT;
      const r = await env.http.get(`${base}/api/v1/stations`);
      const list = r.ok && Array.isArray(r.data?.stations) ? (r.data.stations as any[]) : null; // eslint-disable-line @typescript-eslint/no-explicit-any
      if (list) {
        const names = list.map((s) => String(s.id)).join(", ");
        return reply({ ok: true, message: `Connected to the library: ${list.length} ${list.length === 1 ? "station" : "stations"}${names ? ` (${names})` : ""}.` });
      }
      const h = await env.http.get(`${base}/health`);
      if (h.ok && typeof h.data?.station === "string") {
        return reply({ ok: false, error: `${port} is a station port (station ${h.data.station}), not the library. The library is usually ${DEFAULT_LIBRARY_PORT}.` });
      }
      if (h.ok) return reply({ ok: false, error: `${host}:${port} answers, but not as the Hotclock library (no station list).` });
      return reply({ ok: false, error: `No answer from ${host}:${port}: ${r.error ?? h.error ?? ""}`.trim() });
    }
    case "stations": {
      const r = await env.http.get(`${base}/api/v1/stations`);
      if (!r.ok) return reply({ ok: false, error: `No answer from ${host}: ${r.error ?? ""}`.trim() });
      const stations = (r.data?.stations ?? []).map((s: any) => ({ id: String(s.id), name: String(s.display_name ?? s.id), port: portOfListen(s.listen) })); // eslint-disable-line @typescript-eslint/no-explicit-any
      return reply({ ok: true, stations });
    }
    case "playlists": {
      const r = await env.http.get(`${base}/api/v1/playlists`);
      if (!r.ok) return reply({ ok: false, error: r.error });
      const playlists = (r.data?.playlists ?? []).map((p: any) => ({ id: p.id, name: String(p.name), station: p.station_id ?? null })); // eslint-disable-line @typescript-eslint/no-explicit-any
      return reply({ ok: true, playlists });
    }
    case "items": {
      const r = await env.http.get(`${base}/api/v1/items?per_page=20&q=${encodeURIComponent(req.q ?? "")}`);
      if (!r.ok) return reply({ ok: false, error: r.error });
      const items = (r.data?.items ?? []).map((i: any) => ({ id: i.id, title: String(i.title), artist: String(i.artist ?? ""), durationMs: i.duration_ms ?? 0, category: i.category ?? "" })); // eslint-disable-line @typescript-eslint/no-explicit-any
      return reply({ ok: true, items });
    }
    case "blocks": {
      if (typeof req.playlistId !== "number") return reply({ ok: false, error: "Pick a playlist first." });
      const r = await env.http.get(`${base}/api/v1/playlists/${req.playlistId}`);
      if (!r.ok) return reply({ ok: false, error: r.error });
      const blocks = (r.data?.entries ?? [])
        .filter((e: any) => e.type === "block" && e.block) // eslint-disable-line @typescript-eslint/no-explicit-any
        .map((e: any) => ({ id: e.block.id, name: String(e.block.name ?? ""), kind: String(e.block.kind ?? "") })); // eslint-disable-line @typescript-eslint/no-explicit-any
      return reply({ ok: true, blocks });
    }
    default:
      return reply({ ok: false, error: "Unknown request." });
  }
}
