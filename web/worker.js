import init, { WorkerEngine } from './pkg/xonata_web.js';

const ready = init().then(() => new WorkerEngine());
let queue = Promise.resolve();
const pending = new Set();
const lastInfo = new Map();

function emit(json) {
  if (!json) return;
  const value = JSON.parse(json);
  if (Array.isArray(value)) value.forEach((event) => postMessage(JSON.stringify(event)));
  else {
    if (value.type === 'info' && !value.info.complete) {
      const previous = lastInfo.get(value.info.id);
      if (previous === Math.floor(value.info.bytes / (1024 * 1024))) return;
      lastInfo.set(value.info.id, Math.floor(value.info.bytes / (1024 * 1024)));
    }
    postMessage(json);
  }
}

function fail(trace, error) {
  postMessage(JSON.stringify({ type: 'error', trace, message: String(error?.message || error) }));
}

function schedule(trace, task) {
  const run = queue.then(async () => {
    const engine = await ready;
    try { emit(await task(engine)); }
    catch (error) { fail(trace, error); throw error; }
  });
  queue = run.catch(() => {});
  return run;
}

async function load(trace, file) {
  await schedule(trace, (engine) => engine.open(trace, file.name));
  try {
    for (let offset = 0; offset < file.size; offset += 64 * 1024) {
      if (!pending.has(trace)) return;
      const bytes = new Uint8Array(await file.slice(offset, offset + 64 * 1024).arrayBuffer());
      if (!pending.has(trace)) return;
      await schedule(trace, (engine) => engine.feed(trace, bytes));
    }
    if (pending.has(trace)) await schedule(trace, (engine) => engine.finish(trace));
  } catch (_) { /* schedule already reported the failure */ }
  pending.delete(trace);
}

self.onmessage = ({ data }) => {
  if (data.type === 'open') {
    pending.add(data.id);
    void load(data.id, data.file);
  } else if (data.type === 'request') {
    try {
      const request = JSON.parse(data.payload);
      if (request.type === 'close') pending.delete(request.trace);
      void schedule(request.trace, (engine) => engine.request(data.payload)).catch(() => {});
    } catch (error) { fail(0, error); }
  }
};

async function tick() {
  await schedule(0, async (engine) => engine.has_work() ? engine.tick() : '').catch(() => {});
  setTimeout(tick, 16);
}
void tick();
