// SVC-62: real embedded assets, WCAG 2.1 A/AA rules and keyboard regressions.
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { once } from 'node:events';
import { mkdtemp, rm } from 'node:fs/promises';
import http from 'node:http';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';
import { chromium, webkit } from 'playwright';
import { openConsoleView } from './console-view-helper.mjs';

const require = createRequire(import.meta.url);
const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
if (!process.argv[2]) {
  const build = spawnSync('cargo', ['build', '--locked'], {cwd: repo, stdio: 'inherit'});
  if (build.error) throw build.error;
  if (build.status !== 0) process.exit(build.status || 1);
}
const binary = path.resolve(process.argv[2] || path.join(repo, 'target/debug/yabane'));
const work = await mkdtemp(path.join(tmpdir(), 'yabane-accessibility-'));
const reservation = http.createServer();
reservation.listen(0, '127.0.0.1');
await once(reservation, 'listening');
const port = reservation.address().port;
await new Promise(resolve => reservation.close(resolve));
const base = `http://127.0.0.1:${port}`;
const upstream = http.createServer((request, response) => {
  response.setHeader('content-type', 'application/json');
  response.end(JSON.stringify(request.url.endsWith('/models')
    ? {object: 'list', data: [{id: 'model-one', object: 'model'}, {id: 'model-two', object: 'model'}]}
    : {id: 'accessibility', choices: [{message: {role: 'assistant', content: 'Fixture'}}], usage: {prompt_tokens: 10, completion_tokens: 5, total_tokens: 15}}));
});
upstream.listen(0, '127.0.0.1');
await once(upstream, 'listening');
const upstreamUrl = `http://127.0.0.1:${upstream.address().port}/v1`;
const child = spawn(binary, ['--addr', `127.0.0.1:${port}`], {cwd: work, env: {PATH: process.env.PATH}, stdio: ['ignore', 'pipe', 'pipe']});
let log = '';
child.on('error', error => { log += String(error); });
for (const stream of [child.stdout, child.stderr]) stream.on('data', chunk => { log = (log + chunk).slice(-16000); });
const failures = [];
let scans = 0;
let fixtureSeeded = false;
const views = ['home', 'providers', 'models', 'pricing', 'extensions', 'access', 'activity', 'management'];
async function audit(page, label) {
  await page.addScriptTag({path: require.resolve('axe-core/axe.min.js')});
  const result = await page.evaluate(async () => {
    await document.fonts.ready;
    return axe.run(document, {runOnly: {type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa']}});
  });
  scans++;
  console.log(`Scanned ${label}: ${result.violations.length} violations`);
  for (const violation of result.violations) {
    const failure = {label, rule: violation.id, nodes: violation.nodes.map(node => ({target: node.target, detail: node.failureSummary}))};
    failures.push(failure);
    console.log(JSON.stringify(failure, null, 2));
  }
}
async function noOverflow(page, label) {
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1), false, `${label}: horizontal overflow`);
}
try {
  let ready = false;
  for (let attempt = 0; attempt < 100; attempt++) {
    if (child.exitCode !== null) throw new Error(`Server exited: ${log}`);
    try { ready = (await fetch(`${base}/healthz`, {signal: AbortSignal.timeout(500)})).ok; } catch { /* Startup readiness polling. */ }
    if (ready) break;
    await new Promise(resolve => setTimeout(resolve, 50));
  }
  assert.ok(ready, log);
  for (const project of [
    {name: 'chrome-desktop', engine: chromium, launch: {channel: process.env.YABANE_BROWSER_CHANNEL || 'chrome'}, width: 1440, height: 900},
    {name: 'webkit-320', engine: webkit, launch: {}, width: 320, height: 800},
  ]) {
    const browser = await project.engine.launch({...project.launch, headless: true});
    try {
      const context = await browser.newContext({viewport: {width: project.width, height: project.height}, reducedMotion: 'reduce'});
      // Retain the real periodic callback for deterministic pause/resume testing.
      await context.addInitScript(() => {
        const original = window.setInterval;
        window.setInterval = (callback, delay, ...args) => {
          if (delay === 30000) window.__liveRefreshTick = callback;
          return original(callback, delay, ...args);
        };
      });
      const page = await context.newPage();
      const errors = [];
      page.on('pageerror', error => errors.push(error.message));
      await page.goto(base);
      await page.locator('#login-screen').waitFor({state: 'visible'});
      await audit(page, `${project.name}: ${project.name === 'chrome-desktop' ? 'setup' : 'login'}`);
      await page.locator('.skip-link').focus();
      await page.keyboard.press('Enter');
      assert.equal(await page.evaluate(() => document.activeElement.id), 'login-title');
      const auth = await context.request.post(`${base}/admin/${project.name === 'chrome-desktop' ? 'setup' : 'login'}`, {data: {username: 'accessibility', email: 'accessibility@example.test', password: 'fixture-password'}});
      assert.equal(auth.status(), 204);
      await page.reload();
      await page.waitForFunction(() => !document.querySelector('#admin-app').hidden && endpointTypes.length > 0);
      // Safari's default keyboard mode reaches links with Option+Tab.
      await page.keyboard.press(project.engine === webkit ? 'Alt+Tab' : 'Tab');
      assert.equal(await page.locator('.skip-link').evaluate(element => element === document.activeElement), true);
      await page.keyboard.press('Enter');
      assert.equal(await page.evaluate(() => document.activeElement.matches('main h1')), true);
      await page.emulateMedia({reducedMotion: 'no-preference'});
      const longAnimations = await page.evaluate(() => document.getAnimations().filter(animation => {
        const timing = animation.effect.getComputedTiming();
        return timing.endTime > 5000;
      }).map(animation => animation.animationName));
      assert.deepEqual(longAnimations, [], `${project.name}: decorative motion exceeds five seconds`);
      await page.emulateMedia({reducedMotion: 'reduce'});
      assert.equal(await page.evaluate(() => document.getAnimations().length), 0, `${project.name}: reduced motion`);
      await page.locator('#account-menu').click();
      await audit(page, `${project.name}: account controls`);
      await page.keyboard.press('Escape');
      assert.equal(await page.locator('#account-menu').evaluate(element => element === document.activeElement), true);
      await page.locator('#account-menu').click();
      await page.locator('#open-profile').click();
      await page.keyboard.press('Escape');
      await page.waitForFunction(() => document.activeElement === document.querySelector('#account-menu'));
      for (const view of views) {
        await openConsoleView(page, view);
        await audit(page, `${project.name}: ${view}`);
        await noOverflow(page, `${project.name}: ${view}`);
      }
      await openConsoleView(page, 'providers');
      await page.locator('#open-provider').focus();
      await page.keyboard.press('Enter');
      await audit(page, `${project.name}: provider step 1`);
      await page.locator('#display-name').fill('Accessible fixture');
      await page.locator('#next-step').click();
      await audit(page, `${project.name}: provider step 2`);
      await page.keyboard.press('Escape');
      await page.locator('#provider-dialog').waitFor({state: 'hidden'});
      await page.waitForFunction(() => document.activeElement === document.querySelector('#open-provider'));
      // Pointer activation in WebKit need not focus the clicked button.
      await page.evaluate(() => focusPageHeading());
      await page.locator('#open-provider').click();
      await page.keyboard.press('Escape');
      await page.waitForFunction(() => document.activeElement === document.querySelector('#open-provider'));
      // A saved form can replace the opener while its dialog is still open.
      await page.evaluate(() => {
        const opener = document.createElement('button');
        document.querySelector('main').append(opener);
        opener.focus();
        showDialog(document.querySelector('#provider-dialog'), opener);
        opener.remove();
        document.querySelector('#provider-dialog').close();
      });
      await page.waitForFunction(() => document.activeElement.matches('main h1'));
      // Audit every static dialog as well as initialized workflows below.
      const dialogs = await page.locator('dialog').evaluateAll(nodes => nodes.map(node => node.id));
      for (const id of dialogs) {
        await page.evaluate(id => showDialog(document.getElementById(id)), id);
        await audit(page, `${project.name}: ${id}`);
        const tabs = page.locator(`#${id} [role="tablist"]:visible [role="tab"]`);
        if (await tabs.count() > 1) {
          await tabs.first().focus();
          await page.keyboard.press('ArrowRight');
          assert.equal(await tabs.nth(1).getAttribute('aria-selected'), 'true', `${id}: arrow selection`);
          assert.equal(await tabs.nth(1).evaluate(tab => tab === document.activeElement), true, `${id}: arrow focus`);
          await audit(page, `${project.name}: ${id} second tab`);
        }
        await page.evaluate(id => document.getElementById(id).close(), id);
      }
      if (!fixtureSeeded) {
        const api = context.request;
        assert.ok((await api.post(`${base}/admin/providers`, {data: {id: 'accessibility', name: 'Accessibility fixture', endpoint: {id: 'primary', api_type: 'openai_compatible', base_url: upstreamUrl, requires_credential: true, credential_secret: 'fixture-secret'}}})).ok());
        assert.ok((await api.post(`${base}/admin/providers/accessibility/models/refresh`)).ok());
        assert.ok((await api.post(`${base}/admin/routes`, {data: {pattern: 'accessible-route', targets: [{provider_id: 'accessibility', endpoint_id: 'primary', credential_id: 'default', upstream_model: 'model-one', weight: 100}]}})).ok());
        assert.ok((await api.patch(`${base}/admin/pricing`, {data: {models: {'model-one': {input_per_million: 1, output_per_million: 2}}}})).ok());
        const keyResponse = await api.post(`${base}/admin/auth/keys`, {data: {note: 'Accessibility fixture', provider_ids: []}});
        assert.ok(keyResponse.ok());
        const key = await keyResponse.json();
        assert.ok((await api.post(`${base}/v1/chat/completions`, {headers: {authorization: `Bearer ${key.secret}`}, data: {model: 'accessible-route', messages: [{role: 'user', content: 'Fixture'}]}})).ok());
        fixtureSeeded = true;
      }
      await page.reload();
      await page.waitForFunction(() => !document.querySelector('#admin-app').hidden && endpointTypes.length > 0);
      for (const view of views) {
        await openConsoleView(page, view);
        await audit(page, `${project.name}: populated ${view}`);
        await noOverflow(page, `${project.name}: populated ${view}`);
      }
      await openConsoleView(page, 'providers');
      await page.locator('.provider-list-item').first().click();
      await page.locator('.endpoint-card').waitFor();
      await audit(page, `${project.name}: Provider detail`);
      await page.locator('.browse-provider-models').click();
      await audit(page, `${project.name}: Provider catalog`);
      await page.locator('.edit-provider-options').click();
      await page.locator('.add-default-header').click();
      await page.locator('#default-header-rows .defaults-name').last().fill('authorization');
      await page.locator('#default-header-rows .defaults-value').last().fill('fixture');
      await page.waitForFunction(() => document.querySelector('#default-header-rows .row-error').textContent.length > 0);
      assert.equal(await page.locator('#default-header-rows .row-error').last().getAttribute('role'), 'alert');
      await audit(page, `${project.name}: request-default validation`);
      await page.keyboard.press('Escape');
      await page.locator('.add-credential').click();
      await audit(page, `${project.name}: add credential`);
      await page.keyboard.press('Escape');
      await openConsoleView(page, 'models');
      await page.evaluate(() => openRouteDialog(modelRoutes[0]));
      await audit(page, `${project.name}: populated route editor`);
      await page.locator('#route-dialog .route-provider .picker-trigger').first().focus();
      await page.keyboard.press('ArrowDown');
      await page.locator('#route-dialog [role="listbox"]:visible').waitFor();
      await audit(page, `${project.name}: destination picker`);
      assert.equal(await page.evaluate(() => document.activeElement.getAttribute('role')), 'option');
      await page.keyboard.press('Enter');
      await page.locator('#add-route-target').click();
      await audit(page, `${project.name}: second route destination`);
      const duplicateIds = await page.evaluate(() => {
        const ids = [...document.querySelectorAll('[id]')].map(element => element.id);
        return ids.filter((id, index) => ids.indexOf(id) !== index);
      });
      assert.deepEqual(duplicateIds, [], `${project.name}: duplicate accessibility IDs`);
      await page.keyboard.press('Escape');
      await openConsoleView(page, 'pricing');
      await page.locator('#open-pricing-editor').click();
      await audit(page, `${project.name}: price editor`);
      await page.locator('#cancel-pricing-edit').click();
      await openConsoleView(page, 'activity');
      await page.locator('#recent-activity-logs .activity-request-row').first().click();
      await audit(page, `${project.name}: populated Activity detail`);
      await page.keyboard.press('Escape');
      await page.locator('#activity-range-trigger').click();
      await audit(page, `${project.name}: Activity range picker`);
      await page.keyboard.press('Escape');
      await page.locator('#activity-filter-trigger').click();
      await audit(page, `${project.name}: Activity filters`);
      await page.keyboard.press('Escape');
      await page.locator('.show-request-explorer').click();
      assert.equal(await page.locator('[data-activity-tab="requests"]').getAttribute('tabindex'), '0');
      assert.equal(await page.locator('[data-activity-tab="overview"]').getAttribute('tabindex'), '-1');
      await page.locator('#activity-logs .activity-request-row').first().waitFor();
      await audit(page, `${project.name}: request explorer`);
      await page.evaluate(() => document.querySelector('#pause-live-updates').click());
      assert.equal(await page.locator('#pause-live-updates').getAttribute('aria-pressed'), 'true');
      const pausedRequests = await page.evaluate(() => {
        let calls = 0;
        const originalFetch = window.fetch;
        window.fetch = (...args) => { calls++; return originalFetch(...args); };
        try { window.__liveRefreshTick(); return calls; }
        finally { window.fetch = originalFetch; }
      });
      assert.equal(pausedRequests, 0);
      const resumed = page.waitForResponse(response => response.url().includes('/admin/activity/stats'));
      await page.evaluate(() => document.querySelector('#pause-live-updates').click());
      assert.equal(await page.locator('#pause-live-updates').getAttribute('aria-pressed'), 'false');
      assert.ok((await resumed).ok());
      await page.goto(`${base}/docs`);
      await page.locator('.docs-op').first().waitFor();
      await audit(page, `${project.name}: docs`);
      await noOverflow(page, `${project.name}: docs`);
      await page.locator('.docs-op[data-method="post"]').first().click();
      await audit(page, `${project.name}: docs operation`);
      await noOverflow(page, `${project.name}: docs operation`);
      assert.deepEqual(errors, [], `${project.name}: browser errors`);
      await context.close();
    } finally { await browser.close(); }
  }
  assert.deepEqual(failures, [], JSON.stringify(failures, null, 2));
  console.log(`Accessibility passed: ${scans} WCAG 2.1 A/AA axe scans, desktop Chrome and 320px WebKit, populated workflows, keyboard navigation, modal focus return and live-update pause.`);
} finally {
  upstream.closeAllConnections();
  await new Promise(resolve => upstream.close(resolve));
  if (child.pid && child.exitCode === null) { const exited = once(child, 'exit'); child.kill('SIGTERM'); await exited; }
  await rm(work, {recursive: true, force: true});
}
