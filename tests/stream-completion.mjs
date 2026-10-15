// PROXY-45 / PROXY-48 / ACTIVITY-54: protocol completion is independent of
// HTTP EOF and client cancellation. Only loopback, fake keys and temporary data.
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { once } from 'node:events';
import { mkdtemp, readdir, readFile, rm } from 'node:fs/promises';
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
const work = await mkdtemp(path.join(tmpdir(), 'yabane-stream-completion-'));
const servers = [];
const heldBodies = new Set();
let child;
let serverLog = '';
let providerRequests = 0;
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
const listen = async handler => {
  const server = http.createServer(handler);
  servers.push(server);
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  return server;
};
const origin = server => 'http://127.0.0.1:' + server.address().port;
const request = (url, options = {}) => fetch(url, {
  ...options, signal: options.signal ?? AbortSignal.timeout(5000),
});
const json = body => ({ headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });
const frame = value => 'data: ' + JSON.stringify(value) + '\n\n';
const protocols = {
  chat: { path: '/v1/chat/completions', api: 'openai_chat_completions', input: { messages: [] } },
  responses: { path: '/v1/responses', api: 'openai_responses', input: { input: [] } },
  messages: { path: '/v1/messages', api: 'anthropic', input: { messages: [], max_tokens: 32 } },
};
function fixture(protocol, model) {
  const incomplete = model === 'incomplete';
  const failed = model === 'error';
  let prefix;
  let terminal;
  if (protocol === 'chat') {
    prefix = frame({ id: 'c1', model: 'fixture', choices: [{ index: 0, delta: { content: model === 'large' ? 'x'.repeat(1100000) : 'ok' }, finish_reason: incomplete ? 'length' : 'stop' }] })
      + frame({ choices: [], usage: { prompt_tokens: 7, completion_tokens: 3, total_tokens: 10 } });
    terminal = 'data: [DONE]\n\n';
  } else if (protocol === 'responses') {
    prefix = frame({ type: 'response.created', response: { id: 'r1', model: 'fixture' } })
      + frame({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: 'ok' });
    terminal = frame({ type: incomplete ? 'response.incomplete' : 'response.completed', response: {
      id: 'r1', model: 'fixture', status: incomplete ? 'incomplete' : 'completed',
      ...(incomplete ? { incomplete_details: { reason: 'max_output_tokens' } } : {}),
      output: [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'ok' }] }],
      usage: { input_tokens: 7, output_tokens: 3 },
    } });
  } else {
    prefix = frame({ type: 'message_start', message: { id: 'm1', model: 'fixture', usage: { input_tokens: 7, output_tokens: 0 } } })
      + frame({ type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } })
      + frame({ type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'ok' } })
      + frame({ type: 'content_block_stop', index: 0 })
      + frame({ type: 'message_delta', delta: { stop_reason: incomplete ? 'max_tokens' : 'end_turn' }, usage: { output_tokens: 3 } });
    terminal = frame({ type: 'message_stop' });
  }
  if (failed) prefix += protocol === 'responses'
    ? frame({ type: 'response.failed', response: { status: 'failed', error: { message: 'private fixture detail' } } })
    : frame({ type: 'error', error: { message: 'private fixture detail' } });
  if (model === 'finish-only') terminal = '';
  if (model === 'partial-terminal') terminal = terminal.slice(0, -1);
  return prefix + terminal;
}
function hasTerminal(raw, protocol, failed) {
  if (!raw.endsWith('\n\n')) return false;
  if (failed && (raw.includes('"error"') || raw.includes('response.failed'))) return true;
  if (protocol === 'chat') return raw.includes('data: [DONE]\n\n');
  return protocol === 'responses'
    ? /"type":"response\.(completed|incomplete)"/.test(raw)
    : raw.includes('"type":"message_stop"');
}

