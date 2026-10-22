import { createServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { WebSocketServer, type WebSocket } from "ws";
import { BOX0, iso, nowJson, snapshot } from "./helpers.js";

/** A fake Hotclock station: a WebSocket that sends a snapshot then events, a few REST routes, and art. Real sockets on loopback. */
export class FakeBox {
  server!: Server;
  wss!: WebSocketServer;
  port = 0;
  sockets = new Set<WebSocket>();
  requests: { method: string; url: string; body: string }[] = [];
  connections = 0;
  snapshotData: Record<string, unknown> = {};
  /** Answers `GET /api/v1/program/state`. */
  program: unknown = { running: false, program: null, next_block: null, expected: null, back_time: { state: "off" }, blocks: [] };
  /** When true the server accepts a socket and then says nothing, and ignores pings. */
  silent = false;
  art = Buffer.from("fakejpeg");

  async start(port = 0): Promise<this> {
    this.server = createServer((req, res) => {
      let body = "";
      req.on("data", (d) => (body += d));
      req.on("end", () => {
        this.requests.push({ method: req.method ?? "", url: req.url ?? "", body });
        if (req.url?.startsWith("/api/v1/items/") && req.url.includes("/art")) {
          res.writeHead(200, { "content-type": "image/jpeg" });
          res.end(this.art);
        } else if (req.url === "/api/v1/stations") {
          res.writeHead(200, { "content-type": "application/json" });
          res.end(JSON.stringify({ stations: [{ id: "office", display_name: "Office", listen: `0.0.0.0:${this.port}` }] }));
        } else if (req.url === "/health") {
          res.writeHead(200, { "content-type": "application/json" });
          res.end(JSON.stringify({ status: "ok" }));
        } else if (req.url === "/api/v1/program/state") {
          res.writeHead(200, { "content-type": "application/json" });
          res.end(JSON.stringify(this.program));
        } else {
          res.writeHead(202, { "content-type": "application/json" });
          res.end("{}");
        }
      });
    });
    this.wss = new WebSocketServer({ server: this.server, path: "/api/v1/ws" });
    this.wss.on("connection", (ws) => {
      this.connections++;
      this.sockets.add(ws);
      ws.on("close", () => this.sockets.delete(ws));
      if (!this.silent) ws.send(JSON.stringify(snapshot(this.snapshotData)));
    });
    await new Promise<void>((r) => this.server.listen(port, "127.0.0.1", r));
    this.port = (this.server.address() as AddressInfo).port;
    return this;
  }

  send(msg: unknown): void {
    for (const s of this.sockets) s.send(JSON.stringify(msg));
  }

  /** Stop answering, as a box does when its network goes away: the socket stays open, nothing comes back. */
  goSilent(): void {
    this.silent = true;
    for (const s of this.sockets) (s as unknown as { _socket: { pause(): void } })._socket.pause();
  }

  async stop(): Promise<void> {
    for (const s of this.sockets) s.terminate();
    this.sockets.clear();
    await new Promise<void>((r) => this.wss.close(() => r()));
    await new Promise<void>((r) => {
      this.server.close(() => r());
      this.server.closeAllConnections?.();
    });
  }
}

export { BOX0, iso, nowJson };
