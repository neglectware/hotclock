import { Http } from "./http.js";
import { Link, type WsLike } from "./link.js";
import { type Envelope, StationModel } from "./model.js";

export type Listener = () => void;
export type SocketFactory = (url: string) => WsLike;

export interface HubDeps {
  http: Http;
  makeSocket: SocketFactory;
  /** Monotonic milliseconds. */
  clock: () => number;
  backoff?: number[];
  pingEveryMs?: number;
  deadAfterMs?: number;
  connectTimeoutMs?: number;
  log?: (msg: string) => void;
}

const ART_TTL_MS = 10 * 60_000;
const ART_MAX = 40;

/** One station's connection, shared by every key that shows that station: the WebSocket, the model and the art cache. */
export class StationConn {
  readonly model: StationModel;
  readonly base: string;
  private readonly link: Link;
  private readonly listeners = new Set<Listener>();
  private readonly art = new Map<string, { uri: string | null; at: number }>();
  private readonly artPending = new Set<string>();
  private lastRev = -1;
  refs = 0;

  constructor(
    readonly host: string,
    readonly port: number,
    private readonly d: HubDeps,
  ) {
    this.base = `http://${host}:${port}`;
    this.model = new StationModel(d.clock);
    this.link = new Link({
      url: `ws://${host}:${port}/api/v1/ws?topics=state,now,queue,alerts,program,loudness`,
      makeSocket: d.makeSocket,
      backoff: d.backoff,
      pingEveryMs: d.pingEveryMs,
      deadAfterMs: d.deadAfterMs,
      connectTimeoutMs: d.connectTimeoutMs,
      now: d.clock,
      onOpen: () => {
        this.model.setOpen();
        this.changed();
      },
      onMessage: (text) => this.onMessage(text),
      onDown: () => {
        this.model.reset();
        this.changed();
      },
    });
  }

  start(): void {
    this.link.start();
  }
  stop(): void {
    this.link.stop();
    this.model.reset();
  }
  /** The Mac woke up: nothing held over the sleep is trusted. */
  wake(): void {
    this.model.reset();
    this.changed();
    this.link.kick();
  }

  subscribe(l: Listener): () => void {
    this.listeners.add(l);
    return () => this.listeners.delete(l);
  }

  private changed(): void {
    for (const l of [...this.listeners]) {
      try {
        l();
      } catch (e) {
        this.d.log?.(`listener failed: ${e}`);
      }
    }
  }

  private onMessage(text: string): void {
    let env: Envelope;
    try {
      env = JSON.parse(text) as Envelope;
    } catch {
      return;
    }
    this.model.apply(env);
    if (env.type === "snapshot") void this.refreshProgram();
    if (env.type === "snapshot" || env.type === "now") {
      const m = this.model;
      this.fetchArt(m.now?.artUrl);
      this.fetchArt(m.next?.artUrl);
    }
    if (this.model.rev !== this.lastRev) {
      this.lastRev = this.model.rev;
      this.changed();
    }
  }

  /** The `program/state` answer; the snapshot has none and the `program` event only fires on a change. */
  async refreshProgram(): Promise<void> {
    const r = await this.d.http.get(`${this.base}/api/v1/program/state`);
    if (r.ok && r.data && this.model.online) {
      this.model.applyProgramState(r.data);
      this.changed();
    }
  }

  // ---- art ----------------------------------------------------------------------------------------------------

  /** The cached art for an `art_url` of this station (a data URI), or null when there is none yet. Starts a fetch if needed. */
  artFor(artUrl: string | null | undefined): string | null | undefined {
    if (!artUrl) return null;
    const hit = this.art.get(artUrl);
    if (hit && this.d.clock() - hit.at < ART_TTL_MS) return hit.uri;
    this.fetchArt(artUrl);
    return hit?.uri ?? undefined;
  }

  /** True once the art of this URL is known (loaded, or known to be absent). */
  artKnown(artUrl: string | null | undefined): boolean {
    return !artUrl || this.art.has(artUrl);
  }

  fetchArt(artUrl: string | null | undefined): void {
    if (!artUrl || this.artPending.has(artUrl)) return;
    const hit = this.art.get(artUrl);
    if (hit && this.d.clock() - hit.at < ART_TTL_MS) return;
    this.artPending.add(artUrl);
    void this.d.http.dataUri(`${this.base}${artUrl}`).then((uri) => {
      this.artPending.delete(artUrl);
      this.art.set(artUrl, { uri, at: this.d.clock() });
      while (this.art.size > ART_MAX) {
        const oldest = this.art.keys().next().value;
        if (oldest === undefined) break;
        this.art.delete(oldest);
      }
      this.changed();
    });
  }
}

