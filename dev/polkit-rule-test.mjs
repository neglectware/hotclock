// Unit tests of the polkit rule the package installs (packaging/polkit/50-hotclock.rules): every verb on every unit
// the service user might ask for, and the same asks from another user and for another action. The real polkit runs the
// rule in dev/stations-systemd-smoke.sh and dev/display-systemd-smoke.sh; this checks the whole allowlist in a second.
// Run: node dev/polkit-rule-test.mjs   (no polkit, no dependencies)
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import vm from 'node:vm';
const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const rules = [];
const polkit = { Result: { YES: 'yes', NO: 'no', NOT_HANDLED: 'not_handled' }, addRule: f => rules.push(f), log: () => { } };
vm.runInNewContext(readFileSync(join(root, 'packaging/polkit/50-hotclock.rules'), 'utf8'), { polkit });
// What polkit decides when no rule says yes: the default of the action (for these, an administrator's password).
const decide = (user, id, details) => {
  const action = { id, lookup: k => details[k] };
  for (const r of rules) { const v = r(action, { user }); if (v !== undefined && v !== null) return v; }
  return 'default';
};
let pass = 0, fail = 0;
const t = (name, ok, extra = '') => { if (ok) pass++; else { fail++; console.error('FAIL  ' + name + (extra ? '\n      ' + extra : '')); } };
const MANAGE = 'org.freedesktop.systemd1.manage-units';
const ask = (unit, verb, user = 'hotclock', id = MANAGE) => decide(user, id, { unit, verb });
const allowed = (unit, verb) => t(`hotclock may ${verb} ${unit}`, ask(unit, verb) === 'yes', `got ${ask(unit, verb)}`);
const refused = (unit, verb, user, id) => t(`${user || 'hotclock'} may NOT ${verb} ${unit}${id ? ' (' + id + ')' : ''}`, ask(unit, verb, user, id) !== 'yes', `got ${ask(unit, verb, user, id)}`);

t('the file adds exactly one rule', rules.length === 1, `${rules.length} rules`);

// ---- the status display: start and stop (its Settings switch) and restart ----
for (const verb of ['start', 'stop', 'restart']) allowed('hotclock-display.service', verb);
// ---- the stations: start, stop, restart (Stations page) ----
for (const verb of ['start', 'stop', 'restart']) for (const u of ['hotclock-station@office.service', 'hotclock-station@late-night_2.service']) allowed(u, verb);
// ---- the library: restart only (Settings page); it never stops or starts itself ----
allowed('hotclock-library.service', 'restart');
refused('hotclock-library.service', 'start');
refused('hotclock-library.service', 'stop');

// ---- no other verb on any unit: polkit passes reload, kill, reset-failed... as other verbs of the same action ----
const ours = ['hotclock-display.service', 'hotclock-library.service', 'hotclock-station@office.service'];
for (const u of ours) for (const verb of ['reload', 'reload-or-restart', 'try-restart', 'kill', 'reset-failed', 'freeze', 'thaw', 'clean', '', undefined]) refused(u, verb);

// ---- enable and disable of anything: they are another action (manage-unit-files), which carries no unit ----
for (const id of ['org.freedesktop.systemd1.manage-unit-files', 'org.freedesktop.systemd1.reload-daemon', 'org.freedesktop.systemd1.set-environment']) {
  for (const u of [...ours, undefined]) for (const verb of ['enable', 'disable', 'start', 'mask', undefined]) refused(u, verb, 'hotclock', id);
}
// and an `enable` or `disable` named as a verb of manage-units is not one of the three either
for (const u of ours) for (const verb of ['enable', 'disable', 'mask', 'unmask']) refused(u, verb);

// ---- other units: none, including ones that only look like ours ----
for (const u of ['ssh.service', 'systemd-journald.service', 'getty@tty1.service', 'hotclock-kiosk.service', 'hotclock-notify@x.service',
  'hotclock-display.service.d', 'hotclock-display', 'xhotclock-display.service', 'hotclock-display.service ', 'hotclock-display.socket',
  'hotclock-station@..service', 'hotclock-station@.service', 'hotclock-station@Office.service', 'hotclock-station@a b.service',
  'hotclock-station@x.service.bak', 'hotclock-library.service.d', 'multi-user.target', undefined]) {
  for (const verb of ['start', 'stop', 'restart']) refused(u, verb);
}
// ---- other users: none of it ----
for (const user of ['root', 'operator', 'hotclockx', 'nobody']) for (const u of ours) for (const verb of ['start', 'stop', 'restart']) refused(u, verb, user);

console.log(`${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
