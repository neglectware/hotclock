import { readFileSync } from "node:fs";
import { JSDOM } from "jsdom";
import { describe, expect, it } from "vitest";
import { ALL_ACTIONS, uuidOf } from "../../src/actions/registry.js";

const dir = new URL("../../com.neglectware.hotclock.sdPlugin/ui/", import.meta.url);
const html = readFileSync(new URL("pi.html", dir), "utf8").replace(/<script[^>]*><\/script>/, "").replace(/<link[^>]*>/, "");
const js = readFileSync(new URL("pi.js", dir), "utf8");
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

class FakeStreamDeck {
  sent: any[] = []; // eslint-disable-line @typescript-eslint/no-explicit-any
  readyState = 1;
  onopen: (() => void) | null = null;
  onmessage: ((e: { data: string }) => void) | null = null;
  constructor() {
    queueMicrotask(() => this.onopen?.());
  }
  send(t: string) {
    this.sent.push(JSON.parse(t));
  }
  deliver(m: unknown) {
    this.onmessage?.({ data: JSON.stringify(m) });
  }
}

function open(action: string, settings: Record<string, unknown> = {}) {
  const dom = new JSDOM(html, { runScripts: "outside-only", pretendToBeVisual: true });
  const w = dom.window as unknown as Window & { WebSocket: unknown; connectElgatoStreamDeckSocket: (...a: unknown[]) => void; __pi: { SPECS: Record<string, unknown[]> } };
  const socks: FakeStreamDeck[] = [];
  w.WebSocket = class extends FakeStreamDeck {
    constructor() {
      super();
      socks.push(this);
    }
  };
  dom.window.eval(js);
  w.connectElgatoStreamDeckSocket(28196, "ctx-1", "registerPropertyInspector", "{}", JSON.stringify({ action: `com.neglectware.hotclock.${action}`, context: "ctx-1", payload: { settings } }));
  return { dom, w, doc: dom.window.document, sd: socks[0] };
}

const type = (doc: Document, id: string, value: string) => {
  const e = doc.getElementById(id) as HTMLInputElement;
  e.value = value;
  e.dispatchEvent(new (doc.defaultView as unknown as { Event: typeof Event }).Event("input", { bubbles: true }));
};
const change = (doc: Document, e: HTMLElement) => e.dispatchEvent(new (doc.defaultView as unknown as { Event: typeof Event }).Event("change", { bubbles: true }));

