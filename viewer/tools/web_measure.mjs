// Headless-Chrome load/memory probe for the browser build: how much memory a page takes and how
// long it blocks the main thread. Samples every second: JS heap, wasm linear memory (captured by
// wrapping WebAssembly.instantiate*), longest main-thread task so far (PerformanceObserver
// 'longtask'), and the first frame/load-done log line. Prints a one-line summary at the end.
//
//   node tools/web_measure.mjs URL [SECONDS] [OUT.png] [CHROME_EXE]
//
// MM_GPU=1 asks Chrome for the real GPU (--use-angle=d3d11) instead of SwiftShader.
import { spawn } from 'node:child_process';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const [url, secs = '60', out, chrome = 'C:/Program Files/Google/Chrome/Application/chrome.exe'] = process.argv.slice(2);
const profile = mkdtempSync(join(tmpdir(), 'mm-chrome-'));
const port = 9300 + Math.floor(Math.random() * 500);
const gpu = process.env.MM_GPU ? ['--use-angle=d3d11', '--enable-gpu', '--ignore-gpu-blocklist']
  : ['--enable-unsafe-swiftshader', '--use-angle=swiftshader', '--ignore-gpu-blocklist'];
const proc = spawn(chrome, [
  '--headless=new', `--remote-debugging-port=${port}`, `--user-data-dir=${profile}`,
  '--window-size=1280,720', ...gpu, '--no-first-run', '--no-default-browser-check', '--enable-precise-memory-info', 'about:blank',
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
let errors = 0, loaded = null, crashed = false;
ws.addEventListener('message', (ev) => {
  const m = JSON.parse(ev.data);
  if (m.id && pending.has(m.id)) { pending.get(m.id)(m.result); pending.delete(m.id); return; }
  if (m.method === 'Runtime.consoleAPICalled') {
    const text = m.params.args.map((a) => a.value ?? a.description ?? '').join(' ');
    if (m.params.type === 'error') errors++;
    if (!loaded && /loaded .* in [0-9.]+s/.test(text)) loaded = (Date.now() - t0) / 1000;
    if (process.env.MM_VERBOSE || m.params.type === 'error' || /loaded|panic|memory|WARN/.test(text)) console.log(`[${stamp()}] ${m.params.type}: ${text}`.slice(0, 300));
  } else if (m.method === 'Runtime.exceptionThrown') {
    const text = m.params.exceptionDetails.exception?.description ?? m.params.exceptionDetails.text;
    if (!String(text).includes('Using exceptions for control flow')) { errors++; console.log(`[${stamp()}] EXCEPTION: ${text}`.slice(0, 500)); }
  } else if (m.method === 'Inspector.targetCrashed') { crashed = true; console.log(`[${stamp()}] RENDERER CRASHED`); }
});
await send('Runtime.enable'); await send('Page.enable'); await send('Inspector.enable');
await send('Page.addScriptToEvaluateOnNewDocument', { source: `
  window.__mm = { mem: null, longest: 0, longSum: 0, frames: 0 };
  for (const k of ['instantiate', 'instantiateStreaming']) {
    const f = WebAssembly[k];
    WebAssembly[k] = async function (...a) { const r = await f.apply(this, a); const inst = r.instance || r;
      for (const v of Object.values(inst.exports)) if (v instanceof WebAssembly.Memory) window.__mm.mem = v; return r; };
  }
  new PerformanceObserver((l) => { for (const e of l.getEntries()) { __mm.longest = Math.max(__mm.longest, e.duration); __mm.longSum += e.duration; } }).observe({ type: 'longtask', buffered: true });
  if (window.WebGL2RenderingContext) {
    // time every synchronous program link wait (wgpu asks LINK_STATUS right after linking)
    const P = WebGL2RenderingContext.prototype, src = new WeakMap(), att = new WeakMap();
    __mm.links = [];
    const ss = P.shaderSource; P.shaderSource = function (sh, t) { src.set(sh, t); return ss.call(this, sh, t); };
    const as = P.attachShader; P.attachShader = function (pr, sh) { (att.get(pr) || att.set(pr, []).get(pr)).push(sh); return as.call(this, pr, sh); };
    const gp = P.getProgramParameter; P.getProgramParameter = function (pr, k) { const t = performance.now(); const r = gp.call(this, pr, k);
      const d = performance.now() - t; if (d > 50) { const fs = (att.get(pr) || []).map((sh) => src.get(sh) || '');
        __mm.links.push({ ms: Math.round(d), len: fs.map((x) => x.length), vs: fs[0], fs: fs[1] }); } return r; };
  }
  const tick = () => { __mm.frames++; requestAnimationFrame(tick); }; requestAnimationFrame(tick);
` });
if (process.env.MM_PROFILE) { await send('Profiler.enable'); await send('Profiler.setSamplingInterval', { interval: 2000 }); await send('Profiler.start'); }
await send('Page.navigate', { url });
let peakWasm = 0, peakHeap = 0, last;
const probe = `JSON.stringify({ wasm: __mm.mem ? __mm.mem.buffer.byteLength/1e6 : 0, heap: performance.memory.usedJSHeapSize/1e6, longest: __mm.longest, longSum: __mm.longSum, frames: __mm.frames })`;
const n = Number(secs);
for (let s = 1; s <= n && !crashed; s++) {
  // a hung main thread doesn't answer Runtime.evaluate; time out so a freeze shows up as gaps
  const r = await Promise.race([send('Runtime.evaluate', { expression: probe, returnByValue: true }), sleep(1500).then(() => null)]);
  if (r?.result?.value) {
    last = JSON.parse(r.result.value);
    peakWasm = Math.max(peakWasm, last.wasm); peakHeap = Math.max(peakHeap, last.heap);
    if (s % 5 === 0) console.log(`[${stamp()}] wasm ${last.wasm.toFixed(0)} MB, heap ${last.heap.toFixed(0)} MB, longest task ${(last.longest / 1000).toFixed(1)} s, blocked ${(last.longSum / 1000).toFixed(1)} s, frames ${last.frames}`);
  } else console.log(`[${stamp()}] (page not answering)`);
  await sleep(1000);
}
if (process.env.MM_LINKS) {
  const r = await send('Runtime.evaluate', { expression: 'JSON.stringify(__mm.links)', returnByValue: true });
  const links = JSON.parse(r.result.value); let tot = 0;
  links.forEach((l, i) => { tot += l.ms; console.log(`LINK ${String(l.ms).padStart(6)} ms  src ${l.len.join('+')}`); if (process.env.MM_LINKS !== '1') { writeFileSync(`${process.env.MM_LINKS}_${i}.frag`, l.fs || ''); writeFileSync(`${process.env.MM_LINKS}_${i}.vert`, l.vs || ''); } });
  console.log(`LINKS ${links.length} slow programs, ${(tot / 1000).toFixed(1)} s total`);
}
if (process.env.MM_PROFILE) {
  // self time per function (top 40) of the whole run: what the main thread was busy with
  const { profile } = await send('Profiler.stop');
  const self = new Map(), byId = new Map(profile.nodes.map((n) => [n.id, n]));
  const dt = profile.timeDeltas; 
  profile.samples.forEach((sid, i) => { const f = byId.get(sid).callFrame; const k = f.functionName || `(${f.url.split('/').pop()})`; self.set(k, (self.get(k) || 0) + (dt[i] || 0)); });
  [...self.entries()].sort((a, b) => b[1] - a[1]).slice(0, 40).forEach(([k, v]) => console.log(`PROF ${(v / 1e6).toFixed(2).padStart(7)} s  ${k.slice(0, 160)}`));
}
if (out && !crashed) {
  const shot = await Promise.race([send('Page.captureScreenshot', { format: 'png' }), sleep(5000).then(() => null)]);
  if (shot?.data) writeFileSync(out, Buffer.from(shot.data, 'base64'));
}
console.log(`SUMMARY ${url} | load ${loaded ?? '-'} s | peak wasm ${peakWasm.toFixed(0)} MB | peak JS heap ${peakHeap.toFixed(0)} MB | longest task ${((last?.longest ?? 0) / 1000).toFixed(1)} s | blocked ${((last?.longSum ?? 0) / 1000).toFixed(1)} s | frames ${last?.frames ?? 0} | errors ${errors}${crashed ? ' | CRASHED' : ''}`);
ws.close(); proc.kill(); await sleep(500);
try { rmSync(profile, { recursive: true, force: true }); } catch {}
