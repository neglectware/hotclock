import type { ActionDef, Ctx, Env, PressResult } from "./defs.js";
import type { LibraryConn, StationConn } from "./hub.js";
import type { HttpResult } from "./http.js";
import { StationModel } from "./model.js";
import { KeyPainter, PressTracker } from "./painter.js";
import { customize } from "./custom.js";
import { resolveTarget, type KeySettings, type Target } from "./settings.js";
import { COLORS, noStationFace, offlineFace, renderFace, setupFace, toDataUri, type Face } from "./svg.js";

/** The part of a Stream Deck key the controller drives. */
export interface KeyHandle {
  setImage(dataUri: string): unknown;
  showOk(): unknown;
  showAlert(): unknown;
  setSettings(s: KeySettings): unknown;
}

export const LONG_PRESS_MS = 500;

/** One key on a Stream Deck: it binds an action to a station connection, draws it and handles its presses. */
export class KeyController {
  readonly painter: KeyPainter;
  private s: KeySettings = {};
  private target: Target | null = null;
  private conn: StationConn | null = null;
  private lib: LibraryConn | null = null;
  private connKey = "";
  private unsub: (() => void) | null = null;
  private extra: Record<string, unknown> = {};
  private pollTimer: ReturnType<typeof setInterval> | null = null;
  private flipTimer: ReturnType<typeof setTimeout> | null = null;
  private flashUntil = 0;
  private flashText = "";
  private readonly press: PressTracker;
  private readonly empty: StationModel;
  /** Presses that reached the API (tests count them). */
  calls = 0;

  constructor(
    readonly def: ActionDef,
    private readonly key: KeyHandle,
    private readonly env: Env,
  ) {
    this.empty = new StationModel(env.clock);
    this.painter = new KeyPainter((img) => key.setImage(img), env.clock);
    const hasLong = def.longPress === "only" || def.longPress === "extra";
    this.press = new PressTracker(
      LONG_PRESS_MS,
      () => this.fire("short"),
      () => (hasLong ? this.fire("long") : undefined),
    );
  }

  // ---- settings and connections -------------------------------------------------------------------------------

  setSettings(s: KeySettings): void {
    this.s = s ?? {};
    this.rebind();
    this.refresh();
  }

  /** The plugin-wide settings changed (the box address). */
  globalsChanged(): void {
    this.rebind();
    this.refresh();
  }

  private rebind(): void {
    const t = resolveTarget(this.env.globals(), this.s);
    this.target = t;
    const want = t && !this.noStation() ? (this.def.uses === "station" ? `s:${t.host}:${t.stationPort}` : `l:${t.host}:${t.libraryPort}`) : "";
    if (want === this.connKey) return;
    this.unbind();
    this.connKey = want;
    if (!t || !want) return;
    if (this.def.uses === "station") {
      this.conn = this.env.hub.station(t.host, t.stationPort);
      this.unsub = this.conn.subscribe(() => this.refresh());
    } else {
      this.lib = this.env.hub.library(t.host, t.libraryPort);
      this.unsub = this.lib.subscribe(() => this.refresh());
    }
    this.startPoll();
  }

  private unbind(): void {
    this.unsub?.();
    this.unsub = null;
    if (this.conn) this.env.hub.releaseStation(this.conn);
    if (this.lib) this.env.hub.releaseLibrary(this.lib);
    this.conn = null;
    this.lib = null;
    this.connKey = "";
    if (this.pollTimer) clearInterval(this.pollTimer);
    this.pollTimer = null;
    this.extra = {};
  }

  private startPoll(): void {
    const p = this.def.poll;
    if (!p) return;
    const run = (): void => {
      const c = this.ctx();
      if (c && this.isReady()) void p.run(c).then(() => this.refresh()).catch(() => undefined);
    };
    this.pollTimer = setInterval(run, p.everyMs);
    // The first reading as soon as the connection is up.
    if (this.isReady()) run();
    const src = this.conn ?? this.lib;
    const off = src?.subscribe(() => {
      if (this.isReady()) {
        off?.();
        run();
      }
    });
  }

  // ---- drawing ------------------------------------------------------------------------------------------------

