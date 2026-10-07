// ENDPOINT-42: delay sources are response-time facts, survive policy edits, and
// distinguish a zero Retry-After from a missing header. PROXY-43: no retries.
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
const work = await mkdtemp(path.join(tmpdir(), 'yabane-cooldown-history-'));
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
try {
  let reported = '120';
  let attempts = 0;
  const errorBody = '{"error":{"message":"fixture quota exhausted"}}';
  const provider = await listen((req, res) => {
    req.resume();
    res.setHeader('content-type', 'application/json');
    if (req.method === 'GET') {
      res.end('{"data":[{"id":"fixture"}]}');
      return;
    }
    attempts++;
    if (reported !== null) res.setHeader('retry-after', reported);
    res.writeHead(429);
    res.end(errorBody);
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
  for (const stream of [child.stdout, child.stderr]) stream.on('data', chunk => { serverLog = (serverLog + chunk).slice(-32000); });
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
    method: 'POST', ...json({ username: 'cooldown', email: 'cooldown@example.test', password: 'fixture-password' }),
  });
  assert.equal(setup.status, 204);
  const cookie = setup.headers.get('set-cookie').split(';')[0];
  const admin = async (route, method = 'GET', body) => {
    const options = body === undefined ? { headers: {} } : json(body);
    const response = await request(`${base}${route}`, { method, ...options, headers: { ...options.headers, cookie } });
    assert.ok(response.ok, `${route}: ${response.status}`);
    return response.status === 204 ? null : response.json();
  };
  await admin('/admin/auth', 'PATCH', { enabled: false });
  const endpoint = {
    id: 'main', api_type: 'openai_chat_completions', base_url: `${origin(provider)}/v1`,
    requires_credential: true,
  };
  await admin('/admin/providers', 'POST', {
    id: 'fixture', name: 'Fixture', endpoint: {
      ...endpoint, credential_secret: 'not-a-real-secret', rate_limit_cooldown: { seconds: 300, mode: 'fixed' },
    },
  });
  const getEndpoint = async () => (await admin('/admin/providers')).find(p => p.id === 'fixture').endpoints[0];
  const policy = (mode, seconds = 300) => admin('/admin/providers/fixture/endpoints/main', 'PATCH', {
    ...endpoint, socks5_proxy: null, rate_limit_cooldown: { mode, seconds },
  });
  const ids = [];
  const hit = async () => {
    const before = attempts;
    const response = await request(`${base}/v1/chat/completions`, {
      method: 'POST', ...json({ model: 'fixture/fixture', messages: [] }),
    });
    assert.equal(response.status, 429);
    assert.equal(response.headers.get('retry-after'), reported);
    assert.equal(await response.text(), errorBody);
    assert.equal(attempts, before + 1);
    ids.push(response.headers.get('x-yabane-request-id'));
    return (await getEndpoint()).rate_limit_cooldown_activity;
  };
  const fixed = await hit();
  assert.equal(fixed.last_seconds, 300);
  assert.equal(fixed.last_reported_seconds, 120);
  assert.equal(fixed.last_source, 'fixed');
  await policy('prefer_provider', 900);
  assert.deepEqual((await getEndpoint()).rate_limit_cooldown_activity, fixed);

  reported = '120';
  assert.equal((await hit()).last_source, 'provider');
  reported = '1800';
  const capped = await hit();
  assert.equal(capped.last_source, 'capped');
  assert.equal(capped.last_seconds, 900);
  reported = null;
  const fallback = await hit();
  assert.equal(fallback.last_source, 'fallback');
  assert.equal(fallback.last_seconds, 900);
  assert.equal(fallback.last_reported_seconds, undefined);
  await policy('provider_only', 60);
  assert.deepEqual((await getEndpoint()).rate_limit_cooldown_activity, fallback);
  const skipped = await hit();
  assert.equal(skipped.skipped, 1);
  assert.equal(skipped.last_source, 'fallback');
  assert.equal(skipped.last_seconds, 900);

  // Renaming clears prior health so zero can be checked without an older cooldown.
  await admin('/admin/providers/fixture/endpoints/main', 'PATCH', {
    ...endpoint, id: 'fresh', socks5_proxy: null,
    rate_limit_cooldown: { mode: 'prefer_provider', seconds: 300 },
  });
  reported = '0';
  const zero = await hit();
  assert.equal(zero.last_source, 'provider');
  assert.equal(zero.last_reported_seconds, 0);
  assert.equal(zero.last_seconds, 0);
  assert.equal(zero.applied, 1);
  assert.equal((await getEndpoint()).credentials[0].cooldown_seconds_remaining, undefined);

  let records = [];
  for (let i = 0; i < 50; i++) {
    records = await admin('/admin/activity/logs?limit=100');
    if (ids.every(id => records.some(record => record.request_id === id))) break;
    await pause(20);
  }
  for (const id of ids) {
    const record = records.find(record => record.request_id === id);
    assert.ok(record, `missing Activity ${id}`);
    assert.equal(record.status, 429);
    assert.equal(record.failure.stage, 'upstream_response');
  }
  if (process.env.YABANE_COOLDOWN_BROWSER === '1') {
    // Optional targeted UI check: system Chrome, isolated contexts, actual rebuilt
    // app/assets and a real temporary admin session; no personal browser profile.
    const { chromium } = await import('playwright');
    const browser = await chromium.launch({ channel: 'chrome', headless: true });
    try {
      for (const [width, height] of [[1440, 900], [768, 1024], [375, 667], [430, 932]]) {
        const context = await browser.newContext({ viewport: { width, height } });
        await context.addCookies([{ name: 'yabane_session', value: cookie.split('=')[1], url: base, httpOnly: true, sameSite: 'Strict' }]);
        const page = await context.newPage();
        const pageErrors = [];
        page.on('pageerror', error => pageErrors.push(error.message));
        let history = fixed;
        await page.route('**/admin/providers', async route => {
          const response = await route.fetch();
          const body = await response.json();
          const endpoint = body.find(provider => provider.id === 'fixture').endpoints[0];
          endpoint.rate_limit_cooldown = { seconds: 900, mode: 'provider_only' };
          endpoint.rate_limit_cooldown_activity = history;
          await route.fulfill({ response, json: body });
        });
        for (const [facts, expected] of [
          [fixed, 'fixed duration configured at the time'],
          [capped, 'so this cooldown was capped'],
          [fallback, 'configured maximum at the time'],
          [zero, 'no new cooldown was started'],
        ]) {
          history = facts;
          await page.goto(`${base}/providers/fixture`);
          const observed = page.locator('#provider-detail .pool-observed');
          await observed.waitFor({ state: 'visible' });
          const text = await observed.textContent();
          assert.ok(text.includes(expected), `${width}px: ${text}`);
          if (facts.last_seconds === 0) {
            assert.ok(text.includes('Provider asked for 0 seconds'));
            assert.ok(!text.includes('no usable delay'));
          }
          const overflow = await page.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1);
          assert.equal(overflow, false, `${width}px: horizontal overflow`);
        }
        assert.deepEqual(pageErrors, []);
        await context.close();
      }
      console.log('Cooldown browser check passed: system Chrome, 4 viewports, authenticated temporary session');
    } finally {
      await browser.close();
    }
  }
  console.log('Cooldown history passed: decision sources, edits, skipped answers, zero, original 429 and no retries');
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
