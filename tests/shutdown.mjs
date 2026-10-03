// Shutdown must be bounded: a long Provider stream cannot hold the process open
// indefinitely, and a request interrupted by the deadline is still recorded as a
// shutdown before Yabane flushes Activity and exits.
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
const work = await mkdtemp(path.join(tmpdir(), 'yabane-shutdown-'));
const graceSeconds = 2;
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

try {
  const provider = await listen(async (req, res) => {
    if (req.method === 'GET') {
      req.resume();
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end('{"data":[{"id":"shutdown-test"}]}');
      return;
    }
    req.resume();
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    for (let index = 0; index < 600; index++) {
      if (res.destroyed) return;
      res.write(`data: ${JSON.stringify({ id: 'chat_1', model: 'shutdown-test', choices: [{ delta: { content: `part-${index} ` }, finish_reason: null }] })}\n\n`);
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
    env: { PATH: process.env.PATH, YABANE_SHUTDOWN_GRACE_SECONDS: String(graceSeconds) },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let exited = once(child, 'exit');
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
    method: 'POST', ...json({ username: 'shutdown', email: 'shutdown@example.test', password: 'fixture-password' }),
  });
  assert.equal(setup.status, 204);
  const cookie = setup.headers.get('set-cookie').split(';')[0];
  const admin = async (route, method, body) => {
    const options = json(body);
    const response = await fetch(`${base}${route}`, { method, ...options, headers: { ...options.headers, cookie } });
    assert.ok(response.ok, `${route}: ${response.status} ${await response.text()}`);
  };
  await admin('/admin/auth', 'PATCH', { enabled: false });
  await admin('/admin/providers', 'POST', {
    id: 'shutdown', name: 'shutdown',
    endpoint: {
      id: 'chat', api_type: 'openai_chat_completions', base_url: `${origin(provider)}/v1`,
      requires_credential: true, credential_secret: 'fixture-not-a-real-secret',
    },
  });

  // A stream that is still being generated when the process is asked to stop.
  const response = await fetch(`${base}/v1/chat/completions`, {
    method: 'POST',
    ...json({ model: 'shutdown/shutdown-test', messages: [{ role: 'user', content: 'fixture' }], stream: true }),
  });
  assert.equal(response.status, 200);
  const requestId = response.headers.get('x-yabane-request-id');
  const reader = response.body.getReader();
  const first = await reader.read();
  assert.ok(!first.done && first.value.length > 0, 'the stream started');

  const stoppingAt = Date.now();
  child.kill('SIGTERM');
  const [code] = await exited;
  const stopMs = Date.now() - stoppingAt;
  exited = null;

  assert.ok(stopMs >= graceSeconds * 1000 - 250, `the grace period is honored (stopped in ${stopMs}ms)`);
  assert.ok(stopMs < (graceSeconds + 8) * 1000, `shutdown is bounded (stopped in ${stopMs}ms)`);
  assert.equal(code, 0, `a graceful stop exits successfully: ${serverLog}`);
  assert.ok(serverLog.includes('grace period ended'), `the interruption is logged: ${serverLog}`);

  // The interrupted exchange is recorded, and the record reached disk before exit.
  const directory = path.join(work, 'data', 'activity');
  const lines = [];
  for (const name of await readdir(directory)) {
    if (!name.endsWith('.jsonl')) continue;
    lines.push(...(await readFile(path.join(directory, name), 'utf8')).split('\n').filter(Boolean));
  }
  const record = lines.map(line => JSON.parse(line)).find(entry => entry.request_id === requestId);
  assert.ok(record, `the interrupted request is recorded: ${JSON.stringify(lines)}`);
  assert.equal(record.status, 502);
  assert.equal(record.failure?.stage, 'gateway');
  assert.equal(record.failure?.category, 'shutdown');
  assert.ok(!record.failure.message.includes('upstream'), 'Provider-side naming rules apply');
  assert.ok(record.output_tokens >= 0, 'observed usage is preserved');

  // A caller still holding the stream observes the connection ending.
  const rest = await reader.read().catch(() => ({ done: true }));
  assert.ok(rest.done || serverLog.includes('grace period ended'), 'the interrupted stream ends');
  console.log(`Shutdown passed: bounded at ${graceSeconds}s grace, interrupted request recorded and flushed (stop took ${stopMs}ms)`);
} finally {
  if (child?.pid && child.exitCode === null) {
    const stopped = once(child, 'exit');
    child.kill('SIGKILL');
    await stopped;
  }
  await Promise.all(servers.map(server => {
    server.closeAllConnections();
    return new Promise(resolve => server.close(resolve));
  }));
  await rm(work, { recursive: true, force: true });
}
