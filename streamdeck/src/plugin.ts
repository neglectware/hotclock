import streamDeck, {
  SingletonAction,
  type DidReceiveSettingsEvent,
  type KeyDownEvent,
  type KeyUpEvent,
  type SendToPluginEvent,
  type WillAppearEvent,
  type WillDisappearEvent,
} from "@elgato/streamdeck";
import WebSocket from "ws";
import { ALL_ACTIONS, uuidOf } from "./actions/registry.js";
import { KeyController } from "./core/controller.js";
import type { ActionDef, Env } from "./core/defs.js";
import { Hub } from "./core/hub.js";
import { Http } from "./core/http.js";
import type { GlobalSettings, KeySettings } from "./core/settings.js";
import { handlePiRequest, type PiRequest } from "./pi.js";

const log = (m: string): void => void streamDeck.logger.info(m);

let globals: GlobalSettings = {};
const clock = (): number => performance.now();
const http = new Http();
const hub = new Hub({
  http,
  clock,
  makeSocket: (url) => new WebSocket(url, { handshakeTimeout: 4000 }),
  log,
});
const env: Env = {
  hub,
  http,
  clock,
  globals: () => globals,
  openUrl: (u) => void streamDeck.system.openUrl(u),
  log,
};

const controllers = new Map<string, KeyController>();

function makeAction(def: ActionDef) {
  return class HotclockAction extends SingletonAction<KeySettings> {
    override readonly manifestId = uuidOf(def);

    override onWillAppear(ev: WillAppearEvent<KeySettings>): void {
      if (!ev.action.isKey()) return;
      const action = ev.action;
      const c = new KeyController(
        def,
        {
          setImage: (u) => action.setImage(u),
          showOk: () => action.showOk(),
          showAlert: () => action.showAlert(),
          setSettings: (s) => action.setSettings(s),
        },
        env,
      );
      controllers.get(action.id)?.dispose();
      controllers.set(action.id, c);
      c.setSettings(ev.payload.settings ?? {});
    }

    override onWillDisappear(ev: WillDisappearEvent<KeySettings>): void {
      controllers.get(ev.action.id)?.dispose();
      controllers.delete(ev.action.id);
    }

    override onDidReceiveSettings(ev: DidReceiveSettingsEvent<KeySettings>): void {
      controllers.get(ev.action.id)?.setSettings(ev.payload.settings ?? {});
    }

    override onKeyDown(ev: KeyDownEvent<KeySettings>): void {
      controllers.get(ev.action.id)?.keyDown();
    }

    override onKeyUp(ev: KeyUpEvent<KeySettings>): void {
      controllers.get(ev.action.id)?.keyUp();
    }

    override async onSendToPlugin(ev: SendToPluginEvent<any, KeySettings>): Promise<void> { // eslint-disable-line @typescript-eslint/no-explicit-any
      const req = ev.payload as PiRequest | undefined;
      if (!req || typeof req.cmd !== "string") return;
      const reply = await handlePiRequest(env, req);
      await streamDeck.ui.sendToPropertyInspector(reply as never);
    }
  };
}

for (const def of ALL_ACTIONS) streamDeck.actions.registerAction(new (makeAction(def))());

streamDeck.settings.onDidReceiveGlobalSettings<GlobalSettings>((ev) => {
  globals = ev.settings ?? {};
  for (const c of controllers.values()) c.globalsChanged();
});

// The Mac woke from sleep: start every connection over and show nothing from before.
streamDeck.system.onSystemDidWakeUp(() => {
  log("system woke: reconnecting");
  hub.wake();
  for (const c of controllers.values()) {
    c.painter.invalidate();
    c.refresh();
  }
});

// Redraw four times a second: the countdowns tick between the box's events. A key sends a picture only when it changed, at most twice a second.
setInterval(() => {
  for (const c of controllers.values()) c.refresh();
}, 250);

await streamDeck.connect();
globals = (await streamDeck.settings.getGlobalSettings<GlobalSettings>()) ?? {};
for (const c of controllers.values()) c.globalsChanged();
