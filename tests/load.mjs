// Capacity baseline for the proxy path: measures latency percentiles and the
// gateway's resident memory under increasing concurrency, and verifies that every
// request in the run is accounted for in Activity. Opt in with
// `bash tests/check.sh --load` or run `node tests/load.mjs [binary]` directly.
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
  const build = spawnSync('cargo', ['build', '--release', '--locked'], { cwd: repo, stdio: 'inherit' });
  if (build.error) throw build.error;
  if (build.status !== 0) process.exit(build.status || 1);
}
const binary = process.argv[2] ? path.resolve(process.argv[2]) : path.join(repo, 'target/release/yabane');
const work = await mkdtemp(path.join(tmpdir(), 'yabane-load-'));
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

function percentile(samples, fraction) {
  if (samples.length === 0) return 0;
  const sorted = [...samples].sort((left, right) => left - right);
  return sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * fraction))];
}

async function rssKilobytes(pid) {
  const output = await new Promise((resolve) => {
    const ps = spawn('ps', ['-o', 'rss=', '-p', String(pid)]);
    let text = '';
    ps.stdout.on('data', chunk => { text += chunk; });
    ps.on('exit', () => resolve(text.trim()));
  });
  return Number.parseInt(output, 10) || 0;
}

try {
  const provider = await listen((req, res) => {
    let body = '';
    req.on('data', chunk => { body += chunk; });
    req.on('end', () => {
      if (req.method === 'GET') {
        res.writeHead(200, { 'content-type': 'application/json' });
        res.end('{"data":[{"id":"load-test"}]}');
        return;
      }
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify({
        id: 'chat_1', object: 'chat.completion', model: 'load-test',
        choices: [{ index: 0, message: { role: 'assistant', content: 'ok' }, finish_reason: 'stop' }],
        usage: { prompt_tokens: 32, completion_tokens: 8, total_tokens: 40 },
      }));
    });
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
    method: 'POST', ...json({ username: 'load', email: 'load@example.test', password: 'fixture-password' }),
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
    id: 'load', name: 'load',
    endpoint: {
      id: 'chat', api_type: 'openai_chat_completions', base_url: `${origin(provider)}/v1`,
      requires_credential: true, credential_secret: 'fixture-not-a-real-secret',
    },
  });

  const send = async () => {
    const started = process.hrtime.bigint();
    const response = await fetch(`${base}/v1/chat/completions`, {
      method: 'POST',
      ...json({ model: 'load/load-test', messages: [{ role: 'user', content: 'fixture' }] }),
    });
    const body = await response.text();
    assert.equal(response.status, 200, body);
    return Number(process.hrtime.bigint() - started) / 1e6;
  };

  for (let i = 0; i < 50; i++) await send();

  const levels = [1, 8, 32];
  const seconds = 2;
  const rows = [];
  let totalRequests = 0;
  const rssAfter = [];
  for (const concurrency of levels) {
    const samples = [];
    const deadline = Date.now() + seconds * 1000;
    const workers = Array.from({ length: concurrency }, async () => {
      while (Date.now() < deadline) samples.push(await send());
    });
    await Promise.all(workers);
    totalRequests += samples.length;
    rssAfter.push(await rssKilobytes(child.pid));
    rows.push({
      concurrency,
      requests: samples.length,
      rps: (samples.length / seconds).toFixed(0),
      p50: percentile(samples, 0.5).toFixed(2),
      p95: percentile(samples, 0.95).toFixed(2),
      p99: percentile(samples, 0.99).toFixed(2),
      max: percentile(samples, 1).toFixed(2),
    });
  }

  // Activity must still account for every request after the run, including the
  // records still pending in memory.
  const recorded = (await readdir(path.join(work, 'data/activity')))
    .filter(name => name.endsWith('.jsonl'))
    .reduce(async (total, name) => {
      const contents = await readFile(path.join(work, 'data/activity', name), 'utf8');
      return (await total) + contents.split('\n').filter(Boolean).length;
    }, Promise.resolve(0));
  const stats = await (await fetch(`${base}/admin/activity/stats?since=0`, { headers: { cookie } })).json();

  console.log(`\nYabane capacity baseline (${seconds}s per level, ${binary})`);
  console.log('concurrency  requests   req/s    p50ms   p95ms   p99ms   maxms   gateway RSS MiB');
  for (const [index, row] of rows.entries()) {
    console.log(
      `${String(row.concurrency).padStart(11)}  ${String(row.requests).padStart(8)}  ${row.rps.padStart(7)}  ` +
      `${row.p50.padStart(6)}  ${row.p95.padStart(6)}  ${row.p99.padStart(6)}  ${row.max.padStart(6)}  ` +
      `${(rssAfter[index] / 1024).toFixed(1).padStart(14)}`,
    );
  }
  console.log(`Activity: ${stats.requests} requests in stats, ${await recorded} persisted lines for ${totalRequests + 50} requests`);
  assert.ok(stats.requests > 0, 'the baseline must record Activity');
  assert.equal(stats.requests, totalRequests + 50, 'every request in the run is accounted for exactly once');
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
