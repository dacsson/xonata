import {createServer} from 'node:http';
import {readFile, mkdir, writeFile} from 'node:fs/promises';
import {spawn} from 'node:child_process';
import {resolve, extname} from 'node:path';
import assert from 'node:assert/strict';

const root = resolve(process.env.XONATA_WEB_ROOT || 'web');
const base = process.env.XONATA_BASE_PATH || '/';
if (!base.startsWith('/') || !base.endsWith('/') || base.includes('..')) throw Error('Invalid base path');
const requests = [];
const responses = new Set();
const completion = (message, reason = 'stop') => ({id:'mock', object:'chat.completion', created:0, model:'mock', choices:[{index:0,message,finish_reason:reason}],usage:{prompt_tokens:1,completion_tokens:1,total_tokens:2}});
const tool = (name, args) => completion({role:'assistant',content:null,tool_calls:[{id:`call-${requests.length}`,type:'function',function:{name,arguments:JSON.stringify(args)}}]},'tool_calls');
const server = createServer(async (req,res) => {
  try {
    const path = new URL(req.url,'http://localhost').pathname;
    if (path === '/v1/chat/completions') {
      const chunks=[]; for await (const chunk of req) chunks.push(chunk);
      const body=JSON.parse(Buffer.concat(chunks)); requests.push(body);
      const content=[...body.messages].reverse().find(m=>m.role==='user')?.content;
      const question=typeof content==='string'?content:content?.map(part=>part.text||'').join('');
      let response;
      if (question==='cancel this run') {
        responses.add(res); res.on('close',()=>responses.delete(res));
        return; // Held until configuration clearing aborts the browser request.
      }
      if (question==='follow up') response=completion({role:'assistant',content:'The earlier answer used observed phase durations.'});
      else if (question==='after cancellation') response=completion({role:'assistant',content:'Fresh run completed.'});
      else if (requests.length===1) response=tool('trace_info',{});
      else if (requests.length===2) response=tool('phase_help',{});
      else if (requests.length===3) response=tool('query_phases',{text:'phase=E',descending:true});
      else response=completion({role:'assistant',content:'Operation 1 takes 4 cycles.'});
      res.writeHead(200,{'content-type':'application/json'}).end(JSON.stringify(response)); return;
    }
    if (!path.startsWith(base)) {res.writeHead(404).end();return;}
    const file=resolve(root,'.'+path.slice(base.length-1));
    const actual=path===base ? resolve(root,'index.html') : file;
    if (!actual.startsWith(root+'/')) {res.writeHead(403).end();return;}
    const data=await readFile(actual);
    res.writeHead(200,{'content-type':({'.html':'text/html','.js':'text/javascript','.wasm':'application/wasm'})[extname(actual)]||'application/octet-stream'}).end(data);
  } catch {res.writeHead(404).end();}
});
await new Promise(done=>server.listen(0,'127.0.0.1',done));
const origin=`http://127.0.0.1:${server.address().port}`;
const chrome=spawn(process.env.XONATA_CHROMIUM||'chromium',[
  '--headless=new','--no-sandbox','--disable-gpu','--enable-unsafe-swiftshader','--window-size=1100,800',
  '--remote-allow-origins=*','--remote-debugging-port=9224',`--user-data-dir=${resolve('target/browser-agent-profile')}`,`${origin}${base}`,
],{stdio:'ignore'});
const sleep=ms=>new Promise(done=>setTimeout(done,ms));
async function wait(check,label) {for(let i=0;i<200;i++){if(await check())return;await sleep(50);}throw Error(`Timed out: ${label}`);}
let ws,call;
try {
  let page;
  await wait(async()=>{try{page=(await(await fetch('http://127.0.0.1:9224/json')).json()).find(p=>p.type==='page');return !!page;}catch{return false;}},'Chromium');
  ws=new WebSocket(page.webSocketDebuggerUrl);await new Promise((done,reject)=>{ws.onopen=done;ws.onerror=reject;});
  let sequence=0;const pending=new Map();const errors=[];
  ws.onmessage=({data})=>{const message=JSON.parse(data);if(message.method==='Runtime.exceptionThrown')errors.push(message.params);const callback=pending.get(message.id);if(callback){pending.delete(message.id);message.error?callback.reject(Error(JSON.stringify(message.error))):callback.resolve(message.result);}};
  call=(method,params={})=>new Promise((resolve,reject)=>{const id=++sequence;pending.set(id,{resolve,reject});ws.send(JSON.stringify({id,method,params}));});
  const evaluate=async expression=>{const result=await call('Runtime.evaluate',{expression,awaitPromise:true,returnByValue:true});if(result.exceptionDetails)throw Error(JSON.stringify(result.exceptionDetails));return result.result.value;};
  await call('Runtime.enable');
  await wait(()=>evaluate('!!window.xonata'),'viewer mount');
  await evaluate(`(async()=>{
    const canvas=document.querySelector('canvas');window.__agentTraceReady=false;
    canvas.addEventListener('xonata:trace',e=>{if(JSON.parse(e.detail).info.complete)window.__agentTraceReady=true;});
    window.xonata.configure_agent(JSON.stringify({base_url:${JSON.stringify(origin+'/v1')},api_key:'mock-session-key',model:'mock'}));
    await window.xonata.open_file(new File(['Kanata\\t0004\\nC=\\t9007199254740993\\nI\\t0\\t0\\t0\\nL\\t0\\t0\\talpha\\nS\\t0\\t0\\tE\\nC\\t2\\nE\\t0\\t0\\tE\\nR\\t0\\t0\\t0\\nI\\t1\\t1\\t0\\nL\\t1\\t0\\tbeta\\nS\\t1\\t0\\tE\\nC\\t4\\nE\\t1\\t0\\tE\\nR\\t1\\t1\\t0\\n'],'agent.kanata'));
  })()`);
  await wait(()=>evaluate('window.__agentTraceReady'),'trace loading');await sleep(350);
  const click=async(x,y)=>{await call('Input.dispatchMouseEvent',{type:'mouseMoved',x,y});await call('Input.dispatchMouseEvent',{type:'mousePressed',x,y,button:'left',clickCount:1});await call('Input.dispatchMouseEvent',{type:'mouseReleased',x,y,button:'left',clickCount:1});await sleep(150);};
  // Ask agent follows the existing View toolbar control.
  const openChat=()=>click(700,20);
  const submit=async text=>{await call('Input.insertText',{text});await call('Input.dispatchKeyEvent',{type:'keyDown',key:'Enter',code:'Enter',windowsVirtualKeyCode:13});await call('Input.dispatchKeyEvent',{type:'keyUp',key:'Enter',code:'Enter',windowsVirtualKeyCode:13});};
  await openChat();await submit('analyze phases');
  await wait(()=>requests.length>=4,'phase tool round trips');await sleep(500);
  const infoText=requests[1].messages.findLast(m=>m.role==='tool').content;
  const info=JSON.parse(typeof infoText==='string'?infoText:infoText.map(c=>c.text||'').join(''));
  assert.equal(info.data.first_cycle,'9007199254740993');
  const resultText=requests[3].messages.findLast(m=>m.role==='tool').content;
  const result=JSON.parse(typeof resultText==='string'?resultText:resultText.map(c=>c.text||'').join(''));
  assert.equal(result.data.status.filter_total,'2');assert.equal(result.data.page.hits[0].elapsed,'4');
  assert.equal(result.data.page.hits[0].source.start,'9007199254740995');
  await openChat();await openChat();await submit('follow up');
  await wait(()=>requests.length>=5,'follow-up');await sleep(400);
  assert(requests[4].messages.some(m=>m.role==='assistant'&&JSON.stringify(m.content).includes('Operation 1 takes 4 cycles.')));
  await openChat();await openChat();await submit('cancel this run');
  await wait(()=>requests.length>=6 && responses.size===1,'pending model request');
  assert(requests[5].messages.some(m=>m.role==='assistant'&&JSON.stringify(m.content).includes('The earlier answer used observed phase durations.')));
  await evaluate('window.xonata.clear_agent_configuration()');await sleep(250);
  await wait(()=>responses.size===0,'request cancellation');
  await evaluate(`window.xonata.configure_agent(JSON.stringify({base_url:${JSON.stringify(origin+'/v1')},api_key:'mock-session-key',model:'mock'}))`);await sleep(200);
  await openChat();await openChat();await submit('after cancellation');
  await wait(()=>requests.length>=7,'new run after cancellation');await sleep(300);
  const finalQuestion=requests[6].messages.findLast(m=>m.role==='user').content;
  assert(JSON.stringify(finalQuestion).includes('after cancellation'));
  assert(!requests[6].messages.some(m=>m.role==='assistant'&&JSON.stringify(m.content).includes('cancel this run')));
  assert.deepEqual(errors,[]);
  await mkdir('target',{recursive:true});const screenshot=await call('Page.captureScreenshot',{format:'png'});
  await writeFile('target/browser-agent.png',Buffer.from(screenshot.data,'base64'));
  console.log('Browser agent checks passed: phase tools, exact cycles, history, cancellation, reconfiguration.');
} catch(error) {
  if(call){const screenshot=await call('Page.captureScreenshot',{format:'png'});await mkdir('target',{recursive:true});await writeFile('target/browser-agent-failure.png',Buffer.from(screenshot.data,'base64'));}
  throw error;
} finally {
  ws?.close();chrome.kill('SIGTERM');for(const response of responses)response.destroy();server.closeAllConnections();server.close();
}
