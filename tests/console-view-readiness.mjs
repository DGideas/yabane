import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {setTimeout as delay} from 'node:timers/promises';
import {chromium, webkit} from 'playwright';
import {openConsoleView} from './console-view-helper.mjs';

// Test the waiting helper itself with a real HTTP response: headers arrive now,
// but the body is held until the test releases it. route.fulfill cannot model this.
let releaseBody;
const server = createServer((request, response) => {
  if (request.url === '/data') {
    response.writeHead(200, {'content-type': 'application/json'});
    response.flushHeaders();
    // WebKit may buffer headers until the first body bytes arrive. Still withhold
    // the JSON value and closing brace, so response.json() cannot complete.
    response.write('{"name":');
    releaseBody = () => response.end('"Rendered after body"}');
    return;
  }
  response.writeHead(200, {'content-type': 'text/html'});
  response.end(`<!doctype html><html><body>
    <aside id="console-sidebar">
      <button class="nav" data-view="management">Management</button>
      <button class="nav" data-view="activity">Activity</button>
    </aside>
    <section id="management-view" hidden>
      <div id="management-keys-empty" hidden>Empty</div>
      <table id="management-keys-table"><tbody id="management-keys"><tr><td>Stale content</td></tr></tbody></table>
    </section>
    <section id="activity-view" hidden>
      <table><tbody id="recent-activity-logs"><tr><td>Stale overview</td></tr></tbody></table>
      <div id="request-page">Stale request page</div>
    </section>
    <script>
      async function loadManagementKeys() {
        const response = await fetch('/data');
        window.headersReceived = true;
        const data = await response.json();
        document.querySelector('#management-keys td').textContent = data.name;
      }
      async function loadActivityPage() {
        const response = await fetch('/data');
        window.headersReceived = true;
        const data = await response.json();
        document.querySelector('#request-page').textContent = data.name;
      }
      async function loadActivity() {
        document.querySelector('#recent-activity-logs td').textContent = 'New overview';
        loadActivityPage(); // Intentionally not awaited, like the console's explorer load.
      }
      document.querySelectorAll('.nav').forEach(button => button.onclick = () => {
        document.querySelectorAll('.nav').forEach(node => node.classList.toggle('active', node === button));
        document.querySelectorAll('section').forEach(node => node.hidden = node.id !== button.dataset.view + '-view');
        if (window.skipLoader) return;
        if (button.dataset.view === 'management') loadManagementKeys();
        else loadActivity();
      });
    </script></body></html>`);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const base = `http://127.0.0.1:${server.address().port}`;
try {
  for (const [engine, launch] of [[chromium, {channel: process.env.YABANE_BROWSER_CHANNEL || 'chrome'}], [webkit, {}]]) {
    const browser = await engine.launch({...launch, headless: true});
    try {
      const page = await browser.newPage();
      for (const [view, selector] of [['management', '#management-keys td'], ['activity', '#request-page']]) {
        await page.goto(base);
        let settled = false;
        const navigation = openConsoleView(page, view).finally(() => { settled = true; });
        // Observe rejection immediately, even if a regression fails before the body release.
        navigation.catch(() => {});
        try {
          await page.waitForFunction(() => window.headersReceived === true, undefined, {timeout: 2000});
          // Deliberate negative-test hold, not a readiness heuristic: the helper must
          // remain pending even after fetch() has resolved and several frames passed.
          await delay(150);
          assert.equal(settled, false, `${view}: accepted headers/stale DOM before the body arrived`);
          assert.match(await page.locator(selector).textContent(), /^Stale/);
        } finally {
          releaseBody?.();
          await navigation;
        }
        assert.equal(await page.locator(selector).textContent(), 'Rendered after body');
        assert.equal(await page.evaluate(() => '__yabaneViewLoad' in window), false);
      }

      await page.goto(base);
      await page.evaluate(() => { window.skipLoader = true; });
      await assert.rejects(openConsoleView(page, 'management'), /Navigation did not invoke loadManagementKeys/);
      assert.equal(await page.evaluate(() => '__yabaneViewLoad' in window), false);

      await page.goto(base);
      await page.evaluate(() => {
        window.loadManagementKeys = async () => { throw new Error('fixture parse failure'); };
        window.originalLoader = window.loadManagementKeys;
      });
      await assert.rejects(openConsoleView(page, 'management'), /fixture parse failure/);
      assert.equal(await page.evaluate(() => window.loadManagementKeys === window.originalLoader), true);

      await page.goto(base);
      try {
        await assert.rejects(openConsoleView(page, 'management', {timeout: 250}), /Timeout/);
        assert.equal(await page.evaluate(() => '__yabaneViewLoad' in window), false);
      } finally {
        releaseBody?.();
      }
    } finally {
      await browser.close();
    }
  }
  console.log('Console readiness passed: delayed body, nested load, missing trigger, failure, and timeout');
} finally {
  server.closeAllConnections();
  await new Promise(resolve => server.close(resolve));
}
