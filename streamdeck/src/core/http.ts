/** A small JSON-over-HTTP client for the Hotclock API. It never throws: an answer is `{ok, status, data}`. */

export interface HttpResult<T = any> { // eslint-disable-line @typescript-eslint/no-explicit-any
  ok: boolean;
  /** 0 when the box did not answer at all. */
  status: number;
  data: T | null;
  /** The API's own `error.message` when it refused, else the transport error. */
  error?: string;
}

export type FetchLike = (url: string, init?: RequestInit) => Promise<Response>;

export class Http {
  constructor(
    private readonly fetchFn: FetchLike = (u, i) => fetch(u, i),
    private readonly timeoutMs = 4000,
  ) {}

  async request<T = any>(method: string, url: string, body?: unknown): Promise<HttpResult<T>> { // eslint-disable-line @typescript-eslint/no-explicit-any
    const ctl = new AbortController();
    const timer = setTimeout(() => ctl.abort(), this.timeoutMs);
    try {
      const init: RequestInit = { method, signal: ctl.signal, headers: {} };
      if (method !== "GET") {
        // Every mutating request must say it is JSON (architecture 13).
        (init.headers as Record<string, string>)["Content-Type"] = "application/json";
        init.body = JSON.stringify(body ?? {});
      }
      const r = await this.fetchFn(url, init);
      const text = await r.text();
      let data: T | null = null;
      try {
        data = text ? (JSON.parse(text) as T) : null;
      } catch {
        data = null;
      }
      const err = (data as any)?.error?.message; // eslint-disable-line @typescript-eslint/no-explicit-any
      return { ok: r.ok, status: r.status, data, error: r.ok ? undefined : (err ?? `HTTP ${r.status}`) };
    } catch (e) {
      return { ok: false, status: 0, data: null, error: e instanceof Error ? e.message : String(e) };
    } finally {
      clearTimeout(timer);
    }
  }

  get<T = any>(url: string) { // eslint-disable-line @typescript-eslint/no-explicit-any
    return this.request<T>("GET", url);
  }
  post<T = any>(url: string, body?: unknown) { // eslint-disable-line @typescript-eslint/no-explicit-any
    return this.request<T>("POST", url, body);
  }
  put<T = any>(url: string, body?: unknown) { // eslint-disable-line @typescript-eslint/no-explicit-any
    return this.request<T>("PUT", url, body);
  }

  /** A binary body (album art) as a base64 data URI, or null. */
  async dataUri(url: string): Promise<string | null> {
    const ctl = new AbortController();
    const timer = setTimeout(() => ctl.abort(), this.timeoutMs);
    try {
      const r = await this.fetchFn(url, { signal: ctl.signal });
      if (!r.ok) return null;
      const type = r.headers.get("content-type") ?? "image/jpeg";
      const buf = Buffer.from(await r.arrayBuffer());
      return `data:${type};base64,${buf.toString("base64")}`;
    } catch {
      return null;
    } finally {
      clearTimeout(timer);
    }
  }
}
