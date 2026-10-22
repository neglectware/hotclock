import type { JsonValue } from "@elgato/utils";

/** Settings of the plugin: the box address once, a few per-key overrides (architecture 8, "Connection"). */

export interface GlobalSettings {
  /** The box's host name or address, e.g. `hotclock.local`. Empty until the user sets it. */
  host?: string;
  /** The library service's port (default 8300). */
  libraryPort?: number;
  [field: string]: JsonValue | undefined;
}

/** What every key stores; each action adds its own fields. */
export interface KeySettings {
  /** A different box for this key; empty means the plugin's box. */
  host?: string;
  /** The station's id (its name in the box's list). */
  station?: string;
  /** The station's port, saved when the station is picked from the box's list. */
  stationPort?: number;
  /** The key's own face (see `custom.ts`). */
  label?: string;
  labelColor?: string;
  imageIdle?: string;
  imageActive?: string;
  tintActive?: boolean;
  [field: string]: JsonValue | undefined;
}

export const DEFAULT_LIBRARY_PORT = 8300;
export const DEFAULT_STATION_PORT = 8301;

export interface Target {
  host: string;
  stationPort: number;
  libraryPort: number;
  station: string;
  /** A station was picked for this key (or its port typed): a station key without one does nothing, never the first station. */
  stationChosen: boolean;
}

/** Where a key's box and station are, or null while the box address is not set. */
export function resolveTarget(g: GlobalSettings, s: KeySettings): Target | null {
  const host = cleanHost(typeof s.host === "string" && s.host.trim() ? s.host : g.host);
  if (!host) return null;
  return {
    host,
    stationPort: validPort(s.stationPort) ?? DEFAULT_STATION_PORT,
    libraryPort: validPort(g.libraryPort) ?? DEFAULT_LIBRARY_PORT,
    station: typeof s.station === "string" ? s.station : "",
    stationChosen: (typeof s.station === "string" && s.station.trim() !== "") || validPort(s.stationPort) !== null,
  };
}

function validPort(p: unknown): number | null {
  const n = typeof p === "string" ? Number(p) : (p as number);
  return Number.isInteger(n) && n > 0 && n < 65536 ? n : null;
}

/** Strip a scheme, a path and a trailing slash from what a person typed; keep `host` or `[v6]`. */
export function cleanHost(h: string | undefined): string {
  if (!h) return "";
  let t = h.trim();
  t = t.replace(/^[a-z]+:\/\//i, "").replace(/\/.*$/, "");
  // `host:port` typed by habit: the port belongs in its own field.
  if (/^[^:\[\]]+:\d+$/.test(t)) t = t.replace(/:\d+$/, "");
  return t;
}

/** A station's port from the `listen` field of the library's station list (`0.0.0.0:8301`). */
export function portOfListen(listen: string | undefined): number | null {
  const m = /:(\d+)$/.exec(listen ?? "");
  return m ? validPort(Number(m[1])) : null;
}
