/** Sends a key's picture to Stream Deck at most twice a second and only when it changed (Elgato rate-limits `setImage`). */
export class KeyPainter {
  private last: string | null = null;
  private lastAt = -Infinity;
  private want: string | null = null;
  private timer: ReturnType<typeof setTimeout> | null = null;
  /** How many pictures have been sent; the tests count them. */
  sent = 0;

  constructor(
    private readonly send: (image: string) => unknown,
    private readonly clock: () => number,
    private readonly minIntervalMs = 500,
  ) {}

  paint(image: string): void {
    this.want = image;
    if (image === this.last) {
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
          this.paint(this.want ?? "");
          return;
        }
        this.flush();
      }, wait);
    }
  }

  /** Forget what Stream Deck shows (it was reset, or the key came back), so the next picture is sent. */
  invalidate(): void {
    this.last = null;
    this.lastAt = -Infinity;
  }

  private flush(): void {
    this.cancel();
    if (this.want === null || this.want === this.last) return;
    this.last = this.want;
    this.lastAt = this.clock();
    this.sent++;
    try {
      const r = this.send(this.want);
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
