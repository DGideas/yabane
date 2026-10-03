// PROXY-43 / PROXY-44: a Provider redirect must not replay a prompt or credential.
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
const work = await mkdtemp(path.join(tmpdir(), 'yabane-http-redirects-'));
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
  ...options, redirect: 'manual', signal: AbortSignal.timeout(5000),
});

try {
  let redirectedRequests = 0;
  const sink = await listen((req, res) => {
    redirectedRequests++;
    req.resume();
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end('{"unexpected":"redirect was followed"}');
  });
  let providerRequests = 0;
  let redirectStatus = 307;
  const location = `${origin(sink)}/must-not-receive-credentials`;
  const responseBody = 'Provider redirect: configure the correct Endpoint URL';
  const provider = await listen((req, res) => {
    req.resume();
    if (req.method === 'GET') {
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end('{"data":[{"id":"test"}]}');
      return;
    }
    providerRequests++;
    res.writeHead(redirectStatus, { location, 'content-type': 'text/plain' });
    res.end(responseBody);
  });
  // Reserve a free port briefly; the child readiness check detects startup failure.
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
  };
  await admin('/admin/auth', 'PATCH', { enabled: false });
  const surfaces = [
    ['anthropic', '/v1/messages', { messages: [], max_tokens: 1 }],
    ['openai_chat_completions', '/v1/chat/completions', { messages: [] }],
    ['openai_responses', '/v1/responses', { input: 'fixture prompt' }],
  ];
  for (const [apiType] of surfaces) {
    await admin('/admin/providers', 'POST', {
      id: apiType.replaceAll('_', '-'), name: apiType,
      endpoint: {
        api_type: apiType, base_url: `${origin(provider)}/v1`,
        requires_credential: true, credential_secret: 'fixture-not-a-real-secret',
      },
    });
  }
  for (const status of [301, 302, 303, 307, 308]) {
    redirectStatus = status;
    for (const [apiType, route, body] of surfaces) {
      const before = providerRequests;
      const response = await request(`${base}${route}`, {
        method: 'POST', ...json({ ...body, model: `${apiType.replaceAll('_', '-')}/test` }),
      });
      assert.equal(response.status, status, `${apiType}: Provider redirect status must survive`);
      assert.equal(response.headers.get('location'), location);
      assert.equal(await response.text(), responseBody);
      assert.equal(providerRequests - before, 1, 'one caller request reaches the Provider once');
      assert.equal(redirectedRequests, 0, 'redirect destination must never receive a request');
    }
  }
  console.log('HTTP redirect isolation passed: 5 statuses × 3 native protocol surfaces');
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
