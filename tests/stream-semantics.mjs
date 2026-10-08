// PROXY-48 / PROXY-49 / PROXY-50 / PROXY-52: terminal meaning, final usage and item
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

// A Chat tool-only turn may end with an empty content delta. It must not
// become an assistant message between function_call and function_call_output.
const terminalOnlyStream = frame({ type: 'response.created', response: { id: 'r1', model: 'fixture' } })
  + frame({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: '你' })
  + frame({ type: 'response.output_text.done', output_index: 0, content_index: 0, text: '你好' })
  + frame({ type: 'response.output_item.added', output_index: 1, item: {
    type: 'function_call', id: 'fc_a', call_id: 'call_a', name: 'a', arguments: '',
  } })
  + frame({ type: 'response.function_call_arguments.done', output_index: 1, arguments: '{"a":1}' })
  + frame({ type: 'response.completed', response: {
    id: 'r1', status: 'completed', model: 'fixture', output: [
      { type: 'message', role: 'assistant', content: [{ type: 'output_text', text: '你好' }] },
      { type: 'function_call', id: 'fc_a', call_id: 'call_a', name: 'a', arguments: '{"a":1}' },
    ], usage: incompleteResponse.usage,
  } });
const refusalStream = frame(chunk({ refusal: 'Cannot help' }, 'stop')) + 'data: [DONE]\n\n';
const emptyToolStream = toolStream.replace(
  frame({ choices: [], usage }),
  frame(chunk({ content: '' })) + frame({ choices: [], usage }),
);

