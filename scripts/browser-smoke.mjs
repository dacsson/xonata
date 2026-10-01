import { createServer } from 'node:http';
import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { spawn } from 'node:child_process';
import { resolve, join, extname } from 'node:path';

const root = resolve(process.env.XONATA_WEB_ROOT || 'web');
const basePath = process.env.XONATA_BASE_PATH || '/';
const entryPage = process.env.XONATA_ENTRY_PAGE || '';
if (!basePath.startsWith('/') || !basePath.endsWith('/') || basePath.includes('..')) {
  throw new Error('XONATA_BASE_PATH must be an absolute URL path ending in /');
}
const fixtures = resolve('drive-download-20261001T142849Z-1-001');
const server = createServer(async (req, res) => {
  const url = new URL(req.url, 'http://localhost');
  if (url.pathname.startsWith('/fixtures/')) {
    const fixture = resolve(fixtures, '.' + decodeURIComponent(url.pathname.slice('/fixtures'.length)));
    if (!fixture.startsWith(fixtures + '/')) { res.writeHead(403).end(); return; }
    try { res.writeHead(200, {'content-type': 'text/plain'}).end(await readFile(fixture)); }
    catch { res.writeHead(404).end(); }
    return;
  }
  if (!url.pathname.startsWith(basePath)) { res.writeHead(404).end(); return; }
  const path = resolve(root, '.' + decodeURI('/' + url.pathname.slice(basePath.length)));
  if (!path.startsWith(root + '/') && path !== root) { res.writeHead(403).end(); return; }
  const name = path === root ? join(root, 'index.html') : path;
  try {
    const data = await readFile(name);
    const type = { '.html': 'text/html', '.js': 'text/javascript', '.wasm': 'application/wasm' }[extname(name)] || 'application/octet-stream';
    res.writeHead(200, { 'content-type': type }); res.end(data);
  } catch { res.writeHead(404).end(); }
});
await new Promise((done) => server.listen(0, '127.0.0.1', done));
const port = server.address().port;
const chrome = spawn(process.env.XONATA_CHROMIUM || 'chromium', [
  '--headless=new', '--no-sandbox', '--disable-gpu', '--enable-unsafe-swiftshader',
  '--window-size=1100,720',
  '--remote-allow-origins=*', '--remote-debugging-port=9223',
  `--user-data-dir=${resolve('target/browser-smoke-profile')}`,
  `http://127.0.0.1:${port}${basePath}${entryPage}`,
], { stdio: 'ignore' });
let ws;
try {
  let pages;
  for (let i = 0; i < 100; i++) {
    try {
      pages = await (await fetch('http://127.0.0.1:9223/json')).json();
      if (pages?.find((p) => p.type === 'page')) break;
    } catch { /* browser starting */ }
    await new Promise((done) => setTimeout(done, 100));
  }
  const page = pages?.find((p) => p.type === 'page');
  if (!page) throw new Error('Chromium did not start');
  ws = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject; });
  let id = 0;
  let canvasOffset = {x: 0, y: 0};
  const pending = new Map();
  const browserErrors = [];
  ws.onmessage = ({ data }) => {
    const message = JSON.parse(data);
    if (message.method === 'Runtime.exceptionThrown' ||
        (message.method === 'Runtime.consoleAPICalled' && message.params.type === 'error')) {
      browserErrors.push(message.params);
    }
    const entry = pending.get(message.id);
    if (entry) { pending.delete(message.id); message.error ? entry.reject(Error(JSON.stringify(message.error))) : entry.resolve(message.result); }
  };
  function call(method, params = {}) {
    if (method === 'Input.dispatchMouseEvent') {
      params = {...params, x: params.x + canvasOffset.x, y: params.y + canvasOffset.y};
    }
    const request = ++id;
    return new Promise((resolve, reject) => { pending.set(request, {resolve, reject}); ws.send(JSON.stringify({id: request, method, params})); });
  }
  await call('Runtime.enable');
  await new Promise((done) => setTimeout(done, 1200));
  const result = await call('Runtime.evaluate', {
    expression: `(async () => {
      for (let i=0; i<100 && !window.xonata; i++) await new Promise(r=>setTimeout(r,100));
      if (!window.xonata) return {error: document.querySelector('#loading')?.textContent};
      const events=[];
      window.__traceEvents = events;
      const canvas=document.querySelector('canvas');
      window.__selectionEvents = [];
      canvas.addEventListener('xonata:selection', e => window.__selectionEvents.push(e.detail));
      for (const type of ['xonata:trace','xonata:error','xonata:progress'])
        canvas.addEventListener(type, e=>events.push({type, detail:e.detail}));
      const file=new File(['Kanata\\t0004\\nC=\\t100\\nI\\t0\\t0\\t0\\nL\\t0\\t0\\tadd x1,x2,x3\\nS\\t0\\t0\\tF\\nC\\t1\\nR\\t0\\t0\\t0\\n'], 'smoke.kanata');
      window.xonata.open_file(file);
      for (let i=0; i<100 && !events.some(e=>e.type==='xonata:trace' && JSON.parse(e.detail).info.complete); i++) await new Promise(r=>setTimeout(r,100));
      return {events, loading:document.querySelector('#loading')?.textContent ?? null};
    })()`, awaitPromise: true, returnByValue: true,
  });
  if (result.exceptionDetails) throw new Error(JSON.stringify(result.exceptionDetails));
  const value = result.result.value;
  if (value.error || !value.events?.some((e) => e.type === 'xonata:trace' && JSON.parse(e.detail).info.complete)) {
    throw new Error(JSON.stringify(value));
  }
  const bounds = await call('Runtime.evaluate', {
    expression: "(() => { const r = document.querySelector('canvas').getBoundingClientRect(); return {x: r.left, y: r.top}; })()",
    returnByValue: true,
  });
  canvasOffset = bounds.result.value;
  await new Promise((done) => setTimeout(done, 300));
  const initialScreenshot = await call('Page.captureScreenshot', {format: 'png'});
  await mkdir('target', {recursive: true});
  await writeFile('target/browser-before-search.png', Buffer.from(initialScreenshot.data, 'base64'));
  await call('Input.dispatchMouseEvent', {type: 'mousePressed', x: 418, y: 138, button: 'left', clickCount: 1});
  await call('Input.dispatchMouseEvent', {type: 'mouseReleased', x: 418, y: 138, button: 'left', clickCount: 1});
  await new Promise((done) => setTimeout(done, 300));
  const selection = await call('Runtime.evaluate', {expression: 'window.__selectionEvents', returnByValue: true});
  if (!selection.result.value?.some((event) => JSON.parse(event).op_id === '0')) {
    throw new Error('Clicking an instruction did not emit its precise operation ID');
  }
  const inspectorScreenshot = await call('Page.captureScreenshot', {format: 'png'});
  await writeFile('target/browser-inspector.png', Buffer.from(inspectorScreenshot.data, 'base64'));
  await call('Input.dispatchKeyEvent', {type: 'keyDown', key: 'Control', code: 'ControlLeft', windowsVirtualKeyCode: 17, modifiers: 2});
  await call('Input.dispatchKeyEvent', {type: 'keyDown', key: 'f', code: 'KeyF', windowsVirtualKeyCode: 70, modifiers: 2});
  await call('Input.dispatchKeyEvent', {type: 'keyUp', key: 'f', code: 'KeyF', windowsVirtualKeyCode: 70, modifiers: 2});
  await call('Input.dispatchKeyEvent', {type: 'keyUp', key: 'Control', code: 'ControlLeft', windowsVirtualKeyCode: 17});
  await new Promise((done) => setTimeout(done, 200));
  await call('Input.insertText', {text: 'add'});
  await new Promise((done) => setTimeout(done, 500));
  const progress = await call('Runtime.evaluate', {
    expression: "window.__traceEvents.filter(e => e.type === 'xonata:progress').map(e => JSON.parse(e.detail))",
    returnByValue: true,
  });
  if (browserErrors.length || !progress.result.value?.some(e => e.total === 1 && e.done)) {
    throw new Error(`Search did not complete: ${JSON.stringify({browserErrors, progress: progress.result.value})}`);
  }
  const screenshot = await call('Page.captureScreenshot', {format: 'png'});
  await mkdir('target', {recursive: true});
  await writeFile('target/browser-smoke.png', Buffer.from(screenshot.data, 'base64'));
  await call('Input.dispatchKeyEvent', {type: 'keyDown', key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27});
  await call('Input.dispatchKeyEvent', {type: 'keyUp', key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27});
  await new Promise((done) => setTimeout(done, 200));
  await call('Input.dispatchMouseEvent', {type: 'mouseMoved', x: 418, y: 138});
  await call('Input.dispatchKeyEvent', {type: 'keyDown', key: 'Control', code: 'ControlLeft', windowsVirtualKeyCode: 17, modifiers: 2});
  await call('Input.dispatchMouseEvent', {type: 'mouseWheel', x: 418, y: 138, deltaX: 0, deltaY: -120, modifiers: 2});
  await call('Input.dispatchKeyEvent', {type: 'keyUp', key: 'Control', code: 'ControlLeft', windowsVirtualKeyCode: 17});
  await new Promise((done) => setTimeout(done, 350));
  await call('Input.dispatchMouseEvent', {type: 'mouseMoved', x: 600, y: 500});
  const zoomScreenshot = await call('Page.captureScreenshot', {format: 'png'});
  await writeFile('target/browser-zoom.png', Buffer.from(zoomScreenshot.data, 'base64'));
  if (process.argv.includes('--real')) {
    const real = await call('Runtime.evaluate', {
      expression: `(async () => {
        const expected = [
          ['scr_base_lite_kanata_core0(2).log', 9424],
          ['scr_base_lite_kanata_core0.log', 28428],
          ['scr_base_lite_kanata_core0(3).log', 28621],
        ];
        for (const [name] of expected) {
          const response = await fetch('/fixtures/' + encodeURIComponent(name));
          if (!response.ok) throw new Error('Fixture fetch failed: ' + name);
          const blob = await response.blob();
          window.xonata.open_file(new File([blob], name));
        }
        for (let i = 0; i < 300; i++) {
          const infos = window.__traceEvents.filter(e => e.type === 'xonata:trace').map(e => JSON.parse(e.detail).info);
          if (expected.every(([name, count]) => infos.some(info => info.name === name && info.complete && info.count === count))) return {infos};
          await new Promise(r => setTimeout(r, 100));
        }
        return {infos: window.__traceEvents.filter(e => e.type === 'xonata:trace').map(e => JSON.parse(e.detail).info), timeout: true};
      })()`, awaitPromise: true, returnByValue: true,
    });
    if (real.exceptionDetails || real.result.value?.timeout || browserErrors.length) {
      throw new Error(`Real browser traces failed: ${JSON.stringify({real: real.result.value, browserErrors})}`);
    }
    await new Promise((done) => setTimeout(done, 500));
    const realScreenshot = await call('Page.captureScreenshot', {format: 'png'});
    await writeFile('target/browser-real-traces.png', Buffer.from(realScreenshot.data, 'base64'));
    await call('Input.dispatchMouseEvent', {type: 'mousePressed', x: 320, y: 300, button: 'left', clickCount: 1});
    await new Promise((done) => setTimeout(done, 80));
    for (const x of [380, 440, 520, 580]) {
      await call('Input.dispatchMouseEvent', {type: 'mouseMoved', x, y: 300, button: 'left', buttons: 1});
      await new Promise((done) => setTimeout(done, 50));
    }
    await call('Input.dispatchMouseEvent', {type: 'mouseReleased', x: 580, y: 300, button: 'left', clickCount: 1});
    await new Promise((done) => setTimeout(done, 200));
    const expandedScreenshot = await call('Page.captureScreenshot', {format: 'png'});
    await writeFile('target/browser-expanded-disassembly.png', Buffer.from(expandedScreenshot.data, 'base64'));
    await call('Input.dispatchKeyEvent', {type: 'keyDown', key: 'f', code: 'KeyF', windowsVirtualKeyCode: 70});
    await call('Input.dispatchKeyEvent', {type: 'keyUp', key: 'f', code: 'KeyF', windowsVirtualKeyCode: 70});
    await new Promise(done => setTimeout(done, 200));
    await call('Input.insertText', {text: 'vfirst'});
    await new Promise(done => setTimeout(done, 1600));
    const vfirst = await call('Runtime.evaluate', {
      expression: "window.__traceEvents.filter(e => e.type === 'xonata:progress').map(e => JSON.parse(e.detail)).filter(e => e.trace === 4 && e.done)", returnByValue: true,
    });
    if (!vfirst.result.value?.some(e => e.total > 1)) throw new Error('vfirst search failed: ' + JSON.stringify(vfirst));
    const vfirstScreenshot = await call('Page.captureScreenshot', {format: 'png'});
    await writeFile('target/browser-vfirst-search.png', Buffer.from(vfirstScreenshot.data, 'base64'));
    await call('Input.dispatchMouseEvent', {type: 'mouseMoved', x: 650, y: 460});
    await call('Input.dispatchMouseEvent', {type: 'mouseWheel', x: 650, y: 460, deltaX: 0, deltaY: 15000});
    await new Promise(done => setTimeout(done, 700));
    await call('Input.dispatchMouseEvent', {type: 'mouseMoved', x: 950, y: 550});
    const laterResults = await call('Page.captureScreenshot', {format: 'png'});
    await writeFile('target/browser-vfirst-later-results.png', Buffer.from(laterResults.data, 'base64'));
    await call('Input.dispatchKeyEvent', {type: 'keyDown', key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27});
    await call('Input.dispatchKeyEvent', {type: 'keyUp', key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27});
    await new Promise(done => setTimeout(done, 200));
    const selectionsBefore = (await call('Runtime.evaluate', {expression: 'window.__selectionEvents.length', returnByValue: true})).result.value;
    for (const key of ['n', 'n', 'p']) {
      await call('Input.dispatchKeyEvent', {type: 'keyDown', key, code: 'Key' + key.toUpperCase(), windowsVirtualKeyCode: key.toUpperCase().charCodeAt(0)});
      await call('Input.dispatchKeyEvent', {type: 'keyUp', key, code: 'Key' + key.toUpperCase(), windowsVirtualKeyCode: key.toUpperCase().charCodeAt(0)});
      await new Promise(done => setTimeout(done, 300));
    }
    const selectionsAfter = (await call('Runtime.evaluate', {expression: 'window.__selectionEvents.slice(' + selectionsBefore + ')', returnByValue: true})).result.value.map(e => JSON.parse(e));
    if (selectionsAfter.length !== 3 || selectionsAfter[0].op_id !== selectionsAfter[2].op_id || selectionsAfter[0].op_id === selectionsAfter[1].op_id) {
      throw new Error('n/p navigation failed: ' + JSON.stringify(selectionsAfter));
    }
    const jumpScreenshot = await call('Page.captureScreenshot', {format: 'png'});
    await writeFile('target/browser-vfirst-jump.png', Buffer.from(jumpScreenshot.data, 'base64'));
    await call('Input.dispatchKeyEvent', {type: 'keyDown', key: 'p', code: 'KeyP', windowsVirtualKeyCode: 80});
    await call('Input.dispatchKeyEvent', {type: 'keyUp', key: 'p', code: 'KeyP', windowsVirtualKeyCode: 80});
    await new Promise(done => setTimeout(done, 200));
    const wrapScreenshot = await call('Page.captureScreenshot', {format: 'png'});
    await writeFile('target/browser-search-wrap.png', Buffer.from(wrapScreenshot.data, 'base64'));
    await call('Input.dispatchKeyEvent', {type: 'keyDown', key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27});
    await call('Input.dispatchKeyEvent', {type: 'keyUp', key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27});
    await new Promise(done => setTimeout(done, 200));
    await call('Runtime.evaluate', {expression: "window.xonata.navigate(4, '5417')"});
    await new Promise(done => setTimeout(done, 400));
    const opcodeScreenshot = await call('Page.captureScreenshot', {format: 'png'});
    await writeFile('target/browser-vfirst-opcode.png', Buffer.from(opcodeScreenshot.data, 'base64'));
    await call('Input.dispatchMouseEvent', {type: 'mouseMoved', x: 700, y: 400});
    await call('Input.dispatchMouseEvent', {type: 'mouseWheel', x: 700, y: 400, deltaX: 6048, deltaY: 0});
    await new Promise(done => setTimeout(done, 350));
    const executionScreenshot = await call('Page.captureScreenshot', {format: 'png'});
    await writeFile('target/browser-vfirst-execution.png', Buffer.from(executionScreenshot.data, 'base64'));
    await call('Input.dispatchKeyEvent', {type: 'keyDown', key: 'Shift', code: 'ShiftLeft', windowsVirtualKeyCode: 16, modifiers: 8});
    for (const [x, y] of [[700, 400], [950, 480]]) {
      await call('Input.dispatchMouseEvent', {type: 'mousePressed', x, y, button: 'left', clickCount: 1, modifiers: 8});
      await call('Input.dispatchMouseEvent', {type: 'mouseReleased', x, y, button: 'left', clickCount: 1, modifiers: 8});
      await new Promise(done => setTimeout(done, 150));
    }
    await call('Input.dispatchKeyEvent', {type: 'keyUp', key: 'Shift', code: 'ShiftLeft', windowsVirtualKeyCode: 16});
    await call('Input.dispatchMouseEvent', {type: 'mouseMoved', x: 600, y: 560});
    const markersScreenshot = await call('Page.captureScreenshot', {format: 'png'});
    await writeFile('target/browser-markers.png', Buffer.from(markersScreenshot.data, 'base64'));
    await call('Input.dispatchMouseEvent', {type: 'mouseMoved', x: 676, y: 386});
    await call('Input.dispatchKeyEvent', {type: 'keyDown', key: 'Delete', code: 'Delete', windowsVirtualKeyCode: 46});
    await call('Input.dispatchKeyEvent', {type: 'keyUp', key: 'Delete', code: 'Delete', windowsVirtualKeyCode: 46});
    await new Promise(done => setTimeout(done, 200));
    await call('Input.dispatchMouseEvent', {type: 'mouseMoved', x: 600, y: 560});
    const removedScreenshot = await call('Page.captureScreenshot', {format: 'png'});
    await writeFile('target/browser-marker-removed.png', Buffer.from(removedScreenshot.data, 'base64'));
    for (const [key, virtualKey, artifact] of [
      ['ArrowRight', 39, 'browser-horizontal-zoom.png'],
      ['ArrowUp', 38, 'browser-vertical-zoom.png'],
    ]) {
      await call('Input.dispatchKeyEvent', {type: 'keyDown', key, code: key, windowsVirtualKeyCode: virtualKey, modifiers: 2});
      await call('Input.dispatchKeyEvent', {type: 'keyUp', key, code: key, windowsVirtualKeyCode: virtualKey, modifiers: 2});
      await new Promise(done => setTimeout(done, 200));
      const zoom = await call('Page.captureScreenshot', {format: 'png'});
      await writeFile('target/' + artifact, Buffer.from(zoom.data, 'base64'));
    }
    const beforeOverviewClick = (await call('Runtime.evaluate', {expression: 'window.__selectionEvents.length', returnByValue: true})).result.value;
    await call('Input.dispatchMouseEvent', {type: 'mouseMoved', x: 1040, y: 440});
    await call('Input.dispatchMouseEvent', {type: 'mousePressed', x: 1040, y: 440, button: 'left', clickCount: 1});
    await call('Input.dispatchMouseEvent', {type: 'mouseReleased', x: 1040, y: 440, button: 'left', clickCount: 1});
    await new Promise(done => setTimeout(done, 400));
    const overviewSelections = (await call('Runtime.evaluate', {expression: 'window.__selectionEvents.slice(' + beforeOverviewClick + ')', returnByValue: true})).result.value;
    if (overviewSelections.length !== 1 || Number(JSON.parse(overviewSelections[0]).op_id) < 10000) {
      throw new Error('Overview did not jump to the clicked pipeline row: ' + JSON.stringify(overviewSelections));
    }
    await call('Input.dispatchMouseEvent', {type: 'mouseMoved', x: 700, y: 560});
    const overviewJump = await call('Page.captureScreenshot', {format: 'png'});
    await writeFile('target/browser-overview-jump.png', Buffer.from(overviewJump.data, 'base64'));
    await call('Input.dispatchMouseEvent', {type: 'mouseMoved', x: 980, y: 300});
    await call('Input.dispatchMouseEvent', {type: 'mousePressed', x: 980, y: 300, button: 'left', clickCount: 1});
    for (const x of [940, 900, 840, 780]) {
      await call('Input.dispatchMouseEvent', {type: 'mouseMoved', x, y: 300, button: 'left', buttons: 1});
      await new Promise(done => setTimeout(done, 60));
    }
    await call('Input.dispatchMouseEvent', {type: 'mouseReleased', x: 780, y: 300, button: 'left', clickCount: 1});
    await new Promise(done => setTimeout(done, 200));
    const expandedOverview = await call('Page.captureScreenshot', {format: 'png'});
    await writeFile('target/browser-overview-expanded.png', Buffer.from(expandedOverview.data, 'base64'));
    console.log('Whole-trace overview navigation passed');
    console.log('vfirst browser search and n/p navigation passed:', vfirst.result.value.at(-1).total, 'matches');
    console.log('Real browser traces passed:', real.result.value.infos.filter(info => info.complete).map(info => `${info.name}: ${info.count}`).join(', '));
  }
  if (browserErrors.length) throw new Error(`Browser errors: ${JSON.stringify(browserErrors)}`);
  console.log('Browser smoke passed:', value.events.filter((e) => e.type === 'xonata:trace').length, 'trace updates');
} finally {
  ws?.close(); chrome.kill(); server.close();
}