try {
  const provider = await listen(async (req, res) => {
    if (req.method === 'GET') {
      req.resume();
      res.setHeader('content-type', 'application/json');
      res.end(JSON.stringify({ data: ['complete', 'incomplete', 'finish-only', 'partial-terminal', 'error', 'http-error', 'large'].map(id => ({ id })) }));
      return;
    }
    let raw = '';
    for await (const chunk of req) raw += chunk;
    const body = JSON.parse(raw);
    const protocol = Object.keys(protocols).find(key => protocols[key].path === req.url);
    assert.ok(protocol, req.url);
    providerRequests++;
    res.writeHead(body.model === 'http-error' ? 429 : 200, { 'content-type': 'text/event-stream' });
    heldBodies.add(res);
    res.once('close', () => heldBodies.delete(res));
    res.write(fixture(protocol, body.model === 'http-error' ? 'error' : body.model));
    // Intentionally NEVER send HTTP EOF until the assertion has completed. No
    // timing assumption about the interval between the terminal marker and EOF.
  });
  const reservation = await listen((req, res) => res.end());
  const base = origin(reservation);
  const port = reservation.address().port;
  await new Promise(resolve => reservation.close(resolve));
  child = spawn(binary, ['--addr', '127.0.0.1:' + port], {
    cwd: work, env: { PATH: process.env.PATH, YABANE_SHUTDOWN_GRACE_SECONDS: '1' },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  for (const stream of [child.stdout, child.stderr]) stream.on('data', chunk => { serverLog = (serverLog + chunk).slice(-32000); });
  let ready = false;
  for (let i = 0; i < 100; i++) {
    if (child.exitCode !== null) throw new Error('Yabane exited: ' + serverLog);
    try { ready = (await request(base + '/healthz')).ok; } catch { /* Not listening yet. */ }
    if (ready) break;
    await pause(50);
  }
  assert.ok(ready, 'Yabane did not become ready: ' + serverLog);
  const setup = await request(base + '/admin/setup', {
    method: 'POST', ...json({ username: 'fixture', email: 'fixture@example.test', password: 'fixture-password' }),
  });
  assert.equal(setup.status, 204);
  const cookie = setup.headers.get('set-cookie').split(';')[0];
  const admin = async (route, method = 'GET', body) => {
    const options = body === undefined ? {} : json(body);
    const response = await request(base + route, { method, ...options, headers: { ...options.headers, cookie } });
    assert.ok(response.ok, route + ': ' + response.status + ' ' + (response.ok ? '' : await response.text()));
    return response;
  };
  await admin('/admin/auth', 'PATCH', { enabled: false });
  for (const [key, protocol] of Object.entries(protocols)) await admin('/admin/providers', 'POST', {
    id: key, name: key,
    endpoint: { id: 'fixture', api_type: protocol.api, base_url: origin(provider) + '/v1', requires_credential: false },
  });
  const captureConfig = {
    active: true, remaining: 100, expires_at: Math.floor(Date.now() / 1000) + 3600,
    endpoint_id: 'fixture', model: '', body_limit: 65536,
    retention_days: 1, redacted_headers: [],
  };
  const expected = [];
  async function run(source, target, model, cancelWithAbort = false) {
    const label = source + ' -> ' + target + ' / ' + model + (cancelWithAbort ? ' / abort' : ' / reader.cancel');
    await admin('/admin/extensions/traffic-capture/status', 'PATCH', { ...captureConfig, provider_id: source, remaining: 100 - expected.length });
    const abort = new AbortController();
    const response = await request(base + protocols[target].path, {
      method: 'POST', ...json({ ...protocols[target].input, model: source + '/' + model, stream: true }), signal: abort.signal,
    });
    assert.equal(response.status, model === 'http-error' ? 429 : 200, label);
    const id = response.headers.get('x-yabane-request-id');
    assert.ok(id, label);
    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let raw = '';
    const unfinished = model === 'finish-only' || model === 'partial-terminal';
    while (!raw || (!unfinished && !hasTerminal(raw, target, model === 'error' || model === 'http-error'))) {
      const chunk = await reader.read();
      assert.ok(!chunk.done, 'unexpected EOF: ' + label + ' / ' + raw);
      raw += decoder.decode(chunk.value, { stream: true });
    }
    if (source === target) assert.equal(raw, fixture(source, model === 'http-error' ? 'error' : model), 'native bytes unchanged: ' + label);
    if (cancelWithAbort) abort.abort();
    await reader.cancel().catch(() => {});
    let records = [];
    for (let i = 0; i < 100; i++) {
      records = (await (await admin('/admin/activity/logs?limit=200')).json()).filter(entry => entry.request_id === id);
      if (records.length) break;
      await pause(25);
    }
    assert.equal(records.length, 1, 'exactly one terminal record: ' + label + ' / ' + serverLog);
    const record = records[0];
    const status = model === 'http-error' ? 429 : model === 'error' ? 502 : unfinished ? 499 : 200;
    assert.equal(record.status, status, label + ' / ' + JSON.stringify(record));
    if (status === 200) {
      assert.equal(record.failure, undefined, label);
      assert.equal(record.input_tokens, 7, label);
      assert.equal(record.output_tokens, 3, label);
      if (model === 'incomplete') assert.ok(['length', 'max_tokens', 'max_output_tokens'].includes(record.finish_reason), label);
    } else if (status === 499) {
      assert.equal(record.failure?.stage, 'client', label);
      assert.equal(record.failure?.category, 'disconnected', label);
    } else {
      assert.notEqual(record.failure?.stage, 'client', label);
      assert.ok(!JSON.stringify(record).includes('private fixture detail'), label);
    }
    let capture;
    for (let i = 0; i < 100; i++) {
      capture = (await (await admin('/admin/extensions/traffic-capture/captures')).json()).find(entry => entry.request_id === id);
      if (capture && capture.outcome !== 'capturing') break;
      await pause(25);
    }
    assert.ok(capture && capture.outcome !== 'capturing', 'observer finalized: ' + label);
    if (status === 200) assert.equal(capture.outcome, 'complete', label);
    if (status === 499) assert.equal(capture.outcome, 'interrupted', label);
    expected.push({ id, status });
    // Let any provider that did not observe the cancellation finish now; this
    // must neither duplicate nor change the already-written record.
    for (const body of heldBodies) body.end();
  }
  for (const source of Object.keys(protocols)) for (const target of Object.keys(protocols)) {
    await run(source, target, 'complete');
    await run(source, target, 'incomplete');
    await run(source, target, 'finish-only');
    await run(source, target, 'error');
  }
  for (const key of Object.keys(protocols)) {
    await run(key, key, 'complete', true);
    await run(key, key, 'partial-terminal');
    await run(key, key, 'http-error');
  }
  // Converted Responses terminal output exceeds the usage observer's 1 MiB
  // limit: completion comes from conversion state, not reparsing a capped body.
  await run('chat', 'responses', 'large');
  await pause(100);
  const logs = await (await admin('/admin/activity/logs?limit=200')).json();
  assert.equal(logs.length, expected.length, 'no duplicate records after EOF/cleanup');
  assert.equal(providerRequests, expected.length, 'never retry a cancellation');
  const stats = await (await admin('/admin/activity/stats?buckets=1')).json();
  for (const [field, status] of [['successful', 200], ['cancelled', 499]]) assert.equal(stats[field], expected.filter(item => item.status === status).length, field);
  assert.equal(stats.errors, expected.filter(item => item.status >= 400 && item.status !== 499).length);
  assert.equal(stats.requests, stats.successful + stats.cancelled + stats.errors);
  assert.equal(stats.buckets.reduce((sum, bucket) => sum + bucket.cancelled, 0), stats.cancelled);
  for (const dimension of [stats.by_provider, stats.by_model, stats.by_api_key]) assert.equal(dimension.reduce((sum, item) => sum + item.cancelled, 0), stats.cancelled);
  for (const [status, count] of [['cancelled', stats.cancelled], ['error', stats.errors], ['success', stats.successful]]) {
    const page = await (await admin('/admin/activity/logs/page?limit=100&offset=0&status=' + status)).json();
    assert.equal(page.total, count, 'explorer filter: ' + status);
  }
  const captureStatus = await (await admin('/admin/extensions/traffic-capture/status')).json();
  assert.equal(captureStatus.config.remaining, 100 - expected.length, 'all capture quota released');
  // SVC-60 / PROXY-45: stopping the service closes unfinished HTTP bodies, but
  // must not turn an already-emitted protocol terminal into a shutdown failure.
  const shutdownRequests = [];
  for (const model of ['complete', 'finish-only']) {
    const response = await request(base + protocols.chat.path, {
      method: 'POST', ...json({ model: 'chat/' + model, messages: [], stream: true }),
    });
    const reader = response.body.getReader();
    const first = await reader.read();
    assert.ok(!first.done && first.value.length);
    shutdownRequests.push({ model, id: response.headers.get('x-yabane-request-id'), reader });
  }
  const exited = once(child, 'exit');
  child.kill('SIGTERM');
  await Promise.all(shutdownRequests.map(async ({ reader }) => {
    while (!(await reader.read()).done) { /* Read the gateway's shutdown EOF. */ }
  }));
  await exited;
  const activityDir = path.join(work, 'data', 'activity');
  const persisted = (await Promise.all((await readdir(activityDir)).filter(name => name.endsWith('.jsonl')).map(name => readFile(path.join(activityDir, name), 'utf8'))))
    .join('\n').split('\n').filter(Boolean).map(line => JSON.parse(line));
  assert.equal(persisted.length, expected.length + 2, 'shutdown flushes every record once');
  for (const { model, id } of shutdownRequests) {
    const record = persisted.find(entry => entry.request_id === id);
    assert.ok(record, 'shutdown record: ' + model);
    assert.equal(record.status, model === 'complete' ? 200 : 502, 'shutdown after ' + model);
    if (model === 'complete') assert.equal(record.failure, undefined);
    else {
      assert.equal(record.failure?.stage, 'gateway');
      assert.equal(record.failure?.category, 'shutdown');
    }
  }
  console.log('Stream completion passed: ' + expected.length + ' requests plus 2 shutdown cases, native/conversion terminal cancellation, failures, usage, observers and statistics');
} finally {
  for (const body of heldBodies) body.destroy();
  if (child?.pid && child.exitCode === null) {
    const exited = once(child, 'exit');
    child.kill('SIGTERM');
    const deadline = setTimeout(() => child.kill('SIGKILL'), 5000);
    try { await exited; } finally { clearTimeout(deadline); }
  }
  await Promise.all(servers.map(server => { server.closeAllConnections(); return new Promise(resolve => server.close(resolve)); }));
  await rm(work, { recursive: true, force: true });
}
