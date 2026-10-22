import type { ActionDef } from "../core/defs.js";
import { displayActions } from "./display.js";
import { playbackActions } from "./playback.js";
import { programActions } from "./program.js";

export const PLUGIN_UUID = "com.neglectware.hotclock";

export const ALL_ACTIONS: ActionDef[] = [...playbackActions, ...programActions, ...displayActions];

export const uuidOf = (d: ActionDef): string => `${PLUGIN_UUID}.${d.id}`;

export function actionByUuid(uuid: string): ActionDef | undefined {
  return ALL_ACTIONS.find((d) => uuidOf(d) === uuid);
}
