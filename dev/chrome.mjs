// The headless Chrome of the UI checks, which must not outlive its check. `launchChrome(path, args)` starts it in a process
// group of its own and removes it (the group: browser, renderers, GPU and utility processes) and its --user-data-dir:
//   - when the check ends: normally, by process.exit, on an exception or a rejected promise nobody caught;
//   - on SIGINT, SIGTERM and SIGHUP (Ctrl-C, `timeout`, a closed terminal; the .sh wrappers also pass theirs on);
//   - when the check itself is killed outright (SIGKILL): a small watchdog shell polls the check's pid once a second.
// A Chrome left running for a day fills a Mac's disk (its code_sign_clone copies in the temp folder).
import { spawn } from 'node:child_process';
import { rmSync } from 'node:fs';

export function launchChrome(path, args, opts = {}) {
  const chrome = spawn(path, args, { stdio: 'ignore', ...opts, detached: true });
  const profile = args.find(a => a.startsWith('--user-data-dir='))?.slice('--user-data-dir='.length);
  let done = false;
  const kill = () => {
    if (done) return; done = true;
    try { process.kill(-chrome.pid, 'SIGKILL'); } catch { try { chrome.kill('SIGKILL'); } catch { } }
    if (profile) try { rmSync(profile, { recursive: true, force: true }); } catch { }
  };
  process.on('exit', kill);
  for (const [sig, code] of [['SIGINT', 130], ['SIGTERM', 143], ['SIGHUP', 129]]) process.on(sig, () => { kill(); process.exit(code); });
  process.on('uncaughtException', e => { console.error(e); kill(); process.exit(1); });
  process.on('unhandledRejection', e => { console.error(e); kill(); process.exit(1); });
  if (chrome.pid) {
    const q = s => `'${String(s).replace(/'/g, `'\\''`)}'`;
    const watch = spawn('sh', ['-c', `while kill -0 ${process.pid} 2>/dev/null; do sleep 1; done; kill -9 -${chrome.pid} 2>/dev/null; ${profile ? 'rm -rf ' + q(profile) : 'true'}`], { stdio: 'ignore', detached: true });
    watch.unref();
  }
  // the check's own chrome.kill() (in its finally) ends the whole group too
  const own = chrome.kill.bind(chrome);
  chrome.kill = sig => { kill(); return own(sig); };
  return chrome;
}
