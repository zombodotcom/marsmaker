// Headless-Chrome check that real mouse clicks land on MARSSCAPE things in the browser build, at a
// given device pixel ratio. The page runs an --mmo-script whose `report:KIND` steps log where a
// thing is on screen ("mmo-report KIND X Y TARGET", CSS px); this driver moves the real (CDP)
// mouse there, waits for the script's `logpick` line (what the game thinks is under the cursor),
// then clicks. Prints each report / pick / log line and a pass count at the end.
//
//   node tools/web_click.mjs URL [DPR] [SECONDS] [CHROME_EXE]
//
// MM_GPU=1 uses the real GPU (--use-angle=d3d11) instead of SwiftShader; MM_SHOTS=DIR saves a
// screenshot at each pick (device pixels: CSS px x DPR) to check the cursor spot by eye.
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const [url, dpr = '1', secs = '90', chrome = 'C:/Program Files/Google/Chrome/Application/chrome.exe'] = process.argv.slice(2);
const profile = mkdtempSync(join(tmpdir(), 'mm-chrome-'));
const port = 9300 + Math.floor(Math.random() * 500);
const gpu = process.env.MM_GPU ? ['--use-angle=d3d11', '--enable-gpu', '--ignore-gpu-blocklist']
  : ['--enable-unsafe-swiftshader', '--use-angle=swiftshader', '--ignore-gpu-blocklist'];
const proc = spawn(chrome, [
  '--headless=new', `--remote-debugging-port=${port}`, `--user-data-dir=${profile}`,
  '--window-size=1280,720', ...gpu, ...(process.env.MM_EMULATE ? [] : [`--force-device-scale-factor=${dpr}`]), '--no-first-run', '--no-default-browser-check', 'about:blank',
], { stdio: 'ignore' });

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let target;
for (let i = 0; i < 50 && !target; i++) {
  await sleep(200);
  try { target = (await (await fetch(`http://127.0.0.1:${port}/json/list`)).json()).find((t) => t.type === 'page'); } catch {}
}
if (!target) { console.error('could not reach Chrome DevTools'); proc.kill(); process.exit(2); }
const ws = new WebSocket(target.webSocketDebuggerUrl);
await new Promise((r) => ws.addEventListener('open', r));
let id = 0;
const pending = new Map();
const send = (method, params = {}) => new Promise((resolve) => {
  const i = ++id; pending.set(i, resolve); ws.send(JSON.stringify({ id: i, method, params }));
});
const t0 = Date.now();
const stamp = () => ((Date.now() - t0) / 1000).toFixed(1).padStart(5);
let errors = 0, last = null, picks = 0, good = 0, done = false;
const mouse = (type, x, y) => send('Input.dispatchMouseEvent', { type, x, y, button: type === 'mouseMoved' ? 'none' : 'left', buttons: type === 'mousePressed' ? 1 : 0, clickCount: 1 });
ws.addEventListener('message', async (ev) => {
  const m = JSON.parse(ev.data);
  if (m.id && pending.has(m.id)) { pending.get(m.id)(m.result); pending.delete(m.id); return; }
  if (m.method === 'Runtime.consoleAPICalled') {
    const text = m.params.args.map((a) => a.value ?? a.description ?? '').join(' ');
    if (m.params.type === 'error') errors++;
    const rep = text.match(/mmo-report (\S+) ([-\d.]+) ([-\d.]+) (\S+)/);
    if (rep) {
      last = { kind: rep[1], x: +rep[2], y: +rep[3], target: rep[4] };
      console.log(`[${stamp()}] report ${last.kind} at ${last.x},${last.y} ${last.target}`);
      // a few small moves, like a real hand, ending on the target
      for (const [dx, dy] of [[-30, 12], [-12, 5], [0, 0]]) { await mouse('mouseMoved', last.x + dx, last.y + dy); await sleep(60); }
    } else if (/mmo-pick/.test(text)) {
      picks++;
      const ok = last && text.includes(last.target);
      if (ok) good++;
      console.log(`[${stamp()}] ${ok ? 'OK  ' : 'MISS'} ${text.slice(text.indexOf('mmo-pick'), 300)}`);
      const js = await send('Runtime.evaluate', { expression: 'JSON.stringify({dpr: devicePixelRatio, css: [innerWidth, innerHeight], canvas: [document.querySelector("canvas").width, document.querySelector("canvas").height, document.querySelector("canvas").clientWidth]})', returnByValue: true });
      console.log(`[${stamp()}]      page ${js?.result?.value}`);
      if (process.env.MM_SHOTS && last) {
        // what's really under the mouse: a screenshot (device pixels) to check by eye
        const r = await send('Page.captureScreenshot', { format: 'png' });
        writeFileSync(`${process.env.MM_SHOTS}/pick_dpr${dpr}_${picks}.png`, Buffer.from(r.data, 'base64'));
      }
      if (last) { await mouse('mousePressed', last.x, last.y); await sleep(50); await mouse('mouseReleased', last.x, last.y); }
    } else if (/mmo-script: log:|mmo-script: click|WARN|panic/.test(text)) {
      console.log(`[${stamp()}] ${text.slice(text.search(/mmo-script|WARN|panic/), 230)}`);
    } else if (/AppExit|mmo-script: step \d+ quit/.test(text)) {
      done = true;
    }
  } else if (m.method === 'Runtime.exceptionThrown') {
    const text = m.params.exceptionDetails.exception?.description ?? m.params.exceptionDetails.text;
    if (!String(text).includes('Using exceptions for control flow')) { errors++; console.log(`[${stamp()}] EXCEPTION: ${String(text).slice(0, 300)}`); }
  }
});
await send('Runtime.enable'); await send('Page.enable');
// a HiDPI screen: Chrome's own device scale factor, like a real 125% / 150% display. MM_EMULATE=1
// uses DevTools emulation instead, which keeps the canvas backing store at CSS size and so
// misreports positions (Bevy then sees a smaller window): not what real browsers do.
if (process.env.MM_EMULATE) await send('Emulation.setDeviceMetricsOverride', { width: 1280, height: 720, deviceScaleFactor: +dpr, mobile: false });
await send('Page.navigate', { url });
const end = Date.now() + +secs * 1000;
while (Date.now() < end && !done) await sleep(250);
console.log(`dpr ${dpr}: ${good}/${picks} hovers picked the reported thing, ${errors} console errors`);
ws.close(); proc.kill();
await sleep(300);
try { rmSync(profile, { recursive: true, force: true }); } catch {}
process.exit(0);