/** The library service's connection, for the Display View key (the `display` topic) and for the property inspector's lists. */
export class LibraryConn {
  readonly base: string;
  online = false;
  ready = false;
  display: { view: string; station: string | null; rotateS: number } | null = null;
  rev = 0;
  refs = 0;
  private readonly link: Link;
  private readonly listeners = new Set<Listener>();

  constructor(
    readonly host: string,
    readonly port: number,
    private readonly d: HubDeps,
  ) {
    this.base = `http://${host}:${port}`;
    this.link = new Link({
      url: `ws://${host}:${port}/api/v1/ws?topics=display`,
      makeSocket: d.makeSocket,
      backoff: d.backoff,
      pingEveryMs: d.pingEveryMs,
      deadAfterMs: d.deadAfterMs,
      connectTimeoutMs: d.connectTimeoutMs,
      now: d.clock,
      onOpen: () => {
        this.online = true;
        this.rev++;
        this.changed();
      },
      onMessage: (t) => this.onMessage(t),
      onDown: () => {
        this.online = false;
        this.ready = false;
        this.display = null;
        this.rev++;
        this.changed();
      },
    });
  }

  start(): void {
    this.link.start();
  }
  stop(): void {
    this.link.stop();
    this.online = false;
    this.ready = false;
    this.display = null;
  }
  wake(): void {
    this.online = false;
    this.ready = false;
    this.display = null;
    this.rev++;
    this.changed();
    this.link.kick();
  }
  subscribe(l: Listener): () => void {
    this.listeners.add(l);
    return () => this.listeners.delete(l);
  }
  private changed(): void {
    for (const l of [...this.listeners]) {
      try {
        l();
      } catch (e) {
        this.d.log?.(`listener failed: ${e}`);
      }
    }
  }
  private onMessage(text: string): void {
    let env: Envelope;
    try {
      env = JSON.parse(text) as Envelope;
    } catch {
      return;
    }
    const disp = env.type === "snapshot" ? env.data?.display : env.type === "display" ? env.data : undefined;
    if (!disp) return;
    this.ready = true;
    this.display = { view: String(disp.view ?? "single"), station: disp.station ?? null, rotateS: Number(disp.rotate_s ?? 20) };
    this.rev++;
    this.changed();
  }
}

/** Shares one connection per box and station among every key that uses it, and lets it go when the last key leaves. */
export class Hub {
  private readonly stations = new Map<string, StationConn>();
  private readonly libs = new Map<string, LibraryConn>();
  private readonly closing = new Map<string, ReturnType<typeof setTimeout>>();

  constructor(
    readonly deps: HubDeps,
    private readonly graceMs = 10_000,
  ) {}

  station(host: string, port: number): StationConn {
    const key = `s:${host}:${port}`;
    let c = this.stations.get(key);
    if (!c) {
      c = new StationConn(host, port, this.deps);
      this.stations.set(key, c);
      c.start();
    }
    this.cancelClose(key);
    c.refs++;
    return c;
  }

  releaseStation(c: StationConn): void {
    c.refs = Math.max(0, c.refs - 1);
    if (c.refs > 0) return;
    const key = `s:${c.host}:${c.port}`;
    this.closing.set(
      key,
      setTimeout(() => {
        this.closing.delete(key);
        if (c.refs === 0) {
          c.stop();
          this.stations.delete(key);
        }
      }, this.graceMs),
    );
  }

  library(host: string, port: number): LibraryConn {
    const key = `l:${host}:${port}`;
    let c = this.libs.get(key);
    if (!c) {
      c = new LibraryConn(host, port, this.deps);
      this.libs.set(key, c);
      c.start();
    }
    this.cancelClose(key);
    c.refs++;
    return c;
  }

  releaseLibrary(c: LibraryConn): void {
    c.refs = Math.max(0, c.refs - 1);
    if (c.refs > 0) return;
    const key = `l:${c.host}:${c.port}`;
    this.closing.set(
      key,
      setTimeout(() => {
        this.closing.delete(key);
        if (c.refs === 0) {
          c.stop();
          this.libs.delete(key);
        }
      }, this.graceMs),
    );
  }

  private cancelClose(key: string): void {
    const t = this.closing.get(key);
    if (t) clearTimeout(t);
    this.closing.delete(key);
  }

  /** The Mac woke from sleep: every connection starts over, and nothing it showed is kept. */
  wake(): void {
    for (const c of this.stations.values()) c.wake();
    for (const c of this.libs.values()) c.wake();
  }

  get stationCount(): number {
    return this.stations.size;
  }

  dispose(): void {
    for (const t of this.closing.values()) clearTimeout(t);
    this.closing.clear();
    for (const c of this.stations.values()) c.stop();
    for (const c of this.libs.values()) c.stop();
    this.stations.clear();
    this.libs.clear();
  }
}
