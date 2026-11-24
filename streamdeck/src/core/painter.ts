/**
 * Sends what a key shows to Stream Deck at most twice a second and only when it changed (Elgato rate-limits
 * `setImage` and `setTitle`). `sig` says when two values look the same on the key.
 */
export class KeyPainter<T = string> {
  private last: T | null = null;
  private lastSig: string | null = null;
  private lastAt = -Infinity;
  private want: T | null = null;
  private timer: ReturnType<typeof setTimeout> | null = null;
  /** How many pictures have been sent; the tests count them. */
  sent = 0;

  constructor(
    private readonly send: (value: T, prev: T | null) => unknown,
    private readonly clock: () => number,
    private readonly minIntervalMs = 500,
    private readonly sig: (value: T) => string = (v) => String(v),
  ) {}

  /** What was last sent. */
  get current(): T | null {
    return this.last;
  }

  paint(value: T): void {
    this.want = value;
    if (this.lastSig !== null && this.sig(value) === this.lastSig) {
      // the same on the key; keep the newest (its tag and text) for the tests
      this.last = value;
      this.cancel();
      return;
    }
    const wait = this.lastAt + this.minIntervalMs - this.clock();
    if (wait <= 0) {
      this.flush();
    } else if (!this.timer) {
      this.timer = setTimeout(() => {
        this.timer = null;
        // A timer may fire a millisecond early: keep the spacing, so no second ever holds three pictures.
        const left = this.lastAt + this.minIntervalMs - this.clock();
        if (left > 0) {
          if (this.want !== null) this.paint(this.want);
          return;
        }
        this.flush();
      }, wait);
    }
  }

  /** Forget what Stream Deck shows (it was reset, or the key came back), so the next picture is sent. */
  invalidate(): void {
    this.last = null;
    this.lastSig = null;
    this.lastAt = -Infinity;
  }

  private flush(): void {
    this.cancel();
    if (this.want === null) return;
    const sig = this.sig(this.want);
    if (sig === this.lastSig) return;
    const prev = this.last;
    this.last = this.want;
    this.lastSig = sig;
    this.lastAt = this.clock();
    this.sent++;
    try {
      const r = this.send(this.want, prev);
      if (r && typeof (r as Promise<void>).catch === "function") (r as Promise<void>).catch(() => undefined);
    } catch {
      /* the key is gone */
    }
  }

  private cancel(): void {
    if (this.timer) clearTimeout(this.timer);
    this.timer = null;
  }

  dispose(): void {
    this.cancel();
  }
}

/** Tells a short press from a long one. A long press fires while the key is still down, once. */
export class PressTracker {
  private timer: ReturnType<typeof setTimeout> | null = null;
  private longFired = false;
  private down = false;

  constructor(
    private readonly longMs: number,
    private readonly onShort: () => void,
    private readonly onLong: () => void,
  ) {}

  keyDown(): void {
    this.down = true;
    this.longFired = false;
    if (this.timer) clearTimeout(this.timer);
    this.timer = setTimeout(() => {
      this.timer = null;
      if (!this.down) return;
      this.longFired = true;
      this.onLong();
    }, this.longMs);
  }

  keyUp(): void {
    if (!this.down) return;
    this.down = false;
    if (this.timer) clearTimeout(this.timer);
    this.timer = null;
    if (!this.longFired) this.onShort();
  }

  dispose(): void {
    if (this.timer) clearTimeout(this.timer);
    this.timer = null;
    this.down = false;
  }
}
