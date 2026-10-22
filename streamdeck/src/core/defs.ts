import type { Hub, LibraryConn, StationConn } from "./hub.js";
import type { Http, HttpResult } from "./http.js";
import type { StationModel } from "./model.js";
import type { GlobalSettings, KeySettings, Target } from "./settings.js";
import type { Face } from "./svg.js";

/** What every key controller shares. */
export interface Env {
  hub: Hub;
  http: Http;
  /** Monotonic milliseconds. */
  clock: () => number;
  globals: () => GlobalSettings;
  openUrl: (url: string) => void;
  log: (msg: string) => void;
}

/** What an action's `face` and `press` get to work with. */
export interface Ctx {
  s: KeySettings;
  t: Target;
  /** The key's station connection (actions that use a station), else null. */
  conn: StationConn | null;
  /** The station's model; an empty one for actions that do not use a station. */
  m: StationModel;
  /** The library service connection (actions that use it), else null. */
  lib: LibraryConn | null;
  env: Env;
  nowMs: number;
  /** Per-key scratch: poll results, the cart's lit-until time, and so on. Not saved. */
  extra: Record<string, any>; // eslint-disable-line @typescript-eslint/no-explicit-any
  /** POST to the station's API: `post("/transport/next")`. */
  post: (path: string, body?: unknown) => Promise<HttpResult>;
  /** DELETE on the station's API. */
  del: (path: string) => Promise<HttpResult>;
  /** Save a change to this key's settings. */
  save: (patch: KeySettings) => void;
  /** Ask for a redraw now. */
  redraw: () => void;
}

export type PressResult = HttpResult | null | undefined | void;

export interface ActionDef {
  /** The last part of the manifest UUID: `now-playing` for `com.neglectware.hotclock.now-playing`. */
  id: string;
  /** What the key listens to. */
  uses: "station" | "library";
  /** The face while the box does not answer, when it is not the plain grey OFFLINE key (Station Status goes red). */
  offlineFace?: (label: string) => Face;
  /**
   * `only`: the press does nothing unless held (Stop, Cut); a short press shows `hint`.
   * `extra`: a short press does one thing and a long press another.
   */
  longPress?: "only" | "extra";
  hint?: string;
  face(c: Ctx): Face;
  press?(c: Ctx): Promise<PressResult> | PressResult;
  long?(c: Ctx): Promise<PressResult> | PressResult;
  /** Work the key does on a timer while it is on screen and its box answers. */
  poll?: { everyMs: number; run(c: Ctx): Promise<void> };
}
