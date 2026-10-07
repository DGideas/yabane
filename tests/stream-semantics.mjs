// PROXY-48 / PROXY-49 / PROXY-50: terminal meaning, final usage and item
// identity survive conversion; native JSON/SSE remain unchanged (PROXY-08).
// Loopback fixtures and temporary data only; no installed config or credentials.
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
const work = await mkdtemp(path.join(tmpdir(), 'yabane-stream-semantics-'));
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
const request = (url, options = {}) => fetch(url, { ...options, signal: AbortSignal.timeout(5000) });
const json = body => ({ headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });
const frame = (value, ending = '\n\n') => `data: ${JSON.stringify(value)}${ending}`;
const events = raw => raw.split('\n').filter(line => line.startsWith('data: {')).map(line => JSON.parse(line.slice(6)));
const chunk = (delta, finish = null) => ({ id: 'c1', model: 'fixture', choices: [{ index: 0, delta, finish_reason: finish }] });
const usage = { prompt_tokens: 10, completion_tokens: 5, total_tokens: 15, prompt_tokens_details: { cached_tokens: 3 } };
const jsonReply = {
  id: 'c1', object: 'chat.completion', model: 'fixture',
  choices: [{ index: 0, message: { role: 'assistant', content: 'partial' }, finish_reason: 'length' }], usage,
};
const lateUsageStream = frame(chunk({ content: 'partial' }, 'length'), '\r\n\r\n')
  + frame({ id: 'c1', model: 'fixture', choices: [], usage }) + 'data: [DONE]\n\n';
const incompleteResponse = {
  id: 'r1', model: 'fixture', status: 'incomplete', incomplete_details: { reason: 'max_output_tokens' },
  output: [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'partial' }] }],
  usage: { input_tokens: 10, output_tokens: 5, input_tokens_details: { cached_tokens: 3 } },
};
const incompleteStream = frame({ type: 'response.created', response: { id: 'r1', model: 'fixture' } })
  + frame({ type: 'response.output_text.delta', output_index: 0, delta: 'partial' })
  + frame({ type: 'response.incomplete', response: incompleteResponse });
const toolStream = frame(chunk({ tool_calls: [
  { index: 2, id: 'call_a', type: 'function', function: { name: 'a', arguments: '{"a":' } },
  { index: 7, id: 'call_b', type: 'function', function: { name: 'b', arguments: '{"b":' } },
] }), '\r\n\r\n') + frame(chunk({ tool_calls: [
  { index: 7, function: { arguments: '2}' } },
  { index: 2, function: { arguments: '1}' } },
] }, 'tool_calls')) + frame({ choices: [], usage }) + 'data: [DONE]\n\n';

