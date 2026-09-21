import { chromium, webkit } from 'playwright';

const base = process.env.YABANE_UI_BASE || 'http://127.0.0.1:8080';
const sessionCookie = process.env.YABANE_SESSION_COOKIE;
const captureRequestBody = JSON.stringify({model: 'gpt-fixture', input: 'diagnostic request', tools: Array.from({length: 60}, (_, index) => ({type: 'function', name: `tool-${index}`, description: `Diagnostic tool ${index}`}))}, null, 2);
const captureResponseBody = 'event: response.created\r\ndata: {"type":"response.created","response":{"id":"resp-ui","object":"response","status":"in_progress","model":"gpt-fixture","output":[]}}\r\n\r\nevent: response.output_text.delta\r\nid: 2\r\ndata: {"type":"response.output_text.delta",\r\ndata: "delta":"hello"}\r\n\r\nevent: response.completed\r\ndata: {"type":"response.completed","response":{"id":"resp-ui","object":"response","status":"completed","model":"gpt-fixture","output":[{"id":"msg-ui","type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":"hello <script>","annotations":[]}]}],"usage":{"input_tokens":4,"output_tokens":2,"total_tokens":6}}}\r\n\r\ndata: [DONE]\r\n\r\n';
const bytes = value => [...new TextEncoder().encode(value)];
const captureFixture = {
  request_id: 'ui-capture', timestamp: 1700000000, expires_at: 4102444800, public_model: 'ui-subscription/gpt-fixture', upstream_model: 'gpt-fixture',
  provider_id: 'ui-subscription', endpoint_id: 'chatgpt', caller_protocol: 'openai_responses', upstream_protocol: 'openai_responses', streaming: true,
  request_headers: [{name: 'content-type', value: 'application/json'}], request_body: bytes(captureRequestBody), request_truncated: false, status: 200, duration_ms: 17700,
  response_headers: [{name: 'content-type', value: 'text/event-stream'}], response_body: bytes(captureResponseBody), response_truncated: false, outcome: 'complete',
};
const projects = [
  { name: 'desktop-chrome', engine: chromium, launch: { channel: process.env.YABANE_BROWSER_CHANNEL || 'chrome' }, width: 1440, height: 900 },
  { name: 'tablet-chrome', engine: chromium, launch: { channel: process.env.YABANE_BROWSER_CHANNEL || 'chrome' }, width: 768, height: 1024 },
  { name: 'iphone-se-webkit', engine: webkit, launch: {}, width: 375, height: 667, mobile: true },
  { name: 'iphone-pro-max-webkit', engine: webkit, launch: {}, width: 430, height: 932, mobile: true },
];