describe("property inspector", () => {
  it("registers with Stream Deck and asks for the plugin's settings", async () => {
    const { sd } = open("next");
    await sleep(5);
    expect(sd.sent[0]).toEqual({ event: "registerPropertyInspector", uuid: "ctx-1" });
    expect(sd.sent[1]).toEqual({ event: "getGlobalSettings", context: "ctx-1" });
  });

  it("saves the box address as a plugin-wide setting, with no host baked in", async () => {
    const { doc, sd } = open("next");
    expect((doc.getElementById("g-host") as HTMLInputElement).placeholder).toBe("hotclock.local");
    expect((doc.getElementById("g-host") as HTMLInputElement).value).toBe("");
    type(doc, "g-host", "radio.example");
    await sleep(300);
    expect(sd.sent.at(-1)).toEqual({ event: "setGlobalSettings", context: "ctx-1", payload: { host: "radio.example" } });
  });

  it("fills the station list from the box through the plugin, and saves the station and its port", async () => {
    const { doc, sd } = open("next");
    sd.deliver({ event: "didReceiveGlobalSettings", payload: { settings: { host: "radio.example" } } });
    await sleep(5);
    const ask = sd.sent.find((m) => m.event === "sendToPlugin" && m.payload.cmd === "stations");
    expect(ask).toBeTruthy();
    expect(ask.payload.host).toBe("radio.example");
    sd.deliver({ event: "sendToPropertyInspector", payload: { reply: "stations", tag: ask.payload.tag, ok: true, stations: [{ id: "office", name: "Office", port: 8301 }, { id: "eve", name: "Eve", port: 8302 }] } });
    const sel = doc.getElementById("k-station") as HTMLSelectElement;
    expect([...sel.options].map((o) => o.value)).toEqual(["", "office", "eve"]);
    sel.value = "eve";
    change(doc, sel);
    await sleep(300);
    expect(sd.sent.at(-1)).toEqual({ event: "setSettings", context: "ctx-1", payload: { station: "eve", stationPort: 8302 } });
  });

  it("shows the box's error when it cannot list stations", async () => {
    const { doc, sd } = open("next");
    sd.deliver({ event: "didReceiveGlobalSettings", payload: { settings: { host: "radio.example" } } });
    await sleep(5);
    const ask = sd.sent.find((m) => m.event === "sendToPlugin");
    sd.deliver({ event: "sendToPropertyInspector", payload: { reply: "stations", tag: ask.payload.tag, ok: false, error: "No answer from radio.example" } });
    expect((doc.getElementById("station-error") as HTMLElement).textContent).toContain("No answer");
  });

  it("shows the fields of the action and saves a picked playlist with its name", async () => {
    const { doc, sd } = open("switch-playlist", { how: "now" });
    sd.deliver({ event: "didReceiveGlobalSettings", payload: { settings: { host: "radio.example" } } });
    await sleep(5);
    const ask = sd.sent.find((m) => m.event === "sendToPlugin" && m.payload.cmd === "playlists");
    sd.deliver({ event: "sendToPropertyInspector", payload: { reply: "playlists", tag: ask.payload.tag, ok: true, playlists: [{ id: 1, name: "Pre-show" }, { id: 2, name: "Elevator" }] } });
    const sel = doc.querySelector('[data-field="playlist"] select') as HTMLSelectElement;
    sel.value = "2";
    change(doc, sel);
    await sleep(300);
    expect(sd.sent.at(-1).payload).toMatchObject({ playlistId: 2, playlistName: "Elevator", how: "now" });
  });

  it("a program key lists the programs of its station only; Refresh reads the lists again", async () => {
    const { doc, sd } = open("program-start", { station: "eve", stationPort: 8302 });
    sd.deliver({ event: "didReceiveGlobalSettings", payload: { settings: { host: "radio.example" } } });
    await sleep(5);
    const answer = () => {
      const ask = sd.sent.filter((m) => m.event === "sendToPlugin" && m.payload.cmd === "playlists").at(-1);
      sd.deliver({ event: "sendToPropertyInspector", payload: { reply: "playlists", tag: ask.payload.tag, ok: true, playlists: [
        { id: 1, name: "Eve Monday", station: "eve" }, { id: 2, name: "Office Morning", station: "office" }, { id: 3, name: "Any station", station: null }] } });
    };
    answer();
    const names = () => [...(doc.querySelector('[data-field="playlist"] select') as HTMLSelectElement).options].map((o) => o.textContent);
    expect(names()).toEqual(["Choose a playlist", "Eve Monday", "Any station"]);
    const asked = sd.sent.filter((m) => m.payload?.cmd === "playlists").length;
    (doc.getElementById("btn-refresh") as HTMLButtonElement).click();
    await sleep(600);
    expect(sd.sent.filter((m) => m.payload?.cmd === "playlists").length).toBeGreaterThan(asked);
    expect(sd.sent.filter((m) => m.payload?.cmd === "stations").length).toBeGreaterThan(1);
  });

  it("the key face: a label, a colour and the tint are saved with the key", async () => {
    const { doc, sd } = open("program-start", { station: "eve" });
    type(doc, "f-label", "SHOW");
    const c = doc.getElementById("f-color") as HTMLInputElement;
    c.value = "#ff8800";
    c.dispatchEvent(new (doc.defaultView as unknown as { Event: typeof Event }).Event("input", { bubbles: true }));
    const t = doc.getElementById("f-tint") as HTMLInputElement;
    t.checked = true;
    change(doc, t);
    await sleep(300);
    expect(sd.sent.at(-1)).toMatchObject({ event: "setSettings", payload: { station: "eve", label: "SHOW", labelColor: "#ff8800", tintActive: true } });
  });

  it("shows the schedule field that goes with the chosen mode", async () => {
    const { doc } = open("schedule-change", { when: "offset" });
    expect(doc.querySelector('[data-field="offsetMin"]')).toBeTruthy();
    expect(doc.querySelector('[data-field="minute"]')).toBeNull();
    const m = open("schedule-change", {});
    expect(m.doc.querySelector('[data-field="minute"]')).toBeTruthy();
  });

  it("hides the station section for the Display View key, which talks to the box's library", () => {
    const { doc } = open("display-view");
    expect((doc.getElementById("station-section") as HTMLElement).hidden).toBe(true);
  });

  it("has a field set (or a note) for every action that needs settings", () => {
    const { w } = open("next");
    const needs = ["elapsed-remaining", "fade-next", "switch-playlist", "schedule-change", "fire-item", "program-start", "program-arm", "next-block", "fire-block", "abort-program", "end-at", "display-view"];
    for (const id of needs) expect(w.__pi.SPECS[id], id).toBeTruthy();
    for (const d of ALL_ACTIONS) expect(uuidOf(d)).toContain(d.id);
  });
});
