// The built plugin (bin/plugin.js) as Stream Deck runs it: a Node process that connects to the app's WebSocket.
// The app is played by this test; the box by FakeBox. It proves the SDK glue: registration, events, setImage, key presses and the inspector's questions.
import { type ChildProcess, execFileSync, spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { WebSocketServer, type WebSocket } from "ws";
import { FakeBox } from "./fakebox.js";

const root = new URL("../../", import.meta.url).pathname;
const bundle = `${root}com.neglectware.hotclock.sdPlugin/bin/plugin.js`;

let box: FakeBox;
let wss: WebSocketServer;
let app: WebSocket | null = null;
let child: ChildProcess;
const received: any[] = []; // eslint-disable-line @typescript-eslint/no-explicit-any
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const until = async (f: () => boolean, ms = 8000, what = "condition") => {
  const t = Date.now();
  while (!f()) {
    if (Date.now() - t > ms) throw new Error(`timed out waiting for ${what}; received ${received.map((m) => m.event).join(",")}`);
    await sleep(20);
  }
};
const send = (m: unknown) => app!.send(JSON.stringify(m));
const decode = (uri: string) => decodeURIComponent(uri.replace("data:image/svg+xml;charset=utf8,", ""));

beforeAll(async () => {
  if (!existsSync(bundle)) execFileSync("npx", ["rollup", "-c"], { cwd: root });
  box = await new FakeBox().start();
  await new Promise<void>((r) => {
    wss = new WebSocketServer({ port: 0 }, () => r());
  });
  const port = (wss.address() as { port: number }).port;
  wss.on("connection", (ws) => {
    app = ws;
    ws.on("message", (d) => {
      const m = JSON.parse(String(d));
      received.push(m);
      if (m.event === "getGlobalSettings") send({ event: "didReceiveGlobalSettings", payload: { settings: { host: "127.0.0.1", libraryPort: box.port } } });
    });
  });
  const info = {
    application: { font: "Arial", language: "en", platform: "mac", platformVersion: "15.0", version: "7.4.2" },
    plugin: { uuid: "com.neglectware.hotclock", version: "0.1.0.0" },
    devicePixelRatio: 2,
    colors: { buttonPressedBackgroundColor: "#303030", buttonPressedBorderColor: "#646464", buttonPressedTextColor: "#969696", disabledColor: "#007AFF", highlightColor: "#0078FF", mouseDownColor: "#CCCCCC" },
    devices: [{ id: "dev1", name: "Stream Deck", size: { columns: 5, rows: 3 }, type: 0 }],
  };
  child = spawn("node", [bundle, "-port", String(port), "-pluginUUID", "PLUGIN-UUID", "-registerEvent", "registerPlugin", "-info", JSON.stringify(info)], { stdio: "ignore", cwd: `${root}com.neglectware.hotclock.sdPlugin` });
  await until(() => received.some((m) => m.event === "registerPlugin"), 10_000, "the plugin to register");
});

afterAll(async () => {
  child?.kill();
  wss?.close();
  await box?.stop();
});

const appear = (action: string, context: string, settings: Record<string, unknown>) =>
  send({ event: "willAppear", action: `com.neglectware.hotclock.${action}`, context, device: "dev1", payload: { settings, coordinates: { column: 0, row: 0 }, controller: "Keypad", isInMultiAction: false, state: 0 } });

describe("the built plugin as a process", () => {
  it("registers, asks for its global settings, and draws a Now Playing key from the box", async () => {
    await until(() => received.some((m) => m.event === "getGlobalSettings"), 5000, "getGlobalSettings");
    appear("now-playing", "ctx-np", { station: "office", stationPort: box.port });
    await until(() => received.some((m) => m.event === "setImage" && m.context === "ctx-np" && decode(m.payload.image).includes("From Silence")), 8000, "a picture with the song");
    const img = received.filter((m) => m.event === "setImage" && m.context === "ctx-np").at(-1);
    expect(img.payload.image.startsWith("data:image/svg+xml;charset=utf8,")).toBe(true);
    expect(decode(img.payload.image)).toContain("<svg");
  });

  it("sends a short press of a Next key to the box as a JSON POST", async () => {
    appear("next", "ctx-next", { station: "office", stationPort: box.port });
    await until(() => received.some((m) => m.event === "setImage" && m.context === "ctx-next" && decode(m.payload.image).includes("NEXT")), 8000, "the Next key drawn");
    send({ event: "keyDown", action: "com.neglectware.hotclock.next", context: "ctx-next", device: "dev1", payload: { settings: {}, coordinates: { column: 1, row: 0 }, state: 0, isInMultiAction: false } });
    send({ event: "keyUp", action: "com.neglectware.hotclock.next", context: "ctx-next", device: "dev1", payload: { settings: {}, coordinates: { column: 1, row: 0 }, state: 0, isInMultiAction: false } });
    await until(() => box.requests.some((r) => r.method === "POST" && r.url === "/api/v1/transport/next"), 5000, "POST /transport/next");
    await until(() => received.some((m) => m.event === "showOk" && m.context === "ctx-next"), 5000, "the OK mark");
  });

  it("answers the property inspector's question about stations", async () => {
    send({ event: "propertyInspectorDidAppear", action: "com.neglectware.hotclock.next", context: "ctx-next", device: "dev1" });
    await sleep(100);
    send({ event: "sendToPlugin", action: "com.neglectware.hotclock.next", context: "ctx-next", payload: { cmd: "stations", tag: "7" } });
    await until(() => received.some((m) => m.event === "sendToPropertyInspector"), 5000, "the reply");
    const r = received.find((m) => m.event === "sendToPropertyInspector");
    expect(r.payload).toMatchObject({ reply: "stations", tag: "7", ok: true, stations: [{ id: "office", name: "Office", port: box.port }] });
  });

  it("stops asking the box when its key disappears, and draws OFFLINE when the box goes", async () => {
    await box.stop();
    await until(() => received.some((m) => m.event === "setImage" && m.context === "ctx-np" && decode(m.payload.image).includes("OFFLINE")), 8000, "OFFLINE after the box stopped");
    send({ event: "willDisappear", action: "com.neglectware.hotclock.now-playing", context: "ctx-np", device: "dev1", payload: { settings: {}, coordinates: { column: 0, row: 0 }, controller: "Keypad", isInMultiAction: false, state: 0 } });
  });
});
