/** A WebSocket that reconnects by itself and notices when the other end has gone quiet (architecture 8). */

/** The part of the `ws` package's WebSocket that the link uses, so a test can stand in. */
export interface WsLike {
  on(event: "open", cb: () => void): unknown;
  on(event: "close", cb: () => void): unknown;
  on(event: "error", cb: (e: unknown) => void): unknown;
  on(event: "message", cb: (data: unknown) => void): unknown;
  on(event: "pong", cb: () => void): unknown;
  send(data: string): void;
  ping(): void;
  terminate(): void;
}

export interface LinkOptions {
  url: string;
  makeSocket: (url: string) => WsLike;
  onOpen: () => void;
  onMessage: (text: string) => void;
  /** The connection is gone (closed, refused, or silent): show OFFLINE. */
  onDown: () => void;
  /** Delays between connection attempts, ms; the last one repeats. Architecture 8: 1, 2, 5, 10 s. */
  backoff?: number[];
  /** How often the link pings an open connection. */
  pingEveryMs?: number;
  /** A connection with no traffic for this long is dead (the server sends a `timing` event every second while playing). */
  deadAfterMs?: number;
  /** A connection attempt that has not opened after this long is abandoned. */
  connectTimeoutMs?: number;
  now?: () => number;
}

export const DEFAULT_BACKOFF = [1000, 2000, 5000, 10000];

export class Link {
  private ws: WsLike | null = null;
  private attempt = 0;
  private retry: ReturnType<typeof setTimeout> | null = null;
  private watch: ReturnType<typeof setInterval> | null = null;
  private connectTimer: ReturnType<typeof setTimeout> | null = null;
  private lastRx = 0;
  private running = false;
  private open = false;
  /** Bumped on every new socket, so events of an abandoned socket are ignored. */
  private gen = 0;
  private readonly o: Required<Omit<LinkOptions, "now">> & { now: () => number };

  constructor(o: LinkOptions) {
    // An option that is given as `undefined` takes its default, as when the hub passes on what it was not given.
    this.o = {
      url: o.url,
      makeSocket: o.makeSocket,
      onOpen: o.onOpen,
      onMessage: o.onMessage,
      onDown: o.onDown,
      backoff: o.backoff ?? DEFAULT_BACKOFF,
      pingEveryMs: o.pingEveryMs ?? 2000,
      deadAfterMs: o.deadAfterMs ?? 4500,
      connectTimeoutMs: o.connectTimeoutMs ?? 4000,
      now: o.now ?? (() => Date.now()),
    };
  }

  get isOpen(): boolean {
    return this.open;
  }

  start(): void {
    if (this.running) return;
    this.running = true;
    this.connect();
  }

  stop(): void {
    this.running = false;
    this.teardown();
    if (this.retry) clearTimeout(this.retry);
    this.retry = null;
  }

  /** Drop the connection and reconnect at once, with no backoff: the Mac has woken up. */
  kick(): void {
    if (!this.running) return;
    const wasOpen = this.open;
    this.teardown();
    if (this.retry) clearTimeout(this.retry);
    this.retry = null;
    this.attempt = 0;
    if (wasOpen) this.o.onDown();
    this.connect();
  }

  send(text: string): boolean {
    if (!this.ws || !this.open) return false;
    try {
      this.ws.send(text);
      return true;
    } catch {
      return false;
    }
  }

  private connect(): void {
    if (!this.running) return;
    const gen = ++this.gen;
    let ws: WsLike;
    try {
      ws = this.o.makeSocket(this.o.url);
    } catch {
      this.down(gen);
      return;
    }
    this.ws = ws;
    this.lastRx = this.o.now();
    this.connectTimer = setTimeout(() => {
      if (gen === this.gen && !this.open) this.down(gen);
    }, this.o.connectTimeoutMs);
    ws.on("open", () => {
      if (gen !== this.gen) return;
      if (this.connectTimer) clearTimeout(this.connectTimer);
      this.connectTimer = null;
      this.open = true;
      this.lastRx = this.o.now();
      this.watch = setInterval(() => this.check(gen), this.o.pingEveryMs);
      this.o.onOpen();
    });
    ws.on("message", (data) => {
      if (gen !== this.gen) return;
      this.lastRx = this.o.now();
      // The first message of a connection is its snapshot: the connection is good, so start the backoff over.
      this.attempt = 0;
      this.o.onMessage(typeof data === "string" ? data : String(data));
    });
    ws.on("pong", () => {
      if (gen === this.gen) this.lastRx = this.o.now();
    });
    ws.on("error", () => {
      /* a failed attempt is followed by `close`; nothing to do here */
    });
    ws.on("close", () => this.down(gen));
  }

  private check(gen: number): void {
    if (gen !== this.gen || !this.ws) return;
    if (this.o.now() - this.lastRx > this.o.deadAfterMs) {
      this.down(gen);
      return;
    }
    try {
      this.ws.ping();
    } catch {
      this.down(gen);
    }
  }

  private teardown(): void {
    this.gen++;
    if (this.watch) clearInterval(this.watch);
    if (this.connectTimer) clearTimeout(this.connectTimer);
    this.watch = null;
    this.connectTimer = null;
    const ws = this.ws;
    this.ws = null;
    this.open = false;
    if (ws) {
      try {
        ws.terminate();
      } catch {
        /* already gone */
      }
    }
  }

  private down(gen: number): void {
    if (gen !== this.gen) return;
    const wasOpen = this.open;
    this.teardown();
    if (wasOpen || this.attempt === 0) this.o.onDown();
    if (!this.running) return;
    const delay = this.o.backoff[Math.min(this.attempt, this.o.backoff.length - 1)];
    this.attempt++;
    this.retry = setTimeout(() => {
      this.retry = null;
      this.connect();
    }, delay);
  }
}