for (const project of projects) {
  const browser = await project.engine.launch({ ...project.launch, headless: true });
  try {
    const context = await browser.newContext({
      viewport: { width: project.width, height: project.height },
      isMobile: project.mobile || false,
      hasTouch: project.mobile || false,
    });
    if (project.name === 'desktop-chrome') await context.grantPermissions(['clipboard-read', 'clipboard-write'], {origin: base});
    if (sessionCookie) {
      await context.addCookies([{ name: 'yabane_session', value: sessionCookie, url: base, httpOnly: true, sameSite: 'Strict' }]);
    }
    const page = await context.newPage();
    page.on('pageerror', error => console.error(`${project.name}: page error: ${error.stack || error.message}`));
    await page.route('https://models.dev/api.json', async route => {
      await route.fulfill({
        status: 200,
        headers: {'access-control-allow-origin': '*'},
        contentType: 'application/json',
        body: JSON.stringify({
          'reference-provider': {
            name: 'Reference Provider',
            models: {
              'activity-only-model': {id: 'activity-only-model', name: 'Activity Reference', cost: {input: 0.42, output: 1.75, cache_read: 0.08}},
            },
          },
        }),
      });
    });
    await page.route(`${base}/admin/extensions/traffic-capture/captures`, async route => {
      if (route.request().method() !== 'GET') return route.continue();
      await route.fulfill({status: 200, contentType: 'application/json', body: JSON.stringify([{...captureFixture, bytes: captureRequestBody.length + captureResponseBody.length, truncated: false}])});
    });
    await page.route(`${base}/admin/extensions/traffic-capture/captures/ui-capture`, async route => {
      if (route.request().method() !== 'GET') return route.continue();
      await route.fulfill({status: 200, contentType: 'application/json', body: JSON.stringify(captureFixture)});
    });
    await page.route('**/admin/openai-subscriptions/device-code', async route => {
      if (route.request().method() !== 'POST') return route.continue();
      await route.fulfill({status: 201, contentType: 'application/json', body: JSON.stringify({id: 'device-flow', status: 'pending', user_code: 'ABCD-EFGH', verification_uri: 'https://auth.openai.com/codex/device', interval_seconds: 60, expires_at: 4102444800})});
    });
    await page.route('**/admin/openai-subscriptions/device-code/device-flow', async route => {
      await route.fulfill({status: 200, contentType: 'application/json', body: JSON.stringify({id: 'device-flow', status: 'pending', user_code: 'ABCD-EFGH', verification_uri: 'https://auth.openai.com/codex/device', interval_seconds: 60, expires_at: 4102444800})});
    });
    await page.route('**/admin/openai-subscriptions/oauth', async route => {
      if (route.request().method() !== 'POST') return route.continue();
      await route.fulfill({status: 201, contentType: 'application/json', body: JSON.stringify({id: 'browser-flow', authorization_url: 'https://auth.openai.com/oauth/authorize?state=browser-state', expires_at: 4102444800})});
    });
    await page.route('**/admin/openai-subscriptions/oauth/browser-flow/complete', async route => {
      const body = route.request().postDataJSON();
      if (body.redirect_url !== 'http://localhost:1455/auth/callback?code=oauth-code&state=browser-state') throw new Error(`${project.name}: browser OAuth did not submit the complete callback URL`);
      await route.fulfill({status: 204});
    });
    await page.route('**/admin/providers', async route => {
      if (route.request().method() !== 'GET') return route.continue();
      const response = await route.fetch();
      if (!response.ok()) return route.fulfill({response});
      const body = await response.json();
      body.push({
        id: 'ui-subscription', name: 'UI subscription fixture', extra_headers: {}, extra_body: {}, defaults_endpoint_ids: [],
        endpoints: [{
          id: 'chatgpt', api_type: 'openai_codex', base_url: 'https://chatgpt.com/backend-api', socks5_proxy: null,
          extra_headers: {}, extra_body: {}, requires_api_key: false, api_keys: [], subscription_connected: true,
          subscription_expires_at: 1,
        }],
        discovered_models: ['gpt-fixture'], model_endpoints: {'gpt-fixture': ['chatgpt']}, model_endpoint_preferences: [],
        models_discovered_at: 1, model_discovery_error: null,
      }, {
        id: 'ui-keyless', name: 'UI keyless fixture', extra_headers: {}, extra_body: {}, defaults_endpoint_ids: [],
        endpoints: [{
          id: 'local', api_type: 'openai_compatible', base_url: 'http://127.0.0.1:18080/v1', socks5_proxy: null,
          extra_headers: {}, extra_body: {}, requires_api_key: false, api_keys: [], subscription_connected: false,
          subscription_expires_at: null,
        }],
        discovered_models: ['local-model'], model_endpoints: {'local-model': ['local']}, model_endpoint_preferences: [],
        models_discovered_at: 1, model_discovery_error: null,
      });
      await route.fulfill({response, json: body});
    });
    await page.route('**/admin/activity/logs*', async route => {
      const url = new URL(route.request().url());
      if (route.request().method() !== 'GET' || url.pathname !== '/admin/activity/logs') return route.continue();
      const response = await route.fetch();
      if (!response.ok()) return route.fulfill({response});
      const body = await response.json();
      body.unshift({
        timestamp: Math.floor(Date.now() / 1000), request_id: `ui-unchanged-model-${project.name}`, source_instance_id: 'responsive-remote-instance',
        path: '/v1/responses', model: 'activity-only-model', upstream_model: 'activity-only-model',
        provider: 'ui-subscription', endpoint: 'chatgpt', caller_protocol: 'openai_responses', upstream_protocol: 'openai_responses',
        status: 200, latency_ms: 110, gateway_ms: 4, upstream_response_ms: 18, first_byte_ms: 28, generation_ms: 82,
        input_tokens: 100, output_tokens: 30, cached_tokens: 10, cost: null, finish_reason: 'completed', streaming: false,
      }, {
        timestamp: Math.floor(Date.now() / 1000) - 1, request_id: `ui-mapped-model-${project.name}`,
        path: '/v1/messages', model: 'ui-subscription/gpt-fixture', upstream_model: 'activity-only-model',
        provider: 'ui-subscription', endpoint: 'chatgpt', caller_protocol: 'anthropic_messages', upstream_protocol: 'openai_responses',
        status: 200, latency_ms: 120, gateway_ms: 4, upstream_response_ms: 20, first_byte_ms: 30, generation_ms: 90,
        input_tokens: 120, output_tokens: 40, cached_tokens: 20, cost: null, finish_reason: 'completed', streaming: false,
      });
      await route.fulfill({response, json: body});
    });
    await page.route('**/admin/activity/recalculate-costs', async route => {
      if (route.request().method() !== 'POST') return route.continue();
      await route.fulfill({status: 200, contentType: 'application/json', body: JSON.stringify({candidates: 2, updated: 2, filled: 1, recalculated: 1, reported_preserved: 3, skipped_missing_route: 0, skipped_missing_pricing: 0, skipped_missing_usage: 0})});
    });
    const testLiveRefresh = project.name === 'desktop-chrome';
    if (testLiveRefresh) await page.clock.install();

    await page.goto(`${base}/docs`, {waitUntil: 'domcontentloaded'});
    await page.locator('.docs-op').first().waitFor();
    await assertNoPageOverflow(page, project.name, 'API docs');
    if (project.width > 900) {
      const docsScroll = await page.locator('#docs-nav').evaluate(nav => {
        const main = document.querySelector('#docs-main');
        const spacer = document.createElement('div');
        spacer.style.height = '150vh';
        main.append(spacer);
        nav.scrollTop = nav.scrollHeight;
        main.scrollTop = main.scrollHeight;
        return {
          navScrollTop: nav.scrollTop,
          mainScrollTop: main.scrollTop,
          navBottom: nav.getBoundingClientRect().bottom,
          mainBottom: main.getBoundingClientRect().bottom,
          viewportHeight: window.innerHeight,
          pageScrollY: window.scrollY,
        };
      });
      if (docsScroll.navScrollTop <= 0) throw new Error(`${project.name}: API docs Endpoint navigation is not independently scrollable`);
      if (docsScroll.mainScrollTop <= 0) throw new Error(`${project.name}: API docs reference content is not independently scrollable`);
      if (docsScroll.pageScrollY !== 0) throw new Error(`${project.name}: scrolling API docs content also scrolls the page`);
      if (Math.abs(docsScroll.navBottom - docsScroll.viewportHeight) > 1 || Math.abs(docsScroll.mainBottom - docsScroll.viewportHeight) > 1) throw new Error(`${project.name}: API docs panes are not contained by the viewport`);
    }

    await page.goto(base, { waitUntil: 'domcontentloaded' });
    await page.waitForFunction(() => !document.querySelector('#login-screen')?.hidden || !document.querySelector('#admin-app')?.hidden);
    if (await page.locator('#login-screen').isVisible()) {
      await page.locator('#login-form [name="username"]').waitFor();
      await page.waitForFunction(() => document.querySelector('#login-form [name="username"]') === document.activeElement);
      const authBackdrop = page.locator('#login-screen > .auth-backdrop');
      if (!(await authBackdrop.isVisible())) throw new Error(`${project.name}: authentication background artwork is not visible`);
      const authBackdropBehavior = await authBackdrop.evaluate(element => ({ pointerEvents: getComputedStyle(element).pointerEvents, ariaHidden: element.getAttribute('aria-hidden') }));
      if (authBackdropBehavior.pointerEvents !== 'none' || authBackdropBehavior.ariaHidden !== 'true') throw new Error(`${project.name}: authentication artwork can interfere with interaction or accessibility`);
      const introColor = await page.locator('.login-intro').evaluate(element => getComputedStyle(element).backgroundColor);
      if (introColor === 'rgb(255, 255, 255)') throw new Error(`${project.name}: authentication form has no branded identity surface`);
      await page.emulateMedia({ reducedMotion: 'reduce' });
      const authAnimations = await authBackdrop.locator('g').evaluateAll(groups => groups.map(group => getComputedStyle(group).animationName));
      if (authAnimations.some(name => name !== 'none')) throw new Error(`${project.name}: authentication background ignores reduced-motion preference`);
      await page.emulateMedia({ reducedMotion: 'no-preference' });
      await assertNoPageOverflow(page, project.name, 'login page');
      console.log(`${project.name}: login layout checked (authenticated dialog checks skipped)`);
      await context.close();
      continue;
    }

    const backdrop = page.locator('main > .console-backdrop');
    if (!(await backdrop.isVisible())) throw new Error(`${project.name}: console background artwork is not visible`);
    const backdropBehavior = await backdrop.evaluate(element => ({ pointerEvents: getComputedStyle(element).pointerEvents, ariaHidden: element.getAttribute('aria-hidden') }));
    if (backdropBehavior.pointerEvents !== 'none' || backdropBehavior.ariaHidden !== 'true') throw new Error(`${project.name}: console background artwork can interfere with interaction or accessibility`);
    await page.emulateMedia({ reducedMotion: 'reduce' });
    const backdropAnimations = await backdrop.locator('g').evaluateAll(groups => groups.map(group => getComputedStyle(group).animationName));
    if (backdropAnimations.some(name => name !== 'none')) throw new Error(`${project.name}: console background ignores reduced-motion preference`);
    await page.emulateMedia({ reducedMotion: 'no-preference' });

    await page.goto(`${base}/providers`, {waitUntil: 'domcontentloaded'});
    await page.waitForFunction(() => location.pathname === '/providers' && document.querySelector('.nav.active')?.dataset.view === 'providers');
    if (!(await page.locator('#provider-list-page').isVisible())) throw new Error(`${project.name}: directly opening /providers does not show the Provider list`);
    await page.evaluate(() => document.querySelector('[data-view="home"]').click());

    const accountAvatar = page.locator('#account-menu');
    const topbar = page.locator('.topbar');
    const [avatarBox, topbarBox] = await Promise.all([accountAvatar.boundingBox(), topbar.boundingBox()]);
    if (!avatarBox || !topbarBox || topbarBox.x + topbarBox.width - (avatarBox.x + avatarBox.width) > 21) throw new Error(`${project.name}: administrator avatar is not aligned to the top-right of the console`);

    const mobileNavToggle = page.locator('#mobile-nav-toggle');
    if (project.mobile) {
      const toggleBox = await mobileNavToggle.boundingBox();
      if (!toggleBox || toggleBox.width < 40 || toggleBox.height < 40) throw new Error(`${project.name}: mobile navigation trigger is missing or too small`);
      await mobileNavToggle.click();
      if (await mobileNavToggle.getAttribute('aria-expanded') !== 'true') throw new Error(`${project.name}: mobile navigation does not expose its open state`);
      await page.waitForFunction(() => document.querySelector('#console-sidebar').getBoundingClientRect().x >= -1);
      const sidebarBox = await page.locator('#console-sidebar').boundingBox();
      if (!sidebarBox || sidebarBox.x < -1 || sidebarBox.x + sidebarBox.width > project.width + 1) throw new Error(`${project.name}: mobile navigation drawer escapes the viewport`);
      await page.locator('#console-sidebar [data-view="providers"]').click();
      if (await mobileNavToggle.getAttribute('aria-expanded') !== 'false' || !(await page.locator('#mobile-nav-backdrop').isHidden())) throw new Error(`${project.name}: mobile navigation does not close after selection`);
      await mobileNavToggle.click();
      await page.keyboard.press('Escape');
      if (await mobileNavToggle.getAttribute('aria-expanded') !== 'false') throw new Error(`${project.name}: Escape does not close mobile navigation`);
      await page.evaluate(() => document.querySelector('[data-view="home"]').click());
    } else {
      if (await mobileNavToggle.isVisible()) throw new Error(`${project.name}: mobile navigation trigger is visible on a wide layout`);
      const aboutFooter = page.locator('#console-sidebar .about-link');
      const beforeScroll = await aboutFooter.boundingBox();
      await page.evaluate(() => {
        const spacer = document.createElement('div');
        spacer.id = 'sidebar-scroll-test-spacer';
        spacer.style.height = '150vh';
        document.querySelector('main').append(spacer);
        window.scrollTo(0, document.documentElement.scrollHeight);
      });
      await page.waitForFunction(() => window.scrollY > 0);
      const afterScroll = await aboutFooter.boundingBox();
      if (!beforeScroll || !afterScroll || Math.abs(afterScroll.y - beforeScroll.y) > 1) throw new Error(`${project.name}: About Yabane footer moves with page content`);
      await page.evaluate(() => {
        window.scrollTo(0, 0);
        document.querySelector('#sidebar-scroll-test-spacer')?.remove();
      });
    }

    for (const emptyState of [
      {view: 'access', container: '#gateway-keys-empty', action: '#empty-gateway-key', dialog: '#gateway-key-dialog'},
      {view: 'management', container: '#management-keys-empty', action: '#empty-management-key', dialog: '#management-key-dialog'},
    ]) {
      await page.evaluate(view => document.querySelector(`[data-view="${view}"]`).click(), emptyState.view);
      if (emptyState.view === 'management' && project.mobile) {
        const descriptionWidth = await page.locator('#management-view .card-head p').evaluate(element => element.getBoundingClientRect().width);
        if (descriptionWidth < 250) throw new Error(`${project.name}: Management API card header remains cramped beside its docs action`);
      }
      if (await page.locator(emptyState.container).isVisible()) {
        if (!(await page.locator(`${emptyState.container} .empty-illustration`).isVisible())) throw new Error(`${project.name}: ${emptyState.view} empty state has no illustration`);
        await page.locator(emptyState.action).click();
        if (!(await page.locator(emptyState.dialog).isVisible())) throw new Error(`${project.name}: ${emptyState.view} empty-state action did not open its dialog`);
        await page.keyboard.press('Escape');
      }
    }
    await page.evaluate(() => document.querySelector('[data-view="extensions"]').click());
    if (await page.locator('.extension-build-commands code').allTextContents().then(values => !values.includes('cargo build --release') || !values.includes('cargo build --release --no-default-features'))) throw new Error(`${project.name}: Extensions page omits build instructions`);
    const developmentLink = page.locator('.extension-development-link');
    if (!(await developmentLink.isVisible()) || await developmentLink.getAttribute('href') !== 'https://github.com/DGideas/yabane/blob/master/.agents/skills/yabane-extensions/SKILL.md') throw new Error(`${project.name}: Extensions page omits the development guide link`);
    const requestDefaultsExtension = page.locator('.extension-card').filter({hasText: 'request-defaults'});
    if (!(await requestDefaultsExtension.isVisible())) throw new Error(`${project.name}: Request Defaults is missing from Extensions`);
    const openAiSubscriptionExtension = page.locator('.extension-card').filter({hasText: 'openai-subscription'});
    if (!(await openAiSubscriptionExtension.isVisible()) || !(await openAiSubscriptionExtension.getByText('1 Endpoint', {exact: true}).isVisible())) throw new Error(`${project.name}: OpenAI Subscription Extension is missing or not linked to its Endpoint resources`);
    if (!(await openAiSubscriptionExtension.getByText('provider endpoint', {exact: true}).isVisible())) throw new Error(`${project.name}: OpenAI Subscription Extension does not declare its Endpoint stage`);
    if (project.name === 'desktop-chrome') {
      let disableWarning = '';
      page.once('dialog', async dialog => { disableWarning = dialog.message(); await dialog.dismiss(); });
      await openAiSubscriptionExtension.locator('[data-extension-toggle="openai-subscription"]').evaluate(input => input.click());
      await page.waitForFunction(() => document.querySelector('[data-extension-toggle="openai-subscription"]').checked);
      if (!disableWarning.includes('1 configured OpenAI subscription Endpoint') || !disableWarning.includes('routing, model discovery, sign-in, token refresh, and inference')) throw new Error(`${project.name}: disabling OpenAI Subscription does not confirm the impact on configured Endpoints`);
    }
    const trafficCaptureExtension = page.locator('.extension-card').filter({hasText: 'traffic-capture'});
    if (!(await trafficCaptureExtension.isVisible()) || !(await trafficCaptureExtension.getByText('Sensitive diagnostic data', {exact: true}).isVisible())) throw new Error(`${project.name}: Traffic Capture is missing or lacks its sensitive-data treatment`);
    if (!(await trafficCaptureExtension.getByText('traffic-capture · v0.1.1', {exact: true}).isVisible())) throw new Error(`${project.name}: Traffic Capture does not expose version 0.1.1`);
    if (!project.mobile) {
      const settingsSearch = page.locator('#settings-search');
      await settingsSearch.fill('extension');
      const extensionSearchLabels = await page.locator('#search-results strong').allTextContents();
      const expectedExtensionSearchLabels = await page.evaluate(() => ['Extensions', ...extensions.map(extension => `${extension.name} extension`)]);
      for (const label of expectedExtensionSearchLabels) {
        if (!extensionSearchLabels.includes(label)) throw new Error(`${project.name}: settings search omits the current ${label} entry`);
      }
      await settingsSearch.fill('traffic-capture');
      await page.locator('#search-results').getByText('Traffic Capture extension', {exact: true}).click();
      await page.locator('#traffic-capture-view').waitFor({state: 'visible'});
      await page.locator('#back-to-extensions').click();
    }
    await trafficCaptureExtension.getByRole('button', {name: 'Configure capture'}).click();
    await page.locator('#traffic-capture-view').waitFor({state: 'visible'});
    if (!(await page.getByText('Captured bodies may contain prompts, files, tool calls, and model output.', {exact: true}).isVisible())) throw new Error(`${project.name}: Traffic Capture does not warn about captured content`);
    if (!(await page.locator('#capture-form [name="provider_id"]').evaluate(element => element.required))) throw new Error(`${project.name}: Traffic Capture lacks a required Provider scope selector`);
    if (!(await page.getByText('Capture window', {exact: true}).isVisible()) || await page.getByText('Idle timeout', {exact: true}).count()) throw new Error(`${project.name}: Traffic Capture mislabels its fixed capture window as an idle timeout`);
    if (!(await page.getByText('Truncating a stream may remove its terminal event and prevent the Assembled view.', {exact: true}).isVisible())) throw new Error(`${project.name}: Traffic Capture does not explain the Body limit impact`);
    await page.locator('#capture-form [name="provider_id"]').selectOption({index: 1});
    await page.locator('#capture-form [name="endpoint_id"]').selectOption({index: 1});
    if (!(await page.locator('#capture-scope-summary').getByText(/Capture the next .* through .* for .* for up to/).isVisible())) throw new Error(`${project.name}: Traffic Capture lacks an exact preflight scope summary`);
    const refreshCaptures = page.getByRole('button', {name: 'Refresh captures'});
    if (!(await refreshCaptures.isVisible())) throw new Error(`${project.name}: Traffic Capture lacks a manual refresh action`);
    await Promise.all([
      page.waitForResponse(response => response.request().method() === 'GET' && response.url().endsWith('/admin/extensions/traffic-capture/status')),
      page.waitForResponse(response => response.request().method() === 'GET' && response.url().endsWith('/admin/extensions/traffic-capture/captures')),
      refreshCaptures.click(),
    ]);
    await page.waitForFunction(() => !document.querySelector('#refresh-captures').disabled);
    if (!project.mobile) {
      for (const heading of ['Capture', 'Route', 'Status', 'Upstream time / size']) {
        if (!(await page.locator('#capture-list-head').getByText(heading, {exact: true}).isVisible())) throw new Error(`${project.name}: Traffic Capture list lacks the ${heading} heading`);
      }
    }
    if (!(await page.locator('[data-capture-id="ui-capture"] .capture-result').getByText('200', {exact: true}).isVisible())) throw new Error(`${project.name}: Traffic Capture list lacks a scannable status result`);
    if (project.name === 'desktop-chrome') {
      await page.evaluate(() => {
        const form = document.querySelector('#capture-form');
        trafficCaptureStatus = {retained: 1, dropped: 0, config: {active: true, remaining: 2, expires_at: Math.floor(Date.now() / 1000) + 600, provider_id: form.elements.provider_id.value, endpoint_id: form.elements.endpoint_id.value, model: '', body_limit: 1048576, retention_days: 1, redacted_headers: []}};
        renderTrafficCapture();
      });
      if (!(await page.locator('#capture-active-summary').isVisible()) || !(await page.locator('#capture-active-title').getByText('Capturing next 2 matching requests', {exact: true}).isVisible())) throw new Error(`${project.name}: active Traffic Capture lacks a clear running-state summary`);
      if (!(await page.locator('#capture-form [name="provider_id"]').isDisabled()) || !(await page.locator('#capture-form [type="submit"]').isHidden())) throw new Error(`${project.name}: active Traffic Capture allows its scope to be changed before stopping`);
      await page.evaluate(() => loadTrafficCapture());
    }
    await page.locator('[data-capture-id="ui-capture"]').click();
    await page.locator('#traffic-capture-detail-dialog').waitFor({state: 'visible'});
    const requestBodyWasScrollable = await page.locator('#traffic-capture-detail-dialog').evaluate(dialog => { const body = dialog.querySelector('#capture-detail-body'); const detail = dialog.querySelector('.capture-detail-body'); body.scrollTop = body.scrollHeight; detail.scrollTop = detail.scrollHeight; return body.scrollTop > 0 || detail.scrollTop > 0; });
    if (!requestBodyWasScrollable) throw new Error(`${project.name}: Traffic Capture request fixture does not exercise body scrolling`);
    await page.locator('#traffic-capture-detail-dialog').getByRole('tab', {name: 'Response'}).click();
    if (await page.locator('#capture-detail-body').evaluate(element => element.scrollTop) !== 0 || await page.locator('.capture-detail-body').evaluate(element => element.scrollTop) !== 0) throw new Error(`${project.name}: Traffic Capture direction change preserves stale scroll position`);
    await assertDialog(page, '#traffic-capture-detail-dialog', project.name);
    const captureDialog = page.locator('#traffic-capture-detail-dialog');
    if (!(await captureDialog.getByRole('button', {name: 'Copy headers'}).isVisible()) || !(await captureDialog.getByRole('button', {name: 'Copy body'}).isVisible())) throw new Error(`${project.name}: Traffic Capture detail lacks copy actions`);
    if (project.name === 'desktop-chrome') {
      const dialogBox = await captureDialog.boundingBox();
      if (!dialogBox || dialogBox.width < project.width * .8 || dialogBox.height < project.height * .8) throw new Error(`${project.name}: Traffic Capture detail is still too small for captured payloads`);
    }
    await captureDialog.getByRole('tab', {name: 'Response'}).click();
    if (!(await captureDialog.getByText(/17.7 s upstream/).isVisible()) || !(await page.locator('[data-capture-id="ui-capture"]').getByText('17.7 s', {exact: true}).isVisible())) throw new Error(`${project.name}: Traffic Capture does not show upstream duration in its list and detail`);
    const assembledTab = captureDialog.getByRole('tab', {name: 'Assembled'});
    if (!(await assembledTab.isVisible()) || await assembledTab.getAttribute('aria-selected') !== 'true') throw new Error(`${project.name}: streaming response does not default to its assembled non-streaming structure`);
    const assembledBody = captureDialog.locator('#capture-detail-body');
    if (!(await assembledBody.getByText(/"status": "completed"/).isVisible()) || !(await assembledBody.getByText(/"text": "hello <script>"/).isVisible())) throw new Error(`${project.name}: assembled response does not use the terminal response object`);
    if (await assembledBody.locator('script').count()) throw new Error(`${project.name}: syntax highlighting injects captured markup`);
    if (!(await assembledBody.locator('.syntax-key').count()) || !(await assembledBody.locator('.syntax-string').count()) || !(await assembledBody.locator('.syntax-number').count())) throw new Error(`${project.name}: assembled JSON lacks syntax highlighting`);
    await captureDialog.getByRole('tab', {name: 'Raw'}).click();
    await assembledBody.evaluate(element => { element.scrollTop = element.scrollHeight; });
    await assembledTab.click();
    if (await assembledBody.evaluate(element => element.scrollTop) !== 0) throw new Error(`${project.name}: Traffic Capture body mode preserves stale scroll position`);
    await captureDialog.getByRole('tab', {name: 'Raw'}).click();
    if (!(await assembledBody.getByText(/event: response.created/).isVisible()) || !(await assembledBody.locator('.syntax-sse').count())) throw new Error(`${project.name}: assembled response cannot return to highlighted raw SSE`);
    if (project.name === 'desktop-chrome') {
      await captureDialog.getByRole('button', {name: 'Copy body'}).click();
      if (await page.evaluate(() => navigator.clipboard.readText()) !== captureResponseBody) throw new Error(`${project.name}: Copy body does not preserve the raw captured bytes`);
      await assembledTab.click();
      await captureDialog.getByRole('button', {name: 'Copy body'}).click();
      if (JSON.parse(await page.evaluate(() => navigator.clipboard.readText())).status !== 'completed') throw new Error(`${project.name}: Copy body does not copy the assembled response in Assembled mode`);
      const protocolAssemblies = await page.evaluate(() => {
        const chat = assembleCaptureResponse('openai_chat_completions', {malformed: false, done: true, events: [
          {id: 'chat-ui', created: 1, model: 'gpt-ui', choices: [{index: 0, delta: {role: 'assistant', content: 'hel'}, finish_reason: null}]},
          {choices: [{index: 0, delta: {content: 'lo'}, finish_reason: 'stop'}], usage: {prompt_tokens: 2, completion_tokens: 1, total_tokens: 3}},
        ]});
        const anthropic = assembleCaptureResponse('anthropic_messages', {malformed: false, done: false, events: [
          {type: 'message_start', message: {id: 'msg-ui', type: 'message', role: 'assistant', model: 'claude-ui', content: [], usage: {input_tokens: 2, output_tokens: 0}}},
          {type: 'content_block_start', index: 0, content_block: {type: 'text', text: ''}},
          {type: 'content_block_delta', index: 0, delta: {type: 'text_delta', text: 'hello'}},
          {type: 'message_delta', delta: {stop_reason: 'end_turn', stop_sequence: null}, usage: {output_tokens: 1}},
          {type: 'message_stop'},
        ]});
        return {chat, anthropic};
      });
      if (protocolAssemblies.chat.choices[0].message.content !== 'hello' || protocolAssemblies.chat.choices[0].finish_reason !== 'stop') throw new Error(`${project.name}: Chat Completions stream does not assemble into a non-streaming choice`);
      if (protocolAssemblies.anthropic.content[0].text !== 'hello' || protocolAssemblies.anthropic.stop_reason !== 'end_turn') throw new Error(`${project.name}: Anthropic stream does not assemble into a non-streaming message`);
    }
    await captureDialog.locator('.close-capture-detail').first().click();
    await page.locator('[data-capture-id="ui-capture"]').click();
    await captureDialog.waitFor({state: 'visible'});
    if (await assembledBody.evaluate(element => element.scrollTop) !== 0 || await captureDialog.locator('.capture-detail-body').evaluate(element => element.scrollTop) !== 0) throw new Error(`${project.name}: opening another Traffic Capture preserves stale scroll position`);
    await captureDialog.locator('.close-capture-detail').first().click();
    await assertNoPageOverflow(page, project.name, 'Traffic Capture page');
    await page.locator('#back-to-extensions').click();
    if (!(await requestDefaultsExtension.getByText('Native Rust', {exact: true}).isVisible())) throw new Error(`${project.name}: extension implementation type is not visible`);
    if (!(await requestDefaultsExtension.getByText(/^v\d+$/, {exact: true}).isVisible())) throw new Error(`${project.name}: Extension API version is not visible`);
    const extensionToggle = requestDefaultsExtension.locator('[data-extension-toggle="request-defaults"]');
    if (!(await extensionToggle.isChecked()) || !(await extensionToggle.isEnabled())) throw new Error(`${project.name}: Request Defaults does not expose its enabled runtime setting`);
    if (project.name === 'desktop-chrome') {
      await extensionToggle.evaluate(input => input.click());
      await requestDefaultsExtension.getByText('Disabled', {exact: true}).waitFor();
      await page.evaluate(() => document.querySelector('[data-view="providers"]').click());
      await page.locator('#providers .provider-list-item').first().click();
      const disabledDefaultsCard = page.locator('.defaults-card.defaults-disabled').first();
      await disabledDefaultsCard.waitFor({state: 'visible'});
      if (!(await disabledDefaultsCard.getByText('Request defaults are off', {exact: true}).isVisible())) throw new Error(`${project.name}: disabled Request Defaults lacks a prominent warning`);
      if (!(await disabledDefaultsCard.getByText(/No saved headers or body fields will be added.*Enable it from Extensions/).isVisible())) throw new Error(`${project.name}: disabled Request Defaults does not explain its traffic impact or how to enable it`);
      await page.evaluate(() => document.querySelector('[data-view="extensions"]').click());
      await requestDefaultsExtension.locator('[data-extension-toggle="request-defaults"]').evaluate(input => input.click());
      await requestDefaultsExtension.getByText('Enabled', {exact: true}).waitFor();
    }
    await assertNoPageOverflow(page, project.name, 'Extensions page');
    await page.evaluate(() => document.querySelector('[data-view="providers"]').click());
    await page.waitForFunction(() => document.querySelectorAll('#providers .provider-list-activity svg').length === document.querySelectorAll('#providers .provider-list-item').length);
    const providerActivity = page.locator('#providers .provider-list-activity').first();
    if (!(await providerActivity.isVisible()) || !(await providerActivity.getByText(/requests · 24h/).isVisible())) throw new Error(`${project.name}: Provider list does not show its 24-hour activity sparkline`);
    if (!await providerActivity.getAttribute('aria-label').then(label => /requests? in the last 24 hours/.test(label || ''))) throw new Error(`${project.name}: Provider activity sparkline lacks an accessible request summary`);
    const stretchedProviderLabels = await page.locator('.provider-list-main code').evaluateAll(labels => labels.filter(label => {
      const range = document.createRange();
      range.selectNodeContents(label);
      const style = getComputedStyle(label);
      const contentWidth = range.getBoundingClientRect().width + parseFloat(style.paddingLeft) + parseFloat(style.paddingRight);
      return label.getBoundingClientRect().width > contentWidth + 1;
    }).map(label => label.textContent));
    if (stretchedProviderLabels.length) throw new Error(`${project.name}: Provider model labels stretch past their content (${stretchedProviderLabels.join(', ')})`);
    await page.evaluate(() => document.querySelector('[data-view="home"]').click());
    const homeCommand = page.locator('#home-view .home-command');
    if (!(await homeCommand.isVisible()) || !(await page.locator('#home-traffic-chart').isVisible())) throw new Error(`${project.name}: Home is missing its operational header or traffic visualization`);
    if (await page.locator('.home-health-strip > div').count() !== 4) throw new Error(`${project.name}: Home does not summarize runtime, provider, success, and latency health`);
    if (!(await page.locator('.home-command-actions .button-icon').first().isVisible()) || !(await page.locator('#home-api-keys').isVisible())) throw new Error(`${project.name}: Home omits command icons or Gateway API key traffic`);
    const homeChartResult = await page.evaluate(() => {
      const start = Math.floor(Date.now() / 1000) - 86400;
      renderHomeTraffic(Array.from({length: 48}, (_, index) => ({start: start + index * 1800, requests: 1})));
      const chart = document.querySelector('#home-traffic-chart');
      const width = chart.clientWidth;
      const expectedColumns = width >= 768 ? 48 : width >= 400 ? 24 : 12;
      const columns = [...chart.querySelectorAll('.home-chart-column')];
      return {
        width,
        expectedColumns,
        columns: columns.length,
        representedRequests: columns.reduce((total, column) => total + Number(column.title.match(/: (\d+) request/)?.[1] || 0), 0),
        description: document.querySelector('#home-traffic-description').textContent,
      };
    });
    if (homeChartResult.columns !== homeChartResult.expectedColumns) throw new Error(`${project.name}: ${homeChartResult.width}px Home chart renders ${homeChartResult.columns} bars instead of ${homeChartResult.expectedColumns}`);
    if (homeChartResult.representedRequests !== 48) throw new Error(`${project.name}: responsive Home chart aggregation changes the represented request total`);
    const expectedInterval = homeChartResult.expectedColumns === 48 ? '30-minute' : homeChartResult.expectedColumns === 24 ? 'Hourly' : '2-hour';
    if (!homeChartResult.description.startsWith(expectedInterval)) throw new Error(`${project.name}: responsive Home chart does not describe its ${expectedInterval} intervals`);

    if (testLiveRefresh) {
      const homeRefresh = page.waitForResponse(response => response.url().includes('/admin/activity/stats?since='));
      await page.clock.fastForward(10000);
      await homeRefresh;
    }

    await page.evaluate(() => document.querySelector('[data-view="access"]').click());
    const editGatewayKey = page.locator('.edit-gateway-key').first();
    if (await editGatewayKey.isVisible()) {
      const firstKey = await page.evaluate(async () => (await (await fetch('/admin/auth')).json()).api_keys[0]);
      const listedKey = page.locator('#gateway-keys .listed-key').first();
      if (await listedKey.locator('code').textContent() !== firstKey.prefix) throw new Error(`${project.name}: API key list does not use the masked key prefix`);
      if ((await listedKey.evaluate(element => element.outerHTML)).includes(firstKey.secret)) throw new Error(`${project.name}: API key list embeds the full secret in its markup`);
      if (!(await listedKey.locator('.icon-copy-key[aria-label="Copy API key"]').isVisible())) throw new Error(`${project.name}: masked API key cannot be copied`);
      await editGatewayKey.click();
      await assertDialog(page, '#edit-gateway-key-dialog', project.name);
      await page.locator('#edit-gateway-key-dialog .close-edit-gateway-key').first().click();
    }
    await page.locator('.contextual-help[data-help-context="access"]').click();
    await assertDialog(page, '#help-dialog', project.name);
    if (await page.locator('[data-help-tab]').count() !== 3) throw new Error(`${project.name}: help guide does not use the compact three-tab layout`);
    const agentSelect = page.locator('.help-agent-select');
    if (!(await agentSelect.isVisible())) throw new Error(`${project.name}: Agent selector is missing from Agent setup`);
    await page.locator('[data-help-tab="generic"]').click();
    await page.locator('#help-dialog [data-help-panel="generic"]').waitFor({state: 'visible'});
    if (await agentSelect.isVisible()) throw new Error(`${project.name}: Generic Agent incorrectly shows the Agent selector`);
    const agentBodyHeight = await page.locator('.help-body').evaluate(element => element.getBoundingClientRect().height);
    await page.locator('[data-help-tab="curl"]').click();
    await page.locator('#help-dialog [data-help-panel="curl"]').waitFor({state: 'visible'});
    if (await agentSelect.isVisible()) throw new Error(`${project.name}: Shell / test request incorrectly shows the Agent selector`);
    await page.waitForTimeout(120);
    const transitioningBodyHeight = await page.locator('.help-body').evaluate(element => element.getBoundingClientRect().height);
    if (!(transitioningBodyHeight < agentBodyHeight - 1)) throw new Error(`${project.name}: Help content height does not animate toward the shorter Shell / test request panel`);
    await page.locator('[data-help-tab="agent"]').click();
    if (!(await agentSelect.isVisible())) throw new Error(`${project.name}: returning to Agent setup does not restore the Agent selector`);
    await page.locator('#help-agent').selectOption('codex');
    await page.locator('#help-dialog [data-help-panel="codex"]').waitFor({state: 'visible'});
    await page.locator('#help-dialog .close-help').first().click();
    await page.evaluate(() => document.querySelector('.open-about').click());
    await assertDialog(page, '#about-dialog', project.name);
    if (await page.getByText('Built from source').count()) throw new Error(`${project.name}: About dialog still shows implementation-focused build text`);
    if (await page.getByText('Open source LLM gateway').count() !== 1) throw new Error(`${project.name}: About dialog is missing its product descriptor`);
    const aboutBodyPadding = await page.locator('#about-dialog .about-body').evaluate(element => parseFloat(getComputedStyle(element).paddingTop));
    if (aboutBodyPadding < 36) throw new Error(`${project.name}: About build identity is too close to the hero`);
    const githubLink = page.locator('#about-dialog a[href="https://github.com/DGideas/yabane"]');
    if (await githubLink.count() !== 1 || await githubLink.getAttribute('target') !== '_blank') throw new Error(`${project.name}: About dialog is missing the project GitHub link`);
    const logoAnimation = await page.locator('#about-dialog .about-logo').evaluate(element => getComputedStyle(element).animationName);
    if (logoAnimation !== 'none') throw new Error(`${project.name}: About icon still animates (${logoAnimation})`);
    const heroPalette = await page.locator('#about-dialog').evaluate(dialog => {
      const colors = selector => [...dialog.querySelectorAll(selector)].map(element => getComputedStyle(element).fill);
      return { background: getComputedStyle(dialog.querySelector('.about-hero')).backgroundColor, facets: colors('.about-facets path'), frontArrow: colors('.about-arrows-front path')[0] };
    });
    const channels = color => (color.match(/\d+/g) || []).slice(0, 3).map(Number);
    const cyan = channels(heroPalette.facets[2]);
    const arrow = channels(heroPalette.frontArrow);
    if (new Set(heroPalette.facets).size < 4 || cyan.length !== 3 || Math.max(...cyan) > 185 || Math.max(...cyan) - Math.min(...cyan) > 100 || arrow.length !== 3 || Math.max(...arrow) > 230) throw new Error(`${project.name}: About hero does not use its restrained ink-blue and muted-cyan palette`);
    const logoPaths = await page.locator('#about-dialog .about-logo path').evaluateAll(paths => paths.map(path => path.getAttribute('d')));
    if (logoPaths.join('|') !== 'M14 4h32l14 14v32c0 5.5-4.5 10-10 10H14C8.5 60 4 55.5 4 50V14C4 8.5 8.5 4 14 4Z|m13 18 13 14-13 14h8l13-14-13-14Z|m31 18 13 14-13 14h8l13-14-13-14Z') throw new Error(`${project.name}: About dialog does not use the Yabane mark`);
    const brandLoaded = await page.locator('.topbar .brand-mark').evaluate(image => image.complete && image.naturalWidth > 0);
    if (!brandLoaded) throw new Error(`${project.name}: Yabane application icon did not load`);
    await page.locator('#about-dialog .close-about').first().click();
    await page.evaluate(() => document.querySelector('#open-provider').click());
    await page.locator('#display-name').fill('OpenAI subscription');
    await page.locator('#next-step').click();
    const providerFieldOrder = await page.locator('#provider-form [data-step="2"] > label.field').evaluateAll(fields => fields.slice(0, 3).map(field => field.querySelector('input, select')?.name));
    if (providerFieldOrder.join('|') !== 'endpoint_id|api_type|base_url') throw new Error(`${project.name}: initial Endpoint setup does not ask for API type before Base URL`);
    await page.locator('#api-type-choices input[value="openai_codex"]').check();
    await assertDialog(page, '#provider-dialog', project.name);
    if (await page.locator('#initial-endpoint-id').inputValue() !== 'chatgpt') throw new Error(`${project.name}: first endpoint does not expose the API-type default ID`);
    if (await page.locator('#base-url').isVisible()) throw new Error(`${project.name}: subscription setup exposes Base URL`);
    if (!(await page.locator('#provider-form [name="socks5_proxy"]').isVisible())) throw new Error(`${project.name}: subscription setup hides SOCKS5 proxy`);
    await page.locator('#provider-form [name="socks5_proxy"]').fill('socks5h://127.0.0.1:1080');
    if (await page.locator('#api-key').isVisible()) throw new Error(`${project.name}: subscription setup exposes API key input`);
    if (await page.locator('#create-provider').textContent() !== 'Connect OpenAI') throw new Error(`${project.name}: subscription setup has the wrong primary action`);
    await page.locator('#create-provider').click();
    await page.locator('#openai-subscription-dialog').waitFor({state: 'visible'});
    if (!(await page.locator('#openai-device-signin').isVisible()) || await page.locator('#openai-oauth-signin').isVisible()) throw new Error(`${project.name}: OpenAI subscription does not default to device-code sign-in`);
    if (await page.locator('#openai-subscription-code').textContent() !== 'ABCD-EFGH') throw new Error(`${project.name}: device-code sign-in does not show its one-time code`);
    await page.locator('#openai-use-oauth').click();
    await page.locator('#openai-oauth-signin').waitFor({state: 'visible'});
    if (await page.locator('#openai-device-signin').isVisible()) throw new Error(`${project.name}: browser OAuth did not replace the device-code instructions`);
    const oauthWarning = page.locator('#openai-oauth-signin .oauth-expected-warning');
    if (!(await oauthWarning.isVisible()) || !(await oauthWarning.getByText('A localhost error page is expected', {exact: true}).isVisible())) throw new Error(`${project.name}: browser OAuth does not prominently prepare users for the localhost error page`);
    const warningText = await oauthWarning.textContent();
    if (!warningText.includes('This does not mean OAuth failed') || !warningText.includes('localhost:1455')) throw new Error(`${project.name}: browser OAuth warning does not explain that the localhost failure is intentional`);
    const oauthSteps = await page.locator('#openai-oauth-signin .oauth-steps li strong').allTextContents();
    if (oauthSteps.join('|') !== 'Sign in with OpenAI|Expect the localhost error|Return and paste once') throw new Error(`${project.name}: browser OAuth steps do not describe the expected failure before launch`);
    if (await page.locator('#openai-oauth-link').textContent() !== 'I understand — open OpenAI sign-in') throw new Error(`${project.name}: browser OAuth launch does not require an explicit acknowledgement`);
    if (await page.locator('#openai-oauth-link').getAttribute('href') !== 'https://auth.openai.com/oauth/authorize?state=browser-state') throw new Error(`${project.name}: browser OAuth does not expose the Extension authorization URL`);
    await page.locator('#openai-oauth-callback').fill('http://localhost:1455/auth/callback?code=oauth-code&state=browser-state');
    await page.locator('#openai-oauth-signin [type="submit"]').click();
    await page.locator('#openai-subscription-dialog').waitFor({state: 'hidden'});
    await page.evaluate(() => document.querySelector('[data-view="providers"]').click());
    const subscriptionProvider = page.locator('#providers .provider-list-item').filter({has: page.locator('code', {hasText: 'ui-subscription/model-id'})});
    await subscriptionProvider.click();
    const credential = page.locator('.subscription-credential');
    await credential.waitFor({state: 'visible'});
    await page.evaluate(() => {
      window.__helpTestKeys = authSettings.api_keys;
      authSettings.api_keys = [
        {id: 'wrong-provider', note: 'Wrong Provider', prefix: 'sk-wrong', secret: 'sk-wrong-provider', expires_at: null, provider_ids: ['ui-keyless']},
        {id: 'expired-provider', note: 'Expired Provider key', prefix: 'sk-expired', secret: 'sk-expired-provider', expires_at: 1, provider_ids: ['ui-subscription']},
        {id: 'matching-provider', note: 'Matching Provider', prefix: 'sk-matching', secret: 'sk-matching-provider', expires_at: null, provider_ids: ['ui-subscription']},
      ];
    });
    await page.locator('.contextual-help[data-help-context="provider"]').click();
    await page.locator('#help-dialog').waitFor({state: 'visible'});
    if (await page.locator('#help-model').inputValue() !== 'ui-subscription/gpt-fixture') throw new Error(`${project.name}: Provider guide does not default to a discovered model from that Provider`);
    if (await page.locator('#help-key').inputValue() !== 'sk-matching-provider') throw new Error(`${project.name}: Provider guide does not default to a non-expired Gateway key authorized for that Provider`);
    await page.locator('#help-dialog .close-help').first().click();
    await page.evaluate(() => { authSettings.api_keys = window.__helpTestKeys; delete window.__helpTestKeys; });
    if (await credential.getByText('Automatic renewal enabled', {exact: true}).count() !== 1) throw new Error(`${project.name}: connected OpenAI subscription does not present automatic renewal as its primary state`);
    if (await credential.getByText(/^Token expires /).count()) throw new Error(`${project.name}: OpenAI subscription still presents access-token expiry as its primary state`);
    const details = credential.locator('.credential-details');
    if (await details.count() !== 1 || await details.getAttribute('open') !== null) throw new Error(`${project.name}: OpenAI access-token details are missing or expanded by default`);
    await details.locator('summary').click();
    const detailText = await details.locator('p').textContent();
    if (!detailText.includes('current access token') || !detailText.includes('next request')) throw new Error(`${project.name}: elapsed OpenAI access-token detail does not explain lazy renewal`);
    await page.locator('#back-to-providers').click();
    await page.locator('#provider-list-page').waitFor({state: 'visible'});
    const firstProvider = page.locator('#providers .provider-list-item').first();
    if (await firstProvider.count()) {
      await firstProvider.click();
      const defaultsCard = page.locator('.defaults-card');
      const defaultsIncluded = await defaultsCard.getByText(/^Extension (enabled|disabled)/).count();
      if (defaultsIncluded) {
        if (!(await defaultsCard.getByRole('button', {name: 'Configure', exact: true}).isVisible())) throw new Error(`${project.name}: included Request Defaults cannot be configured`);
        if (await defaultsCard.getByRole('button', {name: 'View extension'}).count()) throw new Error(`${project.name}: included Request Defaults retains a redundant View extension action`);
      } else {
        if (!(await defaultsCard.getByRole('button', {name: 'How to include'}).isVisible())) throw new Error(`${project.name}: unavailable Request Defaults does not explain how to include it`);
      }
      await page.locator('.edit-provider').click();
      await assertDialog(page, '#provider-identity-dialog', project.name);
      const immutableProviderId = page.locator('#provider-identity-form [name="id"]');
      if (!(await immutableProviderId.isDisabled())) throw new Error(`${project.name}: Provider ID is editable after creation`);
      if (await page.locator('#provider-identity-form .immutable-badge').textContent() !== 'Permanent') throw new Error(`${project.name}: Provider ID lacks an explicit permanent marker`);
      const immutableStyles = await immutableProviderId.evaluate(element => { const style = getComputedStyle(element); return {backgroundColor: style.backgroundColor, cursor: style.cursor}; });
      if (immutableStyles.backgroundColor === 'rgb(255, 255, 255)' || immutableStyles.cursor !== 'not-allowed') throw new Error(`${project.name}: Provider ID does not look visibly immutable`);
      if (await page.locator('#provider-id-immutable-help strong').textContent() !== 'Cannot be changed after creation.') throw new Error(`${project.name}: Provider ID immutability is not stated directly`);
      await page.locator('#provider-identity-dialog .close-provider-identity').first().click();
      const coverageBox = await page.locator('.endpoint-coverage').boundingBox();
      const insightsBox = await page.locator('.model-insights').boundingBox();
      if (!coverageBox || !insightsBox || coverageBox.x > insightsBox.x + 2 || coverageBox.width < insightsBox.width - 4) throw new Error(`${project.name}: Endpoint coverage remains squeezed against the right edge of the model summary`);
      const browseCatalog = page.locator('.browse-provider-models');
      if (!(await browseCatalog.isDisabled())) {
        await browseCatalog.click();
        const [catalogBox, overviewBox, defaultsBox] = await Promise.all([
          page.locator('.model-summary-card').boundingBox(),
          page.locator('.provider-overview').boundingBox(),
          page.locator('.defaults-card').boundingBox(),
        ]);
        if (!catalogBox || !overviewBox || catalogBox.width < overviewBox.width - 4) throw new Error(`${project.name}: expanded model catalog does not use the full overview width`);
        if (!defaultsBox || defaultsBox.y < catalogBox.y + catalogBox.height - 2) throw new Error(`${project.name}: Request defaults remains beside the expanded model catalog`);
        if (await page.locator('.model-table').evaluate(element => element.scrollWidth > element.clientWidth + 1)) throw new Error(`${project.name}: model catalog requires horizontal scrolling`);
        const modelSearch = page.locator('.model-browser-toolbar [role="searchbox"]');
        await modelSearch.fill('gpt');
        if (!(await page.locator('.model-search-clear').isVisible())) throw new Error(`${project.name}: model catalog does not expose its clear-search action`);
        if (await modelSearch.getAttribute('type') === 'search') throw new Error(`${project.name}: model catalog renders both native and custom clear-search actions`);
        await page.locator('.model-search-clear').click();
        if (await modelSearch.inputValue()) throw new Error(`${project.name}: model catalog clear-search action does not clear the query`);
        await browseCatalog.click();
      }
      const editableEndpoint = page.locator('.endpoint-card:not(.subscription-endpoint) .endpoint-edit').first();
      if (await editableEndpoint.count()) {
        await editableEndpoint.click();
        await assertDialog(page, '#endpoint-dialog', project.name);
        const editableEndpointId = page.locator('#endpoint-form [name="id"]');
        if (await editableEndpointId.isDisabled()) throw new Error(`${project.name}: Endpoint ID cannot be edited after creation`);
        if (await page.locator('#endpoint-form .endpoint-id-permanent').isVisible()) throw new Error(`${project.name}: editable Endpoint ID is still marked permanent`);
        const endpointHelp = await page.locator('#endpoint-id-help').textContent();
        if (!endpointHelp.includes('model availability') || !endpointHelp.includes('model-route destinations') || !endpointHelp.includes('historical Activity')) throw new Error(`${project.name}: Endpoint rename does not explain linked updates and historical records`);
        await page.locator('#endpoint-dialog .close-endpoint').first().click();
      }
      await page.locator('.add-endpoint').click();
      await assertDialog(page, '#endpoint-dialog', project.name);
      if (!(await page.locator('#endpoint-form [name="id"]').inputValue())) throw new Error(`${project.name}: additional endpoint ID is not suggested`);
      if (await page.locator('#endpoint-form .endpoint-id-permanent').isVisible()) throw new Error(`${project.name}: new Endpoint ID is incorrectly marked permanent before creation`);
      if (!(await page.locator('#endpoint-form .endpoint-id-required').isVisible())) throw new Error(`${project.name}: new Endpoint ID does not remain visibly required`);
      if (!(await page.locator('#endpoint-id-help').textContent()).includes('can be changed later')) throw new Error(`${project.name}: new Endpoint ID does not explain that it remains editable`);
      const endpointFieldOrder = await page.locator('#endpoint-form .form-body > label.field').evaluateAll(fields => fields.slice(0, 3).map(field => field.querySelector('.field-label')?.childNodes[0]?.textContent.trim()));
      if (endpointFieldOrder.join('|') !== 'Endpoint ID|API type|Base URL') throw new Error(`${project.name}: additional Endpoint setup does not ask for API type before Base URL`);
      await page.locator('#endpoint-form [name="api_type"]').selectOption('openai_codex');
      if (await page.locator('#endpoint-form [name="base_url"]').isVisible()) throw new Error(`${project.name}: additional subscription Endpoint setup exposes Base URL`);
      await page.locator('#endpoint-dialog .close-endpoint').first().click();
    }
    if (project.name === 'desktop-chrome') {
      const providerFixture = {
        id: 'ui-delete-provider', name: 'UI delete provider', extra_headers: {}, extra_body: {}, defaults_endpoint_ids: [],
        pricing: {updated_at: 1, models: {'ui-provider-price*': {input_per_million: 1, output_per_million: 2}}},
        endpoints: [{id: 'deletable', api_type: 'openai_compatible', base_url: 'http://127.0.0.1:18080/v1', socks5_proxy: null, extra_headers: {}, extra_body: {}, pricing: {updated_at: 1, models: {'ui-endpoint-price*': {output_per_million: 3}}}, requires_api_key: true, api_keys: [{id: 'delete-key', name: 'Delete key', weight: 100, enabled: true}], subscription_connected: false, subscription_expires_at: null}],
        discovered_models: [], model_endpoints: {}, model_endpoint_preferences: [], models_discovered_at: null, model_discovery_error: null,
      };
      const routeFixture = {pattern: 'ui-delete-route', targets: [{provider_id: providerFixture.id, endpoint_id: 'deletable', api_key_id: 'delete-key', upstream_model: 'upstream-delete-model', weight: 100, enabled: true}]};
      await page.route('**/admin/providers/ui-delete-provider', async route => {
        if (route.request().method() !== 'DELETE') return route.continue();
        await route.fulfill({status: 204});
      });
      await page.route('**/admin/routes', async route => {
        if (route.request().method() !== 'GET') return route.continue();
        await route.fulfill({status: 200, contentType: 'application/json', body: '[]'});
      }, {times: 1});
      await page.route('**/admin/auth', async route => {
        if (route.request().method() !== 'GET') return route.continue();
        await route.fulfill({status: 200, contentType: 'application/json', body: JSON.stringify({enabled: true, api_keys: []})});
      }, {times: 1});
      await page.evaluate(({providerFixture, routeFixture}) => {
        providers.push(providerFixture); modelRoutes.push(routeFixture);
        authSettings.api_keys.push({id: 'ui-delete-scoped-key', note: 'Delete scoped key', prefix: 'sk-ui…test', secret: '', created_at: 1, expires_at: null, provider_ids: [providerFixture.id]});
        renderProviders(); selectedProviderId = providerFixture.id; renderProviderPage();
      }, {providerFixture, routeFixture});
      let confirmation = '';
      page.once('dialog', async dialog => { confirmation = dialog.message(); await dialog.accept(); });
      await Promise.all([
        page.waitForResponse(response => response.request().method() === 'GET' && response.url().endsWith('/admin/routes')),
        page.locator('.delete-provider').click(),
      ]);
      if (!confirmation.includes('1 Endpoint') || !confirmation.includes('1 upstream credential') || !confirmation.includes('1 model-route destination') || !confirmation.includes('deletes 1 route left without a destination') || !confirmation.includes('revokes 1 Gateway API key scoped only to this Provider')) throw new Error(`${project.name}: Provider deletion does not explain its cascading route, credential, and Gateway key impact`);
      if (await page.locator('#routes').getByText('ui-delete-route', {exact: true}).count()) throw new Error(`${project.name}: Provider deletion leaves stale model routes rendered in the console`);
      await page.evaluate(() => document.querySelector('[data-view="pricing"]').click());
      const deletedPricingText = await page.locator('#pricing-list-page').textContent();
      if (deletedPricingText.includes('ui-provider-price') || deletedPricingText.includes('ui-endpoint-price')) throw new Error(`${project.name}: Provider deletion leaves its pricing overrides in the central pricing list`);
    }
    await page.evaluate(() => document.querySelector('[data-view="models"]').click());
    const renderedRoute = page.locator('#routes .route-destination').first();
    if (await renderedRoute.count()) {
      if (!(await renderedRoute.locator('.route-upstream').isVisible()) || !(await renderedRoute.locator('.route-status').isVisible()) || !(await renderedRoute.locator('.route-target-state > strong').isVisible())) throw new Error(`${project.name}: route destination does not visually separate its upstream, status, and traffic share`);
      const routeModelCell = page.locator('#routes .route-model-cell').first();
      const [modelCellBox, modelHeadingBox, matchKind] = await Promise.all([
        routeModelCell.boundingBox(),
        routeModelCell.locator('.route-model-heading').boundingBox(),
        routeModelCell.locator('.route-match-kind').textContent(),
      ]);
      if (!modelCellBox || !modelHeadingBox || modelHeadingBox.height > 42) throw new Error(`${project.name}: route match identity becomes tall when a rule has multiple destinations`);
      if (!['Exact', 'Prefix'].includes(matchKind?.trim())) throw new Error(`${project.name}: route match type is not rendered as a compact label`);
      if (!(await routeModelCell.locator('small').textContent()).includes('destination')) throw new Error(`${project.name}: route destination count is missing from the public model summary`);
      const routeOverflow = await page.locator('#routes-table').evaluate(element => element.scrollWidth > element.clientWidth + 1);
      if (routeOverflow) throw new Error(`${project.name}: structured route summary overflows its table viewport`);
    }
    await page.evaluate(() => document.querySelector('#open-route').click());
    await assertDialog(page, '#route-dialog', project.name);
    const keylessDestination = page.locator('#route-targets .route-target option', {hasText: 'UI keyless fixture · local · No API key'});
    if (await keylessDestination.count() !== 1) throw new Error(`${project.name}: route editor omits an Endpoint configured without an API key`);
    if (await page.locator('#route-targets .route-weight-field').first().isVisible()) throw new Error(`${project.name}: traffic share is visible for a simple alias`);
    await page.locator('#add-route-target').click();
    await page.locator('#route-split-head').waitFor({ state: 'visible' });
    const shares = await page.locator('#route-targets [name="target_weight"]').evaluateAll(inputs => inputs.map(input => input.value));
    if (shares.join(',') !== '50,50') throw new Error(`${project.name}: initial traffic split is not 50/50 (${shares.join(',')})`);
    await page.locator('#route-targets [name="target_weight"]').first().fill('60');
    if (!(await page.locator('#save-route').isDisabled())) throw new Error(`${project.name}: invalid traffic total does not disable saving`);
    if (await page.locator('#route-split-total').textContent() !== '110%') throw new Error(`${project.name}: invalid traffic total is not explained`);
    await page.locator('#route-targets [name="target_enabled"]').first().uncheck();
    let switchedShares = await page.locator('#route-targets [name="target_weight"]').evaluateAll(inputs => inputs.map(input => ({value: input.value, disabled: input.disabled})));
    if (switchedShares[0].value !== '0' || switchedShares[0].disabled || switchedShares[1].value !== '100') throw new Error(`${project.name}: turning off a route target does not produce a visible 0/100 split`);
    if (await page.locator('#save-route').isDisabled()) throw new Error(`${project.name}: one 100% target cannot be saved`);
    await page.locator('#route-targets [name="target_enabled"]').first().check();
    switchedShares = await page.locator('#route-targets [name="target_weight"]').evaluateAll(inputs => inputs.map(input => input.value));
    if (switchedShares.join(',') !== '50,50') throw new Error(`${project.name}: turning a destination back on does not restore an even 50/50 split (${switchedShares.join(',')})`);
    await page.locator('#route-targets [name="target_weight"]').first().fill('0');
    switchedShares = await page.locator('#route-targets [name="target_weight"]').evaluateAll(inputs => inputs.map(input => input.value));
    if (switchedShares.join(',') !== '0,100' || await page.locator('#route-targets [name="target_enabled"]').first().isChecked()) throw new Error(`${project.name}: a 0% share does not turn off and redistribute the destination`);
    await page.locator('#route-targets [name="target_enabled"]').nth(1).uncheck();
    if (!(await page.locator('#save-route').isDisabled()) || await page.locator('#route-split-total').textContent() !== '0%') throw new Error(`${project.name}: route permits every target to be turned off`);
    await page.locator('#route-dialog .close-route').first().click();
    await page.evaluate(() => document.querySelector('[data-view="pricing"]').click());
    await page.locator('#pricing-view').waitFor({state: 'visible'});
    if (!(await page.locator('#pricing-list-page').isVisible()) || !(await page.locator('#open-pricing-editor').isVisible())) throw new Error(`${project.name}: Model pricing lacks a dedicated top-level management entry`);
    await page.locator('#open-pricing-editor').click();
    if (!(await page.locator('#pricing-editor-page').isVisible()) || !(await page.locator('#pricing-list-page').isHidden())) throw new Error(`${project.name}: price editing does not use the full pricing workspace`);
    const suggestedModels = await page.locator('#pricing-model-suggestions option').evaluateAll(options => options.map(option => option.value));
    if (!suggestedModels.includes('gpt-fixture')) throw new Error(`${project.name}: pricing model input does not suggest runtime-discovered model IDs`);
    if (!suggestedModels.includes('activity-only-model')) throw new Error(`${project.name}: pricing model input does not suggest historical Activity upstream model IDs`);
    await page.locator('#pricing-model').fill('model-family-*');
    if (!(await page.locator('#pricing-model-notice').textContent()).includes('Matches every upstream model beginning with')) throw new Error(`${project.name}: pricing model input does not explain prefix wildcard matching`);
    if (await page.locator('[name="cache_write_per_million"]').count()) throw new Error(`${project.name}: pricing editor exposes a cache-write rate that current usage cannot apply`);
    const rateInputModes = await page.locator('.pricing-money-input input').evaluateAll(inputs => inputs.map(input => ({type: input.type, inputMode: input.inputMode})));
    if (rateInputModes.some(input => input.type !== 'number' || input.inputMode !== 'decimal')) throw new Error(`${project.name}: pricing rates do not preserve validated numeric input and the mobile decimal keyboard hint`);
    if (project.name === 'desktop-chrome') {
      if (!(await page.locator('#models-dev-reference').isVisible())) throw new Error(`${project.name}: wide pricing editor does not use the available space for models.dev references`);
      await page.locator('#pricing-model').fill('activity-only-model');
      await page.clock.fastForward(400);
      await page.locator('.pricing-reference-item').waitFor({state: 'visible'});
      if (!(await page.locator('.pricing-reference-item').getByText('Reference Provider', {exact: true}).isVisible())) throw new Error(`${project.name}: models.dev references do not identify the serving provider`);
      const referenceLayout = await page.locator('.pricing-reference-item').first().evaluate(item => {
        const panel = item.closest('.pricing-reference').getBoundingClientRect(); const rates = item.querySelector('dl').getBoundingClientRect(); const button = item.querySelector('button').getBoundingClientRect();
        const labels = [...item.querySelectorAll('dt')].map(label => ({clientWidth: label.clientWidth, scrollWidth: label.scrollWidth, height: label.getBoundingClientRect().height}));
        return {panelWidth: panel.width, ratesRight: rates.right, buttonLeft: button.left, labels};
      });
      if (referenceLayout.panelWidth < 400 || referenceLayout.ratesRight > referenceLayout.buttonLeft || referenceLayout.labels.some(label => label.scrollWidth > label.clientWidth || label.height > 16)) throw new Error(`${project.name}: models.dev reference results are compressed (${JSON.stringify(referenceLayout)})`);
      const referenceTypography = await page.locator('.pricing-reference-item').first().evaluate(item => ({model: parseFloat(getComputedStyle(item.querySelector('strong')).fontSize), price: parseFloat(getComputedStyle(item.querySelector('dd')).fontSize), button: parseFloat(getComputedStyle(item.querySelector('button')).fontSize)}));
      if (referenceTypography.model < 15 || referenceTypography.price < 16 || referenceTypography.button < 12) throw new Error(`${project.name}: models.dev reference typography remains too small (${JSON.stringify(referenceTypography)})`);
      if (process.env.YABANE_UI_SCREENSHOT_DIR) await page.screenshot({path: `${process.env.YABANE_UI_SCREENSHOT_DIR}/${project.name}-pricing.png`, fullPage: true});
      await page.locator('.use-reference-rates').click();
      const referenceRates = await page.locator('#central-pricing-form').evaluate(form => [form.elements.input_per_million.value, form.elements.output_per_million.value, form.elements.cache_read_per_million.value]);
      if (referenceRates.join(',') !== '0.42,1.75,0.08') throw new Error(`${project.name}: selecting a models.dev reference does not fill the visible rates (${referenceRates.join(',')})`);
    } else if (!(await page.locator('#models-dev-reference').isHidden())) throw new Error(`${project.name}: models.dev reference panel crowds a narrow pricing editor`);
    await page.locator('#pricing-model').fill('arbitrary/vendor-model');
    if (!(await page.locator('#pricing-model-notice').textContent()).includes('will still be saved as entered')) throw new Error(`${project.name}: pricing model input does not explicitly permit arbitrary exact IDs`);
    await page.locator('#central-pricing-form [name="scope"][value="endpoint"]').check();
    await page.locator('#pricing-provider').selectOption('ui-subscription');
    await page.locator('#pricing-endpoint').selectOption('chatgpt');
    if (!(await page.locator('#pricing-resource-fields').isVisible()) || !(await page.locator('#pricing-endpoint-field').isVisible())) throw new Error(`${project.name}: centralized pricing cannot select an Endpoint override`);
    await assertNoPageOverflow(page, project.name, 'Model pricing editor');
    const [cancelBox, saveBox] = await Promise.all([
      page.locator('#cancel-pricing-edit').boundingBox(),
      page.locator('#central-pricing-form button[type="submit"]').boundingBox(),
    ]);
    if (!cancelBox || !saveBox || cancelBox.width > Math.max(180, saveBox.width * 2)) throw new Error(`${project.name}: pricing Cancel action stretches across the workspace`);
    await page.locator('#cancel-pricing-edit').click();
    const initialActivityLoad = testLiveRefresh ? Promise.all([
      page.waitForResponse(response => response.url().includes('/admin/activity/stats?since=') && response.url().includes('buckets=48') && response.url().includes('until=')),
      page.waitForResponse(response => response.url().includes('/admin/activity/logs?since=') && response.url().includes('limit=100')),
    ]) : null;
    await page.evaluate(() => document.querySelector('[data-view="activity"]').click());
    if (initialActivityLoad) await initialActivityLoad;
    await page.waitForFunction(() => document.querySelectorAll('#activity-chart .chart-column').length === 48);
    const coverageFallback = await page.evaluate(() => costCoverageLabel({requests: 19146, priced_requests: 0, reported_requests: 12, estimated_requests: 14668}));
    if (coverageFallback !== '12 reported · 14,668 estimated value · 14,680 / 19,146 requests valued') throw new Error(`${project.name}: Activity cost coverage trusts an inconsistent aggregate over its source counts (${coverageFallback})`);
    if (!(await page.locator('#refresh-missing-costs').isVisible())) throw new Error(`${project.name}: Activity does not expose the explicit non-reported cost refresh action`);
    if (project.name === 'desktop-chrome') {
      page.once('dialog', dialog => dialog.accept());
      const costRefresh = page.waitForResponse(response => new URL(response.url()).pathname === '/admin/activity/recalculate-costs');
      await page.locator('#refresh-missing-costs').click();
      await costRefresh;
      if (!(await page.locator('#activity-cost-refresh-status').textContent()).includes('Updated 2 costs')) throw new Error(`${project.name}: Activity cost refresh does not show its result summary`);
    }
    if (project.name === 'desktop-chrome') {
      await page.locator('#activity-filter-trigger').click();
      const providerFilter = page.locator('#activity-filter-options [data-filter-group="providers"]').first();
      if (!(await providerFilter.count())) throw new Error(`${project.name}: Activity filter picker has no runtime Provider options`);
      const providerValue = await providerFilter.inputValue();
      const filteredOverview = Promise.all([
        page.waitForResponse(response => new URL(response.url()).pathname === '/admin/activity/stats' && new URL(response.url()).searchParams.get('providers') === providerValue),
        page.waitForResponse(response => new URL(response.url()).pathname === '/admin/activity/logs' && new URL(response.url()).searchParams.get('providers') === providerValue),
      ]);
      await providerFilter.check();
      await filteredOverview;
      if (await page.locator('#activity-filter-count').textContent() !== '1' || !(await page.locator('#activity-filter-chips button').filter({hasText: providerValue}).isVisible())) throw new Error(`${project.name}: selected Activity filter is not summarized as a count and removable chip`);
      const filteredExplorer = page.waitForResponse(response => new URL(response.url()).pathname === '/admin/activity/logs/page' && new URL(response.url()).searchParams.get('providers') === providerValue);
      await page.locator('[data-activity-tab="requests"]').click();
      await filteredExplorer;
      await page.locator('[data-activity-tab="overview"]').click();
      await page.locator('#activity-filter-trigger').click();
      const resetStats = page.waitForResponse(response => new URL(response.url()).pathname === '/admin/activity/stats' && !new URL(response.url()).searchParams.has('providers'));
      await page.locator('#reset-activity-filters').click();
      await resetStats;
      if (!(await page.locator('#activity-filter-count').isHidden()) || !(await page.locator('#activity-filter-chips').isHidden())) throw new Error(`${project.name}: resetting Activity filters leaves active state visible`);
    }
    await page.locator('#activity-range-trigger').click();
    if (!(await page.locator('#activity-range-popover').isVisible())) throw new Error(`${project.name}: advanced Activity time range picker does not open`);
    await page.locator('#activity-range-search').fill('90 days');
    if (!(await page.locator('#activity-range-options').getByText('Last 90 days', {exact: true}).isVisible())) throw new Error(`${project.name}: Activity range presets cannot be searched`);
    await page.keyboard.press('ArrowDown');
    if (!(await page.locator('#activity-range-options button', {hasText: 'Last 90 days'}).evaluate(element => element === document.activeElement))) throw new Error(`${project.name}: Activity range presets do not support arrow-key focus`);
    await page.keyboard.press('Escape');
    if (await page.locator('#activity-range-popover').isVisible()) throw new Error(`${project.name}: Activity range picker does not dismiss with Escape`);
    const customStats = page.waitForResponse(response => response.url().includes('/admin/activity/stats?since=') && response.url().includes('buckets=36'));
    await page.locator('#activity-range-trigger').click();
    await page.evaluate(() => {
      const to = new Date(); const from = new Date(to.getTime() - 6 * 60 * 60 * 1000);
      const local = date => new Date(date.getTime() - date.getTimezoneOffset() * 60000).toISOString().slice(0, 16);
      document.querySelector('#activity-range-from').value = local(from); document.querySelector('#activity-range-to').value = local(to);
    });
    await page.locator('#apply-activity-range').click(); await customStats;
    await page.waitForFunction(() => document.querySelectorAll('#activity-chart .chart-column').length === 36);
    if (!(await page.locator('#activity-range-label').textContent()).includes('–')) throw new Error(`${project.name}: custom six-hour Activity range is not applied at ten-minute resolution`);
    const presetStats = page.waitForResponse(response => response.url().includes('/admin/activity/stats?since=') && response.url().includes('buckets=48'));
    await page.locator('#activity-range').evaluate(select => select.dispatchEvent(new Event('change'))); await presetStats;
    await page.waitForFunction(() => document.querySelectorAll('#activity-chart .chart-column').length === 48);
    if (testLiveRefresh) {
      const activityRefresh = Promise.all([
        page.waitForResponse(response => response.url().includes('/admin/activity/stats?since=') && response.url().includes('buckets=48') && response.url().includes('until=')),
        page.waitForResponse(response => response.url().includes('/admin/activity/logs?since=') && response.url().includes('limit=100')),
      ]);
      await page.clock.fastForward(30000);
      await activityRefresh;
    }
    const timelineColumns = page.locator('#activity-chart .chart-column');
    if (await timelineColumns.count() !== 48) throw new Error(`${project.name}: 24-hour Activity timeline does not expose every 30-minute interval`);
    if (!(await page.locator('#activity-chart .traffic-line').count()) || !(await page.locator('#activity-chart .traffic-glow').count()) || await page.locator('#activity-chart .traffic-area').count()) throw new Error(`${project.name}: Activity pace is not rendered as a pure line with a local glow`);
    if (await page.locator('#activity-chart .error-rate-line').count() !== 1 || await page.locator('#activity-chart .error-axis').count() !== 3 || !(await page.locator('#activity-chart-legend').getByText('Error rate', {exact: true}).isVisible())) throw new Error(`${project.name}: Request timeline does not overlay a labeled error-rate curve and percentage axis`);
    const errorLineStyle = await page.locator('#activity-chart .error-rate-line').evaluate(element => { const style = getComputedStyle(element); return {width: style.strokeWidth, opacity: style.opacity, dash: style.strokeDasharray}; });
    if (errorLineStyle.width !== '1.5px' || Number(errorLineStyle.opacity) >= 0.75 || errorLineStyle.dash === 'none') throw new Error(`${project.name}: error-rate curve competes with the primary request line (${JSON.stringify(errorLineStyle)})`);
    const chartPaths = await page.locator('#activity-chart').evaluate(element => ({line: element.querySelector('.traffic-line').getAttribute('d'), glow: element.querySelector('.traffic-glow').getAttribute('d')}));
    if (!chartPaths.line.includes(' C') || chartPaths.glow !== chartPaths.line) throw new Error(`${project.name}: Activity pace does not use the same smooth monotone path for its line and local glow`);
    const timelineAlignment = await page.locator('#activity-chart').evaluate(chart => {
      const chartBox = chart.getBoundingClientRect();
      const lineStart = Number(chart.querySelector('.traffic-line').getAttribute('d').match(/^M([\d.]+)/)?.[1]);
      const firstColumnBox = chart.querySelector('.chart-column').getBoundingClientRect();
      const firstColumnCenter = firstColumnBox.left - chartBox.left + firstColumnBox.width / 2;
      return {lineStart, firstColumnCenter};
    });
    if (Math.abs(timelineAlignment.lineStart - timelineAlignment.firstColumnCenter) > 1) throw new Error(`${project.name}: Activity interval value is not centered in its interactive interval (${JSON.stringify(timelineAlignment)})`);
    const inspectorBefore = await page.locator('#chart-inspector-time').textContent();
    if (project.mobile) await timelineColumns.first().click(); else await timelineColumns.first().hover();
    const inspectorAfter = await page.locator('#chart-inspector-time').textContent();
    const firstColumnClass = await timelineColumns.first().getAttribute('class');
    if (!inspectorAfter || inspectorAfter === '—' || (inspectorAfter === inspectorBefore && !firstColumnClass.includes('selected'))) throw new Error(`${project.name}: Activity timeline does not respond to interval interaction`);
    const requestSeriesTooltips = await page.locator('#activity-chart .chart-column').first().evaluate(column => {
      column.dataset.requestsY = '20'; column.dataset.errorY = '180';
      const chartBox = column.closest('#activity-chart').getBoundingClientRect();
      const inspectAt = y => {
        column.dispatchEvent(new PointerEvent('pointerover', {bubbles: true, clientY: chartBox.top + chartBox.height * y / 250}));
        return {series: column.dataset.activeSeries, label: column.querySelector('.chart-value').textContent, pointY: column.style.getPropertyValue('--point-y'), pointColor: column.style.getPropertyValue('--point-color')};
      };
      return {error: inspectAt(180), requests: inspectAt(20)};
    });
    const errorTooltip = requestSeriesTooltips.error; const requestsTooltip = requestSeriesTooltips.requests;
    if (errorTooltip.series !== 'error-rate' || !errorTooltip.label.startsWith('Error rate ') || errorTooltip.pointY !== '180px' || errorTooltip.pointColor !== '#b86f67') throw new Error(`${project.name}: Request timeline does not select the Error rate tooltip nearest the pointer (${JSON.stringify(errorTooltip)})`);
    if (requestsTooltip.series !== 'requests' || !requestsTooltip.label.startsWith('Requests ') || requestsTooltip.pointY !== '20px' || requestsTooltip.pointColor !== '#0b57d0') throw new Error(`${project.name}: Request timeline does not select the Requests tooltip nearest the pointer (${JSON.stringify(requestsTooltip)})`);
    await page.locator('[data-chart-metric="tokens"]').click();
    if (await page.locator('#activity-chart .token-input-line').count() !== 1 || await page.locator('#activity-chart .token-output-line').count() !== 1 || await page.locator('#activity-chart .token-cache-line').count() !== 1) throw new Error(`${project.name}: Token timeline does not separate input, output, and cache hit rate`);
    if (!(await page.locator('#activity-chart-legend').isVisible()) || await page.locator('#activity-chart .traffic-axis-right.token-axis').count() !== 3) throw new Error(`${project.name}: Token timeline does not explain its series or percentage scale`);
    for (const label of ['Input', 'Output', 'Cache hit']) if (!(await page.locator('#chart-inspector-values').getByText(label, {exact: true}).isVisible())) throw new Error(`${project.name}: Token timeline inspector omits ${label}`);
    const outputTooltip = await page.locator('#activity-chart .chart-column').first().evaluate(column => {
      column.dataset.inputY = '20'; column.dataset.outputY = '180'; column.dataset.cacheY = '60';
      const chartBox = column.closest('#activity-chart').getBoundingClientRect();
      column.dispatchEvent(new PointerEvent('pointerover', {bubbles: true, clientY: chartBox.top + chartBox.height * 180 / 250}));
      return {series: column.dataset.activeSeries, label: column.querySelector('.chart-value').textContent, pointY: column.style.getPropertyValue('--point-y'), pointColor: column.style.getPropertyValue('--point-color')};
    });
    if (outputTooltip.series !== 'output' || !outputTooltip.label.startsWith('Output ') || outputTooltip.pointY !== '180px' || outputTooltip.pointColor !== '#0b57d0') throw new Error(`${project.name}: Token timeline does not select the Output tooltip nearest the pointer (${JSON.stringify(outputTooltip)})`);
    await page.locator('[data-chart-metric="latency"]').click();
    if (await page.locator('[data-chart-metric="latency"]').getAttribute('aria-pressed') !== 'true') throw new Error(`${project.name}: Activity timeline metric cannot be changed`);
    if (!(await page.locator('#chart-inspector-values').getByText('Avg latency', {exact: true}).isVisible())) throw new Error(`${project.name}: Activity timeline inspector omits latency`);
    const successTones = await page.evaluate(() => [modelSuccessTone(99.4), modelSuccessTone(97), modelSuccessTone(94.9)]);
    if (successTones.join(',') !== 'model-healthy,model-warning,model-critical') throw new Error(`${project.name}: model success-rate severity does not distinguish healthy, warning, and critical rates`);
    const modelTable = page.locator('.model-analysis-table');
    if (!(await modelTable.locator('th', {hasText: 'Input cache hit'}).count()) || !(await modelTable.locator('th', {hasText: 'Usage value'}).count())) throw new Error(`${project.name}: per-model analysis omits cache efficiency or usage value`);
    if (!(await page.locator('.api-key-analysis-panel').isVisible())) throw new Error(`${project.name}: Activity overview omits Gateway API key analytics`);
    const modelRow = page.locator('#model-stats tr').first();
    if (await modelRow.count()) {
      if (!(await modelRow.textContent()).includes('reported')) throw new Error(`${project.name}: per-model spend does not identify its cost source`);
      if (project.mobile && (!(await modelRow.locator('[data-label="Input cache hit"]').isVisible()) || !(await modelRow.locator('[data-label="Usage value"]').isVisible()))) throw new Error(`${project.name}: mobile model card hides cache efficiency or usage value`);
    }
    if (project.mobile) {
      const modelLayout = await page.locator('.model-analysis-wrap').evaluate(element => ({scrollWidth: element.scrollWidth, clientWidth: element.clientWidth}));
      if (modelLayout.scrollWidth > modelLayout.clientWidth + 1) throw new Error(`${project.name}: per-model analysis hides metrics behind horizontal scrolling`);
    }
    if (!project.mobile) {
      await page.locator('[data-activity-tab="requests"]').click();
      const sevenDayStats = page.waitForResponse(response => response.url().includes('/admin/activity/stats?since=') && response.url().includes('buckets=56'));
      await page.locator('#activity-range').selectOption('604800');
      await sevenDayStats;
      const dayStats = page.waitForResponse(response => response.url().includes('/admin/activity/stats?since=') && response.url().includes('buckets=48'));
      await page.locator('#activity-range').selectOption('86400');
      await dayStats;
      await page.locator('[data-activity-tab="overview"]').click();
      await page.waitForFunction(() => {
        const chart = document.querySelector('#activity-chart'); const svg = chart?.querySelector('.traffic-area-chart');
        return svg && Math.abs(svg.viewBox.baseVal.width - chart.clientWidth) < 2;
      });
      const chartGeometry = await page.locator('#activity-chart').evaluate(chart => ({clientWidth: chart.clientWidth, viewBoxWidth: chart.querySelector('.traffic-area-chart').viewBox.baseVal.width, labelMinutes: [...chart.querySelectorAll('.chart-column small')].map(label => label.textContent.match(/:(\d{2})/)?.[1]).filter(Boolean)}));
      if ((project.width >= 1200 && chartGeometry.viewBoxWidth < 600) || Math.abs(chartGeometry.viewBoxWidth - chartGeometry.clientWidth) >= 2) throw new Error(`${project.name}: Activity chart keeps a hidden-tab fallback width after changing ranges (${JSON.stringify(chartGeometry)})`);
      if (!chartGeometry.labelMinutes.length || chartGeometry.labelMinutes.some(minutes => !['00', '30'].includes(minutes))) throw new Error(`${project.name}: 24-hour Activity intervals are not aligned to wall-clock half hours (${JSON.stringify(chartGeometry.labelMinutes)})`);
    }
    const metricLayout = await page.locator('.activity-metrics').evaluate(element => ({scrollable: element.scrollWidth > element.clientWidth + 1, display: getComputedStyle(element).display}));
    if (project.mobile && !metricLayout.scrollable) throw new Error(`${project.name}: Activity summaries are not swipeable on a narrow screen`);
    if (!project.mobile && project.width >= 1200 && metricLayout.scrollable) throw new Error(`${project.name}: Activity summaries waste wide-screen space`);
    const explorerLoad = testLiveRefresh ? page.waitForResponse(response => response.url().includes('/admin/activity/logs/page?') && response.url().includes('limit=100')) : null;
    await page.locator('[data-activity-tab="requests"]').click();
    if (explorerLoad) await explorerLoad;
    const explorerPanel = page.locator('.request-explorer-panel');
    if (!(await explorerPanel.isVisible())) throw new Error(`${project.name}: Request explorer is not visible`);
    if (!project.mobile) {
      const explorerBox = await explorerPanel.boundingBox();
      const viewport = page.viewportSize();
      if (!explorerBox || !viewport || explorerBox.height < Math.max(440, viewport.height - 290)) throw new Error(`${project.name}: Request explorer does not adapt to available viewport height`);
    }
    if (!(await page.locator('#activity-page-previous').isDisabled())) throw new Error(`${project.name}: Request explorer enables Previous on the first page`);
    if (!(await page.locator('#activity-page-status').textContent()).includes('Page 1')) throw new Error(`${project.name}: Request explorer does not expose page status`);
    const explorerRow = page.locator('#activity-logs .activity-request-row').first();
    if (await explorerRow.count()) {
      if (!(await explorerRow.locator('.activity-model').isVisible()) || !(await explorerRow.locator('.activity-upstream-model').isVisible()) || !(await explorerRow.locator('.route-cell').isVisible()) || !(await explorerRow.locator('.activity-output').isVisible())) throw new Error(`${project.name}: Request explorer does not emphasize requested/upstream models, route, and output usage`);
    }
    await page.locator('[data-activity-tab="overview"]').click();
    const recentActivityRows = page.locator('#recent-activity-logs .activity-request-row');
    const activityRow = recentActivityRows.filter({hasText: 'Same model ID'}).first();
    const mappedActivityRow = recentActivityRows.filter({hasText: 'ui-subscription/gpt-fixture'}).first();
    if (await recentActivityRows.count()) {
      if (!(await activityRow.count()) || !(await mappedActivityRow.count())) throw new Error(`${project.name}: Activity model routing fixtures are missing from recent requests`);
      const routingVisibility = await activityRow.locator('.activity-model').evaluate(element => [...element.querySelectorAll('.activity-model-leg b, .activity-model-unchanged')].map(item => { const box = item.getBoundingClientRect(); const style = getComputedStyle(item); return {text: item.textContent, width: box.width, height: box.height, display: style.display, visibility: style.visibility, overflow: style.overflow}; }));
      if (!(await activityRow.getByText('Client', {exact: true}).isVisible()) || !(await activityRow.getByText('Provider', {exact: true}).isVisible()) || !(await activityRow.getByText('Same model ID', {exact: true}).isVisible())) throw new Error(`${project.name}: unchanged Activity model routing is not explicitly identified by client and Provider side (${JSON.stringify(routingVisibility)})`);
      if (!(await mappedActivityRow.getByText('ui-subscription/gpt-fixture', {exact: true}).isVisible()) || !(await mappedActivityRow.getByText('activity-only-model', {exact: true}).isVisible()) || await mappedActivityRow.getByText('Same model ID', {exact: true}).count()) throw new Error(`${project.name}: mapped Activity model routing does not show both distinct model IDs`);
      await activityRow.click();
      await assertDialog(page, '#activity-detail-dialog', project.name);
      if (!project.mobile) {
        const drawerBox = await page.locator('#activity-detail-dialog').boundingBox();
        const viewport = page.viewportSize();
        if (!drawerBox || !viewport || Math.abs(drawerBox.x + drawerBox.width - viewport.width) > 1 || Math.abs(drawerBox.y) > 1 || Math.abs(drawerBox.height - viewport.height) > 1) throw new Error(`${project.name}: request details are not a right-side full-height desktop drawer (${JSON.stringify(drawerBox)})`);
        const drawerLayout = await page.locator('#activity-detail-dialog').evaluate(dialog => {
          const head = dialog.querySelector('.activity-detail-head').getBoundingClientRect();
          const body = dialog.querySelector('.activity-detail-body');
          const bodyBox = body.getBoundingClientRect();
          return {flexDirection: getComputedStyle(dialog).flexDirection, headHeight: head.height, headBottom: head.bottom, bodyTop: bodyBox.top, bodyWidth: body.clientWidth, bodyScrollWidth: body.scrollWidth};
        });
        if (drawerLayout.flexDirection !== 'column' || drawerLayout.bodyTop < drawerLayout.headBottom - 1 || drawerLayout.headHeight > 72 || drawerLayout.bodyScrollWidth > drawerLayout.bodyWidth + 1) throw new Error(`${project.name}: request details header consumes the drawer width or body content is clipped (${JSON.stringify(drawerLayout)})`);
      }
      if (!(await page.locator('#activity-detail-request').getByText('Request ID', { exact: true }).isVisible())) throw new Error(`${project.name}: request detail dialog omits request metadata`);
      if (!(await page.locator('.activity-routing-request-group #activity-detail-request').isVisible())) throw new Error(`${project.name}: request metadata remains in a separate detail section instead of Routing`);
      const requestMetadataLayout = await page.locator('#activity-detail-request').evaluate(element => {
        const items = [...element.children];
        const valueLines = items.map(item => {
          const range = document.createRange(); range.selectNodeContents(item.querySelector('code'));
          return range.getClientRects().length;
        });
        return {
          items: items.length,
          rows: new Set(items.map(item => Math.round(item.getBoundingClientRect().top))).size,
          columnsPerItem: items.map(item => getComputedStyle(item).gridTemplateColumns.split(' ').length),
          valueLines,
        };
      });
      if (requestMetadataLayout.rows !== requestMetadataLayout.items) throw new Error(`${project.name}: request metadata still places multiple facts on one row (${JSON.stringify(requestMetadataLayout)})`);
      if (project.width > 600 && (requestMetadataLayout.columnsPerItem.some(count => count !== 2) || requestMetadataLayout.valueLines.some(count => count !== 1))) throw new Error(`${project.name}: desktop request metadata does not keep each label and value together on one full-width row (${JSON.stringify(requestMetadataLayout)})`);
      if (project.width <= 600 && requestMetadataLayout.columnsPerItem.some(count => count !== 1)) throw new Error(`${project.name}: narrow request metadata does not stack labels over full-width values (${JSON.stringify(requestMetadataLayout)})`);
      if (!(await page.locator('#activity-detail-model-route').getByText('Client requested', { exact: true }).isVisible()) || !(await page.locator('#activity-detail-model-route').getByText('Sent to Provider', { exact: true }).isVisible())) throw new Error(`${project.name}: request detail does not name the client and Provider sides of model routing`);
      if (!(await page.locator('#activity-detail-model-outcome').getByText('Model ID unchanged', { exact: true }).isVisible())) throw new Error(`${project.name}: request detail does not explain identical client and Provider model IDs`);
      if (!(await page.locator('#activity-detail-api-route').getByText('Client API', { exact: true }).isVisible()) || !(await page.locator('#activity-detail-api-route').getByText('Provider API', { exact: true }).isVisible()) || !(await page.locator('#activity-detail-api-route').getByText('No API conversion', { exact: true }).isVisible())) throw new Error(`${project.name}: request detail does not explain the client and Provider API formats`);
      if (!(await page.locator('#activity-detail-destination').getByText('Provider', { exact: true }).isVisible()) || !(await page.locator('#activity-detail-destination').getByText('Endpoint', { exact: true }).isVisible())) throw new Error(`${project.name}: request detail does not group Provider and Endpoint under the routing destination`);
      if (await page.locator('#activity-detail-request').getByText('Caller protocol', { exact: true }).count() || await page.locator('#activity-detail-request').getByText('Upstream protocol', { exact: true }).count()) throw new Error(`${project.name}: request metadata still uses unexplained protocol terminology`);
      if (!(await page.locator('#activity-detail-request').getByText('Provider finish reason', { exact: true }).isVisible())) throw new Error(`${project.name}: request detail dialog omits the Provider finish reason`);
      const detailTypography = await page.locator('#activity-detail-dialog').evaluate(dialog => {
        const fontSize = selector => parseFloat(getComputedStyle(dialog.querySelector(selector)).fontSize);
        return {
          header: fontSize('.activity-detail-head h2'), summary: fontSize('#activity-detail-route'), section: fontSize('.activity-detail-section h3'), description: fontSize('.detail-section-heading p'),
          modelLabel: fontSize('.activity-detail-model-route > div > span'), modelValue: fontSize('.activity-detail-model-route code'), modelHelp: fontSize('.activity-detail-model-route small'),
          factLabel: fontSize('.activity-routing-facts span'), factValue: fontSize('.activity-routing-facts strong'), factHelp: fontSize('.activity-routing-facts small'), requestValue: fontSize('.activity-request-facts code'),
          timelineAxis: fontSize('.timeline-axis'), timelineStage: fontSize('.timeline-stage'), timelineHelp: fontSize('.timeline-stage-label small'), usageLabel: fontSize('.activity-detail-grid span'), usageValue: fontSize('.activity-detail-grid strong'), privacy: fontSize('.activity-privacy-note'),
        };
      });
      const minimumDetailTypography = {header: 20, summary: 13, section: 16, description: 13, modelLabel: 11, modelValue: 15, modelHelp: 12, factLabel: 11, factValue: 14, factHelp: 11, requestValue: 13, timelineAxis: 10, timelineStage: 12, timelineHelp: 10, usageLabel: 12, usageValue: 18, privacy: 12};
      const undersizedDetailText = Object.entries(minimumDetailTypography).filter(([name, minimum]) => detailTypography[name] < minimum);
      if (undersizedDetailText.length) throw new Error(`${project.name}: request detail typography is too small (${JSON.stringify({detailTypography, undersizedDetailText})})`);
      if (!await page.locator('#activity-detail-total').textContent() || await page.locator('#activity-detail-total').textContent() === '—' || await page.locator('#activity-detail-timing .timeline-total').count()) throw new Error(`${project.name}: request timeline does not keep one heading total or still repeats it as a table row`);
      if (!(await page.locator('#activity-detail-dialog').getByText('Request timeline', { exact: true }).isVisible())) throw new Error(`${project.name}: request detail dialog omits the shared-scale request timeline`);
      const timelineGeometry = await page.locator('#activity-detail-timing').evaluate(element => {
        const scale = element.querySelector('.timeline-scale').getBoundingClientRect(); const track = element.querySelector('.timeline-track'); const trackBox = track.getBoundingClientRect(); const style = getComputedStyle(track); const label = element.querySelector('.timeline-stage-label strong'); const bar = element.querySelector('.timeline-stage-bar');
        return {scaleLeft: scale.left, scaleRight: scale.right, trackLeft: trackBox.left, trackRight: trackBox.right, trackBorder: style.borderWidth, trackBackground: style.backgroundImage, labelAlign: getComputedStyle(label).textAlign, barClip: getComputedStyle(bar).clipPath, barHeight: getComputedStyle(bar).height, barStartRadius: getComputedStyle(bar).borderTopLeftRadius, barEndRadius: getComputedStyle(bar).borderTopRightRadius, barDecoration: getComputedStyle(bar, '::after').content};
      });
      if (Math.abs(timelineGeometry.scaleLeft - timelineGeometry.trackLeft) > 1 || Math.abs(timelineGeometry.scaleRight - timelineGeometry.trackRight) > 1) throw new Error(`${project.name}: request timeline scale and tracks are not aligned (${JSON.stringify(timelineGeometry)})`);
      if (timelineGeometry.trackBorder !== '0px' || timelineGeometry.trackBackground !== 'none' || timelineGeometry.labelAlign !== 'left' || timelineGeometry.barClip !== 'none' || timelineGeometry.barHeight !== '5px' || timelineGeometry.barDecoration !== 'none') throw new Error(`${project.name}: request timeline lacks borderless tracks or thin rounded stage bars (${JSON.stringify(timelineGeometry)})`);
      if (!(await page.locator('#activity-detail-request').getByText('Gateway API key', { exact: true }).isVisible())) throw new Error(`${project.name}: request detail dialog omits the authenticated Gateway API key identity`);
      if (!(await page.locator('#activity-detail-failure').count())) throw new Error(`${project.name}: request detail dialog omits the failure diagnosis region`);
      if (!(await page.locator('#activity-detail-dialog').getByText('Prompt and response content are not retained.').isVisible())) throw new Error(`${project.name}: request detail dialog omits the content-retention notice`);
      if (await page.locator('#activity-capture-link').count()) throw new Error(`${project.name}: request detail dialog still presents the unrelated Traffic Capture lookup`);
      const refreshCost = page.locator('#activity-detail-usage .refresh-activity-cost');
      if (!(await refreshCost.count())) throw new Error(`${project.name}: non-reported Activity detail omits its cost refresh action`);
      page.once('dialog', dialog => dialog.accept());
      const costRefreshResponse = page.waitForResponse(response => response.url().endsWith('/admin/activity/recalculate-costs') && response.request().method() === 'POST');
      await refreshCost.click();
      const costRefreshRequest = (await costRefreshResponse).request().postDataJSON();
      if (costRefreshRequest.request_id !== `ui-unchanged-model-${project.name}` || costRefreshRequest.source_instance_id !== 'responsive-remote-instance') throw new Error(`${project.name}: single-record cost refresh omits the stable Activity source identity (${JSON.stringify(costRefreshRequest)})`);
      await page.waitForFunction(() => document.querySelector('#activity-cost-refresh-status').textContent.length > 0);
      await page.locator('#activity-detail-dialog').evaluate(dialog => dialog.close());
      await mappedActivityRow.click();
      await assertDialog(page, '#activity-detail-dialog', project.name);
      if (!(await page.locator('#activity-detail-model-outcome').getByText('Model ID changed', {exact: true}).isVisible()) || !(await page.locator('#activity-detail-api-route').getByText('Converted by Yabane', {exact: true}).isVisible())) throw new Error(`${project.name}: mapped request detail does not identify model mapping and API conversion`);
      if (!(await page.locator('#activity-detail-api-route').getByText('Anthropic Messages', {exact: true}).isVisible()) || !(await page.locator('#activity-detail-api-route').getByText('OpenAI Responses', {exact: true}).isVisible())) throw new Error(`${project.name}: converted request detail does not show both API formats`);
      await page.locator('#activity-detail-dialog').evaluate(dialog => dialog.close());
      await activityRow.click();
      await assertDialog(page, '#activity-detail-dialog', project.name);
      const editActivityPricing = page.locator('#activity-detail-usage .edit-activity-pricing');
      if (!(await editActivityPricing.count())) throw new Error(`${project.name}: missing-cost Activity detail does not offer a price action`);
      const upstreamModel = await page.locator('#activity-detail-upstream-model').textContent();
      await editActivityPricing.click();
      await page.locator('#pricing-view').waitFor({state: 'visible'});
      if (!(await page.locator('#pricing-editor-page').isVisible())) throw new Error(`${project.name}: Activity price action does not open the centralized pricing editor`);
      if (await page.locator('#pricing-model').inputValue() !== upstreamModel) throw new Error(`${project.name}: Activity pricing action does not prefill the exact upstream model`);
      if (!(await page.locator('#central-pricing-form [name="scope"][value="global"]').isChecked())) throw new Error(`${project.name}: a new Activity price does not default to the global scope`);
      await page.locator('#cancel-pricing-edit').click();
      await page.evaluate(() => document.querySelector('[data-view="activity"]').click());
    }
    await page.locator('#manage-activity-data').click();
    await assertDialog(page, '#activity-data-dialog', project.name);
    await page.locator('[data-activity-data-tab="import"]').click();
    const previewRecord = {
      timestamp: Math.floor(Date.now() / 1000), request_id: `responsive-preview-${project.name}`,
      path: '/v1/responses', model: 'preview/model', provider: 'preview', endpoint: 'preview',
      status: 200, latency_ms: 1200, gateway_ms: 4, upstream_response_ms: 180, first_byte_ms: 250, generation_ms: 950,
      input_tokens: 120, output_tokens: 40, cached_tokens: 20, cost: null, finish_reason: 'completed', streaming: false,
    };
    await page.locator('#activity-import-file').setInputFiles({
      name: 'activity-preview.json', mimeType: 'application/json',
      buffer: Buffer.from(JSON.stringify({ format: 'yabane-activity', version: 1, instance_id: `responsive-${project.name}`, records: [previewRecord] })),
    });
    await page.locator('#activity-import-summary').waitFor({ state: 'visible' });
    await page.getByRole('button', { name: 'Import 1 new records' }).waitFor();
    await page.locator('[data-activity-data-tab="storage"]').click();
    await page.locator('#activity-data-dialog .close-activity-data').first().click();
    await assertNoPageOverflow(page, project.name, 'console');
    console.log(`${project.name}: responsive console and dialogs passed`);
    await context.close();
  } finally {
    await browser.close();
  }
}

async function assertNoPageOverflow(page, name, surface) {
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth > document.documentElement.clientWidth);
  if (overflow) throw new Error(`${name}: ${surface} has horizontal overflow`);
}

async function assertDialog(page, selector, name) {
  const dialog = page.locator(selector);
  await dialog.evaluate(element => Promise.all(element.getAnimations().map(animation => animation.finished.catch(() => {}))));
  const box = await dialog.boundingBox();
  const viewport = page.viewportSize();
  if (!box || !viewport) throw new Error(`${name}: ${selector} is not visible`);
  const tolerance = 1;
  if (box.x < -tolerance || box.y < -tolerance || box.x + box.width > viewport.width + tolerance || box.y + box.height > viewport.height + tolerance) {
    throw new Error(`${name}: ${selector} escapes viewport (${JSON.stringify(box)})`);
  }
  const close = await page.locator(`${selector} .icon-button`).first().boundingBox();
  if (!close || close.width < 40 || close.height < 40) throw new Error(`${name}: ${selector} close target is too small`);
}
