// Smoke test for the live GitHub Pages build (adapted copy of viewer/tools/web_smoke.mjs): opens the
// page in headless Chrome, streams console messages and exceptions, holds the on-screen throttle
// (window.mmSendKey from play.html's overlay) from 40% to 80% of the run, then saves a screenshot.
//
//   node deploy/smoke_live.mjs URL OUT.png [SECONDS] [CHROME_EXE]
//
// MM_RESOLVE="host=ip" makes Chrome resolve host to ip (for machines whose DNS blocks github.io).
//
// Needs Node >= 22 (built-in WebSocket). Uses a throwaway Chrome profile. WebGL in headless
// Chrome runs on SwiftShader (CPU), so it's slow but real.
import { spawn } from 'node:child_process';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const [url, out, secs = '60', chrome = 'C:/Program Files/Google/Chrome/Application/chrome.exe'] = process.argv.slice(2);
const profile = mkdtempSync(join(tmpdir(), 'mm-chrome-'));
const port = 9333;
const proc = spawn(chrome, [
  '--headless=new', `--remote-debugging-port=${port}`, `--user-data-dir=${profile}`,
  '--window-size=1280,720', '--enable-unsafe-swiftshader', '--use-angle=swiftshader',
  '--ignore-gpu-blocklist', '--no-first-run', '--no-default-browser-check',
  ...(process.env.MM_RESOLVE ? [`--host-resolver-rules=MAP ${process.env.MM_RESOLVE.replace('=', ' ')}`] : []),
  'about:blank',
], { stdio: 'ignore' });

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let target;
for (let i = 0; i < 50 && !target; i++) {
  await sleep(200);
  try {
    const list = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
    target = list.find((t) => t.type === 'page');
  } catch {}
}
if (!target) { console.error('could not reach Chrome DevTools'); proc.kill(); process.exit(2); }

const ws = new WebSocket(target.webSocketDebuggerUrl);
await new Promise((r) => ws.addEventListener('open', r));
let id = 0;
const pending = new Map();
const send = (method, params = {}) => new Promise((resolve) => {
  const i = ++id;
  pending.set(i, resolve);
  ws.send(JSON.stringify({ id: i, method, params }));
});
let errors = 0;
const t0 = Date.now();
const stamp = () => ((Date.now() - t0) / 1000).toFixed(1).padStart(5);
ws.addEventListener('message', (ev) => {
  const m = JSON.parse(ev.data);
  if (m.id && pending.has(m.id)) { pending.get(m.id)(m.result); pending.delete(m.id); return; }
  if (m.method === 'Runtime.consoleAPICalled') {
    const text = m.params.args.map((a) => a.value ?? a.description ?? '').join(' ');
    if (m.params.type === 'error') errors++;
    console.log(`[${stamp()}] console.${m.params.type}: ${text}`.slice(0, 400));
  } else if (m.method === 'Runtime.exceptionThrown') {
    const d = m.params.exceptionDetails;
    const text = d.exception?.description ?? d.text;
    if (!String(text).includes('Using exceptions for control flow')) errors++;
    console.log(`[${stamp()}] EXCEPTION: ${text}`.slice(0, 600));
  } else if (m.method === 'Log.entryAdded') {
    console.log(`[${stamp()}] log.${m.params.entry.level}: ${m.params.entry.text}`.slice(0, 400));
    if (m.params.entry.level === 'error') errors++;
  }
});
await send('Runtime.enable');
await send('Log.enable');
await send('Page.enable');
await send('Page.navigate', { url });
const ms = Number(secs) * 1000;
await sleep(ms * 0.4);
const key = (type) => send('Runtime.evaluate', { expression: `window.mmSendKey ? (window.mmSendKey('${type}', 'ArrowUp'), 'sent') : 'no overlay'`, returnByValue: true });
console.log(`[${stamp()}] throttle down via overlay: ${(await key('keydown'))?.result?.value}`);
await sleep(ms * 0.4);
await key('keyup');
console.log(`[${stamp()}] throttle up`);
await sleep(ms * 0.2);
const shot = await send('Page.captureScreenshot', { format: 'png' });
writeFileSync(out, Buffer.from(shot.data, 'base64'));
const mem = await send('Runtime.evaluate', { expression: 'performance.memory ? Math.round(performance.memory.usedJSHeapSize/1e6) : -1', returnByValue: true });
console.log(`[${stamp()}] screenshot -> ${out}; JS heap ${mem?.result?.value} MB; ${errors} error(s)`);
ws.close();
proc.kill();
await sleep(500);
try { rmSync(profile, { recursive: true, force: true }); } catch {}
process.exit(errors ? 1 : 0);
