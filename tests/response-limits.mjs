// PROXY-46: response observation and conversion are bounded, while native
// passthrough stays byte-transparent even for a response the observer must skip.
// Uses only loopback servers, fake credentials, and a temporary data directory.
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
const work = await mkdtemp(path.join(tmpdir(), 'yabane-response-limits-'));
const servers = [];
let child;
let serverLog = '';
const mebibyte = 1024 * 1024;
const conversionLimit = 32 * mebibyte;
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
const listen = async handler => {
  const server = http.createServer(handler);
  servers.push(server);
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  return server;
};
const origin = server => `http://127.0.0.1:${server.address().port}`;

try {
  // A Provider that can answer with an oversized JSON body, an oversized event
  // stream, or one very long event-stream line.
  let providerRequests = 0;
  const provider = await listen(async (req, res) => {
    let body = '';
    for await (const chunk of req) body += chunk;
    if (req.method === 'GET') {
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end('{"data":[{"id":"limits-test"}]}');
      return;
    }
    providerRequests++;
    const mode = body.includes('"stream":true') ? 'stream' : 'json';
    if (req.url.startsWith('/huge-line')) {
      res.writeHead(200, { 'content-type': 'text/event-stream' });
      res.write(`data: ${JSON.stringify({ id: 'chat_1', model: 'limits-test', choices: [{ delta: { content: 'x'.repeat(10 * mebibyte) }, finish_reason: null }] })}\n\n`);
      res.write('data: [DONE]\n\n');
      res.end();
      return;
    }
    if (req.url.startsWith('/aggregate')) {
      res.writeHead(200, { 'content-type': 'text/event-stream' });
      for (let index = 0; index < 40; index++) {
        res.write(`data: ${JSON.stringify({ id: 'chat_1', model: 'limits-test', choices: [{ delta: { content: 'y'.repeat(mebibyte) }, finish_reason: null }] })}\n\n`);
      }
      res.write('data: [DONE]\n\n');
      res.end();
      return;
    }
    if (req.url.startsWith('/oversized')) {
      const padding = 'z'.repeat(conversionLimit + mebibyte);
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify({
        id: 'chat_1', object: 'chat.completion', model: 'limits-test',
        choices: [{ index: 0, message: { role: 'assistant', content: padding }, finish_reason: 'stop' }],
        usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 },
      }));
      return;
    }
    if (mode === 'stream') {
      res.writeHead(200, { 'content-type': 'text/event-stream' });
      res.write(`data: ${JSON.stringify({ id: 'chat_1', model: 'limits-test', choices: [{ delta: { content: 'ok' }, finish_reason: null }] })}\n\n`);
      res.write('data: [DONE]\n\n');
      res.end();
      return;
    }
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify({
      id: 'chat_1', object: 'chat.completion', model: 'limits-test',
      choices: [{ index: 0, message: { role: 'assistant', content: 'ok' }, finish_reason: 'stop' }],
      usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 },
    }));
  });
  const reservation = await listen((req, res) => res.end());
  const base = origin(reservation);
  const port = reservation.address().port;
  await new Promise(resolve => reservation.close(resolve));
  child = spawn(binary, ['--addr', `127.0.0.1:${port}`], {
    cwd: work,
    env: { PATH: process.env.PATH },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  for (const stream of [child.stdout, child.stderr]) {
    stream.on('data', chunk => { serverLog = (serverLog + chunk).slice(-32000); });
  }
  let ready = false;
  for (let i = 0; i < 100; i++) {
    if (child.exitCode !== null) throw new Error(`Yabane exited: ${serverLog}`);
    try { ready = (await fetch(`${base}/healthz`, { signal: AbortSignal.timeout(1000) })).ok; } catch { /* Not listening yet. */ }
    if (ready) break;
    await pause(50);
  }
  assert.ok(ready, `Yabane did not become ready: ${serverLog}`);
  const json = body => ({ headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });
  const setup = await fetch(`${base}/admin/setup`, {
    method: 'POST', ...json({ username: 'limits', email: 'limits@example.test', password: 'fixture-password' }),
  });
  assert.equal(setup.status, 204);
  const cookie = setup.headers.get('set-cookie').split(';')[0];
  const admin = async (route, method, body) => {
    const options = json(body);
    const response = await fetch(`${base}${route}`, { method, ...options, headers: { ...options.headers, cookie } });
    assert.ok(response.ok, `${route}: ${response.status} ${await response.text()}`);
  };
  await admin('/admin/auth', 'PATCH', { enabled: false });
  const addProvider = (id, route) => admin('/admin/providers', 'POST', {
    id, name: id,
    endpoint: {
      id: 'chat', api_type: 'openai_chat_completions', base_url: `${origin(provider)}${route}/v1`,
      requires_credential: true, credential_secret: 'fixture-not-a-real-secret',
    },
  });
  await addProvider('oversized', '/oversized');
  await addProvider('aggregate', '/aggregate');
  await addProvider('huge-line', '/huge-line');

  const records = async () => {
    const logs = await (await fetch(`${base}/admin/activity/logs?limit=50`, { headers: { cookie } })).json();
    return logs;
  };

  // 1. A non-streaming cross-protocol conversion refuses an oversized answer.
  const oversized = await fetch(`${base}/v1/responses`, {
    method: 'POST', ...json({ model: 'oversized/limits-test', input: 'fixture' }),
  });
  assert.equal(oversized.status, 502);
  const oversizedBody = await oversized.text();
  assert.ok(oversizedBody.includes('conversion limit'), oversizedBody);
  const oversizedRecord = (await records()).find(entry => entry.provider === 'oversized');
  assert.equal(oversizedRecord?.status, 502);
  assert.equal(oversizedRecord?.failure?.stage, 'protocol_conversion');
  assert.equal(oversizedRecord?.failure?.category, 'response_too_large');

  // 2. An aggregated conversion of an event stream is bounded the same way.
  const aggregated = await fetch(`${base}/v1/responses`, {
    method: 'POST', ...json({ model: 'aggregate/limits-test', input: 'fixture' }),
  });
  assert.equal(aggregated.status, 502);
  assert.ok((await aggregated.text()).includes('conversion limit'));
  const aggregateRecord = (await records()).find(entry => entry.provider === 'aggregate');
  assert.equal(aggregateRecord?.failure?.category, 'response_too_large', JSON.stringify(aggregateRecord));

  // 3. A native passthrough stream is byte-transparent even when one event is far
  //    longer than the observer's line limit: observation may drop what it cannot
  //    parse, but it must never change what the caller receives.
  const passthrough = await fetch(`${base}/v1/chat/completions`, {
    method: 'POST', ...json({ model: 'huge-line/limits-test', messages: [{ role: 'user', content: 'fixture' }], stream: true }),
  });
  assert.equal(passthrough.status, 200);
  const received = await passthrough.text();
  assert.ok(received.includes('x'.repeat(10 * mebibyte)), 'the long event arrives unchanged');
  assert.ok(received.includes('data: [DONE]'));
  await pause(200);
  const passthroughRecord = (await records()).find(entry => entry.provider === 'huge-line');
  assert.equal(passthroughRecord?.status, 200, 'observation limits must not turn passthrough into a failure');
  assert.equal(providerRequests, 3);
  console.log('Response limits passed: conversion bounded, passthrough byte-transparent');
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
