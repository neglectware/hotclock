import { parseTime } from "./format.js";

/** One message of the station's WebSocket (architecture 7.4). */
export interface Envelope {
  v?: number;
  type: string;
  station?: string;
  seq?: number;
  at?: string;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  data: any;
}

export interface NowInfo {
  entryId: number | null;
  itemId: number | null;
  title: string;
  artist: string;
  album: string;
  artUrl: string | null;
  startedMs: number;
  segueMs: number | null;
  endsMs: number | null;
  continuityNext: boolean;
  durationMs: number;
}

export interface NextInfo {
  itemId: number | null;
  title: string;
  artist: string;
  artUrl: string | null;
  /** Box wall time the next item starts, epoch ms. */
  startsMs: number | null;
}

export interface Alert {
  id: number;
  code: string;
  severity: "info" | "warning" | "critical" | string;
  message: string;
}

export interface BlockRef {
  blockId: number;
  name: string;
  type: string;
  badge: string;
  /** Local (monotonic) time at which the block is due. */
  dueLocal: number;
}

export interface ProgramInfo {
  running: boolean;
  /** `running`, `waiting`, `halted`, `stopped`; null when no program runs. */
  state: string | null;
  playlistId: number | null;
  playlist: string | null;
  waiting: { blockId: number; name: string; kind: string | null } | null;
  nextBlock: BlockRef | null;
  expected: { playlistId: number; playlist: string | null; label: string; armed: boolean; dueLocal: number } | null;
  backTime: { state: string; label: string | null; deltaS: number | null; targetLocal: number | null } | null;
  blocks: { blockId: number; status: string }[];
  timeZone: string | null;
}

export interface LoudnessInfo {
  target: number | null;
  window: number | null;
  shortTerm: number | null;
  integrated: number | null;
  state: string;
}

export interface Anchor {
  /** Local time the figures were measured at. */
  local: number;
  elapsedMs: number;
  toSegueMs: number | null;
  toEndMs: number | null;
}

/** Everything a key needs to know about one station, built from its snapshot and events. */
export class StationModel {
  /** The WebSocket is open and answering. */
  online = false;
  /** A snapshot has arrived on this connection. */
  ready = false;
  stationId = "";
  displayName = "";
  state = "stopped";
  mode = "auto";
  hold = false;
  safeMode = false;
  health = "ok";
  activePlaylist: { id: number; name: string } | null = null;
  alerts: Alert[] = [];
  now: NowInfo | null = null;
  next: NextInfo | null = null;
  anchor: Anchor | null = null;
  program: ProgramInfo | null = null;
  loudness: LoudnessInfo | null = null;
  /** Box clock minus local clock, ms; learned from every message. */
  offsetMs = 0;
  /** Bumped on every change, so a key can tell whether anything may have changed. */
  rev = 0;
  /** Carts the box reports as playing (`state.carts`), by item. */
  carts: { itemId: number }[] = [];
  /** Items of the queue after the playing one, for art prefetch. */
  queueItemIds: number[] = [];

  constructor(public readonly clock: () => number) {}

  /** The box's wall time now, epoch ms. */
  boxNow(): number {
    return this.clock() + this.offsetMs;
  }

  /** Forget everything from the box: nothing may stay on a key after a disconnect (sleep, restart). */
  reset(): void {
    this.online = false;
    this.ready = false;
    this.state = "stopped";
    this.hold = false;
    this.safeMode = false;
    this.activePlaylist = null;
    this.alerts = [];
    this.now = null;
    this.next = null;
    this.anchor = null;
    this.program = null;
    this.loudness = null;
    this.queueItemIds = [];
    this.carts = [];
    this.rev++;
  }

  setOpen(): void {
    this.online = true;
    this.rev++;
  }

  /** Box time minus local time of recent messages. A message that was held up on the way shows a smaller figure. */
  private offsets: number[] = [];

  /**
   * Learn the box's clock from a message's own time stamp. The best estimate is the largest figure of the last twenty:
   * the message that arrived with the least delay. Returns the local time the box sent the message at.
   */
  private learnOffset(env: Envelope, local: number): number {
    const at = parseTime(env.at) ?? parseTime(env.data?.at);
    if (at === null) return local;
    this.offsets.push(at - local);
    if (this.offsets.length > 20) this.offsets.shift();
    this.offsetMs = Math.max(...this.offsets);
    return Math.min(local, at - this.offsetMs);
  }