  private isReady(): boolean {
    if (!this.target) return false;
    if (this.def.uses === "station") return !!this.conn && this.conn.model.online && this.conn.model.ready;
    return !!this.lib && this.lib.online && this.lib.ready;
  }

  private ctx(): Ctx | null {
    const t = this.target;
    if (!t) return null;
    const conn = this.conn;
    return {
      s: this.s,
      t,
      conn,
      m: conn ? conn.model : this.empty,
      lib: this.lib,
      env: this.env,
      nowMs: this.env.clock(),
      extra: this.extra as Record<string, any>, // eslint-disable-line @typescript-eslint/no-explicit-any
      post: (path, body) => {
        if (!conn) return Promise.resolve({ ok: false, status: 0, data: null, error: "no station" });
        return this.env.http.post(`${conn.base}/api/v1${path}`, body ?? {});
      },
      del: (path) => {
        if (!conn) return Promise.resolve({ ok: false, status: 0, data: null, error: "no station" });
        return this.env.http.request("DELETE", `${conn.base}/api/v1${path}`);
      },
      save: (patch) => {
        this.s = { ...this.s, ...patch };
        void this.key.setSettings(this.s);
      },
      redraw: () => this.refresh(),
    };
  }

  /** A station key with no station chosen: it shows so and sends nothing (never the box's first station). */
  private noStation(): boolean {
    return this.def.uses === "station" && !!this.target && !this.target.stationChosen;
  }

  /** The face the key shows now. */
  face(): Face {
    if (this.noStation()) return noStationFace();
    const c = this.ctx();
    if (!c) return setupFace();
    if (!this.isReady()) {
      const label = c.conn?.model.displayName || c.t.station || "";
      return this.def.offlineFace ? this.def.offlineFace(label) : offlineFace(label || undefined);
    }
    // the person's label and pictures over the action's face (problems, setup and offline faces stay as they are)
    const f = customize(this.def.face(c), this.s);
    if (this.flashUntil > c.nowMs) {
      return { ...f, art: null, lines: [{ text: this.flashText, size: 26, color: COLORS.amber, weight: 700, y: 82 }], bar: null };
    }
    return f;
  }

  refresh(): void {
    const face = this.face();
    this.painter.paint(toDataUri(renderFace(face)));
    // A countdown redraws itself at the moment its digits change, not at the next tick, so it never lags the box by a tick.
    if (this.flipTimer) clearTimeout(this.flipTimer);
    this.flipTimer = null;
    if (face.repaintMs !== undefined) {
      this.flipTimer = setTimeout(() => {
        this.flipTimer = null;
        this.refresh();
      }, face.repaintMs);
    }
  }

  // ---- presses ------------------------------------------------------------------------------------------------

  keyDown(): void {
    this.press.keyDown();
  }
  keyUp(): void {
    this.press.keyUp();
  }

  private fire(kind: "short" | "long"): void {
    const c = this.ctx();
    // A press while the box is offline is ignored, never queued: it must not fire when the box comes back. A key
    // with no station sends nothing.
    if (!c || this.noStation() || !this.isReady()) {
      void this.key.showAlert();
      return;
    }
    const d = this.def;
    if (kind === "short" && d.longPress === "only") {
      this.flashText = d.hint ?? "HOLD";
      this.flashUntil = c.nowMs + 1200;
      this.refresh();
      setTimeout(() => this.refresh(), 1250);
      return;
    }
    const fn = kind === "long" ? d.long : d.press;
    if (!fn) return;
    this.calls++;
    void Promise.resolve(fn.call(d, c))
      .then((r: PressResult) => {
        if (r && typeof r === "object" && "ok" in r) {
          if ((r as HttpResult).ok) void this.key.showOk();
          else {
            this.env.log(`${d.id}: ${(r as HttpResult).error ?? "refused"}`);
            void this.key.showAlert();
          }
        }
      })
      .catch((e) => {
        this.env.log(`${d.id}: ${e}`);
        void this.key.showAlert();
      })
      .finally(() => this.refresh());
  }

  dispose(): void {
    if (this.flipTimer) clearTimeout(this.flipTimer);
    this.flipTimer = null;
    this.press.dispose();
    this.painter.dispose();
    this.unbind();
  }
}
