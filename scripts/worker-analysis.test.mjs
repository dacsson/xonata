import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {test} from 'node:test';
import vm from 'node:vm';

// Exercise the real worker dispatcher with a fake engine, including rejected WASM calls.
const source = (await readFile(new URL('../web/worker.js', import.meta.url), 'utf8'))
  .replace(/^import .*;\n/, 'const {init, WorkerEngine} = testModules;\n');
const flush = () => new Promise(setImmediate);

async function worker(analysis, init = async () => {}) {
  const messages = [];
  const self = {};
  class WorkerEngine {
    has_work() { return false; }
    request() { return JSON.stringify({type: 'ready'}); }
  }
  if (analysis) WorkerEngine.prototype.analysis = analysis;
  vm.runInNewContext(source, {
    testModules: {init, WorkerEngine}, self,
    postMessage: text => messages.push(JSON.parse(text)),
    setTimeout: () => {},
  });
  await flush();
  return {
    messages,
    async send(request) {
      self.onmessage({data: {type: 'analysis', payload: JSON.stringify(request)}});
      await flush();
    },
    async ordinaryRequest() {
      self.onmessage({data: {type: 'request', payload: JSON.stringify({type: 'view', trace: 7})}});
      await flush();
    },
  };
}

const request = {trace: 7, session: 3, request: 4, action: {type: 'info'}};

test('thrown analysis calls reply to the requesting tool and leave the queue usable', async () => {
  const host = await worker(async payload => {
    const call = JSON.parse(payload);
    if (call.request === 4) throw Error('storage unavailable');
    return JSON.stringify({type: 'analysis', trace: call.trace, session: call.session,
      request: call.request, result: {Ok: {first_cycle: '9007199254740993'}}});
  });
  await host.send(request);
  const failure = host.messages.find(message => message.type === 'analysis');
  assert(failure, 'A worker exception must resolve the pending tool, not leave it until timeout');
  assert.equal(failure.trace, 7);
  assert.equal(failure.session, 3);
  assert.equal(failure.request, 4);
  assert.match(failure.result.Err, /storage unavailable/);
  await host.send({...request, request: 5});
  assert.deepEqual(host.messages.at(-1).result, {Ok: {first_cycle: '9007199254740993'}});
  await host.ordinaryRequest();
  assert.equal(host.messages.at(-1).type, 'ready');
});

test('older WASM without analysis support returns an actionable tool error', async () => {
  const host = await worker();
  await host.send(request);
  const response = host.messages.find(message => message.type === 'analysis');
  assert(response, 'Missing WASM methods must not strand tool requests');
  assert.match(response.result.Err, /rebuild.*web assets/i);
});

test('initialization failures still return a correlated analysis error', async () => {
  const host = await worker(async () => '', async () => { throw Error('WASM initialization failed'); });
  await host.send(request);
  const response = host.messages.find(message => message.type === 'analysis');
  assert(response);
  assert.equal(response.session, 3);
  assert.equal(response.request, 4);
  assert.match(response.result.Err, /WASM initialization failed/);
});