  apply(env: Envelope): void {
    const received = this.clock();
    // Figures in a message are as of the moment the box sent it, not the moment this process got round to reading it.
    const local = this.learnOffset(env, received);
    const d = env.data ?? {};
    switch (env.type) {
      case "snapshot": {
        this.ready = true;
        this.stationId = d.station ?? this.stationId;
        this.displayName = d.display_name ?? this.displayName;
        this.setTransport(d);
        this.alerts = Array.isArray(d.alerts) ? d.alerts.map(toAlert) : [];
        this.health = d.health ?? "ok";
        // The snapshot's `now` can lag a stop by up to a second: a stopped station plays nothing.
        this.setNow(this.state === "stopped" ? null : (d.now ?? null), d.next ?? null, local);
        this.setQueue(d.queue, false);
        break;
      }
      case "now":
        this.setNow(d, d.next ?? this.nextRaw, local);
        break;
      case "timing":
        if (this.now && d.entry_id === this.now.entryId) {
          this.anchor = {
            local,
            elapsedMs: num(d.elapsed_ms) ?? 0,
            toSegueMs: num(d.remaining_to_segue_ms),
            toEndMs: num(d.remaining_to_end_ms),
          };
          if (d.next) this.setNext(d.next);
        }
        break;
      case "state":
        this.setTransport(d);
        if (this.state === "stopped") {
          this.now = null;
          this.anchor = null;
        }
        break;
      case "queue":
        this.setQueue(d.entries);
        break;
      case "alert":
        this.alerts = [...this.alerts.filter((a) => a.id !== d.id), toAlert(d)];
        break;
      case "alert_cleared":
        this.alerts = this.alerts.filter((a) => a.id !== d.id);
        break;
      case "program":
        this.setProgram(d, local);
        break;
      case "loudness":
        this.loudness = {
          target: num(d.target),
          window: num(d.window),
          shortTerm: num(d.short_term),
          integrated: num(d.integrated),
          state: String(d.state ?? "off"),
        };
        break;
      default:
        return;
    }
    this.rev++;
  }

  /** The `program/state` answer fetched over REST (the snapshot carries none). */
  applyProgramState(d: unknown): void {
    this.setProgram(d, this.clock());
    this.rev++;
  }

  private nextRaw: unknown = null;

  private setTransport(d: Record<string, unknown>): void {
    if (typeof d.state === "string") this.state = d.state;
    if (typeof d.mode === "string") this.mode = d.mode;
    if (typeof d.hold === "boolean") this.hold = d.hold;
    if (typeof d.safe_mode === "boolean") this.safeMode = d.safe_mode;
    if (Array.isArray(d.carts)) this.carts = (d.carts as { item_id?: number }[]).filter((x) => typeof x.item_id === "number").map((x) => ({ itemId: x.item_id as number }));
    const ap = d.active_playlist as { id?: number; name?: string } | null | undefined;
    if (ap === null) this.activePlaylist = null;
    else if (ap && typeof ap.id === "number") this.activePlaylist = { id: ap.id, name: String(ap.name ?? "") };
  }

  private setNow(n: Record<string, unknown> | null, next: unknown, local: number): void {
    if (!n) {
      this.now = null;
      this.anchor = null;
    } else {
      const started = parseTime(n.started_at) ?? local + this.offsetMs;
      const segue = parseTime(n.segue_at);
      const ends = parseTime(n.ends_at);
      this.now = {
        entryId: num(n.entry_id),
        itemId: num(n.item_id),
        title: String(n.title ?? ""),
        artist: String(n.artist ?? ""),
        album: String(n.album ?? ""),
        artUrl: typeof n.art_url === "string" ? n.art_url : null,
        startedMs: started,
        segueMs: segue,
        endsMs: ends,
        continuityNext: n.continuity_next === true,
        durationMs: num(n.duration_ms) ?? 0,
      };
      // Until the first `timing` event, work from the box's own times and the box clock offset.
      const boxNow = local + this.offsetMs;
      this.anchor = {
        local,
        elapsedMs: Math.max(0, boxNow - started),
        toSegueMs: segue === null ? null : segue - boxNow,
        toEndMs: ends === null ? null : ends - boxNow,
      };
    }
    this.setNext(next);
  }

  private setNext(raw: unknown): void {
    this.nextRaw = raw;
    const n = raw as Record<string, unknown> | null;
    if (!n || typeof n !== "object") {
      this.next = null;
      return;
    }
    const prev = this.next;
    this.next = {
      itemId: num(n.item_id) ?? (prev && prev.title === n.title ? prev.itemId : null),
      title: String(n.title ?? ""),
      artist: String(n.artist ?? ""),
      artUrl: typeof n.art_url === "string" ? n.art_url : prev && prev.title === n.title ? prev.artUrl : null,
      startsMs: parseTime(n.starts_at),
    };
  }

