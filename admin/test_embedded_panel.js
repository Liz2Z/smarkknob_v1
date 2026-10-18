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
  const api = runInNewContext(source + '\napi', {
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

test('leaving sends an empty stop beacon only for a run owned by this page', () => {
  const calls = [];
  const source = script.match(/^function leave\(.*?(?=window\.addEventListener)/m)[0];
  const context = { ownsSession: false, navigator: { sendBeacon: (...args) => calls.push(args) } };
  runInNewContext(source + '\nleave()', context);
  assert.equal(calls.length, 0);
  context.ownsSession = true;
  runInNewContext('leave(); leave()', context);
  assert.deepEqual(calls, [['/api/stop']]);
  assert.equal(context.ownsSession, false);
});
