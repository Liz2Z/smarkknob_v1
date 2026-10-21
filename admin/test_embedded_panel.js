// Board requests have a different wire format from the optional Python proxy.
const { test } = require('node:test');
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { runInNewContext } = require('node:vm');
const { join } = require('node:path');
const html = readFileSync(join(__dirname, '../src/bin/motor_http/panel.html'), 'utf8');
const script = html.split('<script>')[1].split('</script>')[0];

function apiHarness(response = { ok: true, json: async () => ({ running: false }) }) {
  const calls = [];
  const source = script.match(/^async function api\(.*$/m)[0];
  const api = runInNewContext('let mutationEpoch=0;\n'+source + '\napi', {
    AbortSignal, URLSearchParams,
    fetch: async (...args) => { calls.push(args); return response; },
  });
  return { api, calls };
}

test('configuration and start use board form encoding, including 0Hz and reverse', async () => {
  const { api, calls } = apiHarness();
  await api('/api/config', { voltage: 0.5, frequency_hz: 0, direction: 'reverse' });
  await api('/api/start', { duration_ms: 3000 });
  assert.equal(calls[0][1].body, 'voltage=0.5&frequency_hz=0&direction=reverse');
  assert.equal(calls[1][1].body, 'duration_ms=3000');
  for (const [, init] of calls) {
    assert.equal(init.method, 'POST');
    assert.equal(init.headers['Content-Type'], 'application/x-www-form-urlencoded');
  }
});

test('an older status request cannot overwrite a newer control command', async () => {
  let resolveRead;
  const renders=[];
  const apiSource=script.match(/^async function api\(.*$/m)[0];
  const statusSource=script.match(/^async function status\(.*$/m)[0];
  const {api,status}=runInNewContext('let mutationEpoch=0,statusBusy=false,lastPoll=0;\n'+apiSource+'\n'+statusSource+'\n({api,status})',{
    AbortSignal,URLSearchParams,render:s=>renders.push(s),offline:e=>{throw e;},
    fetch:(path)=>path==='/api/status'?new Promise(resolve=>{resolveRead=resolve;}):Promise.resolve({ok:true,json:async()=>({running:true})}),
  });
  const pending=status();
  await api('/api/haptic/start',{duration_ms:3000});
  resolveRead({ok:true,json:async()=>({running:false})});
  await pending;
  assert.equal(renders.length,0);
});

test('status, stop, and keepalive never send a JSON body to the board', async () => {
  const { api, calls } = apiHarness();
  await api('/api/status', undefined, 'GET');
  await api('/api/stop', {});
  await api('/api/keepalive', {});
  assert.equal(calls[0][1].method, 'GET');
  for (const [, init] of calls) assert.equal(init.body, undefined);
});

test('board errors remain visible to the caller', async () => {
  const { api } = apiHarness({ ok: false, status: 409, json: async () => ({ error: 'driver fault' }) });
  await assert.rejects(api('/api/start', { duration_ms: 3000 }), /driver fault/);
});

test('detected magnet with a strength warning still displays live angle', () => {
  const nodes = new Map();
  const node = id => {
    if (!nodes.has(id)) nodes.set(id, {
      style: {}, setAttribute() {}, querySelector() { return this; },
    });
    return nodes.get(id);
  };
  const source = script.slice(script.indexOf('function renderEncoder('), script.indexOf('function route('));
  const context = { $: node, e: { online: true, paused: false, status: 0x73,
    magnet: 'weak', raw_angle: 1804, angle_valid: false, age_ms: 50 } };
  runInNewContext(source + '\nrenderEncoder(e)', context);
  assert.equal(node('encoderAngle').textContent, '158.55');
  assert.match(node('encoderMagnet').textContent, /已检测到/);
  assert.match(node('encoderQuality').textContent, /实时角度/);
  assert.match(node('encoderQuality').textContent, /告警/);
  context.e.status = 0x13;
  context.e.magnet = 'missing';
  runInNewContext('renderEncoder(e)', context);
  assert.match(node('encoderQuality').textContent, /未达到磁铁检测阈值/);
  context.e = null;
  runInNewContext('renderEncoder(e)', context);
  assert.equal(node('encoderAngle').textContent, '—');
});

test('whole panel initializes seven modes without starting a motor or sending heartbeats', async () => {
  const nodes = new Map(), calls = [], intervals = [], events = [];
  const element = () => ({value: '', style: {}, dataset: {}, children: [],
    classList: {toggle() {}}, addEventListener() {}, setAttribute() {},
    querySelector(selector) { return selector==='path'||selector==='circle'?this:null; }, append(...items) { this.children.push(...items); },
    prepend(item) { this.children.unshift(item); }});
  const node = id => { if (!nodes.has(id)) nodes.set(id, element()); return nodes.get(id); };
  const state = {running:false,fault:false,direction:'forward',voltage:1,frequency_hz:8,lease_ms:500,uptime_ms:1,
    haptic:{mode:'detents',active:false,calibrated:false,phase:'stopped',position:0,angle_deg:0,
      strength:0.4,step_deg:15,voltage:0,error:'',sensor_direction:0}};
  const context = {
    document: {getElementById:node,createElement:element,hidden:false,addEventListener(event, fn){events.push([event, fn]);},
      querySelectorAll(selector){return selector==='[data-knob]'?node('knobModes').children:[];}},
    window:{addEventListener(event, fn){events.push([event, fn]);}},location:{origin:'http://device',hash:'#knob'},
    setInterval(fn, ms){intervals.push([fn,ms]);},AbortSignal,URLSearchParams,
    fetch:async(path,init)=>{calls.push([path,init]);return {ok:true,json:async()=>state};},
  };
  runInNewContext(script, context);
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(intervals.map(x=>x[1]),[100]);
  assert.equal(node('knobModes').children.length,7);
  assert.equal(node('pageTitle').textContent,'旋钮手感');
  assert.equal(node('knobStart').disabled,true);
  assert.equal(node('knobCalibration').textContent,'未校准');
  assert.ok(calls.every(([path,init])=>path==='/api/status'&&init.method==='GET'));
  node('knobModes').children[2].onclick();
  assert.equal(node('knobStep').value,60);
  await node('knobApply').onclick();
  const [path,init]=calls.at(-1);
  assert.equal(path,'/api/haptic/config');
  assert.equal(init.body,'mode=switch&strength=0.4&step_deg=60');
  // Losing focus during a start used to POST /stop immediately. The board now
  // owns the session, including when the start response arrives while hidden.
  context.document.hidden=true;
  node('knobDuration').value='3000';
  await node('knobStart').onclick();
  for(const event of ['visibilitychange','pagehide']) {
    for(const [name,fn] of events) if(name===event) fn();
  }
  const postPaths=calls.filter(([,init])=>init.method==='POST').map(([path])=>path);
  assert.ok(postPaths.includes('/api/haptic/start'));
  assert.ok(!postPaths.includes('/api/stop'));
  assert.ok(!postPaths.includes('/api/keepalive'));
  // Network failures only change the dashboard; they never send a motor stop.
  context.fetch=async()=>{throw Error('connection lost');};
  await runInNewContext('status()',context);
  assert.match(node('stateNote').textContent,/板端继续/);
  assert.ok(!calls.some(([path])=>path==='/api/stop'));
});
