// Writes manifest.json and the icon files of the plugin from one table, so the actions, their names and their icons cannot drift apart.
//   npx tsx tools/gen-manifest.ts
import { Resvg } from "@resvg/resvg-js";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { ALL_ACTIONS, PLUGIN_UUID } from "../src/actions/registry.js";
import { glyphPath, type Glyph } from "../src/core/svg.js";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "com.neglectware.hotclock.sdPlugin");

interface Meta { name: string; tooltip: string; glyph: Glyph }
const META: Record<string, Meta> = {
  "now-playing": { name: "Now Playing", tooltip: "The playing song: album art, title, artist and the time left to the segue. Amber under 30 s, red under 10 s.", glyph: "note" },
  "elapsed-remaining": { name: "Elapsed / Remaining", tooltip: "The time elapsed or remaining as big digits. Press to switch.", glyph: "clock" },
  "next-up": { name: "Next Up", tooltip: "The next song and when it starts.", glyph: "list" },
  next: { name: "Next (Segue Now)", tooltip: "Segue to the next song now.", glyph: "next" },
  "fade-next": { name: "Fade to Next", tooltip: "Fade out the playing song and start the next.", glyph: "fade" },
  "hold-resume": { name: "Hold / Resume", tooltip: "Stop after the current song, or release the hold.", glyph: "pause" },
  stop: { name: "Stop", tooltip: "Stop the station. Hold the key for half a second.", glyph: "stop" },
  cut: { name: "Cut", tooltip: "Cut the playing song at once. Hold the key for half a second.", glyph: "cut" },
  "switch-playlist": { name: "Start / Switch Playlist", tooltip: "Switch to a playlist (or start it when the station is stopped).", glyph: "play" },
  "schedule-change": { name: "Schedule Change", tooltip: "Schedule a one-time playlist change at a time or after a delay. Press again to cancel.", glyph: "clock" },
  reshuffle: { name: "Reshuffle", tooltip: "Reshuffle the active playlist and show the new next song.", glyph: "shuffle" },
  "fire-item": { name: "Fire Item (Cart)", tooltip: "Play a library item over the current audio.", glyph: "cart" },
  loudness: { name: "Loudness", tooltip: "The window loudness against the target. Hold to reset the integrated reading.", glyph: "loud" },
  "station-status": { name: "Station Status", tooltip: "Green, amber or red: the station's alerts and whether it answers. Press to open the web UI.", glyph: "status" },
  "program-start": { name: "Start Playlist as Program", tooltip: "Start a playlist as the program now. Hold to cancel tonight's automatic start.", glyph: "play" },
  "program-arm": { name: "Arm / Disarm Program", tooltip: "Arm or disarm the fallback automatic start of a manual program start.", glyph: "clock" },
  "next-block": { name: "Next Block", tooltip: "The next program block and its countdown. Amber under 60 s, red under 10 s.", glyph: "block" },
  "fire-block": { name: "Fire Block", tooltip: "Fire a program block now.", glyph: "block" },
  continue: { name: "Continue", tooltip: "Release a wait block that waits for the operator.", glyph: "continue" },
  "skip-block": { name: "Skip Next Block", tooltip: "Skip the next program block.", glyph: "skip" },
  "abort-program": { name: "Abort Program", tooltip: "End the program without further blocks. Hold the key for half a second.", glyph: "abort" },
  "end-at": { name: "Back-time End", tooltip: "Back-time the program so it ends at a set time; shows the countdown to that end.", glyph: "clock" },
  "display-view": { name: "Display View", tooltip: "Switch what the box's display shows: a station, both, rotating, or the processing view.", glyph: "display" },
};

const iconSvg = (g: Glyph, color: string, bg: string | null, size: number): string =>
  `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 ${bg ? 36 : 24} ${bg ? 36 : 24}">` +
  (bg ? `<rect width="36" height="36" rx="7" fill="${bg}"/><g transform="translate(6 6)">${glyphPath(g, color)}</g>` : glyphPath(g, color)) +
  `</svg>\n`;

const write = (rel: string, text: string): void => {
  const p = join(root, rel);
  mkdirSync(dirname(p), { recursive: true });
  writeFileSync(p, text);
};

const actions = ALL_ACTIONS.map((d) => {
  const m = META[d.id];
  if (!m) throw new Error(`no metadata for ${d.id}`);
  write(`imgs/actions/${d.id}/icon.svg`, iconSvg(m.glyph, "#ffffff", null, 20));
  write(`imgs/actions/${d.id}/key.svg`, iconSvg(m.glyph, "#a3a9b3", "#15171b", 144));
  return {
    Name: m.name,
    UUID: `${PLUGIN_UUID}.${d.id}`,
    Icon: `imgs/actions/${d.id}/icon`,
    Tooltip: m.tooltip,
    PropertyInspectorPath: "ui/pi.html",
    Controllers: ["Keypad"],
    UserTitleEnabled: false,
    States: [{ Image: `imgs/actions/${d.id}/key`, ShowTitle: false }],
  };
});

write("imgs/plugin/category-icon.svg", iconSvg("note", "#ffffff", null, 28));
for (const [name, size] of [["marketplace.png", 256], ["marketplace@2x.png", 512]] as const) {
  const png = new Resvg(iconSvg("note", "#ffffff", "#15171b", size), { fitTo: { mode: "width", value: size } }).render().asPng();
  const p = join(root, "imgs/plugin", name);
  mkdirSync(dirname(p), { recursive: true });
  writeFileSync(p, png);
}

const manifest = {
  $schema: "https://schemas.elgato.com/streamdeck/plugins/manifest.json",
  Name: "Hotclock",
  Version: "0.1.0.0",
  Author: "Neglectware",
  Description: "Control Hotclock radio automation from a Stream Deck: now playing, transport, playlists, program blocks and the box's display.",
  Category: "Hotclock",
  CategoryIcon: "imgs/plugin/category-icon",
  Icon: "imgs/plugin/marketplace",
  UUID: PLUGIN_UUID,
  SDKVersion: 2,
  Software: { MinimumVersion: "6.9" },
  OS: [
    { Platform: "mac", MinimumVersion: "12" },
    { Platform: "windows", MinimumVersion: "10" },
  ],
  Nodejs: { Version: "20", Debug: "enabled" },
  CodePath: "bin/plugin.js",
  Actions: actions,
};
write("manifest.json", JSON.stringify(manifest, null, 2) + "\n");
console.log(`wrote manifest with ${actions.length} actions`);
