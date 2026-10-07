// PROXY-47 / PROXY-09 / PROXY-51: replayed Responses reasoning never becomes an invalid
// Chat message; message/tool/image history survives, with or without streaming.
// Uses only loopback servers, fake credentials, and a temporary data directory.
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
const work = await mkdtemp(path.join(tmpdir(), 'yabane-reasoning-conversion-'));
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
  ...options, signal: AbortSignal.timeout(5000),
});
const json = body => ({ headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });

try {
  const received = [];
  const provider = await listen(async (req, res) => {
    let raw = '';
    for await (const chunk of req) raw += chunk;
    res.setHeader('content-type', 'application/json');
    if (req.method === 'GET') {
      res.end('{"data":[{"id":"fixture"}]}');
      return;
    }
    const body = JSON.parse(raw);
    received.push({ path: req.url, body });
    const roles = new Set(['system', 'developer', 'user', 'assistant', 'tool']);
    if (req.url !== '/v1/chat/completions' || !body.messages?.every(message => roles.has(message.role))) {
      res.writeHead(400);
      res.end('{"error":{"message":"Invalid Chat message"}}');
      return;
    }
    // PROXY-51: a strict Provider answers each assistant tool_calls message with
    // the tool messages for exactly those calls, so a parallel batch split across
    // assistant messages is rejected the way the real upstream rejects it.
    const unpaired = (() => {
      const messages = body.messages;
      for (let index = 0; index < messages.length; index++) {
        const message = messages[index];
        if (message.role !== 'assistant' || !message.tool_calls?.length) continue;
        const pending = new Set(message.tool_calls.map(call => call.id));
        while (pending.size) {
          const next = messages[++index];
          if (!next || next.role !== 'tool' || !pending.delete(next.tool_call_id)) {
            return `assistant tool_calls at message ${index} is not answered by matching tool messages`;
          }
        }
      }
      return null;
    })();
    if (unpaired) {
      res.writeHead(400);
      res.end(JSON.stringify({ error: { message: unpaired } }));
      return;
    }
    if (body.stream) {
      res.setHeader('content-type', 'text/event-stream');
      for (const [delta, finish] of [[{ role: 'assistant', content: 'ok' }, null], [{}, 'stop']]) {
        res.write(`data: ${JSON.stringify({
          id: 'chat_1', object: 'chat.completion.chunk', model: 'fixture',
          choices: [{ index: 0, delta, finish_reason: finish }],
        })}\n\n`);
      }
      res.end('data: [DONE]\n\n');
      return;
    }
    res.end(JSON.stringify({
      id: 'chat_1', object: 'chat.completion', model: 'fixture',
      choices: [{ index: 0, message: { role: 'assistant', content: 'ok' }, finish_reason: 'stop' }],
      usage: { prompt_tokens: 10, completion_tokens: 1, total_tokens: 11 },
    }));
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
    method: 'POST', ...json({ username: 'reasoning', email: 'reasoning@example.test', password: 'fixture-password' }),
  });
  assert.equal(setup.status, 204);
  const cookie = setup.headers.get('set-cookie').split(';')[0];
  const admin = async (route, method, body) => {
    const options = json(body);
    const response = await request(`${base}${route}`, {
      method, ...options, headers: { ...options.headers, cookie },
    });
    assert.ok(response.ok, `${route}: ${response.status} ${await response.text()}`);
  };
  await admin('/admin/auth', 'PATCH', { enabled: false });
  await admin('/admin/providers', 'POST', {
    id: 'chat', name: 'Chat only',
    endpoint: {
      api_type: 'openai_chat_completions', base_url: `${origin(provider)}/v1`,
      requires_credential: true, credential_secret: 'fixture-not-a-real-secret',
    },
  });

  // Replays history created before the Endpoint changed to Chat, including an
  // opaque signature. Only supported conversational content reaches the Provider.
  const input = [
    { role: 'user', content: 'Inspect the screenshot' },
    { type: 'reasoning', id: 'rs_1', summary: [], content: [{ type: 'reasoning_text', text: 'private thought' }] },
    { type: 'function_call', call_id: 'call_1', name: 'shot', arguments: '{}' },
    { type: 'function_call_output', call_id: 'call_1', output: [
      { type: 'input_text', text: 'screen' },
      { type: 'input_image', image_url: 'data:image/png;base64,abc' },
    ] },
    { type: 'reasoning', id: 'rs_2', summary: [{ type: 'summary_text', text: 'private summary' }], encrypted_content: 'opaque-state' },
    { type: 'message', id: 'msg_1', status: 'completed', role: 'assistant', content: [{ type: 'output_text', text: '883' }] },
    { role: 'user', content: 'Continue' },
  ];
  const expectedMessages = [
    { role: 'user', content: 'Inspect the screenshot' },
    { role: 'assistant', content: null, tool_calls: [
      { id: 'call_1', type: 'function', function: { name: 'shot', arguments: '{}' } },
    ] },
    { role: 'tool', tool_call_id: 'call_1', content: [
      { type: 'text', text: 'screen' },
      { type: 'image_url', image_url: { url: 'data:image/png;base64,abc' } },
    ] },
    { role: 'assistant', content: [{ type: 'text', text: '883' }] },
    { role: 'user', content: 'Continue' },
  ];
  for (const stream of [false, true]) {
    const before = received.length;
    const response = await request(`${base}/v1/responses`, {
      method: 'POST', ...json({ model: 'chat/fixture', input, stream }),
    });
    assert.equal(response.status, 200, await response.clone().text());
    assert.ok(response.headers.has('x-yabane-protocol-conversion'));
    if (stream) {
      const events = (await response.text()).split('\n')
        .filter(line => line.startsWith('data: {')).map(line => JSON.parse(line.slice(6)));
      const completed = events.find(event => event.type === 'response.completed');
      assert.equal(completed?.response.output[0].content[0].text, 'ok');
    } else {
      assert.equal((await response.json()).output[0].content[0].text, 'ok');
    }
    assert.equal(received.length - before, 1);
    assert.equal(received.at(-1).path, '/v1/chat/completions');
    assert.equal(received.at(-1).body.model, 'fixture');
    assert.deepEqual(received.at(-1).body.messages, expectedMessages);
  }

  // PROXY-51: a parallel batch replays as one assistant message whose calls are
  // answered immediately, which is the shape a strict Provider requires.
  const parallelInput = [
    { role: 'user', content: 'Check both' },
    { type: 'function_call', id: 'fc_a', call_id: 'call_a', name: 'bash', arguments: '{"command":"date"}' },
    { type: 'function_call', id: 'fc_b', call_id: 'call_b', name: 'read', arguments: '{"path":"/etc/hosts"}' },
    { type: 'function_call_output', call_id: 'call_a', output: 'Sep 20' },
    { type: 'function_call_output', call_id: 'call_b', output: '127.0.0.1 localhost' },
    { role: 'user', content: 'Continue' },
  ];
  for (const stream of [false, true]) {
    const before = received.length;
    const response = await request(`${base}/v1/responses`, {
      method: 'POST', ...json({ model: 'chat/fixture', input: parallelInput, stream }),
    });
    assert.equal(response.status, 200, await response.clone().text());
    assert.equal(received.length - before, 1);
    const messages = received.at(-1).body.messages;
    assert.equal(messages.length, 5);
    assert.equal(messages[0].role, 'user');
    assert.equal(messages[1].role, 'assistant');
    assert.deepEqual(messages[1].tool_calls.map(call => call.id), ['call_a', 'call_b']);
    assert.deepEqual(messages[1].tool_calls.map(call => call.function.name), ['bash', 'read']);
    assert.deepEqual(messages.slice(2, 4).map(message => [message.role, message.tool_call_id]), [['tool', 'call_a'], ['tool', 'call_b']]);
    if (stream) {
      const events = (await response.text()).split('\n')
        .filter(line => line.startsWith('data: {')).map(line => JSON.parse(line.slice(6)));
      const completed = events.find(event => event.type === 'response.completed');
      assert.equal(completed?.response.output[0].content[0].text, 'ok');
    } else {
      assert.equal((await response.json()).output[0].content[0].text, 'ok');
    }
  }

  // Unsupported items and malformed messages fail locally, never as a Provider 400.
  for (const item of [
    { type: 'item_reference', id: 'private-ref' },
    { type: 'future_item', role: 'assistant', content: 'private-text' },
    { type: 'message', content: 'private-text' },
    { type: 'reasoning', encrypted_content: 'private-state' },
  ]) {
    const before = received.length;
    const response = await request(`${base}/v1/responses`, {
      method: 'POST', ...json({ model: 'chat/fixture', input: [item] }),
    });
    assert.equal(response.status, 400);
    assert.equal(response.headers.get('x-yabane-error-origin'), 'yabane');
    assert.ok(response.headers.has('x-yabane-request-id'));
    const error = await response.text();
    assert.ok(error.includes('Responses input'), error);
    assert.ok(!error.includes('private-'), 'error must not echo input');
    assert.equal(received.length, before, 'rejected request must not reach the Provider');
  }
  console.log('Reasoning conversion passed: JSON/SSE replay, tool images, parallel calls, local rejection');
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
