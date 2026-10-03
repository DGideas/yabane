// A caller that disconnects mid-stream must still leave one explained Activity
// record, and traffic capture must release its observer instead of staying in
// the "capturing" state. Uses only loopback servers, fake credentials, and a
// temporary data directory.
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { once } from 'node:events';
import { mkdtemp, rm } from 'node:fs/promises';
import http from 'node:http';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
if (!process.argv[2]) {
  const build = spawnSync('cargo', ['build', '--locked'], { cwd: repo, stdio: 'inherit' });
  if (build.error) throw build.error;
  if (build.status !== 0) process.exit(build.status || 1);
}
const binary = process.argv[2] ? path.resolve(process.argv[2]) : path.join(repo, 'target/debug/yabane');
const work = await mkdtemp(path.join(tmpdir(), 'yabane-client-disconnect-'));
const servers = [];
let child;
let serverLog = '';
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
const listen = async handler => {
  const server = http.createServer(handler);
  servers.push(server);
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  return server;
};
const origin = server => `http://127.0.0.1:${server.address().port}`;
const request = (url, options = {}) => fetch(url, {
  ...options, signal: options.signal ?? AbortSignal.timeout(5000),
});

try {
  // A Provider whose event stream keeps flowing for longer than the caller reads.
  let providerRequests = 0;
  const provider = await listen(async (req, res) => {
    if (req.method === 'GET') {
      req.resume();
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end('{"data":[{"id":"stream-test"}]}');
      return;
    }
    let body = '';
    for await (const chunk of req) body += chunk;
    providerRequests++;
    if (!body.includes('"stream":true')) {
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify({
        id: 'chat_1', object: 'chat.completion', model: 'stream-test',
        choices: [{ index: 0, message: { role: 'assistant', content: 'ok' }, finish_reason: 'stop' }],
        usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 },
      }));
      return;
    }
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    for (let index = 0; index < 60; index++) {
      if (res.destroyed) return;
      const chunk = index === 59
        ? 'data: [DONE]\n\n'
        : `data: ${JSON.stringify({ id: 'chat_1', model: 'stream-test', choices: [{ delta: { content: `part-${index} ` }, finish_reason: null }] })}\n\n`;
      res.write(chunk);
      await pause(50);
    }
    res.end();
  });
  const reservation = await listen((req, res) => res.end());
  const base = origin(reservation);
  const port = reservation.address().port;
  await new Promise(resolve => reservation.close(resolve));
  child = spawn(binary, ['--addr', `127.0.0.1:${port}`], {
    cwd: work,
    // Do not inherit production Turnstile, proxy, or Gateway settings.
    env: { PATH: process.env.PATH },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  for (const stream of [child.stdout, child.stderr]) {
    stream.on('data', chunk => { serverLog = (serverLog + chunk).slice(-32000); });
  }
  let ready = false;
  for (let i = 0; i < 100; i++) {
    if (child.exitCode !== null) throw new Error(`Yabane exited: ${serverLog}`);
    try { ready = (await request(`${base}/healthz`)).ok; } catch { /* Not listening yet. */ }
    if (ready) break;
    await pause(50);
  }
  assert.ok(ready, `Yabane did not become ready: ${serverLog}`);
  const json = body => ({ headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });
  const setup = await request(`${base}/admin/setup`, {
    method: 'POST', ...json({ username: 'audit', email: 'audit@example.test', password: 'fixture-password' }),
  });
  assert.equal(setup.status, 204);
  const cookie = setup.headers.get('set-cookie').split(';')[0];
  const admin = async (route, method, body) => {
    const options = json(body);
    const response = await request(`${base}${route}`, {
      method, ...options, headers: { ...options.headers, cookie },
    });
    assert.ok(response.ok, `${route}: ${response.status} ${await response.text()}`);
    return response;
  };
  await admin('/admin/auth', 'PATCH', { enabled: false });
  await admin('/admin/providers', 'POST', {
    id: 'stream', name: 'stream',
    endpoint: {
      id: 'chat', api_type: 'openai_chat_completions', base_url: `${origin(provider)}/v1`,
      requires_credential: true, credential_secret: 'fixture-not-a-real-secret',
    },
  });
  await admin('/admin/extensions/traffic-capture/status', 'PATCH', {
    active: true, remaining: 10, expires_at: Math.floor(Date.now() / 1000) + 3600,
    provider_id: 'stream', endpoint_id: 'chat', model: '', body_limit: 65536,
    retention_days: 1, redacted_headers: [],
  });

  // Read the first events, then disconnect while the Provider is still streaming.
  const abort = new AbortController();
  const response = await request(`${base}/v1/chat/completions`, {
    method: 'POST',
    ...json({ model: 'stream/stream-test', messages: [{ role: 'user', content: 'fixture' }], stream: true }),
    signal: abort.signal,
  });
  assert.equal(response.status, 200);
  const requestId = response.headers.get('x-yabane-request-id');
  assert.ok(requestId, 'the proxy reports the request id to the caller');
  const reader = response.body.getReader();
  const first = await reader.read();
  assert.ok(!first.done && first.value.length > 0, 'the stream started');
  abort.abort();
  await reader.cancel().catch(() => {});
  assert.equal(providerRequests, 1);

  // The record is written asynchronously from the dropped body, so poll briefly.
  let record = null;
  for (let i = 0; i < 100; i++) {
    const logs = await (await request(`${base}/admin/activity/logs?limit=200`, { headers: { cookie } })).json();
    record = logs.find(entry => entry.request_id === requestId);
    if (record) break;
    await pause(50);
  }
  assert.ok(record, `the disconnected request must be recorded (${serverLog})`);
  assert.equal(record.status, 502, 'an unfinished exchange is not recorded as a success');
  assert.equal(record.failure?.stage, 'client');
  assert.equal(record.failure?.category, 'disconnected');
  assert.ok(record.failure.message.includes('disconnected'), record.failure.message);
  assert.ok(!record.failure.message.includes('upstream'), 'Provider-side naming rules apply');

  // Observers finalize too, so capture does not stay in the "capturing" state
  // and no quota is leaked.
  let capture = null;
  for (let i = 0; i < 100; i++) {
    const captures = await (await request(`${base}/admin/extensions/traffic-capture/captures`, { headers: { cookie } })).json();
    capture = captures.find(entry => entry.request_id === requestId);
    if (capture && capture.outcome !== 'capturing') break;
    await pause(50);
  }
  assert.ok(capture, 'the disconnected exchange is still captured');
  assert.equal(capture.outcome, 'interrupted', 'an interrupted exchange finalizes its observer');
  const status = await (await request(`${base}/admin/extensions/traffic-capture/status`, { headers: { cookie } })).json();
  assert.equal(status.config.remaining, 9, 'the finished observer returns its quota');

  // The gateway keeps serving after a caller disconnect.
  const after = await request(`${base}/v1/chat/completions`, {
    method: 'POST',
    ...json({ model: 'stream/stream-test', messages: [{ role: 'user', content: 'fixture' }] }),
  });
  assert.equal(after.status, 200);
  assert.equal(await after.json().then(body => body.choices?.[0]?.message ? true : false), true);
  console.log('Client disconnect passed: record, observer completion, and quota are all consistent');
} finally {
  if (child?.pid && child.exitCode === null) {
    const exited = once(child, 'exit');
    child.kill('SIGTERM');
    const deadline = setTimeout(() => child.kill('SIGKILL'), 5000);
    try { await exited; } finally { clearTimeout(deadline); }
  }
  await Promise.all(servers.map(server => {
    server.closeAllConnections();
    return new Promise(resolve => server.close(resolve));
  }));
  await rm(work, { recursive: true, force: true });
}
