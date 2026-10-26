import {createServer} from 'node:http';
import {readFile, mkdir, writeFile} from 'node:fs/promises';
import {spawn} from 'node:child_process';
import {resolve, extname} from 'node:path';
import assert from 'node:assert/strict';
import {inflateSync} from 'node:zlib';

const root = resolve(process.env.XONATA_WEB_ROOT || 'web');
const base = process.env.XONATA_BASE_PATH || '/';
const workerFailure = process.env.XONATA_TEST_WORKER_FAILURE === '1';
if (!base.startsWith('/') || !base.endsWith('/') || base.includes('..')) throw Error('Invalid base path');
const requests = [];
const responses = new Set();
const completion = (message, reason = 'stop') => ({id:'mock', object:'chat.completion', created:0, model:'mock', choices:[{index:0,message,finish_reason:reason}],usage:{prompt_tokens:1,completion_tokens:1,total_tokens:2}});
const tool = (name, args) => completion({role:'assistant',content:null,tool_calls:[{id:`call-${requests.length}`,type:'function',function:{name,arguments:JSON.stringify(args)}}]},'tool_calls');
const server = createServer(async (req,res) => {
  try {
    const path = new URL(req.url,'http://localhost').pathname;
    if (path === '/v1/chat/completions') {
      assert.equal(req.headers.authorization,'Bearer mock-session-key');
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
      else if (question==='explain format') response=body.messages.at(-1).role==='tool'
        ? completion({role:'assistant',content:'Kanata uses tab-delimited records.'})
        : tool('read_specification',{id:'kanata-trace'});
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
    let data=await readFile(actual);
    if (workerFailure && actual===resolve(root,'worker.js')) {
      const source=data.toString();
      assert(source.includes('return engine.analysis(data.payload);'));
      data=Buffer.from(source.replace('return engine.analysis(data.payload);',
        "if (!self.__testAnalysisFailed) { self.__testAnalysisFailed=true; throw Error('injected analysis worker failure'); } return engine.analysis(data.payload);"));
    }
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
    await window.xonata.open_file(new File(['Kanata\\t0004\\nC=\\t9007199254740993\\nI\\t0\\t0\\t0\\nL\\t0\\t0\\talpha\\nS\\t0\\t0\\tE\\nC\\t2\\nE\\t0\\t0\\tE\\nR\\t0\\t0\\t0\\nI\\t1\\t1\\t0\\nL\\t1\\t0\\tbeta\\nS\\t1\\t0\\tE\\nC\\t4\\nE\\t1\\t0\\tE\\nR\\t1\\t1\\t0\\n'],'agent.kanata'));
  })()`);
  await wait(()=>evaluate('window.__agentTraceReady'),'trace loading');await sleep(350);
  const click=async(x,y)=>{await call('Input.dispatchMouseEvent',{type:'mouseMoved',x,y});await call('Input.dispatchMouseEvent',{type:'mousePressed',x,y,button:'left',clickCount:1});await call('Input.dispatchMouseEvent',{type:'mouseReleased',x,y,button:'left',clickCount:1});await sleep(150);};
  // Ask agent follows the existing View toolbar control.
  const openChat=()=>click(700,20);
  const submit=async text=>{await call('Input.insertText',{text});await call('Input.dispatchKeyEvent',{type:'keyDown',key:'Enter',code:'Enter',windowsVirtualKeyCode:13});await call('Input.dispatchKeyEvent',{type:'keyUp',key:'Enter',code:'Enter',windowsVirtualKeyCode:13});};
  // Configure from the chat form without the console API; opening an unconfigured chat focuses Base URL.
  const press=async(key,virtualKey)=>{await call('Input.dispatchKeyEvent',{type:'keyDown',key,windowsVirtualKeyCode:virtualKey});await call('Input.dispatchKeyEvent',{type:'keyUp',key,windowsVirtualKeyCode:virtualKey});await sleep(100);};
  const replaceField=async text=>{await call('Input.dispatchKeyEvent',{type:'keyDown',key:'a',code:'KeyA',windowsVirtualKeyCode:65,modifiers:2});await call('Input.dispatchKeyEvent',{type:'keyUp',key:'a',code:'KeyA',windowsVirtualKeyCode:65,modifiers:2});await call('Input.insertText',{text});};
  await openChat();
  await replaceField(origin+'/v1');
  await press('Tab',9);await replaceField('mock');
  await press('Tab',9);await call('Input.insertText',{text:'mock-session-key'});
  await press('Tab',9);await press(' ',32);
  await openChat();
  assert.equal(requests.length,0,'Applying connection settings must not call the model');
  // Copy a selected area through the actual browser clipboard, then paste into chat.
  await call('Browser.grantPermissions',{origin,permissions:['clipboardReadWrite','clipboardSanitizedWrite']});
  await evaluate('navigator.clipboard.writeText("")');
  await call('Input.dispatchMouseEvent',{type:'mouseMoved',x:370,y:130,modifiers:2});
  await call('Input.dispatchMouseEvent',{type:'mousePressed',x:370,y:130,button:'left',clickCount:1,modifiers:2});
  await call('Input.dispatchMouseEvent',{type:'mouseMoved',x:650,y:190,button:'left',buttons:1,modifiers:2});await sleep(100);
  await call('Input.dispatchMouseEvent',{type:'mouseReleased',x:650,y:190,button:'left',clickCount:1,modifiers:2});await sleep(400);
  await mkdir('target',{recursive:true});
  const areaScreenshot=await call('Page.captureScreenshot',{format:'png'});
  await writeFile('target/browser-agent-area.png',Buffer.from(areaScreenshot.data,'base64'));
  await click(350,await evaluate('innerHeight - 65'));
  const area=await evaluate('navigator.clipboard.readText()');
  assert(area.includes('Cycles: 9007199254740993–9007199254740997 (inclusive).'),area);
  assert(area.includes('Operation IDs: 0–1 (inclusive interval; IDs may have gaps).'),area);
  assert(area.includes('Unfiltered rows: 0–1 (inclusive, zero-based).'),area);
  await openChat();
  await call('Input.dispatchKeyEvent',{type:'keyDown',key:'v',code:'KeyV',windowsVirtualKeyCode:86,modifiers:2});
  await call('Input.dispatchKeyEvent',{type:'keyUp',key:'v',code:'KeyV',windowsVirtualKeyCode:86});await sleep(200);
  await call('Input.insertText',{text:'existing question'});
  await openChat();
  await click(520,await evaluate('innerHeight - 65'));
  assert.equal(await evaluate('navigator.clipboard.readText()'),area,'Send to agent chat changed the clipboard');
  assert.equal(requests.length,0,'Appending area context must not call the model');
  await call('Input.dispatchKeyEvent',{type:'keyDown',key:'Enter',code:'Enter',windowsVirtualKeyCode:13,modifiers:8});
  await call('Input.dispatchKeyEvent',{type:'keyUp',key:'Enter',code:'Enter',windowsVirtualKeyCode:13,modifiers:8});await sleep(200);
  assert.equal(requests.length,0,'Shift+Enter must insert a newline without sending');
  await submit('analyze phases');
  await wait(()=>requests.length>=4,'phase tool round trips');await sleep(500);
  assert(JSON.stringify(requests[0].messages.findLast(m=>m.role==='user').content).includes('Operation IDs: 0–1'), 'Copied area was not pasted into the question');
  const question=requests[0].messages.findLast(m=>m.role==='user').content;
  const questionText=typeof question==='string'?question:question.map(part=>part.text||'').join('');
  assert.equal(questionText,`${area}existing question\n${area}\n\nanalyze phases`,'Area context must append to the existing draft with surrounding newlines');
  assert(JSON.stringify(requests[0].messages.findLast(m=>m.role==='user').content).includes('\\nanalyze phases'), 'Shift+Enter newline was not preserved');
  const infoText=requests[1].messages.findLast(m=>m.role==='tool').content;
  const infoValue=typeof infoText==='string'?infoText:infoText.map(c=>c.text||'').join('');
  if (workerFailure) {
    assert(infoValue.includes('Analysis worker failed: injected analysis worker failure'), 'Worker failure did not reach the model through the pending tool');
  } else {
    assert.equal(JSON.parse(infoValue).data.first_cycle,'9007199254740993');
  }
  const resultText=requests[3].messages.findLast(m=>m.role==='tool').content;
  const result=JSON.parse(typeof resultText==='string'?resultText:resultText.map(c=>c.text||'').join(''));
  assert.equal(result.data.status.filter_total,'2');assert.equal(result.data.page.hits[0].elapsed,'4');
  assert.equal(result.data.page.hits[0].source.start,'9007199254740995');
  // Keep the compact status above the input, then inspect the latest run's tool details.
  await click(65,196); // Collapse connection settings to leave more room for activity.
  await click(69,520);await sleep(200);
  await click(220,420);await sleep(200); // Expand the first tool in the activity list.
  const activityScreenshot=await call('Page.captureScreenshot',{format:'png'});
  await writeFile('target/browser-agent-activity.png',Buffer.from(activityScreenshot.data,'base64'));
  await click(69,376);await sleep(100);
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
  const source='https://github.com/shioyadan/Konata/blob/master/docs/kanata-log-format.md';
  assert(requests.every(request=>!JSON.stringify(request.messages).includes(source)), 'Unused knowledge entered model context');
  await openChat();await openChat();await submit('explain format');
  await wait(()=>requests.length>=9,'on-demand specification');await sleep(500);
  assert(!JSON.stringify(requests[7].messages).includes(source));
  assert(JSON.stringify(requests[8].messages.findLast(m=>m.role==='tool')).includes(source), 'Requested knowledge missing');
  await openChat();await openChat();await submit('follow up');
  await wait(()=>requests.length>=10,'follow-up after specification');await sleep(300);
  assert(!JSON.stringify(requests[9].messages).includes(source), 'Retrieved pages persisted into follow-up');
  assert.deepEqual(errors,[]);
  await mkdir('target',{recursive:true});const screenshot=await call('Page.captureScreenshot',{format:'png'});
  await writeFile('target/browser-agent.png',Buffer.from(screenshot.data,'base64'));
  await openChat();
  const backgroundPixel=async(x=900,y=550)=>{
    const result=await call('Page.captureScreenshot',{format:'png',clip:{x,y,width:1,height:1,scale:1}});
    const png=Buffer.from(result.data,'base64'),chunks=[];
    for(let offset=8;offset<png.length;){const length=png.readUInt32BE(offset);if(png.toString('ascii',offset+4,offset+8)==='IDAT')chunks.push(png.subarray(offset+8,offset+8+length));offset+=length+12;}
    // A one-pixel PNG has no previous pixels or scanlines for its filter predictors.
    return [...inflateSync(Buffer.concat(chunks)).subarray(1,4)];
  };
  await click(630,20);
  await call('Input.dispatchMouseEvent',{type:'mouseMoved',x:650,y:55});await sleep(350);
  await click(850,72);await press('Escape',27);
  await call('Input.dispatchMouseEvent',{type:'mouseMoved',x:750,y:550});await sleep(250);
  assert.deepEqual(await backgroundPixel(),[38,41,48],'Classic row background was not applied');
  assert.deepEqual(await backgroundPixel(900,506),[45,48,54],'Classic upstream stripe was not applied');
  assert.deepEqual(await backgroundPixel(990,550),[38,41,48],'Classic map background was not applied');
  const classic=await call('Page.captureScreenshot',{format:'png'});
  await writeFile('target/browser-classic.png',Buffer.from(classic.data,'base64'));
  await click(630,20);
  await call('Input.dispatchMouseEvent',{type:'mouseMoved',x:650,y:55});await sleep(350);
  await click(850,48);await press('Escape',27);await sleep(250);
  assert.deepEqual(await backgroundPixel(),[19,23,29],'Dark (better!) theme was not applied');
  console.log('Browser agent checks passed: themes, connection UI, area selection/copy/paste, phase tools, exact cycles, history, cancellation, reconfiguration.');
} catch(error) {
  if(call){const screenshot=await call('Page.captureScreenshot',{format:'png'});await mkdir('target',{recursive:true});await writeFile('target/browser-agent-failure.png',Buffer.from(screenshot.data,'base64'));}
  throw error;
} finally {
  ws?.close();chrome.kill('SIGTERM');for(const response of responses)response.destroy();server.closeAllConnections();server.close();
}