try {
  const received = [];
  const provider = await listen(async (req, res) => {
    let raw = '';
    for await (const chunk of req) raw += chunk;
    if (req.method === 'GET') {
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end('{"data":[{"id":"json"},{"id":"sse"},{"id":"tools"},{"id":"truncated"}]}');
      return;
    }
    const body = JSON.parse(raw);
    received.push({ path: req.url, body });
    const isResponses = req.url === '/v1/responses';
    res.setHeader('content-type', body.model === 'json' ? 'application/json' : 'text/event-stream');
    if (body.model === 'json') {
      res.end(JSON.stringify(isResponses ? incompleteResponse : jsonReply));
    } else if (body.model === 'tools') {
      res.end(toolStream);
    } else if (body.model === 'truncated') {
      // Generation says stop, but the protocol's terminal marker never arrives.
      res.end(frame(chunk({ content: 'partial' }, 'stop')));
    } else {
      // One network write includes both CRLF and LF SSE delimiters.
      res.end(isResponses ? incompleteStream : lateUsageStream);
    }
  });
  const reservation = await listen((req, res) => res.end());
  const base = origin(reservation);
  const port = reservation.address().port;
  await new Promise(resolve => reservation.close(resolve));
  child = spawn(binary, ['--addr', `127.0.0.1:${port}`], {
    cwd: work, env: { PATH: process.env.PATH }, stdio: ['ignore', 'pipe', 'pipe'],
  });
  let spawnError;
  child.on('error', error => { spawnError = error; });
  for (const stream of [child.stdout, child.stderr]) {
    stream.on('data', chunk => { serverLog = (serverLog + chunk).slice(-32000); });
  }
  let ready = false;
  for (let i = 0; i < 100; i++) {
    if (spawnError) throw spawnError;
    if (child.exitCode !== null) throw new Error(`Yabane exited: ${serverLog}`);
    try { ready = (await request(`${base}/healthz`)).ok; } catch { /* Not listening yet. */ }
    if (ready) break;
    await pause(50);
  }
  assert.ok(ready, `Yabane did not become ready: ${serverLog}`);
  const setup = await request(`${base}/admin/setup`, {
    method: 'POST', ...json({ username: 'semantics', email: 'semantics@example.test', password: 'fixture-password' }),
  });
  assert.equal(setup.status, 204);
  const cookie = setup.headers.get('set-cookie').split(';')[0];
  const admin = async (route, method, body) => {
    const options = json(body);
    const response = await request(`${base}${route}`, { method, ...options, headers: { ...options.headers, cookie } });
    assert.ok(response.ok, `${route}: ${response.status} ${await response.text()}`);
  };
  await admin('/admin/auth', 'PATCH', { enabled: false });
  for (const [id, api_type] of [['chat', 'openai_chat_completions'], ['responses', 'openai_responses']]) {
    await admin('/admin/providers', 'POST', {
      id, name: id, endpoint: { api_type, base_url: `${origin(provider)}/v1`, requires_credential: false },
    });
  }
  const post = async (route, body) => {
    const response = await request(`${base}${route}`, { method: 'POST', ...json(body) });
    const raw = await response.text();
    return { response, raw };
  };
  const successfulIds = [];
  const incompleteIds = [];
  for (const model of ['json', 'sse']) {
    for (const stream of model === 'json' ? [false] : [false, true]) {
      const { response, raw } = await post('/v1/responses', { model: `chat/${model}`, input: 'fixture', stream });
      assert.equal(response.status, 200, raw);
      successfulIds.push(response.headers.get('x-yabane-request-id'));
      const result = stream ? events(raw).at(-1).response : JSON.parse(raw);
      assert.equal(result.status, 'incomplete');
      assert.equal(result.incomplete_details.reason, 'max_output_tokens');
      assert.equal(result.usage.total_tokens, 15);
      assert.equal(result.usage.input_tokens_details.cached_tokens, 3);
      assert.equal(result.output[0].content[0].text, 'partial');
      if (stream) assert.equal(events(raw).at(-1).type, 'response.incomplete');

      const chat = await post('/v1/chat/completions', {
        model: `responses/${model}`, messages: [{ role: 'user', content: 'fixture' }], stream,
      });
      assert.equal(chat.response.status, 200, chat.raw);
      incompleteIds.push(chat.response.headers.get('x-yabane-request-id'));
      const final = stream ? events(chat.raw).at(-1) : JSON.parse(chat.raw);
      assert.equal(final.choices[0].finish_reason, 'length');
      assert.equal(final.usage.total_tokens, 15);
    }
  }
  const tools = await post('/v1/responses', { model: 'chat/tools', input: 'fixture', stream: true });
  assert.equal(tools.response.status, 200, tools.raw);
  const toolEvents = events(tools.raw);
  const final = toolEvents.at(-1).response;
  assert.equal(final.status, 'completed');
  assert.equal(final.usage.total_tokens, 15);
  const added = toolEvents.filter(event => event.type === 'response.output_item.added');
  const done = toolEvents.filter(event => event.type === 'response.output_item.done');
  assert.deepEqual(added.map(event => event.output_index), [0, 1]);
  assert.deepEqual(done.map(event => event.output_index), [0, 1]);
  assert.deepEqual(done.map(event => event.item), final.output);
  assert.deepEqual(final.output.map(item => [item.call_id, item.arguments]), [['call_a', '{"a":1}'], ['call_b', '{"b":2}']]);
  toolEvents.forEach((event, index) => assert.equal(event.sequence_number, index));
  for (const event of toolEvents.filter(event => event.item_id)) {
    assert.equal(event.item_id, final.output[event.output_index].id);
  }

  for (const stream of [false, true]) {
    const truncated = await post('/v1/responses', { model: 'chat/truncated', input: 'fixture', stream });
    assert.equal(truncated.response.status, stream ? 200 : 502);
    assert.ok(!truncated.raw.includes('response.completed'));
    assert.ok(truncated.raw.includes(stream ? 'response.failed' : 'terminal event'));
  }

  // Native passthrough keeps every byte and request field (no terminal rewriting).
  for (const [provider, route, content, expected] of [
    ['chat', '/v1/chat/completions', { messages: [] }, lateUsageStream],
    ['responses', '/v1/responses', { input: 'fixture' }, incompleteStream],
  ]) {
    const body = { model: `${provider}/sse`, ...content, stream: true, vendor_field: { opaque: true } };
    const native = await post(route, body);
    assert.equal(native.response.status, 200);
    assert.equal(native.raw, expected);
    assert.equal(native.response.headers.get('x-yabane-protocol-conversion'), null);
    assert.deepEqual(received.at(-1), { path: route, body: { ...body, model: 'sse' } });
  }
  const nativeJson = await post('/v1/chat/completions', { model: 'chat/json', messages: [] });
  assert.equal(nativeJson.raw, JSON.stringify(jsonReply));

  // Completion recording is asynchronous for streams. Wait for all records.
  let records = [];
  for (let i = 0; i < 50; i++) {
    records = await (await request(`${base}/admin/activity/logs?limit=100`, { headers: { cookie } })).json();
    if ([...successfulIds, ...incompleteIds].every(id => records.some(record => record.request_id === id))) break;
    await pause(20);
  }
  for (const id of successfulIds) {
    const record = records.find(record => record.request_id === id);
    assert.ok(record, `missing Activity ${id}`);
    assert.equal(record.input_tokens, 10);
    assert.equal(record.output_tokens, 5);
    assert.equal(record.cached_tokens, 3);
    assert.equal(record.finish_reason, 'length');
  }
  for (const id of incompleteIds) {
    const record = records.find(record => record.request_id === id);
    assert.ok(record, `missing Activity ${id}`);
    assert.equal(record.status, 200);
    assert.equal(record.finish_reason, 'max_output_tokens');
    assert.equal(record.input_tokens, 10);
    assert.equal(record.output_tokens, 5);
    assert.equal(record.cached_tokens, 3);
  }
  console.log('Stream semantics passed: limits, final usage, tool lifecycle, mixed frames, native passthrough');
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