try {
  const received = [];
  const provider = await listen(async (req, res) => {
    let raw = '';
    for await (const chunk of req) raw += chunk;
    if (req.method === 'GET') {
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ data: ['json', 'sse', 'tools', 'empty-tools', 'replay', 'truncated', 'terminal', 'bare-done', 'divergent', 'json-arguments', 'refusal'].map(id => ({ id })) }));
      return;
    }
    const body = JSON.parse(raw);
    received.push({ path: req.url, body });
    const isResponses = req.url === '/v1/responses';
    if (body.model === 'replay') {
      // Strict fixture: like a Responses Provider backed by Chat, reject an
      // intervening assistant message while tool calls still await results.
      const pending = new Set();
      let valid = true;
      for (const item of isResponses ? body.input : body.messages) {
        if (item.type === 'function_call') pending.add(item.call_id);
        else if (item.role === 'assistant' && item.tool_calls) {
          if (pending.size) valid = false;
          for (const call of item.tool_calls) pending.add(call.id);
        } else if (item.type === 'function_call_output' || item.role === 'tool') {
          if (!pending.delete(item.call_id ?? item.tool_call_id)) valid = false;
        } else if (pending.size) valid = false;
      }
      valid &&= pending.size === 0;
      res.writeHead(valid ? 200 : 400, { 'content-type': 'application/json' });
      res.end(JSON.stringify(valid ? (isResponses ? incompleteResponse : jsonReply) : {
        error: { message: 'insufficient tool messages following tool_calls message' },
      }));
      return;
    }
    res.setHeader('content-type', body.model === 'json' ? 'application/json' : 'text/event-stream');
    if (body.model === 'json-arguments') {
      const argumentsText = '{ "id":123456789012345678901234567890, "n":1.00 }';
      res.setHeader('content-type', 'application/json');
      res.end(JSON.stringify(isResponses ? {
        status: 'completed', output: [{ type: 'function_call', call_id: 'a', name: 'lookup', arguments: argumentsText }],
      } : {
        choices: [{ message: { role: 'assistant', content: null, tool_calls: [{ id: 'a', type: 'function', function: { name: 'lookup', arguments: argumentsText } }] }, finish_reason: 'tool_calls' }],
      }));
    } else if (body.model === 'json') {
      res.end(JSON.stringify(req.url === '/v1/messages' ? {
        id: 'm1', type: 'message', role: 'assistant', model: 'fixture', content: [{ type: 'text', text: 'ok' }],
        stop_reason: 'end_turn', usage: { input_tokens: 10, output_tokens: 1 },
      } : isResponses ? incompleteResponse : jsonReply));
    } else if (body.model === 'refusal') {
      res.end(refusalStream);
    } else if (body.model === 'terminal') {
      res.end(terminalOnlyStream);
    } else if (body.model === 'divergent') {
      res.end(terminalOnlyStream.replace('"text":"你好"', '"text":"different"'));
    } else if (body.model === 'bare-done') {
      res.end('data: [DONE]\n\n');
    } else if (body.model === 'tools' || body.model === 'empty-tools') {
      res.end(body.model === 'tools' ? toolStream : emptyToolStream);
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
  for (const [id, api_type] of [['chat', 'openai_chat_completions'], ['responses', 'openai_responses'], ['anthropic', 'anthropic']]) {
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

  // PROXY-54 / PROXY-55: inspect the real Provider request after conversion.
  const chatHistory = await post('/v1/chat/completions', { model: 'responses/json', messages: [{
    role: 'assistant', content: [{ type: 'text', text: 'checking' }],
    tool_calls: [{ id: 'a', type: 'function', function: { name: 'lookup', arguments: '{}' } }],
  }] });
  assert.equal(chatHistory.response.status, 200, chatHistory.raw);
  assert.equal(received.at(-1).body.input[0].content[0].type, 'output_text');
  const anthropicHistory = await post('/v1/messages', { model: 'responses/json', max_tokens: 50, messages: [{
    role: 'assistant', content: [
      { type: 'thinking', thinking: 'private-thought', signature: 'private-signature' },
      { type: 'redacted_thinking', data: 'private-data' },
      { type: 'text', text: 'answer' },
    ],
  }] });
  assert.equal(anthropicHistory.response.status, 200, anthropicHistory.raw);
  assert.ok(!JSON.stringify(received.at(-1).body).includes('private-'));
  assert.equal(received.at(-1).body.input[0].content[0].text, 'answer');

  // PROXY-63: actual HTTP conversion preserves refusal content, not blank output.
  for (const stream of [false, true]) {
    const refusal = await post('/v1/responses', { model: 'chat/refusal', input: 'fixture', stream });
    assert.equal(refusal.response.status, 200, refusal.raw);
    const result = stream ? events(refusal.raw).at(-1).response : JSON.parse(refusal.raw);
    assert.deepEqual(result.output[0].content, [{ type: 'refusal', refusal: 'Cannot help' }]);
    if (stream) {
      assert.ok(events(refusal.raw).some(e => e.type === 'response.refusal.done'));
      assert.ok(!events(refusal.raw).some(e => e.type === 'response.output_text.done'));
    }
  }

  // PROXY-57: non-streaming JSON conversion must not round large tool arguments.
  for (const [route, model, content, pointer] of [
    ['/v1/responses', 'chat/json-arguments', { input: 'fixture' }, r => r.output[0].arguments],
    ['/v1/chat/completions', 'responses/json-arguments', { messages: [] }, r => r.choices[0].message.tool_calls[0].function.arguments],
  ]) {
    const result = await post(route, { model, ...content });
    assert.equal(result.response.status, 200, result.raw);
    assert.equal(pointer(JSON.parse(result.raw)), '{ "id":123456789012345678901234567890, "n":1.00 }');
  }
  // PROXY-61 / PROXY-62: controls and document data on actual Provider requests.
  const controlled = await post('/v1/chat/completions', { model: 'anthropic/json',
    max_tokens: 20, max_completion_tokens: 30, stop: 'END', parallel_tool_calls: false,
    tools: [{ type: 'function', function: { name: 'lookup', parameters: { type: 'object' } } }],
    messages: [{ role: 'user', content: [{ type: 'file', file: { filename: 'report.pdf', file_data: 'data:application/pdf;base64,JVBERi0=' } }] }],
  });
  assert.equal(controlled.response.status, 200, controlled.raw);
  assert.equal(received.at(-1).path, '/v1/messages');
  assert.equal(received.at(-1).body.max_tokens, 30);
  assert.deepEqual(received.at(-1).body.stop_sequences, ['END']);
  assert.equal(received.at(-1).body.tool_choice.disable_parallel_tool_use, true);
  assert.deepEqual(received.at(-1).body.messages[0].content[0], {
    type: 'document', title: 'report.pdf', source: { type: 'base64', media_type: 'application/pdf', data: 'JVBERi0=' },
  });
  for (const provider of ['chat', 'responses']) {
    const result = await post('/v1/messages', { model: `${provider}/json`, max_tokens: 50, messages: [{ role: 'user', content: [{
      type: 'document', title: 'report.pdf', source: { type: 'base64', media_type: 'application/pdf', data: 'JVBERi0=' },
    }] }] });
    assert.equal(result.response.status, 200, result.raw);
    const body = received.at(-1).body;
    const file = provider === 'chat' ? body.messages[0].content[0].file : body.input[0].content[0];
    assert.equal(file.file_data, 'data:application/pdf;base64,JVBERi0=');
    assert.equal(file.filename, 'report.pdf');
  }

  // PROXY-58: model an Anthropic SDK's append-on-start, index-on-delta assembler.
  for (const model of ['tools', 'empty-tools']) {
    const wire = await post('/v1/messages', { model: `chat/${model}`, messages: [], max_tokens: 50, stream: true });
    assert.equal(wire.response.status, 200, wire.raw);
    const blocks = [];
    const args = [];
    const closed = [];
    for (const event of events(wire.raw)) {
      if (event.type === 'content_block_start') {
        assert.equal(event.index, blocks.length);
        blocks.push(event.content_block); args.push(''); closed.push(false);
      } else if (event.type === 'content_block_delta') {
        assert.equal(closed[event.index], false);
        assert.equal(blocks[event.index].type, 'tool_use');
        args[event.index] += event.delta.partial_json;
      } else if (event.type === 'content_block_stop') {
        assert.equal(closed[event.index], false);
        closed[event.index] = true;
        blocks[event.index].input = JSON.parse(args[event.index]);
      }
    }
    assert.deepEqual(closed, [true, true]);
    const aggregate = await post('/v1/messages', { model: `chat/${model}`, messages: [], max_tokens: 50, stream: false });
    assert.deepEqual(JSON.parse(aggregate.raw).content, blocks);
  }
  // PROXY-59: terminal snapshots supplement streamed prefixes exactly once.
  for (const stream of [false, true]) {
    const result = await post('/v1/chat/completions', { model: 'responses/terminal', messages: [], stream });
    assert.equal(result.response.status, 200, result.raw);
    if (stream) {
      const deltas = events(result.raw).map(e => e.choices?.[0]?.delta).filter(Boolean);
      assert.equal(deltas.map(d => d.content ?? '').join(''), '你好');
      assert.equal(deltas.flatMap(d => d.tool_calls ?? []).map(t => t.function?.arguments ?? '').join(''), '{"a":1}');
    } else {
      const message = JSON.parse(result.raw).choices[0].message;
      assert.equal(message.content, '你好');
      assert.equal(message.tool_calls[0].function.arguments, '{"a":1}');
    }
  }
  for (const model of ['responses/divergent', 'chat/bare-done']) {
    const result = await post('/v1/messages', { model, messages: [], max_tokens: 50, stream: false });
    assert.equal(result.response.status, 502, result.raw);
    assert.equal(result.response.headers.get('x-yabane-error-origin'), 'yabane');
  }

  // PROXY-52: replay the converted tool-only turn on both a native Responses
  // destination and a Chat-only destination; no empty assistant item may split it.
  for (const stream of [false, true]) {
    const first = await post('/v1/responses', { model: 'chat/empty-tools', input: 'fixture', stream });
    assert.equal(first.response.status, 200, first.raw);
    const result = stream ? events(first.raw).at(-1).response : JSON.parse(first.raw);
    const input = [
      { role: 'user', content: 'fixture' },
      ...result.output,
      { type: 'function_call_output', call_id: 'call_a', output: 'one' },
      { type: 'function_call_output', call_id: 'call_b', output: 'two' },
    ];
    for (const provider of ['responses', 'chat']) {
      const replay = await post('/v1/responses', { model: `${provider}/replay`, input, stream: false });
      assert.equal(replay.response.status, 200, replay.raw);
      if (provider === 'responses') assert.deepEqual(received.at(-1).body.input, input);
    }
    assert.deepEqual(result.output.map(item => item.type), ['function_call', 'function_call']);
    assert.equal(result.usage.total_tokens, 15);
    if (stream) {
      assert.deepEqual(events(first.raw).filter(e => e.type === 'response.output_item.done').map(e => e.item), result.output);
    }
  }
  // The same empty delta is still delivered byte-for-byte on the native path.
  const nativeEmpty = await post('/v1/chat/completions', {
    model: 'chat/empty-tools', messages: [], stream: true,
  });
  assert.equal(nativeEmpty.response.status, 200);
  assert.equal(nativeEmpty.raw, emptyToolStream);

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
  console.log('Stream semantics passed: limits, final usage, tool lifecycle and replay, empty deltas, mixed frames, native passthrough');
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
