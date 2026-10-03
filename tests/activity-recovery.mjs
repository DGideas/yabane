// Activity durability rules exercised against the real binary:
//   - a torn final record from an interrupted append is quarantined and startup continues;
//   - damage that no append can produce fails startup with the file and line named.
// Uses a temporary data directory and fake configuration only.
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, mkdtemp, readdir, readFile, rm, writeFile } from 'node:fs/promises';
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
const work = await mkdtemp(path.join(tmpdir(), 'yabane-activity-recovery-'));
const activityDirectory = path.join(work, 'data', 'activity');
const today = new Date().toISOString().slice(0, 10);
const dayFile = path.join(activityDirectory, `${today}.jsonl`);
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));

function record(id) {
  return JSON.stringify({
    timestamp: Math.floor(Date.now() / 1000), request_id: id, path: '/v1/responses', model: 'recovery/model',
    upstream_model: 'model', provider: 'recovery', endpoint: 'main', status: 200, latency_ms: 5,
    input_tokens: 1, output_tokens: 1, cached_tokens: 0, streaming: false,
  });
}

async function freePort() {
  const server = http.createServer();
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const { port } = server.address();
  await new Promise(resolve => server.close(resolve));
  return port;
}

async function startServer(port) {
  const child = spawn(binary, ['--addr', `127.0.0.1:${port}`], {
    cwd: work,
    // Do not inherit production Turnstile, proxy, or Gateway settings.
    env: { PATH: process.env.PATH },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let log = '';
  for (const stream of [child.stdout, child.stderr]) stream.on('data', chunk => { log = (log + chunk).slice(-32000); });
  const base = `http://127.0.0.1:${port}`;
  for (let i = 0; i < 100; i++) {
    if (child.exitCode !== null) throw new Error(`Yabane exited before becoming ready: ${log}`);
    try { if ((await fetch(`${base}/healthz`, {signal: AbortSignal.timeout(1000)})).ok) return {child, base}; } catch { /* not listening yet */ }
    await pause(50);
  }
  child.kill('SIGKILL');
  throw new Error(`Yabane did not become ready: ${log}`);
}

async function stopServer(child) {
  if (child.exitCode !== null) return;
  const exited = once(child, 'exit');
  child.kill('SIGTERM');
  const deadline = setTimeout(() => child.kill('SIGKILL'), 5000);
  try { await exited; } finally { clearTimeout(deadline); }
}

try {
  await mkdir(activityDirectory, { recursive: true });

  // Case 1: a torn final record is quarantined and startup continues.
  const complete = `${record('first')}\n${record('second')}\n`;
  const fragment = '{"timestamp":1,"request_id":"torn","path":"/v1/responses","model":"recovery/model"';
  await writeFile(dayFile, complete + fragment);

  const port = await freePort();
  let server = await startServer(port);
  assert.equal(await (await fetch(`${server.base}/healthz`)).status, 200, 'startup continues after a torn tail');
  await stopServer(server.child);

  assert.equal(await readFile(dayFile, 'utf8'), complete, 'the complete prefix stays byte for byte');
  const quarantined = (await readdir(activityDirectory)).filter(name => name.startsWith(`${today}.jsonl.truncated-`));
  assert.equal(quarantined.length, 1, 'the torn fragment is preserved for inspection');
  assert.equal(await readFile(path.join(activityDirectory, quarantined[0]), 'utf8'), fragment);

  // Case 2: damage between complete records fails startup and names the file and line.
  await writeFile(dayFile, `${complete}{not json}\n${record('third')}\n`);
  const failing = spawn(binary, ['--addr', `127.0.0.1:${port}`], {
    cwd: work, env: { PATH: process.env.PATH }, stdio: ['ignore', 'pipe', 'pipe'],
  });
  let failingLog = '';
  for (const stream of [failing.stdout, failing.stderr]) stream.on('data', chunk => { failingLog = (failingLog + chunk).slice(-32000); });
  const [code] = await once(failing, 'exit');
  assert.notEqual(code, 0, 'mid-file corruption must fail startup visibly');
  assert.ok(failingLog.includes(`${today}.jsonl`), `the failing file must be named: ${failingLog}`);
  assert.ok(failingLog.includes('line 3'), `the failing line must be named: ${failingLog}`);

  console.log('Activity recovery passed: torn tail quarantined, mid-file corruption rejected');
} finally {
  await rm(work, { recursive: true, force: true });
}