  private setQueue(entries: unknown, deriveNext = true): void {
    if (!Array.isArray(entries)) return;
    const tracks = entries.filter((e) => e && e.kind !== "block" && typeof e.item_id === "number");
    this.queueItemIds = tracks.map((e) => e.item_id as number);
    // The first track of the queue is the next song: a reshuffle or a queue edit changes it at once, not at the next `timing`.
    const head = tracks[0];
    if (!deriveNext) return;
    if (head) {
      this.nextRaw = { item_id: head.item_id, title: head.title, artist: head.artist, art_url: head.art_url ?? null, starts_at: head.planned_start };
      this.setNext(this.nextRaw);
    } else if (this.now) {
      this.next = null;
      this.nextRaw = null;
    }
  }

  private setProgram(raw: unknown, local: number): void {
    const d = (raw ?? {}) as Record<string, unknown>;
    const p = (d.program ?? null) as Record<string, any> | null; // eslint-disable-line @typescript-eslint/no-explicit-any
    const nb = d.next_block as Record<string, any> | null | undefined; // eslint-disable-line @typescript-eslint/no-explicit-any
    const ex = d.expected as Record<string, any> | null | undefined; // eslint-disable-line @typescript-eslint/no-explicit-any
    const bt = d.back_time as Record<string, any> | null | undefined; // eslint-disable-line @typescript-eslint/no-explicit-any
    const wait = p?.waiting as Record<string, any> | null | undefined; // eslint-disable-line @typescript-eslint/no-explicit-any
    const targetBox = parseTime(bt?.target);
    this.program = {
      running: !!p && (p.state === "running" || p.state === "waiting"),
      state: p ? String(p.state ?? "running") : null,
      playlistId: p ? num(p.playlist_id) : null,
      playlist: p ? (p.playlist ?? null) : null,
      waiting: wait && typeof wait.block_id === "number" ? { blockId: wait.block_id, name: String(wait.name ?? ""), kind: wait.kind ?? null } : null,
      nextBlock:
        nb && typeof nb.block_id === "number"
          ? { blockId: nb.block_id, name: String(nb.name ?? ""), type: String(nb.type ?? ""), badge: String(nb.badge ?? ""), dueLocal: local + (num(nb.in_ms) ?? 0) }
          : null,
      expected:
        ex && typeof ex.playlist_id === "number"
          ? { playlistId: ex.playlist_id, playlist: ex.playlist ?? null, label: String(ex.label ?? ""), armed: ex.armed === true, dueLocal: local + (num(ex.in_ms) ?? 0) }
          : null,
      backTime:
        bt && bt.state && bt.state !== "off"
          ? {
              state: String(bt.state),
              label: bt.label ?? null,
              deltaS: num(bt.delta_s),
              targetLocal: targetBox === null ? null : targetBox - this.offsetMs,
            }
          : null,
      blocks: Array.isArray(d.blocks) ? (d.blocks as any[]).map((b) => ({ blockId: b.block_id, status: String(b.status ?? "") })) : [], // eslint-disable-line @typescript-eslint/no-explicit-any
      timeZone: typeof d.time_zone === "string" ? d.time_zone : null,
    };
  }

  // ---- derived figures: always from the anchor and the local clock, never from the box's clock ------------------

  elapsedMs(local = this.clock()): number | null {
    return this.anchor ? this.anchor.elapsedMs + (local - this.anchor.local) : null;
  }

  toSegueMs(local = this.clock()): number | null {
    return this.anchor && this.anchor.toSegueMs !== null ? Math.max(0, this.anchor.toSegueMs - (local - this.anchor.local)) : null;
  }

  toEndMs(local = this.clock()): number | null {
    return this.anchor && this.anchor.toEndMs !== null ? Math.max(0, this.anchor.toEndMs - (local - this.anchor.local)) : null;
  }

  /** The countdown the Now Playing key shows: to the segue, or to the end inside a gapless run. */
  remainingMs(local = this.clock()): { ms: number; to: "segue" | "end" } | null {
    if (!this.now || !this.anchor) return null;
    const end = this.toEndMs(local);
    const seg = this.toSegueMs(local);
    if (this.now.continuityNext && end !== null) return { ms: end, to: "end" };
    if (seg !== null) return { ms: seg, to: "segue" };
    return end !== null ? { ms: end, to: "end" } : null;
  }

  /** Worst alert severity: 0 none, 1 info/warning, 2 critical. */
  status(): "ok" | "warning" | "critical" {
    if (this.alerts.some((a) => a.severity === "critical") || this.health === "critical") return "critical";
    if (this.alerts.some((a) => a.severity === "warning") || this.health === "warning") return "warning";
    return "ok";
  }
}

function num(v: unknown): number | null {
  return typeof v === "number" && Number.isFinite(v) ? v : null;
}

function toAlert(a: Record<string, unknown>): Alert {
  return { id: Number(a.id), code: String(a.code ?? ""), severity: String(a.severity ?? "warning"), message: String(a.message ?? "") };
}
