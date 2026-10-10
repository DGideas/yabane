import { chromium, webkit } from 'playwright';
import { openConsoleView } from './console-view-helper.mjs';

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
      const session = await context.request.get(`${base}/admin/session`, {timeout: 5000});
      if (!session.ok() || !(await session.json()).authenticated) throw new Error(`${project.name}: supplied administrator session did not authenticate; refusing to skip authenticated coverage`);
    }
    const page = await context.newPage();
    const pageErrors = [];
    page.on('pageerror', error => pageErrors.push(error));
    const assertNoPageErrors = () => {
      if (pageErrors.length) throw new AggregateError(pageErrors, `${project.name}: uncaught browser errors`);
    };
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
    const deviceCodeBodies = [];
    await page.route('**/admin/endpoint-types/openai_codex/sign-in/device-code', async route => {
      if (route.request().method() !== 'POST') return route.continue();
      deviceCodeBodies.push(route.request().postDataJSON());
      await route.fulfill({status: 201, contentType: 'application/json', body: JSON.stringify({id: 'device-flow', status: 'pending', user_code: 'ABCD-EFGH', verification_uri: 'https://auth.openai.com/codex/device', interval_seconds: 60, expires_at: 4102444800})});
    });
    await page.route('**/admin/endpoint-types/openai_codex/sign-in/device-code/device-flow', async route => {
      await route.fulfill({status: 200, contentType: 'application/json', body: JSON.stringify({id: 'device-flow', status: 'pending', user_code: 'ABCD-EFGH', verification_uri: 'https://auth.openai.com/codex/device', interval_seconds: 60, expires_at: 4102444800})});
    });
    await page.route('**/admin/endpoint-types/openai_codex/sign-in/oauth', async route => {
      if (route.request().method() !== 'POST') return route.continue();
      await route.fulfill({status: 201, contentType: 'application/json', body: JSON.stringify({id: 'browser-flow', authorization_url: 'https://auth.openai.com/oauth/authorize?state=browser-state', expires_at: 4102444800})});
    });
    await page.route('**/admin/endpoint-types/openai_codex/sign-in/oauth/browser-flow/complete', async route => {
      const body = route.request().postDataJSON();
      if (body.redirect_url !== 'http://localhost:1455/auth/callback?code=oauth-code&state=browser-state') throw new Error(`${project.name}: browser OAuth did not submit the complete callback URL`);
      await route.fulfill({status: 204});
    });
    await page.route('**/admin/endpoint-types', async route => {
      if (route.request().method() !== 'GET') return route.continue();
      const response = await route.fetch();
      if (!response.ok()) return route.fulfill({response});
      const body = await response.json();
      body.push({
        id: 'acme_plan', label: 'Acme plan', description: 'Acme plan Endpoints', default_endpoint_id: 'acme',
        fixed_base_url: 'https://api.acme.test/plan', native: false,
        credential_kinds: [{id: 'acme_account', label: 'Acme account', flow: 'subscription'}],
        sign_in: {device_code: false, browser: true},
      });
      await route.fulfill({response, json: body});
    });
    // Provider runtime state that changes between reads, so the live-refresh checks
    // can end a cooldown the console already fetched instead of only changing it
    // across a reload.
    const providerRuntimeFixture = {chatgptAccountCooldown: 90};
    // Group numbers that drifted apart in the file, delivered by a Provider read the test
    // awaits instead of edited in memory, so the reload that follows a save cannot replace
    // them behind the dialog that is being asserted.
    let tieredPriorityDrift = null;
    await page.route('**/admin/providers', async route => {
      if (route.request().method() !== 'GET') return route.continue();
      const response = await route.fetch();
      if (!response.ok()) return route.fulfill({response});
      const body = await response.json();
      body.push({
        id: 'ui-subscription', name: 'UI subscription fixture', extra_headers: {}, extra_body: {}, defaults_endpoint_ids: [],
        endpoints: [{
          id: 'chatgpt', api_type: 'openai_codex', endpoint_type_label: 'OpenAI subscription', fixed_base_url: 'https://chatgpt.com/backend-api',
          sign_in: {device_code: true, browser: true}, base_url: 'https://chatgpt.com/backend-api', socks5_proxy: null,
          extra_headers: {}, extra_body: {}, requires_credential: true, rate_limit_cooldown: {seconds: 3600, mode: 'prefer_provider'},
          rate_limit_cooldown_activity: {applied: 3, skipped: 1, last_seconds: 3600, last_reported_seconds: 7200, last_source: 'capped', last_applied_at: 1700000000, last_observed_at: 1700000600},
          credentials: [{id: 'account', name: 'OpenAI account', weight: 50, enabled: true, kind: 'openai_subscription', kind_label: 'OAuth account', subscription_expires_at: 1, ...(providerRuntimeFixture.chatgptAccountCooldown ? {cooldown_seconds_remaining: providerRuntimeFixture.chatgptAccountCooldown} : {})}, {id: 'account-2', name: 'Second account', weight: 50, enabled: true, kind: 'openai_subscription', kind_label: 'OAuth account', subscription_expires_at: 1}],
        }],
        discovered_models: ['gpt-fixture'], model_endpoints: {'gpt-fixture': ['chatgpt']}, model_endpoint_preferences: [],
        models_discovered_at: 1, model_discovery_error: null,
      }, {
        id: 'ui-keyless', name: 'UI keyless fixture', extra_headers: {}, extra_body: {}, defaults_endpoint_ids: [],
        endpoints: [{
          id: 'local', api_type: 'openai_compatible', base_url: 'http://127.0.0.1:18080/v1', socks5_proxy: null,
          extra_headers: {}, extra_body: {}, requires_credential: false, credentials: [], rate_limit_cooldown: {seconds: 120, mode: 'fixed'},
        }],
        discovered_models: ['local-model'], model_endpoints: {'local-model': ['local']}, model_endpoint_preferences: [],
        models_discovered_at: 1, model_discovery_error: null,
      }, {
        id: 'ui-plain', name: 'UI plain fixture', extra_headers: {}, extra_body: {}, defaults_endpoint_ids: [],
        endpoints: [{
          id: 'main', api_type: 'openai_compatible', base_url: 'http://127.0.0.1:18080/v1', socks5_proxy: null,
          extra_headers: {}, extra_body: {}, requires_credential: true, rate_limit_cooldown: {seconds: 0, mode: 'fixed'},
          credentials: [{id: 'default', name: 'Default', weight: 100, enabled: true, kind: 'secret'}],
        }],
        discovered_models: ['plain-model'], model_endpoints: {'plain-model': ['main']}, model_endpoint_preferences: [],
        models_discovered_at: 1, model_discovery_error: null,
      }, {
        id: 'ui-tiered', name: 'UI tiered fixture', extra_headers: {}, extra_body: {}, defaults_endpoint_ids: [],
        endpoints: [{
          id: 'pool', api_type: 'openai_compatible', base_url: 'http://127.0.0.1:18080/v1', socks5_proxy: null,
          extra_headers: {}, extra_body: {}, requires_credential: true, rate_limit_cooldown: {seconds: 3600, mode: 'prefer_provider'},
          rate_limit_cooldown_activity: {applied: 1, skipped: 0, last_seconds: 3600, last_reported_seconds: 3600, last_source: 'provider', last_applied_at: 1700000000, last_observed_at: 1700000600},
          credentials: [
            {id: 'primary', name: 'Primary account', weight: 100, priority: 1, enabled: true, kind: 'secret'},
            {id: 'standby', name: 'Standby account', weight: 100, priority: 2, enabled: true, kind: 'secret'},
          ],
        }, {
          id: 'open', api_type: 'openai_compatible', base_url: 'http://127.0.0.1:18080/v1', socks5_proxy: null,
          extra_headers: {}, extra_body: {}, requires_credential: true, rate_limit_cooldown: {seconds: 0, mode: 'fixed'},
          credentials: [
            {id: 'primary', name: 'Primary account', weight: 100, priority: 1, enabled: true, kind: 'secret'},
            {id: 'standby', name: 'Standby account', weight: 100, priority: 2, enabled: true, kind: 'secret'},
          ],
        }],
        discovered_models: ['tiered-model'], model_endpoints: {'tiered-model': ['pool', 'open']}, model_endpoint_preferences: [],
        models_discovered_at: 1, model_discovery_error: null,
      });
      // A read can also carry group numbers that drifted apart, which is what the
      // distribution dialog has to normalise; the drift is applied to the assembled
      // list so it follows the same path every other read takes.
      const tieredFixture = tieredPriorityDrift && body.find(provider => provider.id === 'ui-tiered');
      if (tieredFixture) tieredFixture.endpoints[0].credentials.forEach((credential, index) => { credential.priority = tieredPriorityDrift[index]; });
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
        provider: 'ui-subscription', endpoint: 'chatgpt', upstream_credential_id: 'account', upstream_credential_name: 'OpenAI account', credential_cooling: true,
        route_mode: 'failover', route_priority: 2, route_failover: true,
        caller_protocol: 'openai_responses', upstream_protocol: 'openai_responses',
        status: 200, latency_ms: 110, gateway_ms: 4, upstream_response_ms: 18, first_byte_ms: 28, generation_ms: 82,
        input_tokens: 100, output_tokens: 30, cached_tokens: 10, cost: null, finish_reason: 'completed', streaming: false,
      }, {
        timestamp: Math.floor(Date.now() / 1000) - 1, request_id: `ui-mapped-model-${project.name}`,
        path: '/v1/messages', model: 'ui-subscription/gpt-fixture', upstream_model: 'activity-only-model',
        provider: 'ui-subscription', endpoint: 'chatgpt', caller_protocol: 'anthropic_messages', upstream_protocol: 'openai_responses',
        status: 200, latency_ms: 120, gateway_ms: 4, upstream_response_ms: 20, first_byte_ms: 30, generation_ms: 90,
        input_tokens: 120, output_tokens: 40, cached_tokens: 20, cost: null, finish_reason: 'completed', streaming: false,
      }, {
        timestamp: Math.floor(Date.now() / 1000) - 2, request_id: `ui-priced-alias-${project.name}`,
        path: '/v1/messages', model: 'priced-alias', upstream_model: 'alias-sent-two',
        provider: 'ui-subscription', endpoint: 'chatgpt', caller_protocol: 'anthropic_messages', upstream_protocol: 'anthropic_messages',
        status: 200, latency_ms: 90, gateway_ms: 3, upstream_response_ms: 15, first_byte_ms: 20, generation_ms: 70,
        input_tokens: 1000, output_tokens: 100, cached_tokens: 0, cost: 0.0033, cost_source: 'estimated', finish_reason: 'end_turn', streaming: false,
        pricing_sources: {input: {scope: 'global', pattern: 'priced-alias', name: 'incoming'}, output: {scope: 'provider', pattern: 'alias-sent-two', name: 'outgoing'}},
      }, {
        timestamp: Math.floor(Date.now() / 1000) - 3, request_id: `ui-credential-name-${project.name}`,
        path: '/v1/chat/completions', model: 'credential-name-fixture', upstream_model: 'credential-name-sent',
        provider: 'ui-subscription', endpoint: 'chatgpt', upstream_credential_id: 'account-2',
        caller_protocol: 'openai_chat_completions', upstream_protocol: 'openai_chat_completions',
        status: 200, latency_ms: 70, gateway_ms: 3, upstream_response_ms: 12, first_byte_ms: 16, generation_ms: 54,
        input_tokens: 10, output_tokens: 5, cached_tokens: 0, cost: null, finish_reason: 'stop', streaming: false,
      }, {
        timestamp: Math.floor(Date.now() / 1000) - 4, request_id: `ui-credential-remote-${project.name}`, source_instance_id: 'responsive-remote-instance',
        path: '/v1/chat/completions', model: 'credential-remote-fixture', upstream_model: 'credential-remote-sent',
        provider: 'ui-subscription', endpoint: 'chatgpt', upstream_credential_id: 'account', upstream_credential_name: 'Remote ChatGPT account',
        caller_protocol: 'openai_chat_completions', upstream_protocol: 'openai_chat_completions',
        status: 200, latency_ms: 70, gateway_ms: 3, upstream_response_ms: 12, first_byte_ms: 16, generation_ms: 54,
        input_tokens: 10, output_tokens: 5, cached_tokens: 0, cost: null, finish_reason: 'stop', streaming: false,
      }, {
        timestamp: Math.floor(Date.now() / 1000) - 5, request_id: `ui-credential-unknown-${project.name}`,
        path: '/v1/chat/completions', model: 'credential-unknown-fixture', upstream_model: 'credential-unknown-sent',
        provider: 'ui-subscription', endpoint: 'chatgpt', upstream_credential_id: 'account-9',
        caller_protocol: 'openai_chat_completions', upstream_protocol: 'openai_chat_completions',
        status: 200, latency_ms: 70, gateway_ms: 3, upstream_response_ms: 12, first_byte_ms: 16, generation_ms: 54,
        input_tokens: 10, output_tokens: 5, cached_tokens: 0, cost: null, finish_reason: 'stop', streaming: false,
      });
      await route.fulfill({response, json: body});
    });
    await page.route('**/admin/activity/recalculate-costs', async route => {
      if (route.request().method() !== 'POST') return route.continue();
      await route.fulfill({status: 200, contentType: 'application/json', body: JSON.stringify({candidates: 2, updated: 2, filled: 1, recalculated: 1, reported_preserved: 3, skipped_missing_route: 0, skipped_missing_pricing: 0, skipped_missing_usage: 0})});
    });
    const testLiveRefresh = project.name === 'desktop-chrome';
    if (testLiveRefresh) await page.clock.install();

    if (project.name === 'desktop-chrome' && sessionCookie) {
      // A direct link renders the Provider page before the Extensions list arrives; the
      // Request defaults card must catch up instead of claiming the Extension is absent.
      await page.goto(`${base}/providers/ui-keyless`, {waitUntil: 'domcontentloaded'});
      await page.locator('.defaults-card .section-kicker').waitFor();
      await page.waitForFunction(() => /Extension (enabled|disabled)/.test(document.querySelector('.defaults-card .section-kicker')?.textContent || ''), undefined, {timeout: 15000});
      await page.goto(base, {waitUntil: 'domcontentloaded'});
    }
    await page.goto(`${base}/docs`, {waitUntil: 'domcontentloaded'});
    await page.locator('.docs-op').first().waitFor();
    await assertNoUpstreamCopy(page, project.name, 'API docs');
    const specUpstreamCopy = await page.evaluate(async () => {
      const spec = await (await fetch('/openapi.json')).json();
      const found = [];
      const walk = value => {
        if (typeof value === 'string') { if (/\bupstream\b/i.test(value)) found.push(value.replace(/\s+/g, ' ').trim().slice(0, 120)); return; }
        if (Array.isArray(value)) { value.forEach(walk); return; }
        if (value && typeof value === 'object') Object.values(value).forEach(walk);
      };
      walk(spec);
      return [...new Set(found)];
    });
    // The rendered reference shows one operation at a time, so scan every spec string
    // value (descriptions, summaries, examples) instead of only the initially visible page.
    if (specUpstreamCopy.length) throw new Error(`${project.name}: OpenAPI prose still shows ambiguous "upstream" wording: ${JSON.stringify(specUpstreamCopy)}`);
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
      if (sessionCookie) throw new Error(`${project.name}: supplied administrator session did not authenticate; refusing to skip authenticated coverage`);
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
      await assertNoUpstreamCopy(page, project.name, 'login page');
      assertNoPageErrors();
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
    await assertCodeChipsHugContent(page, '.extension-build-commands code', project.name, 'Extension build commands');
    const developmentLink = page.locator('.extension-development-link');
    if (!(await developmentLink.isVisible()) || await developmentLink.getAttribute('href') !== 'https://github.com/DGideas/yabane/blob/master/.agents/skills/yabane-extensions/SKILL.md') throw new Error(`${project.name}: Extensions page omits the development guide link`);
    const requestDefaultsExtension = page.locator('.extension-card').filter({hasText: 'request-defaults'});
    if (!(await requestDefaultsExtension.isVisible())) throw new Error(`${project.name}: Request Defaults is missing from Extensions`);
    const openAiSubscriptionExtension = page.locator('.extension-card').filter({hasText: 'openai-subscription'});
    if (!(await openAiSubscriptionExtension.isVisible()) || !(await openAiSubscriptionExtension.getByText('1 Endpoint', {exact: true}).isVisible())) throw new Error(`${project.name}: OpenAI Subscription Extension is missing or not linked to its Endpoint resources`);
    if (!(await openAiSubscriptionExtension.getByText('Provider Endpoint', {exact: true}).isVisible())) throw new Error(`${project.name}: OpenAI Subscription Extension does not declare its Endpoint stage`);
    // The Extension API keeps the stable Hook IDs; the console prints Provider-side
    // words, so a card may never show an underscored or lower-cased stage ID.
    const hookLabels = await page.locator('.extension-card .extension-facts span').evaluateAll(spans => spans.filter(span => span.querySelector('small')?.textContent.trim() === 'Hooks').map(span => span.querySelector('strong').textContent.trim()));
    if (!hookLabels.length) throw new Error(`${project.name}: no Extension card declares its Hook stages`);
    const strayLabel = hookLabels.find(label => !/^Provider( [A-Za-z][a-z]*)+( · Provider( [A-Za-z][a-z]*)+)*$/.test(label));
    if (strayLabel) throw new Error(`${project.name}: an Extension card prints a Hook stage as ${JSON.stringify(strayLabel)} instead of Provider-side words`);
    if (project.name === 'desktop-chrome') {
      let disableWarning = '';
      page.once('dialog', async dialog => { disableWarning = dialog.message(); await dialog.dismiss(); });
      await openAiSubscriptionExtension.locator('[data-extension-toggle="openai-subscription"]').evaluate(input => input.click());
      await page.waitForFunction(() => document.querySelector('[data-extension-toggle="openai-subscription"]').checked);
      if (!disableWarning.includes('Disable OpenAI Subscription?') || !disableWarning.includes('1 configured Endpoint') || !disableWarning.includes('everything this Extension provides stops')) throw new Error(`${project.name}: disabling an Extension-owned Endpoint type does not confirm the impact on configured Endpoints`);
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
      for (const heading of ['Capture', 'Route', 'Status', 'Provider time / size']) {
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
    if (!(await captureDialog.getByText(/17.7 s at the Provider/).isVisible()) || !(await page.locator('[data-capture-id="ui-capture"]').getByText('17.7 s', {exact: true}).isVisible())) throw new Error(`${project.name}: Traffic Capture does not show Provider duration in its list and detail`);
    await assertNoUpstreamCopy(page, project.name, 'Traffic Capture');
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
    await assertCodeChipsHugContent(page, '.provider-list-main code', project.name, 'Provider model labels');
    // A credential the Provider rate-limited is out of its Endpoint's rotation, so
    // the row states that and when it returns instead of leaving the condition to
    // be found by opening the Provider.
    const coolingProvider = page.locator('#providers .provider-list-item').filter({has: page.locator('code', {hasText: 'ui-subscription/model-id'})});
    if (await coolingProvider.count()) {
      const coolingNote = coolingProvider.locator('.provider-cooling');
      const coolingText = (await coolingNote.textContent()).trim();
      if (await coolingNote.count() !== 1 || !coolingText.includes('1 credential cooling down') || !coolingText.includes('resumes in 1 minute 30 seconds')) throw new Error(`${project.name}: the Provider list does not state a cooling credential and when it returns (${coolingText})`);
      const coolingTitle = (await coolingNote.getAttribute('title')) || '';
      if (!coolingTitle.includes('chatgpt · OpenAI account resumes in 1 minute 30 seconds')) throw new Error(`${project.name}: the Provider cooling note does not name the Endpoint and credential that are out (${coolingTitle})`);
    }
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
      const measuredBars = columns.map(column => { const columnBox = column.getBoundingClientRect(); const barBox = column.querySelector('span').getBoundingClientRect(); return {width: barBox.width, offset: (barBox.left + barBox.width / 2) - (columnBox.left + columnBox.width / 2)}; });
      return {
        width,
        expectedColumns,
        columns: columns.length,
        representedRequests: columns.reduce((total, column) => total + Number(column.title.match(/: (\d+) request/)?.[1] || 0), 0),
        description: document.querySelector('#home-traffic-description').textContent,
        barWidths: measuredBars.map(bar => bar.width),
        barCenterOffsets: measuredBars.map(bar => bar.offset),
      };
    });
    if (homeChartResult.columns !== homeChartResult.expectedColumns) throw new Error(`${project.name}: ${homeChartResult.width}px Home chart renders ${homeChartResult.columns} bars instead of ${homeChartResult.expectedColumns}`);
    if (homeChartResult.representedRequests !== 48) throw new Error(`${project.name}: responsive Home chart aggregation changes the represented request total`);
    const expectedInterval = homeChartResult.expectedColumns === 48 ? '30-minute' : homeChartResult.expectedColumns === 24 ? 'Hourly' : '2-hour';
    if (!homeChartResult.description.startsWith(expectedInterval)) throw new Error(`${project.name}: responsive Home chart does not describe its ${expectedInterval} intervals`);
    const barWidths = homeChartResult.barWidths;
    const widestBar = Math.max(...barWidths);
    const narrowestBar = Math.min(...barWidths);
    if (widestBar - narrowestBar > 0.5) throw new Error(`${project.name}: Home chart bars differ in width (${narrowestBar.toFixed(2)}px to ${widestBar.toFixed(2)}px), so axis labels widen their own intervals`);
    const misalignedBars = homeChartResult.barCenterOffsets.map(offset => Math.abs(offset)).filter(offset => offset > 0.5);
    if (misalignedBars.length) throw new Error(`${project.name}: ${misalignedBars.length} Home chart bars are not centered on their interval (up to ${Math.max(...misalignedBars).toFixed(2)}px off)`);

    // Read the interval from the running page, not possibly mismatched local assets.
    const liveRefreshIntervalMs = testLiveRefresh ? await page.evaluate(() => {
      if (!Number.isFinite(LIVE_REFRESH_INTERVAL_MS) || LIVE_REFRESH_INTERVAL_MS <= 0) throw new Error('Invalid console live-refresh interval');
      return LIVE_REFRESH_INTERVAL_MS;
    }) : null;
    if (testLiveRefresh) {
      const homeRefresh = page.waitForResponse(response => response.url().includes('/admin/activity/stats?since='));
      await page.clock.fastForward(liveRefreshIntervalMs);
      await homeRefresh;
      // SVC-61: Provider runtime state is re-read rather than kept from page load: a
      // credential whose cooldown ends is shown back in rotation without a reload,
      // while an open model catalog, a search in progress, and expanded details keep
      // their state.
      const providerListRead = page.waitForResponse(response => response.request().method() === 'GET' && new URL(response.url()).pathname === '/admin/providers');
      await page.evaluate(() => document.querySelector('[data-view="providers"]').click());
      await providerListRead;
      const liveProvider = page.locator('#providers .provider-list-item').filter({has: page.locator('code', {hasText: 'ui-subscription/model-id'})});
      const providerRead = page.waitForResponse(response => response.request().method() === 'GET' && new URL(response.url()).pathname === '/admin/providers');
      await liveProvider.click();
      await providerRead;
      const liveCredential = page.locator('.subscription-credential');
      await liveCredential.locator('.credential-cooldown').waitFor({state: 'visible'});
      await page.locator('#provider-detail .credential-details summary').click();
      await page.locator('.browse-provider-models').click();
      await page.locator('#provider-detail .provider-model-browser').waitFor({state: 'visible'});
      await page.locator('.model-browser-toolbar [role="searchbox"]').fill('gpt');
      providerRuntimeFixture.chatgptAccountCooldown = 0;
      await page.clock.fastForward(liveRefreshIntervalMs);
      await page.waitForFunction(() => !document.querySelector('.subscription-credential .credential-cooldown'));
      const livePool = await page.locator('.subscription-endpoint .endpoint-pool').textContent();
      if (livePool.includes('is out until')) throw new Error(`${project.name}: a live refresh kept a credential out after its cooldown ended (${livePool})`);
      const liveShares = await liveCredential.locator('.traffic-share').allTextContents();
      if (!liveShares.some(text => text.includes('50%') && !text.includes('while cooling down'))) throw new Error(`${project.name}: a live refresh kept the cooling share (${liveShares.join(' | ')})`);
      if (await page.locator('#provider-detail .provider-model-browser').isHidden()) throw new Error(`${project.name}: a live refresh closed the model catalog it redrew around`);
      if (await page.locator('.model-browser-toolbar [role="searchbox"]').inputValue() !== 'gpt') throw new Error(`${project.name}: a live refresh discarded the model catalog search`);
      if (await page.locator('#provider-detail .credential-details').getAttribute('open') === null) throw new Error(`${project.name}: a live refresh collapsed expanded credential details`);
      if (await liveProvider.locator('.provider-cooling').count()) throw new Error(`${project.name}: the Provider list kept its cooling note after the cooldown ended`);
      // Restore the fixture and let the same refresh pick the cooldown up again: a
      // cooldown that begins while the page stays open is shown without a reload, and
      // the later checks still find the cooling credential they describe.
      providerRuntimeFixture.chatgptAccountCooldown = 90;
      await page.clock.fastForward(liveRefreshIntervalMs);
      await liveCredential.locator('.credential-cooldown').waitFor({state: 'visible'});
      if (!(await liveCredential.locator('.credential-cooldown').textContent()).includes('1 minute 30 seconds')) throw new Error(`${project.name}: a live refresh did not state the cooldown that just began`);
    }

    await page.evaluate(() => document.querySelector('[data-view="access"]').click());
    const modelsExample = await page.locator('#models-example-code').textContent();
    if (modelsExample !== `curl '${base}/v1/models' \\\n  -H 'Authorization: Bearer sk-your-yabane-key'`) throw new Error(`${project.name}: GET /v1/models example does not use the current console origin as a single runnable shell command`);
    const editGatewayKey = page.locator('.edit-gateway-key').first();
    if (await editGatewayKey.isVisible()) {
      const firstKey = await page.evaluate(async () => (await (await fetch('/admin/auth')).json()).api_keys[0]);
      const listedKey = page.locator('#gateway-keys .listed-key').first();
      if (await listedKey.locator('code').textContent() !== firstKey.prefix) throw new Error(`${project.name}: API key list does not use the masked key prefix`);
      if ((await listedKey.evaluate(element => element.outerHTML)).includes(firstKey.secret)) throw new Error(`${project.name}: API key list embeds the full secret in its markup`);
      if (!(await listedKey.locator('.icon-copy-key[aria-label="Copy API key"]').isVisible())) throw new Error(`${project.name}: masked API key cannot be copied`);
      await editGatewayKey.click();
      await assertDialog(page, '#edit-gateway-key-dialog', project.name);
      if (firstKey.expires_at === null && await page.locator('#edit-gateway-key-dialog [name="expires_at"]').inputValue() !== '') throw new Error(`${project.name}: editing a key without expiry fills the expiry field with the Unix epoch`);
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
    await page.locator('#help-agent').selectOption('claude');
    const claudePanel = page.locator('#help-dialog [data-help-panel="claude"]');
    await claudePanel.waitFor({state: 'visible'});
    const claudeGuide = (await page.locator('#help-claude-env').textContent()).split('\n');
    const guideKey = await page.locator('#help-api-key').textContent();
    const consoleOrigin = new URL(page.url()).origin;
    if (claudeGuide[0] !== `export ANTHROPIC_BASE_URL='${consoleOrigin}'`) throw new Error(`${project.name}: Claude Code guide points Claude Code at a base URL that already carries the /v1 path Claude Code appends itself`);
    if (claudeGuide[1] !== `export ANTHROPIC_AUTH_TOKEN='${guideKey}'`) throw new Error(`${project.name}: Claude Code guide does not put the Gateway key where Claude Code sends Authorization: Bearer`);
    if (claudeGuide[2] !== `claude --model '${await page.locator('#help-model-id').textContent()}'`) throw new Error(`${project.name}: Claude Code guide does not start Claude Code on the selected Yabane model`);
    if (claudeGuide.some(line => line.includes('ANTHROPIC_API_KEY'))) throw new Error(`${project.name}: Claude Code guide hands the Gateway key to the x-api-key header Yabane does not authenticate`);
    if (!(await claudePanel.innerText()).includes('x-api-key')) throw new Error(`${project.name}: Claude Code panel does not name the key header Yabane reads instead`);
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
    // Every Endpoint type the API publishes is offered by the console without a
    // console change, together with its own words and its fixed connection.
    const acmeChoice = page.locator('#api-type-choices label.choice').filter({has: page.locator('input[value="acme_plan"]')});
    if (await acmeChoice.count() !== 1 || !(await acmeChoice.innerText()).includes('Acme plan')) throw new Error(`${project.name}: an Extension-declared Endpoint type is missing from the console`);
    await page.locator('#api-type-choices input[value="acme_plan"]').check();
    if (await page.locator('#base-url').isVisible()) throw new Error(`${project.name}: an Endpoint type with a fixed connection exposes Base URL`);
    if (await page.locator('#api-key').isVisible()) throw new Error(`${project.name}: an Endpoint type that signs in accounts exposes API key input`);
    if (await page.locator('#initial-endpoint-id').inputValue() !== 'acme') throw new Error(`${project.name}: an Extension-declared Endpoint type does not suggest its own Endpoint ID`);
    if (await page.locator('#create-provider').textContent() !== 'Connect account') throw new Error(`${project.name}: an Endpoint type that signs in accounts does not offer to connect one`);
    await page.locator('#api-type-choices input[value="openai_codex"]').check();
    if (await page.locator('#initial-endpoint-id').inputValue() !== 'chatgpt') throw new Error(`${project.name}: first endpoint does not expose the API-type default ID`);
    if (await page.locator('#base-url').isVisible()) throw new Error(`${project.name}: subscription setup exposes Base URL`);
    if (!(await page.locator('#provider-form [name="socks5_proxy"]').isVisible())) throw new Error(`${project.name}: subscription setup hides SOCKS5 proxy`);
    await page.locator('#provider-form [name="socks5_proxy"]').fill('socks5h://127.0.0.1:1080');
    if (await page.locator('#api-key').isVisible()) throw new Error(`${project.name}: subscription setup exposes API key input`);
    if (await page.locator('#create-provider').textContent() !== 'Connect account') throw new Error(`${project.name}: sign-in Endpoint setup has the wrong primary action`);
    await page.locator('#create-provider').click();
    await page.locator('#endpoint-sign-in-dialog').waitFor({state: 'visible'});
    if (!(await page.locator('#sign-in-device').isVisible()) || await page.locator('#sign-in-browser').isVisible()) throw new Error(`${project.name}: a signed-in Endpoint type does not default to device-code sign-in`);
    if (await page.locator('#sign-in-code').textContent() !== 'ABCD-EFGH') throw new Error(`${project.name}: device-code sign-in does not show its one-time code`);
    await page.locator('#sign-in-use-oauth').click();
    await page.locator('#sign-in-browser').waitFor({state: 'visible'});
    if (await page.locator('#sign-in-device').isVisible()) throw new Error(`${project.name}: browser sign-in did not replace the device-code instructions`);
    const oauthWarning = page.locator('#sign-in-browser .oauth-expected-warning');
    if (!(await oauthWarning.isVisible()) || !(await oauthWarning.getByText('A localhost error page is expected', {exact: true}).isVisible())) throw new Error(`${project.name}: browser OAuth does not prominently prepare users for the localhost error page`);
    const warningText = await oauthWarning.textContent();
    if (!warningText.includes('This does not mean OAuth failed') || !warningText.includes('localhost:1455')) throw new Error(`${project.name}: browser OAuth warning does not explain that the localhost failure is intentional`);
    const oauthSteps = await page.locator('#sign-in-browser .oauth-steps li strong').allTextContents();
    if (oauthSteps.join('|') !== 'Sign in with the Provider|Expect the localhost error|Return and paste once') throw new Error(`${project.name}: browser sign-in steps do not describe the expected failure before launch`);
    if (await page.locator('#sign-in-browser-link').textContent() !== 'I understand — open sign-in page') throw new Error(`${project.name}: browser sign-in launch does not require an explicit acknowledgement`);
    if (await page.locator('#sign-in-browser-link').getAttribute('href') !== 'https://auth.openai.com/oauth/authorize?state=browser-state') throw new Error(`${project.name}: browser sign-in does not expose the Extension authorization URL`);
    await page.locator('#sign-in-callback').fill('http://localhost:1455/auth/callback?code=oauth-code&state=browser-state');
    await page.locator('#sign-in-browser [type="submit"]').click();
    await page.locator('#endpoint-sign-in-dialog').waitFor({state: 'hidden'});
    await page.evaluate(() => document.querySelector('[data-view="providers"]').click());
    const subscriptionProvider = page.locator('#providers .provider-list-item').filter({has: page.locator('code', {hasText: 'ui-subscription/model-id'})});
    await subscriptionProvider.click();
    const credential = page.locator('.subscription-credential');
    await credential.waitFor({state: 'visible'});
    // Adding an account to an Endpoint that already exists works from the card
    // itself, and addresses sign-in by the Endpoint type the Endpoint reports.
    deviceCodeBodies.length = 0;
    await credential.locator('.connect-account').click();
    await page.locator('#endpoint-sign-in-dialog').waitFor({state: 'visible'});
    if (await page.locator('#sign-in-code').textContent() !== 'ABCD-EFGH') throw new Error(`${project.name}: connecting another account does not show a device code`);
    const connectBody = JSON.stringify(deviceCodeBodies.at(-1));
    if (connectBody !== JSON.stringify({endpoint_type: 'openai_codex', provider_id: 'ui-subscription', endpoint_id: 'chatgpt'})) throw new Error(`${project.name}: connecting another account does not address the Endpoint type and its Endpoint (${connectBody})`);
    await page.locator('#endpoint-sign-in-dialog .close-sign-in').first().click();
    await page.locator('#endpoint-sign-in-dialog').waitFor({state: 'hidden'});
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
    if (await credential.getByText('Automatic renewal enabled', {exact: true}).count() !== 1) throw new Error(`${project.name}: a connected account does not present automatic renewal as its primary state`);
    if (!(await credential.innerText()).includes('the Provider revokes access')) throw new Error(`${project.name}: automatic renewal names a vendor instead of the Provider`);
    if (await credential.getByText(/^Token expires /).count()) throw new Error(`${project.name}: OpenAI subscription still presents access-token expiry as its primary state`);
    const details = credential.locator('.credential-details');
    if (await details.count() !== 1 || await details.getAttribute('open') !== null) throw new Error(`${project.name}: OpenAI access-token details are missing or expanded by default`);
    await details.locator('summary').click();
    const detailText = await details.locator('p').textContent();
    if (!detailText.includes('current access token') || !detailText.includes('next request')) throw new Error(`${project.name}: elapsed OpenAI access-token detail does not explain lazy renewal`);
    const cooldown = credential.locator('.credential-cooldown');
    if (await cooldown.count() !== 1 || !(await cooldown.textContent()).includes('Cooling down')) throw new Error(`${project.name}: a cooling credential does not state that it is out of selection`);
    if (await cooldown.locator('.clear-cooldown').count() !== 1) throw new Error(`${project.name}: a cooling credential offers no explicit action that returns it to selection`);
    const endpointFacts = await page.locator('.endpoint-facts').first().textContent();
    if (!endpointFacts.includes('Rate-limit cooldown') || !endpointFacts.includes('Provider delay first')) throw new Error(`${project.name}: Endpoint facts do not state the configured rate-limit cooldown policy and its delay source (${endpointFacts})`);
    // The pool is stated as a mechanism rather than a policy field standing next
    // to a percentage: how traffic rotates, what a Provider rate limit does to
    // that rotation, and what is true right now.
    if (await page.locator('.endpoint-pool').count() !== 1) throw new Error(`${project.name}: the Endpoint does not state its identity pool exactly once`);
    const poolText = await page.locator('.endpoint-pool').textContent();
    if (!poolText.includes('leaves the pool for the delay the Provider reports') || !poolText.includes('never longer than 1 hour')) throw new Error(`${project.name}: the identity pool does not state what a Provider rate limit does to the rotation (${poolText})`);
    if (!poolText.includes('is out until') || !poolText.includes('carries every request')) throw new Error(`${project.name}: the identity pool does not state which identity is out now and who carries the traffic instead (${poolText})`);
    // A policy whose length can come from the Provider may never fire, so the
    // Endpoint states what it has actually done instead of only what it allows,
    // including whether the Provider's own number was capped by the ceiling.
    if (!poolText.includes('selected a cooldown duration 3 times') || !poolText.includes('took an identity out for 1 hour') || !poolText.includes('The Provider asked for 2 hours') || !poolText.includes('so this cooldown was capped') || !poolText.includes('reported no usable delay')) throw new Error(`${project.name}: the identity pool does not report what the cooldown policy has observed (${poolText})`);
    const identityShares = await credential.locator('.traffic-share').allTextContents();
    if (!identityShares.some(text => text.includes('0%') && text.includes('while cooling down'))) throw new Error(`${project.name}: a cooling identity keeps presenting its configured share as what it carries (${identityShares.join(' | ')})`);
    if (!identityShares.some(text => text.includes('100%') && text.includes('normally 50%'))) throw new Error(`${project.name}: an identity carrying the whole pool does not state both the effective and the configured share (${identityShares.join(' | ')})`);
    // Setting the split and setting the policy that changes what the split means
    // are one mechanism, so each screen states the other half of it.
    await credential.getByRole('button', {name: 'Distribute traffic'}).click();
    await assertDialog(page, '#traffic-dialog', project.name);
    const identityStates = await page.locator('#traffic-rows .identity-state').allTextContents();
    if (!identityStates.some(text => text.includes('cooling down, resumes in')) || !identityStates.some(text => text.includes('healthy'))) throw new Error(`${project.name}: the traffic distribution dialog does not mark each identity's current state (${identityStates.join(' | ')})`);
    // Both identities are in one group, so the dialog states that single order and
    // names the way out of it instead of presenting a group that can be chosen.
    const singleGroupHeadings = await page.locator('.traffic-group-head strong').allTextContents();
    if (singleGroupHeadings.join('|') !== 'Preferred group' || await page.locator('.traffic-move').count()) throw new Error(`${project.name}: a one-group distribution presents groups to choose (${singleGroupHeadings.join(' | ')})`);
    if ((await page.locator('#traffic-rows input[type="number"]').evaluateAll(inputs => inputs.map(input => input.value))).join(',') !== '50,50') throw new Error(`${project.name}: the identities sharing one group do not state their shares`);
    if (!(await page.locator('#traffic-order').textContent()).includes('Add a standby group to keep one back')) throw new Error(`${project.name}: the traffic dialog does not say how a standby group is created`);
    // What the percentages mean is one step away, so the order the dialog is about
    // is readable without leaving it.
    await page.locator('#traffic-dialog .traffic-details summary').click();
    const consequence = await page.locator('#traffic-consequence').textContent();
    if (!consequence.includes('split exactly as configured') || !consequence.includes('never longer than 1 hour')) throw new Error(`${project.name}: the traffic distribution dialog does not state what a rate limit does to the configured percentages (${consequence})`);
    if (!(await page.locator('#traffic-set-cooldown').isHidden())) throw new Error(`${project.name}: the traffic distribution dialog offers a cooldown shortcut while a cooldown policy is already configured`);
    await page.locator('#traffic-dialog .close-traffic').first().click();
    await page.locator('#traffic-dialog').waitFor({state: 'hidden'});
    // The policy states its consequence before saving, including the parts its two
    // controls cannot: that the request which hit the limit still receives the
    // Provider's own answer, and that Disabled changes nothing at all.
    await page.locator('.endpoint-edit').first().click();
    await assertDialog(page, '#endpoint-dialog', project.name);
    // Connection settings and the rate-limit policy are separate tabs, so neither
    // half of the dialog has to be scrolled past to reach the other.
    const connectionTab = page.locator('#endpoint-form [data-endpoint-tab="connection"]');
    const rateLimitsTab = page.locator('#endpoint-form [data-endpoint-tab="ratelimits"]');
    if (await page.locator('#endpoint-form [data-endpoint-tab]').count() !== 2) throw new Error(`${project.name}: the Endpoint dialog does not present connection settings and rate limits as two tabs`);
    if (await connectionTab.getAttribute('aria-selected') !== 'true' || !(await page.locator('#endpoint-panel-connection').isVisible())) throw new Error(`${project.name}: the Endpoint dialog does not open on the connection settings`);
    if (await rateLimitsTab.getAttribute('aria-selected') !== 'false' || await page.locator('#endpoint-panel-ratelimits').isVisible()) throw new Error(`${project.name}: the Endpoint dialog opens with the rate-limit policy already shown`);
    if (!await page.locator('#endpoint-panel-connection [name="id"]').isVisible()) throw new Error(`${project.name}: the connection tab does not hold the connection settings`);
    if (project.width >= 1024 && await page.locator('#endpoint-dialog').evaluate(element => element.scrollHeight - element.clientHeight > 1)) throw new Error(`${project.name}: the connection settings alone do not fit the Endpoint dialog without scrolling`);
    await rateLimitsTab.click();
    if (await rateLimitsTab.getAttribute('aria-selected') !== 'true' || !(await page.locator('#endpoint-panel-ratelimits').isVisible()) || await page.locator('#endpoint-panel-connection').isVisible()) throw new Error(`${project.name}: choosing the rate-limit tab does not bring that policy forward`);
    if (!await page.locator('#endpoint-panel-ratelimits [name="cooldown_duration"]').isVisible()) throw new Error(`${project.name}: the rate-limit tab does not hold the cooldown policy`);
    if (project.width >= 1024 && await page.locator('#endpoint-dialog').evaluate(element => element.scrollHeight - element.clientHeight > 1)) throw new Error(`${project.name}: the rate-limit policy alone does not fit the Endpoint dialog without scrolling`);
    const cooldownPreview = await page.locator('#cooldown-preview').textContent();
    if (!cooldownPreview.includes('429 is returned unchanged') || !cooldownPreview.includes('No request is retried') || !cooldownPreview.includes('pinned identities')) throw new Error(`${project.name}: the illustration implies retry or bypass of a pinned identity (${cooldownPreview})`);
    const enableCooldown = page.locator('.cooldown-enable .switch-label');
    const enabledInput = page.getByRole('switch', {name: 'Cooldown after a 429'});
    const fixedSource = page.getByLabel('Set a fixed duration', {exact: true});
    const providerSource = page.getByLabel('Use the Provider’s wait time', {exact: true});
    const durationSelect = page.locator('#endpoint-form [name="cooldown_duration"]');
    const missingSelect = page.getByLabel('If no usable wait time is provided', {exact: true});
    if (!await enabledInput.isChecked() || !await providerSource.isChecked() || await missingSelect.inputValue() !== 'fallback') throw new Error(`${project.name}: the preferred-Provider policy did not load into independent timing choices`);
    if (await page.locator('.cooldown-policy input').count() !== 2 || await page.locator('#cooldown-flow svg').count() !== 1 || await durationSelect.locator('option[value="0"]').count()) throw new Error(`${project.name}: the editor mixes cooldown enablement with its timing policy`);
    const selectedMode = () => page.locator('#endpoint-form [name="cooldown_mode"]').inputValue();
    if (await selectedMode() !== 'prefer_provider') throw new Error(`${project.name}: stored cooldown mode was lost`);
    const flowState = () => page.locator('#cooldown-flow').getAttribute('data-state');
    if (await flowState() !== 'bypass' || !(await page.locator('#cooldown-flow-caption').textContent()).includes('rejoins automatically')) throw new Error(`${project.name}: the illustration does not explain later requests and automatic return`);
    if (!(await page.locator('.flow-return').isVisible()) || !(await page.locator('.flow-others').isVisible())) throw new Error(`${project.name}: the illustrated alternate path or return is missing`);
    if (!(await page.locator('#cooldown-flow header').textContent()).includes('Illustration · later requests')) throw new Error(`${project.name}: the cooldown illustration could be mistaken for live health`);
    const durationHelp = await page.locator('#cooldown-duration-help').textContent();
    if (!durationHelp.includes('Retry-After in whole seconds') || !durationHelp.includes('capped') || !(await missingSelect.locator('option:checked').textContent()).includes('1 hour')) throw new Error(`${project.name}: the Provider wait does not explain its cap and missing-wait duration`);
    await missingSelect.selectOption('skip');
    if (await selectedMode() !== 'provider_only' || await page.locator('#cooldown-duration-label').textContent() !== 'Maximum cooldown') throw new Error(`${project.name}: Provider-only policy does not separate its ceiling from skipping a missing delay`);
    await fixedSource.check();
    if (await selectedMode() !== 'fixed' || await missingSelect.isVisible() || !(await page.locator('#cooldown-duration-help').textContent()).includes('ignoring the Provider') || await page.locator('#cooldown-duration-label').textContent() !== 'Fixed cooldown') throw new Error(`${project.name}: fixed timing retains an irrelevant fallback choice or labels a maximum`);
    await providerSource.check();
    if (await missingSelect.inputValue() !== 'skip') throw new Error(`${project.name}: changing the timing source destroys the missing-wait choice`);
    await durationSelect.selectOption('300');
    if (!(await missingSelect.locator('option[value="fallback"]').textContent()).includes('5 minutes')) throw new Error(`${project.name}: fallback duration does not follow the chosen maximum`);
    await enableCooldown.click();
    if (await flowState() !== 'shared' || await page.locator('.flow-clock').isVisible() || await page.locator('.flow-return').isVisible()) throw new Error(`${project.name}: disabled cooldown still draws a paused identity`);
    if (!(await page.locator('#cooldown-flow-caption').textContent()).includes('keeps its share') || !(await page.locator('#cooldown-enable-help').textContent()).includes('Cooldown is off')) throw new Error(`${project.name}: disabled cooldown does not explain unchanged selection`);
    if (await durationSelect.isDisabled() || await providerSource.isDisabled() || await missingSelect.isDisabled()) throw new Error(`${project.name}: cooldown settings cannot be prepared while off`);
    await enableCooldown.click();
    if (await durationSelect.inputValue() !== '300' || await selectedMode() !== 'provider_only' || await missingSelect.inputValue() !== 'skip') throw new Error(`${project.name}: toggling cooldown loses unsaved choices`);
    // Exercise real submit payloads as well as the form. A rejected fixture save
    // also verifies that the policy tab is revealed when an API error belongs to it.
    const policyUrl = `${base}/admin/providers/ui-subscription/endpoints/chatgpt`;
    await page.route(policyUrl, route => route.fulfill({status: 400, contentType: 'application/json', body: JSON.stringify({error: {message: 'Rate-limit cooldown fixture rejection'}})}));
    const assertPolicySave = async (seconds, mode) => {
      await connectionTab.click();
      const requestPromise = page.waitForRequest(request => request.url() === policyUrl && request.method() === 'PATCH');
      await page.locator('#endpoint-form button[type="submit"]').click();
      const payload = (await requestPromise).postDataJSON().rate_limit_cooldown;
      await page.locator('#endpoint-panel-ratelimits').waitFor({state: 'visible'});
      await page.locator('#endpoint-error').getByText('Rate-limit cooldown fixture rejection', {exact: true}).waitFor();
      if (payload.seconds !== seconds || payload.mode !== mode || await rateLimitsTab.getAttribute('aria-selected') !== 'true') throw new Error(`${project.name}: cooldown save or error tab mismatch (${JSON.stringify(payload)})`);
    };
    await assertPolicySave(300, 'provider_only');
    await missingSelect.selectOption('fallback');
    await assertPolicySave(300, 'prefer_provider');
    await fixedSource.check();
    await assertPolicySave(300, 'fixed');
    await enableCooldown.click();
    await providerSource.check();
    await missingSelect.selectOption('skip');
    await assertPolicySave(0, 'provider_only');
    // Every stored mode, including a disabled policy and non-shortcut duration,
    // must survive opening and saving without reinterpretation.
    for (const mode of ['fixed', 'prefer_provider', 'provider_only']) {
      for (const seconds of [137, 0]) {
        await page.locator('#endpoint-dialog .close-endpoint').first().click();
        await page.evaluate(policy => { providers.find(provider => provider.id === 'ui-subscription').endpoints[0].rate_limit_cooldown = policy; }, {seconds, mode});
        await page.locator('.endpoint-edit').first().click();
        await rateLimitsTab.click();
        if (await enabledInput.isChecked() !== (seconds > 0) || await selectedMode() !== mode || await fixedSource.isChecked() !== (mode === 'fixed')) throw new Error(`${project.name}: stored policy changed on open (${seconds}, ${mode})`);
        if (mode !== 'fixed' && await missingSelect.inputValue() !== (mode === 'provider_only' ? 'skip' : 'fallback')) throw new Error(`${project.name}: stored missing-delay behavior was lost`);
        if (seconds && await durationSelect.inputValue() !== String(seconds)) throw new Error(`${project.name}: custom duration was normalized on open`);
        await assertPolicySave(seconds, mode);
      }
    }
    await page.unroute(policyUrl);
    await connectionTab.click();
    const storedForm = await page.locator('#endpoint-form').evaluate(form => { const data = new FormData(form); return [data.get('cooldown_seconds'), data.get('cooldown_mode')]; });
    if (storedForm.join(',') !== '0,provider_only') throw new Error(`${project.name}: hidden cooldown tab loses the disabled policy's chosen mode`);
    if (await connectionTab.getAttribute('aria-selected') !== 'true' || !(await page.locator('#endpoint-panel-connection').isVisible())) throw new Error(`${project.name}: the connection tab cannot be returned to`);
    if (await page.locator('.cooldown-policy').isVisible()) throw new Error(`${project.name}: the connection tab still shows the rate-limit policy`);
    await page.locator('#endpoint-dialog .close-endpoint').first().click();
    await page.locator('#endpoint-dialog').waitFor({state: 'hidden'});
    await page.locator('#back-to-providers').click();
    await page.locator('#provider-list-page').waitFor({state: 'visible'});
    // An Endpoint that only ever sends as its single identity is not described as
    // a pool, because there is no split to explain and no second identity to
    // carry the traffic while the first is out.
    const plainProvider = page.locator('#providers .provider-list-item').filter({has: page.locator('code', {hasText: 'ui-plain/model-id'})});
    if (await plainProvider.count()) {
      await plainProvider.click();
      const plainFacts = await page.locator('.endpoint-facts').first().textContent();
      if (plainFacts.includes('Rate-limit cooldown')) throw new Error(`${project.name}: Endpoint facts state a rate-limit policy that is not configured (${plainFacts})`);
      if (await page.locator('.endpoint-pool').count()) throw new Error(`${project.name}: an Endpoint with one identity and no cooldown policy is presented as an identity pool`);
      await page.locator('.endpoint-edit').first().click();
      await assertDialog(page, '#endpoint-dialog', project.name);
      if (await page.locator('#endpoint-form [data-endpoint-tab="connection"]').getAttribute('aria-selected') !== 'true') throw new Error(`${project.name}: reopening the Endpoint dialog does not return to the connection settings`);
      await assertEndpointFieldSpacing(page, project);
      await page.locator('#endpoint-form [data-endpoint-tab="ratelimits"]').click();
      if (await enabledInput.isChecked() || await page.locator('#endpoint-form [name="cooldown_seconds"]').inputValue() !== '0') throw new Error(`${project.name}: an Endpoint without a cooldown policy does not open switched off`);
      if (!(await page.locator('#cooldown-enable-help').textContent()).includes('only when enabled')) throw new Error(`${project.name}: a disabled cooldown does not explain inactive settings`);
      await enableCooldown.click();
      if (await page.locator('#cooldown-flow').getAttribute('data-state') !== 'solo' || await page.locator('.flow-others').isVisible() || await page.locator('.flow-return').isVisible()) throw new Error(`${project.name}: the sole identity is incorrectly drawn as bypassable`);
      if (!(await page.locator('#cooldown-flow-caption').textContent()).includes('Requests still go out')) throw new Error(`${project.name}: the sole identity fallback is unexplained`);
      // A zero-share identity must not be counted as an alternate destination.
      await page.evaluate(() => { const endpoint = providers.find(provider => provider.id === 'ui-plain').endpoints[0]; endpoint.credentials.push({...endpoint.credentials[0], id: 'zero-share', weight: 0}); });
      await page.locator('#endpoint-dialog .close-endpoint').first().click();
      await page.locator('.endpoint-edit').first().click();
      await page.locator('#endpoint-tab-ratelimits').click();
      await enableCooldown.click();
      if (await page.locator('#cooldown-flow').getAttribute('data-state') !== 'solo') throw new Error(`${project.name}: a zero-share identity is drawn as taking over traffic`);
      await page.locator('#endpoint-tab-connection').click();
      const plainEndpointId = await page.locator('#endpoint-form [name="id"]').inputValue();
      await page.locator('#endpoint-form [name="id"]').fill('');
      await page.locator('#endpoint-tab-ratelimits').click();
      await page.locator('#endpoint-form button[type="submit"]').click();
      if (await page.locator('#endpoint-tab-connection').getAttribute('aria-selected') !== 'true' || !(await page.locator('#endpoint-form [name="id"]').evaluate(input => input === document.activeElement))) throw new Error(`${project.name}: validation leaves a required field hidden on the other tab`);
      await page.locator('#endpoint-form [name="id"]').fill(plainEndpointId);
      await page.evaluate(() => { providers.find(provider => provider.id === 'ui-plain').endpoints[0].credentials.forEach(key => { key.weight = 0; }); });
      await page.locator('#endpoint-dialog .close-endpoint').first().click();
      await page.locator('.endpoint-edit').first().click();
      await page.locator('#endpoint-tab-ratelimits').click();
      if (await page.locator('#cooldown-flow').getAttribute('data-state') !== 'empty' || !(await page.locator('#cooldown-flow-caption').textContent()).includes('Requests fail until')) throw new Error(`${project.name}: an Endpoint with no eligible identity is drawn as usable`);
      await page.locator('#endpoint-dialog .close-endpoint').first().click();
      await page.locator('#endpoint-dialog').waitFor({state: 'hidden'});
      await page.locator('#back-to-providers').click();
      await page.locator('#provider-list-page').waitFor({state: 'visible'});
    }
    // A cooldown duration that the console does not offer as a shortcut keeps its
    // exact configured value instead of being rewritten to a nearby option.
    const keylessProvider = page.locator('#providers .provider-list-item').filter({has: page.locator('code', {hasText: 'ui-keyless/model-id'})});
    if (await keylessProvider.count()) {
      await keylessProvider.click();
      await page.locator('.endpoint-edit').first().click();
      await assertDialog(page, '#endpoint-dialog', project.name);
      const cooldownSelect = page.locator('#endpoint-form [name="cooldown_duration"]');
      if (await cooldownSelect.inputValue() !== '120') throw new Error(`${project.name}: editing an Endpoint silently replaces a cooldown duration the console does not offer (${await cooldownSelect.inputValue()})`);
      if (!(await cooldownSelect.locator('option:checked').textContent()).includes('2 minutes')) throw new Error(`${project.name}: a custom cooldown duration is not labeled in readable units`);
      if (await page.locator('#endpoint-form [name="requires_credential"]').isChecked()) throw new Error(`${project.name}: keyless Endpoint does not keep its credential requirement unchecked`);
      await page.locator('#endpoint-tab-ratelimits').click();
      if (await page.locator('#cooldown-flow').getAttribute('data-state') !== 'direct' || await page.locator('.flow-primary').isVisible() || !(await page.locator('#cooldown-flow-caption').textContent()).includes('without credentials')) throw new Error(`${project.name}: keyless Endpoint invents an identity to pause`);
      await page.locator('#endpoint-dialog .close-endpoint').first().click();
      await page.locator('#back-to-providers').click();
      await page.locator('#provider-list-page').waitFor({state: 'visible'});
    }
    // Two accounts can be ordered instead of shared: the pool names the group that
    // carries traffic, and each identity row says which group it belongs to.
    const tieredProvider = page.locator('#providers .provider-list-item').filter({has: page.locator('code', {hasText: 'ui-tiered/model-id'})});
    if (await tieredProvider.count()) {
      await tieredProvider.click();
      const tieredCard = page.locator('.endpoint-card').first();
      await tieredCard.locator('.endpoint-pool').waitFor({state: 'visible'});
      const tieredPool = await tieredCard.locator('.endpoint-pool').textContent();
      if (!tieredPool.includes('Traffic always uses Priority 1 first (Primary account)')) throw new Error(`${project.name}: a tiered pool does not state which group carries traffic (${tieredPool})`);
      if (!tieredPool.includes('Priority 2 (Standby account) only carries it while every identity in the group above it is cooling down')) throw new Error(`${project.name}: a standby group is not stated as a standby (${tieredPool})`);
      if (!tieredPool.includes('took an identity out for 1 hour') || !tieredPool.includes('which was used unchanged')) throw new Error(`${project.name}: a tiered pool does not state that the Provider's own delay armed the last cooldown (${tieredPool})`);
      // The list itself says the numbers are read per group, so a standby row's
      // 0% is not mistaken for a sharing mistake.
      const tierCopy = await tieredCard.locator('.endpoint-keys-head p').first().textContent();
      if (!tierCopy.includes('Each priority group splits its own traffic between the credentials in it')) throw new Error(`${project.name}: the credential list describes one split for a tiered Endpoint (${tierCopy})`);
      const tiers = await tieredCard.locator('.credential-tier').allTextContents();
      if (tiers.join('|') !== 'Priority 1 · first|Priority 2 · standby') throw new Error(`${project.name}: identity rows do not name their priority group (${tiers.join(' | ')})`);
      const tierShares = await tieredCard.locator('.traffic-share').allTextContents();
      if (!tierShares[0].includes('100%') || !tierShares[0].includes('of Priority 1 traffic')) throw new Error(`${project.name}: the group that carries traffic does not state its share (${tierShares.join(' | ')})`);
      if (!tierShares[1].includes('0%') || !tierShares[1].includes('only while Priority 1 is out')) throw new Error(`${project.name}: a standby identity presents its configured share as traffic it carries (${tierShares.join(' | ')})`);
      // The dialog that sets the split is also where a group is chosen, and every
      // group totals 100 on its own because each one describes what happens while
      // it is the group carrying the traffic.
      await tieredCard.getByRole('button', {name: 'Distribute traffic'}).click();
      await assertDialog(page, '#traffic-dialog', project.name);
      await page.locator('#traffic-rows').waitFor({state: 'visible'});
      const tierDescription = await page.locator('#traffic-description').textContent();
      if (!tierDescription.includes('takes inside its own group')) throw new Error(`${project.name}: the traffic dialog describes one split for a tiered Endpoint (${tierDescription})`);
      if (!(await page.locator('#traffic-order').textContent()).includes('Yabane uses the preferred group first')) throw new Error(`${project.name}: the traffic dialog does not state which group carries traffic first`);
      // A group is named by the order it carries traffic in while it keeps the
      // number the pool summary and the API call it by.
      const groupHeadings = await page.locator('.traffic-group-head strong').allTextContents();
      if (groupHeadings.join('|') !== 'Preferred group|Standby group') throw new Error(`${project.name}: the traffic dialog does not name its groups by the order they carry traffic in (${groupHeadings.join(' | ')})`);
      const groupPriorities = await page.locator('.traffic-group-priority').allTextContents();
      if (groupPriorities.join('|') !== 'Priority 1|Priority 2') throw new Error(`${project.name}: a group does not keep its priority number next to its name (${groupPriorities.join(' | ')})`);
      const groupNotes = await page.locator('.traffic-group-head small').allTextContents();
      if (!groupNotes[1].includes('Only used while every identity in Priority 1 is cooling down')) throw new Error(`${project.name}: a standby group does not say when it takes over (${groupNotes.join(' | ')})`);
      // A group holding one identity carries that group's whole traffic, so it is
      // stated as what it means instead of being offered as a percentage field.
      const noteTexts = await page.locator('.traffic-group-note').allTextContents();
      if (await page.locator('#traffic-rows input[type="number"]').count() || noteTexts.length !== 2 || !noteTexts[0].includes('only identity in this group')) throw new Error(`${project.name}: a single-identity group asks for a percentage that only ever reads 100 (${noteTexts.join(' | ')})`);
      // Which identity sits in which group is visible, and a move names the group it
      // would move to instead of hiding that choice behind a number.
      const groupMembers = () => page.locator('.traffic-group').evaluateAll(groups => groups.map(group => [...group.querySelectorAll('.traffic-identity strong')].map(name => name.textContent)));
      if (JSON.stringify(await groupMembers()) !== '[["Primary account"],["Standby account"]]') throw new Error(`${project.name}: the traffic dialog does not show which identity is in which group (${JSON.stringify(await groupMembers())})`);
      const moveOptions = await page.locator('.traffic-move').first().locator('option').allTextContents();
      if (moveOptions.join('|') !== 'Move to group…|Standby group · Priority 2') throw new Error(`${project.name}: moving an identity does not name the group it would move to (${moveOptions.join(' | ')})`);
      const standbyMoveOptions = await page.locator('.traffic-move').nth(1).locator('option').allTextContents();
      if (standbyMoveOptions.join('|') !== 'Move to group…|Preferred group · Priority 1') throw new Error(`${project.name}: a standby identity cannot be moved back into the preferred group (${standbyMoveOptions.join(' | ')})`);
      // The order of the groups is what the dialog decides, so the group itself
      // moves: swapping two groups takes every identity in them with it, and the
      // positions stay numbered from the top.
      await page.locator('.traffic-group').first().locator('[data-action="down"]').click();
      if (JSON.stringify(await groupMembers()) !== '[["Standby account"],["Primary account"]]') throw new Error(`${project.name}: the group controls do not change which group carries traffic first (${JSON.stringify(await groupMembers())})`);
      if ((await page.locator('.traffic-group-priority').allTextContents()).join('|') !== 'Priority 1|Priority 2') throw new Error(`${project.name}: moving a group renames the position of another group`);
      await page.locator('.traffic-group').nth(1).locator('[data-action="up"]').click();
      // Moving an identity between groups shares both groups out again, so a group
      // never has to be repaired by hand after a move.
      await page.locator('.traffic-move').nth(1).selectOption('1');
      if (await page.locator('.traffic-group').count() !== 1 || (await page.locator('#traffic-rows input[type="number"]').evaluateAll(inputs => inputs.map(input => input.value))).join(',') !== '50,50') throw new Error(`${project.name}: moving an identity into a group does not share that group out again`);
      if ((await page.locator('.traffic-group-total').allTextContents()).join('|') !== '100%') throw new Error(`${project.name}: a group is not totalled on its own`);
      // Creating a group is its own action, so the list of groups stays a list of
      // groups, and a position holding no identity yet says so instead of saving as
      // a group that carries nothing.
      await page.locator('#traffic-add-group').click();
      if (await page.locator('.traffic-group').count() !== 2 || !(await page.locator('.traffic-group-empty').textContent()).includes('No identities in this group yet')) throw new Error(`${project.name}: adding a standby group does not create a position to move an identity into`);
      if (!await page.locator('#save-traffic').isDisabled() || !(await page.locator('#traffic-error').textContent()).includes('Standby group has no identities')) throw new Error(`${project.name}: a group holding no identity can be saved as a distribution`);
      await page.locator('.traffic-move').first().selectOption('2');
      const splitNotes = await page.locator('.traffic-group-head small').allTextContents();
      if (JSON.stringify(await groupMembers()) !== '[["Standby account"],["Primary account"]]' || await page.locator('#traffic-rows input[type="number"]').count() !== 0) throw new Error(`${project.name}: moving an identity into a new group does not total each group at 100 (${splitNotes.join(' | ')})`);
      // A group change is saved as the identity property it is, before the
      // percentages that are read inside those groups, and a rejected distribution
      // names the group that still has to be adjusted.
      const savedPriorities = [];
      let rejectTraffic = true;
      await page.route(`${base}/admin/providers/ui-tiered/endpoints/pool/credentials/*`, async route => {
        savedPriorities.push(route.request().postDataJSON());
        await route.fulfill({status: 204, body: ''});
      });
      await page.route(`${base}/admin/providers/ui-tiered/endpoints/pool/traffic`, async route => {
        if (!rejectTraffic) return route.fulfill({status: 204, body: ''});
        await route.fulfill({status: 400, contentType: 'application/json', body: JSON.stringify({error: {message: 'Priority 1 traffic percentages must total 100 (currently 60)'}})});
      });
      await page.locator('#save-traffic').click();
      await page.locator('#traffic-error').getByText('Priority 1 traffic percentages must total 100 (currently 60)').waitFor();
      if (savedPriorities.length !== 2 || savedPriorities[0].priority !== 2 || savedPriorities[1].priority !== 1) throw new Error(`${project.name}: a group change is not saved as the identity's own priority (${JSON.stringify(savedPriorities)})`);
      rejectTraffic = false;
      await page.locator('#save-traffic').click();
      await page.locator('#traffic-dialog').waitFor({state: 'hidden'});
      await page.unroute(`${base}/admin/providers/ui-tiered/endpoints/pool/credentials/*`);
      await page.unroute(`${base}/admin/providers/ui-tiered/endpoints/pool/traffic`);
      // A group number is a position in the console, not a fact about the file: a
      // distribution whose stored numbers drifted apart (or lost their lowest one)
      // must still show Priority 1 first, must still be able to create a group below
      // it, and must write those positions back when it is saved. The drifted numbers
      // arrive with a Provider read the test awaits, so the reload that saving performs
      // cannot overwrite them the way an in-memory edit could.
      tieredPriorityDrift = [4, 9];
      await page.evaluate(() => loadProviders());
      const driftedPool = await page.locator('.endpoint-card').first().locator('.endpoint-pool').textContent();
      if (!driftedPool.includes('Traffic always uses Priority 1 first (Primary account)')) throw new Error(`${project.name}: a stored group number changes which group the pool calls first (${driftedPool})`);
      await page.locator('.endpoint-card').first().getByRole('button', {name: 'Distribute traffic'}).click();
      await page.locator('#traffic-dialog').waitFor({state: 'visible'});
      const driftedHeadings = await page.locator('.traffic-group-head strong').allTextContents();
      if (driftedHeadings.join('|') !== 'Preferred group|Standby group') throw new Error(`${project.name}: stored group numbers are presented as group names (${driftedHeadings.join(' | ')})`);
      if ((await page.locator('.traffic-group-priority').allTextContents()).join('|') !== 'Priority 1|Priority 2') throw new Error(`${project.name}: stored group numbers are presented as positions (${(await page.locator('.traffic-group-priority').allTextContents()).join(' | ')})`);
      const driftedOptions = await page.locator('.traffic-move').first().locator('option').allTextContents();
      if (driftedOptions.join('|') !== 'Move to group…|Standby group · Priority 2') throw new Error(`${project.name}: a drifted numbering removes a group that can still be chosen (${driftedOptions.join(' | ')})`);
      await page.locator('#traffic-add-group').click();
      if ((await page.locator('.traffic-group-priority').allTextContents()).join('|') !== 'Priority 1|Priority 2|Priority 3') throw new Error(`${project.name}: a drifted numbering leaves no group to add below the ones shown`);
      await page.locator('.traffic-group').nth(2).locator('.traffic-group-remove').click();
      if (await page.locator('.traffic-group').count() !== 2 || await page.locator('#save-traffic').isDisabled()) throw new Error(`${project.name}: removing the group an administrator just added leaves the distribution unsaveable`);
      const normalised = [];
      await page.route(`${base}/admin/providers/ui-tiered/endpoints/pool/credentials/*`, async route => {
        normalised.push(route.request().postDataJSON());
        await route.fulfill({status: 204, body: ''});
      });
      await page.route(`${base}/admin/providers/ui-tiered/endpoints/pool/traffic`, async route => route.fulfill({status: 204, body: ''}));
      await page.locator('#save-traffic').click();
      await page.locator('#traffic-dialog').waitFor({state: 'hidden'});
      if (JSON.stringify(normalised) !== '[{"priority":1},{"priority":2}]') throw new Error(`${project.name}: saving a distribution does not write the group positions back (${JSON.stringify(normalised)})`);
      await page.unroute(`${base}/admin/providers/ui-tiered/endpoints/pool/credentials/*`);
      await page.unroute(`${base}/admin/providers/ui-tiered/endpoints/pool/traffic`);
      // A standby group that nothing can ever hand traffic to is a configuration to
      // see, not a plan: without a cooldown policy no identity leaves its group.
      const openCard = page.locator('.endpoint-card').nth(1);
      const openPool = await openCard.locator('.endpoint-pool').textContent();
      if (!openPool.includes('Priority 2 never takes over') || !openPool.includes('Set a rate-limit cooldown')) throw new Error(`${project.name}: a standby group behind a disabled cooldown is not stated as unreachable (${openPool})`);
      await openCard.getByRole('button', {name: 'Distribute traffic'}).click();
      const offNotes = await page.locator('.traffic-group-head small').allTextContents();
      if (!offNotes[1].includes('Never used while rate limits are untracked')) throw new Error(`${project.name}: the traffic dialog presents an unreachable standby group as a plan (${offNotes.join(' | ')})`);
      if (!(await page.locator('#traffic-order').textContent()).includes('standby group never takes over while the preferred group still has an identity that can serve')) throw new Error(`${project.name}: the traffic dialog does not state that a standby group cannot take over without a cooldown policy`);
      if (!(await page.locator('#traffic-consequence').textContent()).includes('nothing leaves the rotation while rate limits are untracked')) throw new Error(`${project.name}: the traffic dialog does not state that a standby group cannot take over without a cooldown policy`);
      await page.locator('#traffic-dialog .close-traffic').first().click();
      await page.locator('#traffic-dialog').waitFor({state: 'hidden'});
      await page.locator('#back-to-providers').click();
      await page.locator('#provider-list-page').waitFor({state: 'visible'});
    }
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
      await assertEndpointFieldSpacing(page, project);
      const credentialToggle = page.locator('#endpoint-panel-connection .checkbox-row');
      if (!(await page.locator('#endpoint-form [name="requires_credential"]').isChecked())) await credentialToggle.click();
      await credentialToggle.click();
      await assertEndpointFieldSpacing(page, project);
      if (await page.locator('#endpoint-form [name="credential_secret"]').isVisible()) throw new Error(`${project.name}: keyless setup leaves an empty credential field in the form`);
      await credentialToggle.click();
      await page.locator('#endpoint-form [name="credential_secret"]').waitFor({state: 'visible'});
      // The generic reveal animation may still be changing this field's height.
      await page.locator('#endpoint-form [name="credential_secret"]').click();
      if (await page.locator('#endpoint-form [name="cooldown_enabled"]').isChecked() || await page.locator('#endpoint-form [name="cooldown_seconds"]').inputValue() !== '0') throw new Error(`${project.name}: a new Endpoint inherits the previous editor's enabled cooldown`);
      if (!(await page.locator('#endpoint-form [name="id"]').inputValue())) throw new Error(`${project.name}: additional endpoint ID is not suggested`);
      if (await page.locator('#endpoint-form .endpoint-id-permanent').isVisible()) throw new Error(`${project.name}: new Endpoint ID is incorrectly marked permanent before creation`);
      if (!(await page.locator('#endpoint-form .endpoint-id-required').isVisible())) throw new Error(`${project.name}: new Endpoint ID does not remain visibly required`);
      if (!(await page.locator('#endpoint-id-help').textContent()).includes('can be changed later')) throw new Error(`${project.name}: new Endpoint ID does not explain that it remains editable`);
      const endpointFieldOrder = await page.locator('#endpoint-panel-connection > label.field').evaluateAll(fields => fields.slice(0, 3).map(field => field.querySelector('.field-label')?.childNodes[0]?.textContent.trim()));
      if (endpointFieldOrder.join('|') !== 'Endpoint ID|API type|Base URL') throw new Error(`${project.name}: additional Endpoint setup does not ask for API type before Base URL`);
      if (!(await page.locator('#endpoint-form [name="api_type"] option[value="acme_plan"]').count())) throw new Error(`${project.name}: the Endpoint dialog omits an Extension-declared Endpoint type`);
      await page.locator('#endpoint-form [name="api_type"]').selectOption('openai_codex');
      if (await page.locator('#endpoint-form [name="base_url"]').isVisible()) throw new Error(`${project.name}: additional subscription Endpoint setup exposes Base URL`);
      await page.locator('#endpoint-dialog .close-endpoint').first().click();
      const addCredential = page.locator('.add-credential').first();
      if (await addCredential.count()) {
        await addCredential.click();
        await assertDialog(page, '#credential-dialog', project.name);
        if (!(await page.locator('#credential-endpoint option').count())) throw new Error(`${project.name}: credential dialog offers no Endpoint to attach the identity to`);
        if (!(await page.locator('#credential-dialog').textContent()).includes('weighted selection')) throw new Error(`${project.name}: credential dialog does not explain how the identity is used`);
        if (!(await page.locator('#credential-form .toggle-key').isVisible())) throw new Error(`${project.name}: credential dialog does not offer to reveal the secret it is about to store`);
        await page.locator('#credential-dialog .close-credential').first().click();
        await page.locator('#credential-dialog').waitFor({state: 'hidden'});
      }
      const renameKey = page.locator('.credential-rename').first();
      if (await renameKey.count()) {
        await renameKey.click();
        await assertDialog(page, '#credential-name-dialog', project.name);
        if (!await page.locator('#credential-name-form [name="name"]').inputValue()) throw new Error(`${project.name}: credential rename does not prefill the current name`);
        if (!(await page.locator('#credential-name-form .field-help').textContent()).includes('model-route references')) throw new Error(`${project.name}: credential rename does not explain which references stay unchanged`);
        await assertNoUpstreamCopy(page, project.name, 'Provider detail');
        await page.locator('#credential-name-dialog .close-credential-name').first().click();
      }
    }
    if (project.name === 'desktop-chrome') {
      const providerFixture = {
        id: 'ui-delete-provider', name: 'UI delete provider', extra_headers: {}, extra_body: {}, defaults_endpoint_ids: [],
        pricing: {updated_at: 1, models: {'ui-provider-price*': {input_per_million: 1, output_per_million: 2}}},
        endpoints: [{id: 'deletable', api_type: 'openai_compatible', base_url: 'http://127.0.0.1:18080/v1', socks5_proxy: null, extra_headers: {}, extra_body: {}, pricing: {updated_at: 1, models: {'ui-endpoint-price*': {output_per_million: 3}}}, requires_credential: true, credentials: [{id: 'delete-key', name: 'Delete key', weight: 100, enabled: true, kind: 'secret'}], rate_limit_cooldown: {seconds: 0, mode: 'fixed'}}],
        discovered_models: [], model_endpoints: {}, model_endpoint_preferences: [], models_discovered_at: null, model_discovery_error: null,
      };
      const routeFixture = {pattern: 'ui-delete-route', targets: [{provider_id: providerFixture.id, endpoint_id: 'deletable', credential_id: 'delete-key', upstream_model: 'upstream-delete-model', weight: 100, enabled: true}]};
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
      if (!confirmation.includes('1 Endpoint') || !confirmation.includes('1 Provider credential') || !confirmation.includes('1 model-route destination') || !confirmation.includes('deletes 1 route left without a destination') || !confirmation.includes('revokes 1 Gateway API key scoped only to this Provider')) throw new Error(`${project.name}: Provider deletion does not explain its cascading route, credential, and Gateway key impact`);
      if (await page.locator('#routes').getByText('ui-delete-route', {exact: true}).count()) throw new Error(`${project.name}: Provider deletion leaves stale model routes rendered in the console`);
      await page.evaluate(() => document.querySelector('[data-view="pricing"]').click());
      const deletedPricingText = await page.locator('#pricing-list-page').textContent();
      if (deletedPricingText.includes('ui-provider-price') || deletedPricingText.includes('ui-endpoint-price')) throw new Error(`${project.name}: Provider deletion leaves its pricing overrides in the central pricing list`);
    }
    await page.evaluate(() => document.querySelector('[data-view="models"]').click());
    const renderedRoute = page.locator('#routes .route-destination').first();
    if (await renderedRoute.count()) {
      if (!(await renderedRoute.locator('.route-destination-sends').isVisible()) || !(await renderedRoute.locator('.route-share').isVisible())) throw new Error(`${project.name}: route destination does not show its Provider model and traffic share`);
      // Provider and Endpoint are resources: only model IDs may look like `provider/model`.
      if (await renderedRoute.locator('.route-destination-route code').count()) throw new Error(`${project.name}: a route destination still writes Provider and Endpoint as a model-ID-shaped code path`);
      // The Endpoint and the identity carrying the traffic lead the row; the model Yabane
      // sends is secondary and the share stays small.
      const identityFont = await renderedRoute.locator('.route-endpoint-name').first().evaluate(node => parseFloat(getComputedStyle(node).fontSize));
      const upstreamFont = await renderedRoute.locator('.route-upstream-model').evaluate(node => parseFloat(getComputedStyle(node).fontSize));
      const shareFont = await renderedRoute.locator('.route-share').evaluate(node => parseFloat(getComputedStyle(node).fontSize));
      if (identityFont <= upstreamFont || shareFont > identityFont) throw new Error(`${project.name}: destination hierarchy is inverted (endpoint ${identityFont}px, model ${upstreamFont}px, share ${shareFont}px)`);
      const destinationHeight = await renderedRoute.evaluate(node => Math.round(node.getBoundingClientRect().height));
      // Where the traffic goes and which model it asks for are two lines; narrower
      // viewports wrap the same content instead of overflowing, so only the cap differs.
      const heightLimit = project.name === 'desktop-chrome' ? 84 : 140;
      if (destinationHeight > heightLimit) throw new Error(`${project.name}: one destination takes ${destinationHeight}px instead of a compact row`);
      // Shares are adjusted often, so a 0% destination stays an ordinary visible row.
      const destinations = await page.evaluate(() => {
        const rows = [...document.querySelectorAll('#routes .route-destination')];
        return {
          rendered: rows.length,
          hidden: rows.filter(row => row.offsetParent === null).length,
          expected: modelRoutes.reduce((total, route) => total + route.targets.length, 0),
          inactive: rows.filter(row => row.classList.contains('is-disabled')).length,
          zeroShares: [...document.querySelectorAll('#routes .route-share')].filter(node => node.textContent.trim() === '0%').length,
        };
      });
      if (destinations.rendered !== destinations.expected || destinations.hidden !== 0) throw new Error(`${project.name}: the list renders ${destinations.rendered}/${destinations.expected} destinations with ${destinations.hidden} hidden`);
      if (destinations.inactive !== destinations.zeroShares) throw new Error(`${project.name}: ${destinations.inactive} inactive rows do not match ${destinations.zeroShares} 0% shares`);
      if (destinations.inactive) {
        const inactiveText = await page.locator('#routes .route-destination.is-disabled .route-share').first().textContent();
        if (!inactiveText.includes('0%') || !inactiveText.includes('inactive')) throw new Error(`${project.name}: an inactive destination does not state 0% and that it is inactive (${inactiveText})`);
        if (await page.locator('#routes .route-status').count()) throw new Error(`${project.name}: an inactive destination is marked with a shape instead of text`);
      }
      if (await page.locator('.route-legend').count()) throw new Error(`${project.name}: the routing list still carries a legend above the table`);
      // Pinning one identity and pinning the Endpoint as a whole are different
      // guarantees, so their marks must not be the same mark: a caller that reads a
      // mark as "my traffic is pinned to this identity" must never be reading a
      // destination that selects no identity at all. A destination that hands traffic to
      // the Endpoint's own rotation or policy is neither, and stays unmarked.
      const identityMarks = await page.evaluate(() => [...document.querySelectorAll('#routes .route-destination')].map(row => {
        const identity = row.querySelector('.route-identity');
        const text = (identity?.textContent || '').trim();
        const policyLine = !!identity?.classList.contains('is-policy');
        return {
          text,
          kind: policyLine ? (/^No identity/.test(text) ? 'no-identity' : 'policy') : 'named',
          exact: !!row.querySelector('.route-identity-mark.is-exact'),
          pinned: !!row.querySelector('.route-identity-mark.is-pinned'),
        };
      }));
      const wrongMark = identityMarks.find(mark => mark.exact && mark.pinned)
        || identityMarks.find(mark => mark.kind === 'no-identity' && !mark.exact)
        || identityMarks.find(mark => mark.kind === 'named' && !mark.pinned)
        || identityMarks.find(mark => mark.kind === 'policy' && (mark.exact || mark.pinned));
      if (wrongMark) throw new Error(`${project.name}: a destination is marked for the wrong identity guarantee (${JSON.stringify(wrongMark)})`);
      if (await page.locator('.routing-explainer').isVisible()) throw new Error(`${project.name}: the three-step routing guide stays above a populated rules list`);
      // One destination is the normal case, so only a rule that splits traffic states its count.
      const routeCounts = await page.locator('#routes tr').evaluateAll(rows => rows.map(row => ({destinations: row.querySelectorAll('.route-destination').length, summary: (row.querySelector('.route-model-cell small')?.textContent || '').trim()})));
      if (routeCounts.some(row => row.destinations === 1 && row.summary)) throw new Error(`${project.name}: a single-destination rule states a destination count (${JSON.stringify(routeCounts)})`);
      if (routeCounts.filter(row => row.destinations > 1).some(row => !row.summary.includes(`${row.destinations} destinations`))) throw new Error(`${project.name}: a rule that splits traffic does not state how many destinations it has (${JSON.stringify(routeCounts)})`);
      // A failover rule is read in the order its groups carry traffic, and every
      // destination states what the route would do with it now instead of leaving
      // "the second Provider" to be inferred from the list order.
      const failoverRow = page.locator('#routes tr').filter({has: page.locator('.route-group-mark')}).first();
      if (!(await failoverRow.count())) throw new Error(`${project.name}: the routing list shows no failover rule and its priority groups`);
      {
        const groupNames = (await failoverRow.locator('.route-group-mark').allTextContents()).map(text => text.trim());
        const priorities = groupNames.map(text => Number(text.replace(/\D+/g, '')));
        if (priorities.some((priority, index) => index && priority < priorities[index - 1])) throw new Error(`${project.name}: a failover rule lists its priority groups out of order (${groupNames.join(', ')})`);
        const destinationStates = (await failoverRow.locator('.route-destination-state').allTextContents()).map(text => text.trim());
        const knownStates = ['standby', 'cooling down', 'cannot serve'];
        if (!destinationStates.length || destinationStates.some(text => !knownStates.includes(text))) throw new Error(`${project.name}: a failover destination does not state what the route does with it (${destinationStates.join(', ')})`);
      }
      // Rules are found by the names an operator thinks in, and a filter that
      // hides everything has to say so instead of looking like an empty page.
      const rulePatterns = () => page.locator('#routes .route-model-cell code').allTextContents();
      const ruleCount = routeCounts.length;
      const firstPattern = (await rulePatterns())[0]?.trim() || '';
      await page.locator('#route-search').fill(firstPattern);
      const matchedPatterns = await rulePatterns();
      if (!matchedPatterns.length || matchedPatterns.some(pattern => !pattern.toLowerCase().includes(firstPattern.toLowerCase()))) throw new Error(`${project.name}: routing search does not narrow the list to matching rules (${JSON.stringify(matchedPatterns)})`);
      await page.locator('#route-search').fill('zzz-no-such-route');
      if (!(await page.locator('#routes-no-match').isVisible())) throw new Error(`${project.name}: routing search does not state when no rule matches`);
      if (await page.locator('#routes tr').count()) throw new Error(`${project.name}: routing search leaves non-matching rules rendered`);
      await page.locator('#route-search').fill('');
      if (await page.locator('#routes tr').count() !== ruleCount) throw new Error(`${project.name}: clearing routing search does not restore every rule`);
      const routeModelCell = page.locator('#routes .route-model-cell').first();
      const [modelCellBox, modelHeadingBox, matchKind] = await Promise.all([
        routeModelCell.boundingBox(),
        routeModelCell.locator('.route-model-heading').boundingBox(),
        routeModelCell.locator('.route-match-kind').textContent(),
      ]);
      if (!modelCellBox || !modelHeadingBox || modelHeadingBox.height > 42) throw new Error(`${project.name}: route match identity becomes tall when a rule has multiple destinations`);
      if (!['Exact', 'Prefix'].includes(matchKind?.trim())) throw new Error(`${project.name}: route match type is not rendered as a compact label`);
      const routeOverflow = await page.locator('#routes-table').evaluate(element => element.scrollWidth > element.clientWidth + 1);
      if (routeOverflow) throw new Error(`${project.name}: structured route summary overflows its table viewport`);
      const policyRoute = page.locator('#routes .route-destination').filter({has: page.locator('.route-destination-route', {hasText: 'ui-subscription'})}).first();
      if (await policyRoute.count()) {
        const identity = await policyRoute.locator('.route-identity').textContent();
        if (!identity.includes('Endpoint policy')) throw new Error(`${project.name}: a destination that uses the Endpoint credential policy does not say so (${identity})`);
        if (/Credential (No identity|Endpoint policy)/.test(identity)) throw new Error(`${project.name}: route destination identity repeats the identity label (${identity})`);
      }
      // A delegated destination whose Endpoint has one enabled identity has no
      // rotation to describe, so the row names the identity that carries the traffic
      // instead of leaving the policy label to stand for it.
      await page.evaluate(() => {
        providers.push({
          id: 'ui-sole-policy', name: 'UI sole policy', extra_headers: {}, extra_body: {}, defaults_endpoint_ids: [],
          endpoints: [{id: 'main', api_type: 'openai_compatible', base_url: 'http://127.0.0.1:18080/v1', socks5_proxy: null, extra_headers: {}, extra_body: {}, requires_credential: true, rate_limit_cooldown: {seconds: 0, mode: 'fixed'}, credentials: [{id: 'solo', name: 'Solo account', weight: 100, enabled: true, kind: 'secret'}]}],
          discovered_models: [], model_endpoints: {}, model_endpoint_preferences: [], models_discovered_at: 1, model_discovery_error: null,
        });
        modelRoutes.push({pattern: 'ui-sole-policy-model', mode: 'weighted', targets: [{provider_id: 'ui-sole-policy', endpoint_id: 'main', credential_id: '', upstream_model: 'solo-model', weight: 100, priority: 1, enabled: true, state: 'serving'}]});
        renderRoutes();
      });
      const solePolicy = page.locator('#routes .route-destination').filter({has: page.locator('.route-destination-route', {hasText: 'ui-sole-policy'})}).first();
      const soleIdentity = (await solePolicy.locator('.route-identity').textContent()).trim();
      if (!soleIdentity.includes('Endpoint policy') || !soleIdentity.includes('Solo account')) throw new Error(`${project.name}: a delegated destination does not name the only identity that can carry its traffic (${soleIdentity})`);
      if (await solePolicy.locator('.route-identity-mark').count()) throw new Error(`${project.name}: a delegated destination is marked like a pinned identity (${soleIdentity})`);
      await page.evaluate(() => {
        modelRoutes.splice(modelRoutes.findIndex(route => route.pattern === 'ui-sole-policy-model'), 1);
        providers.splice(providers.findIndex(provider => provider.id === 'ui-sole-policy'), 1);
        renderRoutes();
      });
    }
    // A failover rule reopens as the mode it uses, keeps every destination's own
    // priority group, and still fits the dialog: the mode, the groups, and their
    // split have to survive a round trip through the editor.
    const editableFailoverRow = page.locator('#routes tr').filter({has: page.locator('.route-group-mark')}).first();
    if (await editableFailoverRow.count()) {
      await editableFailoverRow.locator('.edit-route').click();
      await assertDialog(page, '#route-dialog', project.name);
      if (!(await page.locator('#route-mode-choice input[value="failover"]').isChecked())) throw new Error(`${project.name}: editing a failover rule does not open it in failover mode`);
      const groupFields = page.locator('#route-targets .route-group-field');
      if (await groupFields.count() !== await page.locator('#route-targets .route-target-editor').count() || await groupFields.first().isHidden()) throw new Error(`${project.name}: a failover rule does not offer a group for every destination`);
      const priorityValues = await page.locator('#route-targets .route-target-editor').evaluateAll(editors => editors.map(editor => editor.dataset.group));
      if (priorityValues.join(',') !== '1,2') throw new Error(`${project.name}: reopening a failover rule loses its priority groups (${priorityValues.join(',')})`);

      const groupHeads = await page.locator('#route-targets .route-group-head strong').allTextContents();
      if (groupHeads.join(',') !== 'Priority 1,Priority 2') throw new Error(`${project.name}: the editor does not present the groups in the order they carry traffic (${groupHeads.join(',')})`);
      const editorOverflow = await page.locator('#route-targets').evaluate(element => element.scrollWidth > element.clientWidth + 1);
      if (editorOverflow) throw new Error(`${project.name}: the failover destination row overflows the route dialog`);
      const labelsFit = await page.locator('#route-targets .route-target-row .field-label').evaluateAll(labels => labels.every(label => label.scrollWidth <= label.clientWidth + 1));
      if (!labelsFit) throw new Error(`${project.name}: route field labels overlap adjacent controls`);
      // Provider and Endpoint are read as one destination, so their pickers split the
      // width of the card instead of sharing one narrow column: a stored Provider name
      // that only fits when truncated makes the choice unreadable.
      const destinationSpan = await page.locator('#route-targets .route-target-editor').first().evaluate(node => {
        const style = getComputedStyle(node);
        const box = node.getBoundingClientRect();
        const available = box.width - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight) - parseFloat(style.borderLeftWidth) - parseFloat(style.borderRightWidth);
        return {field: node.querySelector('.route-destination-field').getBoundingClientRect().width, available};
      });
      if (destinationSpan.field < destinationSpan.available - 2) throw new Error(`${project.name}: the destination block takes ${Math.round(destinationSpan.field)}px of its card's ${Math.round(destinationSpan.available)}px, leaving the Provider and Endpoint pickers in a narrow column`);
      const truncatedPickers = await page.locator('#route-targets .route-target-editor').first().locator('.route-destination-field .picker-value').evaluateAll(values => values.filter(value => value.scrollWidth > value.clientWidth + 1).map(value => value.textContent.trim()));
      if (truncatedPickers.length) throw new Error(`${project.name}: a chosen Provider or Endpoint is cut off in the destination block (${JSON.stringify(truncatedPickers)})`);
      const separatedModel = await page.locator('#route-targets .route-target-row').first().evaluate(row => row.querySelector('.route-model-field').getBoundingClientRect().bottom <= row.querySelector('.route-weight-field').getBoundingClientRect().top);
      if (!separatedModel) throw new Error(`${project.name}: model and traffic controls share the same crowded row`);
      if ((await page.locator('.route-group-total').allTextContents()).some(text => text !== '100%')) throw new Error(`${project.name}: group totals are missing from their group headings`);
      // A destination is moved between groups through its own group list, and the
      // group it leaves is closed instead of being left behind holding the position
      // above the group that received it — the reported defect was that moving the
      // first destination down left both destinations in one group with none left
      // carrying the traffic first.
      const groupsInOrder = () => page.locator('#route-targets .route-group').evaluateAll(sections => sections.map(section => [...section.querySelectorAll('.route-target-editor')].map(editor => ({model: editor.querySelector('[name="upstream_model"]').value, group: editor.dataset.group, weight: editor.querySelector('[name="target_weight"]').value}))));
      const opened = await groupsInOrder();
      if (opened.length !== 2 || opened.some(group => group.length !== 1)) throw new Error(`${project.name}: a failover rule does not open with one destination per group (${JSON.stringify(opened)})`);
      await page.locator('#route-targets .route-group').first().locator('[name="target_group"]').selectOption('2');
      const moved = await groupsInOrder();
      if (moved.length !== 1) throw new Error(`${project.name}: moving the only destination of a group leaves the group behind (${moved.length} groups)`);
      if (moved[0].length !== 2) throw new Error(`${project.name}: the moved destination does not join the group it was moved to (${JSON.stringify(moved)})`);
      if (moved[0].map(entry => entry.model).sort().join() !== opened.flat().map(entry => entry.model).sort().join()) throw new Error(`${project.name}: moving a destination between groups loses one of them (${JSON.stringify(moved)})`);
      if (moved[0].some(entry => entry.weight !== '50')) throw new Error(`${project.name}: a group that gained a destination does not share its own 100% (${JSON.stringify(moved)})`);
      if ((await page.locator('#route-targets .route-group-total').allTextContents()).join() !== '100%') throw new Error(`${project.name}: a group that received a destination does not state its own total`);
      // One group is not a choice, so the list that moves a destination between
      // groups is not offered while there is nowhere to move it to.
      if (await page.locator('#route-targets .route-group-field').first().isVisible()) throw new Error(`${project.name}: a single group still offers a list of groups to move a destination to`);
      if (await page.locator('#save-route').isDisabled()) throw new Error(`${project.name}: a repaired failover split cannot be saved`);
      // A standby group is a position to move a destination into, and the arrows
      // reorder the groups themselves so the destinations travel with their group.
      await page.locator('#add-route-group').click();
      const standby = await page.locator('#route-targets .route-group').count();
      if (standby !== 2 || !(await page.locator('#route-targets .route-group').last().locator('.route-group-empty').count())) throw new Error(`${project.name}: adding a standby group does not add a position to move a destination into`);
      if (!(await page.locator('#save-route').isDisabled())) throw new Error(`${project.name}: a group waiting for its first destination can be saved`);
      await page.locator('#route-targets .route-group').first().locator('[name="target_group"]').first().selectOption('2');
      const arranged = await groupsInOrder();
      if (arranged.length !== 2 || arranged.some(group => group.length !== 1)) throw new Error(`${project.name}: moving a destination into the new group does not fill it (${JSON.stringify(arranged)})`);
      await page.locator('.route-group-step[data-action="down"]').first().click();
      const swapped = await groupsInOrder();
      if (swapped.map(group => group[0].model).join() !== arranged.map(group => group[0].model).reverse().join()) throw new Error(`${project.name}: the group arrows do not swap the two groups (${JSON.stringify(swapped)})`);
      // The draft keeps its groups while the other mode is shown, so switching modes
      // and back does not collapse the plan the administrator just arranged.
      await page.locator('#route-mode-choice input[value="weighted"]').check();
      if (await page.locator('#route-targets .route-group-field').first().isVisible()) throw new Error(`${project.name}: a weighted rule offers a group the mode never reads`);
      await page.locator('#route-mode-choice input[value="failover"]').check();
      const kept = await groupsInOrder();
      if (JSON.stringify(kept) !== JSON.stringify(swapped)) throw new Error(`${project.name}: switching modes collapses the planned groups (${JSON.stringify(kept)})`);
      const policyDetails = page.locator('#route-targets .route-policy-details').first();
      if (await policyDetails.getAttribute('open') !== null) throw new Error(`${project.name}: repeated policy details start expanded`);
      await policyDetails.locator('summary').click();
      await policyDetails.locator('.route-destination-effect').waitFor({state: 'visible'});
      await policyDetails.locator('summary').click();
      await page.locator('#route-dialog .close-route').first().click();
      await page.locator('#route-dialog').waitFor({state: 'hidden'});
    }
    await page.evaluate(() => document.querySelector('#open-route').click());
    await assertDialog(page, '#route-dialog', project.name);
    await assertNoUpstreamCopy(page, project.name, 'Model routing');
    // Destinations are drawn by the console: Provider and Endpoint are resource
    // choices, while whether an identity is pinned at all is its own decision.
    const editor = page.locator('#route-targets .route-target-editor').first();
    const picker = level => editor.locator(`.route-${level}`);
    const openPicker = async level => {
      await picker(level).locator('.picker-trigger').click();
      await picker(level).locator('.picker-popup').waitFor({state: 'visible'});
    };
    const triggerText = level => picker(level).locator('.picker-value').textContent();
    const identityPolicy = editor.locator('.route-identity-policy');
    const identityStep = editor.locator('.route-identity-step');
    const destinationEffect = () => editor.locator('.route-destination-effect').textContent();
    await openPicker('provider');
    const providerNames = await picker('provider').locator('.picker-option-text strong').allTextContents();
    if (!providerNames.includes('UI keyless fixture') || !providerNames.includes('UI subscription fixture')) throw new Error(`${project.name}: the Provider list omits a configured Provider (${providerNames.join(', ')})`);
    await page.locator('#route-dialog .dialog-head h2').click();
    await picker('provider').locator('.picker-trigger').click();
    await picker('provider').locator('.picker-option', {hasText: 'UI keyless fixture'}).click();
    if (!(await triggerText('provider')).includes('UI keyless')) throw new Error(`${project.name}: choosing a Provider does not show it on the trigger (${await triggerText('provider')})`);
    if (!(await triggerText('endpoint')).includes('local')) throw new Error(`${project.name}: choosing a Provider does not narrow the Endpoint list to it (${await triggerText('endpoint')})`);
    // An Endpoint that sends no identity has nothing to decide, and the line says so.
    if (await identityPolicy.isVisible() || await identityStep.isVisible()) throw new Error(`${project.name}: an Endpoint without a credential requirement still asks for an identity`);
    if (!(await destinationEffect()).includes('Needs no identity')) throw new Error(`${project.name}: an Endpoint without a credential requirement does not say so (${await destinationEffect()})`);
    await page.locator('#route-dialog .dialog-head h2').click();
    // The consequence line answers the Endpoint's own cooldown policy, so this block
    // states that policy instead of inheriting whatever the previous block left behind.
    await page.evaluate(() => { providers.find(provider => provider.id === 'ui-subscription').endpoints[0].rate_limit_cooldown = {seconds: 3600, mode: 'prefer_provider'}; });
    await picker('provider').locator('.picker-trigger').click();
    await picker('provider').locator('.picker-option', {hasText: 'UI subscription fixture'}).click();
    await openPicker('endpoint');
    const endpointRows = await picker('endpoint').locator('.picker-option-text').allTextContents();
    if (!endpointRows.some(row => row.includes('accounts') || row.includes('chatgpt'))) throw new Error(`${project.name}: the Endpoint list omits the Provider's Endpoint (${endpointRows.join(', ')})`);
    await picker('endpoint').locator('.picker-option').first().click();
    // The models this Endpoint reports are the field's own suggestions, so one can be
    // chosen instead of retyped exactly.
    const suggestionList = await editor.locator('.upstream-model-input').getAttribute('list');
    const endpointSuggestions = await page.locator(`#${suggestionList} option`).evaluateAll(options => options.map(option => option.value));
    if (!endpointSuggestions.includes('gpt-fixture')) throw new Error(`${project.name}: the Provider model field does not suggest the models this Endpoint reports (${endpointSuggestions.join(', ')})`);
    // Choosing an Endpoint finishes the resource decision; delegating the identity
    // to the Endpoint is the default and shows no identity list at all.
    await identityPolicy.waitFor({state: 'visible'});
    const identityChoices = await identityPolicy.locator('label').allTextContents();
    if (identityChoices.join('|') !== 'Let the Endpoint choose|Pin one identity') throw new Error(`${project.name}: the identity decision is not named by its guarantees (${identityChoices.join(', ')})`);
    if (!(await identityPolicy.getByLabel('Let the Endpoint choose').isChecked()) || await identityStep.isVisible()) throw new Error(`${project.name}: the route editor does not delegate the identity to the Endpoint by default`);
    const delegatedEffect = await destinationEffect();
    if (!delegatedEffect.includes('by weight') || !delegatedEffect.includes('rate limit') || !delegatedEffect.includes('cooldown')) throw new Error(`${project.name}: letting the Endpoint choose does not state rotation and the configured cooldown consequence (${delegatedEffect})`);
    await identityPolicy.getByLabel('Pin one identity').check();
    if (!(await identityStep.isVisible())) throw new Error(`${project.name}: pinning one identity does not reveal the identity list`);
    // An option row answers the pointer the same way it answers the keyboard: the row
    // under the cursor becomes the current row instead of leaving no feedback at all.
    // A touch screen has no pointer, so this is the one interaction the mobile
    // viewports do not exercise.
    await openPicker('identity');
    const identityRows = picker('identity').locator('.picker-option');
    if (await identityRows.count() < 2) throw new Error(`${project.name}: pinning one identity does not offer the Endpoint's identities`);
    if (!project.mobile) {
      const hoveredRow = identityRows.nth(1);
      await hoveredRow.hover();
      if (!(await hoveredRow.getAttribute('class')).includes('is-highlighted')) throw new Error(`${project.name}: hovering an option row gives no feedback`);
      if ((await identityRows.first().getAttribute('class')).includes('is-highlighted')) throw new Error(`${project.name}: hovering an option row leaves the previously highlighted row marked`);
    }
    const identityMeta = await identityRows.first().locator('small').textContent();
    if (!identityMeta.includes('%')) throw new Error(`${project.name}: a pinnable identity does not state its share (${identityMeta})`);
    await identityRows.first().click();
    const pinEffect = await destinationEffect();
    if (!pinEffect.includes('Pins') || !pinEffect.includes('cooling down') || !pinEffect.includes('never used')) throw new Error(`${project.name}: pinning an identity does not state that it ignores cooling and the other identities (${pinEffect})`);
    // A stored pin is never replaced by another identity, even after that identity has
    // been disabled: the route states why it cannot be served instead of saving a
    // different identity under the same destination.
    await page.evaluate(() => { providers.find(provider => provider.id === 'ui-subscription').endpoints[0].credentials[0].enabled = false; });
    await page.evaluate(() => {
      const editor = document.querySelector('#route-targets .route-target-editor');
      setIdentityMode(editor, 'pin');
      renderDestination(editor, {providerId: 'ui-subscription', endpointId: 'chatgpt', credentialId: 'account'});
    });
    if (!(await triggerText('identity')).includes('OpenAI account')) throw new Error(`${project.name}: opening a stored pin replaces it with another identity (${await triggerText('identity')})`);
    const disabledPinEffect = await destinationEffect();
    if (!disabledPinEffect.includes('which is disabled')) throw new Error(`${project.name}: a pinned identity that has been disabled is not stated (${disabledPinEffect})`);
    await page.evaluate(() => { providers.find(provider => provider.id === 'ui-subscription').endpoints[0].credentials[0].enabled = true; });
    await identityPolicy.getByLabel('Let the Endpoint choose').check();
    if (await identityStep.isVisible()) throw new Error(`${project.name}: letting the Endpoint choose keeps the pinned identity list on screen`);
    // A rate limit only skips an identity while the Endpoint configures a cooldown,
    // and an Endpoint whose every identity is cooling down still sends the request.
    await page.evaluate(() => providers.find(provider => provider.id === 'ui-plain').endpoints[0].credentials.forEach(key => { key.cooldown_seconds_remaining = 60; }));
    await picker('provider').locator('.picker-trigger').click();
    await picker('provider').locator('.picker-option', {hasText: 'UI plain fixture'}).click();
    const coolingEffect = await destinationEffect();
    if (!coolingEffect.includes('does not track rate limits') || !coolingEffect.includes('still go out')) throw new Error(`${project.name}: a destination without a cooldown policy or with every identity cooling down states the wrong outcome (${coolingEffect})`);
    await page.evaluate(() => providers.find(provider => provider.id === 'ui-plain').endpoints[0].credentials.forEach(key => { delete key.cooldown_seconds_remaining; }));
    await picker('provider').locator('.picker-trigger').click();
    await picker('provider').locator('.picker-option', {hasText: 'UI subscription fixture'}).click();
    if (await page.locator('#route-targets .route-weight-field').first().isVisible()) throw new Error(`${project.name}: traffic share is visible for a simple alias`);
    await page.locator('#add-route-target').click();
    await page.locator('#route-split-head').waitFor({ state: 'visible' });
    // Adding a destination must not clear the identity decision above it: the copy
    // arrives carrying the same radio group name and checked state, and a browser
    // keeps only the newest selection inside one group.
    const destinationChoices = await page.locator('#route-targets .route-target-editor').evaluateAll(editors => editors.map(editor => [...editor.querySelectorAll('.route-identity-policy input')].filter(input => input.checked).map(input => input.value).join(',') || 'none'));
    if (destinationChoices.join('|') !== 'endpoint|endpoint') throw new Error(`${project.name}: adding a destination leaves ${JSON.stringify(destinationChoices)} selected instead of one identity decision per destination`);
    // A cloned destination editor builds one picker per level instead of keeping
    // the markup it was cloned with.
    const pickerCounts = await page.locator('#route-targets .route-target-editor').evaluateAll(editors => editors.map(editor => ['provider', 'endpoint', 'identity'].map(level => editor.querySelectorAll(`.route-${level} .picker-trigger`).length)));
    if (pickerCounts.some(counts => counts.join(',') !== '1,1,1')) throw new Error(`${project.name}: a destination editor renders ${JSON.stringify(pickerCounts)} instead of one picker per level`);
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
    // A turned-off destination dims its own labels and triggers, and that dimming must
    // not trap the menu it opens: the row whose share and toggle follow the picker, and
    // the next destination card, would otherwise paint over the open list.
    await openPicker('endpoint');
    const offDestinationPopup = await picker('endpoint').locator('.picker-popup').evaluate(popup => {
      const box = popup.getBoundingClientRect();
      // A scroll container clips what it scrolls, so only the part of the list its
      // ancestors leave unclipped can tell whether something paints over it.
      const clip = {left: box.left, right: box.right, top: box.top, bottom: box.bottom};
      for (let node = popup.parentElement; node; node = node.parentElement) {
        const style = getComputedStyle(node);
        if (!/auto|scroll|hidden/.test(style.overflowX + style.overflowY)) continue;
        const parentBox = node.getBoundingClientRect();
        clip.left = Math.max(clip.left, parentBox.left + parseFloat(style.borderLeftWidth));
        clip.right = Math.min(clip.right, parentBox.right - parseFloat(style.borderRightWidth));
        clip.top = Math.max(clip.top, parentBox.top + parseFloat(style.borderTopWidth));
        clip.bottom = Math.min(clip.bottom, parentBox.bottom - parseFloat(style.borderBottomWidth));
      }
      let covered = 0, samples = 0;
      const covering = [];
      for (let x = 0.06; x < 1; x += 0.09) for (let y = 0.06; y < 1; y += 0.06) {
        const pointX = box.left + box.width * x, pointY = box.top + box.height * y;
        if (pointX < clip.left || pointX > clip.right || pointY < clip.top || pointY > clip.bottom) continue;
        samples++;
        const node = document.elementFromPoint(pointX, pointY);
        if (!node || popup.contains(node)) continue;
        covered++;
        // Naming what covers the list turns a bare count into something diagnosable.
        if (covering.length < 3) covering.push(`${node.tagName.toLowerCase()}.${node.className}`);
      }
      let dimming = 1;
      for (let node = popup; node && node.nodeType === 1; node = node.parentElement) dimming *= parseFloat(getComputedStyle(node).opacity);
      return {covered, samples, dimming, covering};
    });
    if (offDestinationPopup.covered) throw new Error(`${project.name}: the open Endpoint list of a turned-off destination is painted under the fields that follow it (${offDestinationPopup.covered} of ${offDestinationPopup.samples} points, covered by ${offDestinationPopup.covering.join(', ') || 'nothing named'})`);
    if (offDestinationPopup.dimming < 1) throw new Error(`${project.name}: turning a destination off leaves its open Endpoint list see-through (effective opacity ${offDestinationPopup.dimming})`);
    await page.locator('#route-dialog .dialog-head h2').click();
    await page.locator('#route-targets [name="target_enabled"]').nth(1).uncheck();
    if (!(await page.locator('#save-route').isDisabled()) || await page.locator('#route-split-total').textContent() !== '0%') throw new Error(`${project.name}: route permits every target to be turned off`);
    await page.locator('#route-dialog .close-route').first().click();
    await page.locator('#route-dialog').waitFor({state: 'hidden'});
    // A route that can hand one conversation to more than one Endpoint says so next
    // to the mode, in both modes, because a failover destination receives the
    // conversation as soon as a cooldown moves the traffic. The statement follows
    // the traffic that actually exists, so a destination that is empty, off, or
    // holds no share yet does not make the route a split.
    await page.evaluate(() => document.querySelector('#open-route').click());
    await assertDialog(page, '#route-dialog', project.name);
    const modeNote = page.locator('#route-mode-note');
    const destinationEditor = index => page.locator('#route-targets .route-target-editor').nth(index);
    const chooseDestination = async (index, provider, endpoint) => {
      const field = destinationEditor(index);
      await field.locator('.route-provider .picker-trigger').click();
      await field.locator('.route-provider .picker-option', {hasText: provider}).click();
      await field.locator('.route-endpoint .picker-trigger').click();
      await field.locator('.route-endpoint .picker-option', {hasText: endpoint}).click();
    };
    await chooseDestination(0, 'UI plain fixture', 'main');
    if (await modeNote.isVisible()) throw new Error(`${project.name}: one destination is already described as a conversation split`);
    // Two destinations on one Provider and Endpoint are the same upstream by
    // construction, so no conversation can move and there is nothing to confirm.
    await page.locator('#add-route-target').click();
    await chooseDestination(1, 'UI plain fixture', 'main');
    if (await modeNote.isVisible()) throw new Error(`${project.name}: destinations sharing one Provider and Endpoint are described as a conversation split`);
    await chooseDestination(1, 'UI keyless fixture', 'local');
    if (!(await modeNote.isVisible())) throw new Error(`${project.name}: two destinations that can both receive traffic do not state that a conversation can move between them`);
    const noteText = (await modeNote.textContent()).replace(/\s+/g, ' ').trim();
    if (!noteText.includes('different Provider or Endpoint') || !noteText.includes('reasoning') || !noteText.includes('interchangeable')) throw new Error(`${project.name}: the conversation-continuity note does not name what the operator has to confirm (${noteText})`);
    if (await page.locator('#save-route').isDisabled()) throw new Error(`${project.name}: the conversation-continuity note blocks saving a valid split`);
    await destinationEditor(1).locator('[name="target_enabled"]').uncheck();
    if (await modeNote.isVisible()) throw new Error(`${project.name}: turning a destination off still describes the route as a conversation split`);
    await destinationEditor(1).locator('[name="target_enabled"]').check();
    await page.locator('#route-mode-choice input[value="failover"]').check();
    if (!(await modeNote.isVisible())) throw new Error(`${project.name}: a failover route that switches destinations does not state that a conversation can move between them`);
    await page.locator('#route-mode-choice input[value="weighted"]').check();
    await page.locator('#route-dialog .close-route').first().click();
    await page.locator('#route-dialog').waitFor({state: 'hidden'});
    // A destination that pins an identity keeps it when the route is reopened: the
    // decision is read against the chosen Endpoint, so it can never be reset by the
    // empty destination that exists before the pickers are rendered.
    await page.evaluate(() => {
      modelRoutes.push({pattern: 'ui-pinned-route', targets: [
        {provider_id: 'ui-subscription', endpoint_id: 'chatgpt', credential_id: 'account', upstream_model: 'gpt-fixture', weight: 0, enabled: false},
        {provider_id: 'ui-plain', endpoint_id: 'main', credential_id: '', upstream_model: 'plain-model', weight: 100, enabled: true},
      ]});
      document.querySelector('[data-view="models"]').click();
      renderRoutes();
    });
    await page.locator('#routes').getByText('ui-pinned-route', {exact: true}).waitFor({state: 'visible'});
    await page.evaluate(() => document.querySelector(`.edit-route[data-index="${modelRoutes.length - 1}"]`).click());
    await page.locator('#route-dialog').waitFor({state: 'visible'});
    const pinnedTarget = page.locator('#route-targets .route-target-editor').first();
    if (!(await pinnedTarget.locator('.route-identity-policy input[value="pin"]').isChecked())) throw new Error(`${project.name}: a reopened route shows a pinned destination as if the Endpoint chose the identity`);
    if (await pinnedTarget.locator('.route-identity-step').isHidden()) throw new Error(`${project.name}: a pinned destination does not reveal the identity it pins`);
    const pinnedTrigger = await pinnedTarget.locator('.route-identity .picker-trigger').textContent();
    if (!pinnedTrigger.includes('OpenAI account')) throw new Error(`${project.name}: a pinned destination does not show the identity it pins (${pinnedTrigger})`);
    const pinnedEffect = await pinnedTarget.locator('.route-destination-effect').textContent();
    if (!pinnedEffect.includes('Pins OpenAI account')) throw new Error(`${project.name}: a pinned destination does not state that one identity is pinned (${pinnedEffect})`);
    const clonedTarget = page.locator('#route-targets .route-target-editor').nth(1);
    if (!(await clonedTarget.locator('.route-identity-policy input[value="endpoint"]').isChecked())) throw new Error(`${project.name}: a destination added after a pinned one does not keep the Endpoint's own choice`);
    await page.locator('#route-dialog .close-route').first().click();
    await page.evaluate(() => modelRoutes.pop());
    await page.evaluate(() => document.querySelector('[data-view="pricing"]').click());
    await page.locator('#pricing-view').waitFor({state: 'visible'});
    if (!(await page.locator('#pricing-list-page').isVisible()) || !(await page.locator('#open-pricing-editor').isVisible())) throw new Error(`${project.name}: Model pricing lacks a dedicated top-level management entry`);
    await assertCodeChipsHugContent(page, '#pricing-table-body td:first-child code', project.name, 'Pricing model patterns');
    const incomingChip = await page.evaluate(() => {
      if (!Object.keys(globalPricing.incoming_models || {}).length) return null;
      return document.querySelector('#pricing-table-body .pricing-name-chip.incoming')?.textContent.trim() || '';
    });
    if (incomingChip === '') throw new Error(`${project.name}: the pricing list does not mark prices for the caller's model name as Incoming`);
    await page.locator('#open-pricing-editor').click();
    if (!(await page.locator('#pricing-editor-page').isVisible()) || !(await page.locator('#pricing-list-page').isHidden())) throw new Error(`${project.name}: price editing does not use the full pricing workspace`);
    const suggestedModels = await page.locator('#pricing-model-suggestions option').evaluateAll(options => options.map(option => option.value));
    if (!suggestedModels.includes('gpt-fixture')) throw new Error(`${project.name}: pricing model input does not suggest runtime-discovered model IDs`);
    if (!suggestedModels.includes('activity-only-model')) throw new Error(`${project.name}: pricing model input does not suggest historical Activity upstream model IDs`);
    await page.locator('#pricing-model').fill('model-family-*');
    if (!(await page.locator('#pricing-model-notice').textContent()).includes('Matches every Provider model beginning with')) throw new Error(`${project.name}: pricing model input does not explain prefix wildcard matching`);
    const pricingStepHeadings = await page.locator('#central-pricing-form .pricing-editor-section h2').allTextContents();
    if (pricingStepHeadings[0] !== 'Which model name should this price match?' || pricingStepHeadings[1] !== 'Application scope') throw new Error(`${project.name}: pricing editor asks where a price applies before which model name it prices (${JSON.stringify(pricingStepHeadings)})`);
    const pricingFirstStep = await page.locator('#central-pricing-form').textContent();
    if (!pricingFirstStep.includes('Outgoing model') || !pricingFirstStep.includes('Incoming model')) throw new Error(`${project.name}: pricing editor does not ask which of the two model names a price matches`);
    if (!(await page.locator('#pricing-route-example').isHidden())) throw new Error(`${project.name}: a price for the outgoing name shows a route example that belongs to the caller's name`);
    await page.locator('#central-pricing-form [name="price_name"][value="incoming"]').check();
    const incomingSuggestions = await page.locator('#pricing-model-suggestions option').evaluateAll(options => options.map(option => option.value));
    if (!incomingSuggestions.includes('activity-only-model')) throw new Error(`${project.name}: prices for the caller's model name do not suggest names recorded in Activity`);
    await page.evaluate(() => modelRoutes.push({pattern: 'ui-incoming-route', targets: [{provider_id: 'ui-subscription', endpoint_id: 'chatgpt', credential_id: '', upstream_model: 'gpt-fixture', weight: 100, enabled: true}]}));
    await page.locator('#pricing-model').fill('ui-incoming-route');
    const routeExample = await page.locator('#pricing-route-example').textContent();
    if (!routeExample.includes('gpt-fixture') || !routeExample.includes('ui-subscription/chatgpt')) throw new Error(`${project.name}: a price for the caller's model name does not show where that name is routed (${routeExample})`);
    if (!(await page.locator('#pricing-model-notice').textContent()).includes('no outgoing price')) throw new Error(`${project.name}: a price for the caller's model name does not explain that it only fills missing provider prices`);
    await page.evaluate(() => modelRoutes.pop());
    await page.locator('#central-pricing-form [name="price_name"][value="outgoing"]').check();
    await assertNoUpstreamCopy(page, project.name, 'Model pricing');
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
    await page.locator('#central-pricing-form [name="price_name"][value="incoming"]').check();
    if (!(await page.locator('#central-pricing-form [name="scope"][value="global"]').isChecked()) || !(await page.locator('#pricing-incoming-scope-note').isVisible()) || !(await page.locator('#pricing-scope-hint').textContent()).includes('nothing left to narrow') || !(await page.locator('#central-pricing-form [name="scope"][value="provider"]').isDisabled()) || !(await page.locator('#central-pricing-form [name="scope"][value="endpoint"]').isDisabled())) throw new Error(`${project.name}: picking the caller's model name first does not keep the rule Global with the reason visible`);
    await page.locator('#central-pricing-form [name="price_name"][value="outgoing"]').check();
    if (await page.locator('#pricing-incoming-scope-note').isVisible() || !(await page.locator('#pricing-scope-hint').textContent()).includes('narrower override') || await page.locator('#central-pricing-form [name="scope"][value="provider"]').isDisabled() || await page.locator('#central-pricing-form [name="scope"][value="endpoint"]').isDisabled()) throw new Error(`${project.name}: pricing scope stays narrowed or explained after switching to the outgoing model name`);
    await page.locator('#central-pricing-form [name="scope"][value="endpoint"]').check();
    await page.locator('#pricing-provider').selectOption('ui-subscription');
    await page.locator('#pricing-endpoint').selectOption('chatgpt');
    if (!(await page.locator('#pricing-resource-fields').isVisible()) || !(await page.locator('#pricing-endpoint-field').isVisible())) throw new Error(`${project.name}: centralized pricing cannot select an Endpoint override`);
    if (!(await page.locator('#central-pricing-form [name="price_name"][value="incoming"]').isDisabled())) throw new Error(`${project.name}: Provider and Endpoint scopes still offer prices for the caller's model name`);
    if (!(await page.locator('.pricing-target-options').textContent()).includes('Global only')) throw new Error(`${project.name}: the pricing editor does not explain that caller-facing prices are Global only`);
    if (!(await page.locator('#central-pricing-form [name="price_name"][value="outgoing"]').isChecked())) throw new Error(`${project.name}: narrowing the scope leaves the unavailable price target selected`);
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
      await page.clock.fastForward(liveRefreshIntervalMs);
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
    await page.locator('[data-chart-metric="performance"]').click();
    if (await page.locator('[data-chart-metric="performance"]').getAttribute('aria-pressed') !== 'true') throw new Error(`${project.name}: Activity timeline metric cannot be changed`);
    if (!(await page.locator('#chart-inspector-values').getByText('Avg latency', {exact: true}).isVisible()) || await page.locator('#activity-chart .first-byte-line').count() !== 1 || await page.locator('#activity-chart .throughput-line').count() !== 1) throw new Error(`${project.name}: Performance timeline omits its latency series or inspector value`);
    if (await page.locator('#activity-chart .traffic-axis-right.performance-axis').count() !== 3 || !(await page.locator('#activity-chart-legend').textContent()).includes('tok/s') || !(await page.locator('#traffic-granularity').textContent()).includes('throughput in tok/s')) throw new Error(`${project.name}: Performance timeline does not name its throughput scale and unit`);
    for (const label of ['Time to first token', 'Throughput']) if (!(await page.locator('#chart-inspector-values').getByText(label, {exact: true}).isVisible())) throw new Error(`${project.name}: Performance timeline inspector omits ${label}`);
    const inspectorLabels = await page.locator('#chart-inspector-values span').allTextContents();
    if (inspectorLabels.join(',') !== 'Requests,Input,Output,Cached input,Cache hit,Success,Avg latency,Time to first token,Throughput,Usage value') throw new Error(`${project.name}: interval details no longer group token, health, and performance values in reading order (${inspectorLabels.join(',')})`);
    const inspectorGrid = await page.locator('#chart-inspector-values').evaluate(grid => {
      const cells = [...grid.children].map(cell => ({box: cell.getBoundingClientRect(), bottom: getComputedStyle(cell).borderBottomWidth, right: getComputedStyle(cell).borderRightWidth}));
      const rows = [...new Set(cells.map(cell => Math.round(cell.box.top)))].sort((first, second) => first - second);
      return {
        count: cells.length,
        rows: rows.length,
        columns: cells.filter(cell => Math.round(cell.box.top) === rows[0]).length,
        cellsWithoutRowLine: cells.filter(cell => cell.bottom === '0px').length,
        cellsWithoutColumnLine: cells.filter(cell => cell.right === '0px').length,
        gridEdges: [getComputedStyle(grid).borderTopWidth, getComputedStyle(grid).borderLeftWidth].join(','),
      };
    });
    const expectedColumns = project.mobile ? 2 : 5;
    if (inspectorGrid.count !== 10 || inspectorGrid.rows * inspectorGrid.columns !== inspectorGrid.count || inspectorGrid.columns !== expectedColumns || inspectorGrid.cellsWithoutRowLine || inspectorGrid.cellsWithoutColumnLine || inspectorGrid.gridEdges !== '1px,1px') throw new Error(`${project.name}: interval details wrap into rows without their own separators (${JSON.stringify(inspectorGrid)})`);
    const performanceTooltips = await page.locator('#activity-chart .chart-column').first().evaluate(column => {
      column.dataset.throughputY = '30'; column.dataset.firstByteY = '120'; column.dataset.latencyY = '210';
      const chartBox = column.closest('#activity-chart').getBoundingClientRect();
      const inspectAt = y => {
        column.dispatchEvent(new PointerEvent('pointerover', {bubbles: true, clientY: chartBox.top + chartBox.height * y / 250}));
        return {series: column.dataset.activeSeries, label: column.querySelector('.chart-value').textContent, pointY: column.style.getPropertyValue('--point-y'), pointColor: column.style.getPropertyValue('--point-color')};
      };
      return {throughput: inspectAt(30), firstByte: inspectAt(120), latency: inspectAt(210)};
    });
    if (performanceTooltips.throughput.series !== 'throughput' || !performanceTooltips.throughput.label.startsWith('Throughput ') || performanceTooltips.throughput.pointY !== '30px' || performanceTooltips.throughput.pointColor !== '#7c4dff') throw new Error(`${project.name}: Performance timeline does not select the Throughput tooltip nearest the pointer (${JSON.stringify(performanceTooltips.throughput)})`);
    if (performanceTooltips.firstByte.series !== 'first-byte' || !performanceTooltips.firstByte.label.startsWith('Time to first token ') || performanceTooltips.firstByte.pointY !== '120px' || performanceTooltips.firstByte.pointColor !== '#168c9a') throw new Error(`${project.name}: Performance timeline does not select the Time to first token tooltip nearest the pointer (${JSON.stringify(performanceTooltips.firstByte)})`);
    if (performanceTooltips.latency.series !== 'latency' || !performanceTooltips.latency.label.startsWith('Average latency ') || performanceTooltips.latency.pointY !== '210px' || performanceTooltips.latency.pointColor !== '#0b57d0') throw new Error(`${project.name}: Performance timeline does not select the Average latency tooltip nearest the pointer (${JSON.stringify(performanceTooltips.latency)})`);
    // A series the Provider never measured must break instead of plotting a zero.
    const unmeasured = await page.evaluate(() => {
      activityChartBuckets = [{start: 0, requests: 0, tokens: 0, input: 0, output: 0, cached: 0, cost: 0, latency: 0, samples: 0, first_byte: 0, first_byte_samples: 0, generation: 0, generation_tokens: 0, generation_samples: 0, successful: 0, errors: 0, priced_requests: 0, reported_requests: 0, estimated_requests: 0}, {start: 1800, requests: 2, tokens: 120, input: 20, output: 100, cached: 0, cost: 0, latency: 4000, samples: 2, first_byte: 800, first_byte_samples: 2, generation: 2400, generation_tokens: 100, generation_samples: 2, successful: 2, errors: 0, priced_requests: 0, reported_requests: 0, estimated_requests: 0}];
      renderActivityChart(activityChartBuckets, 3600);
      const chart = document.querySelector('#activity-chart');
      const columns = [...chart.querySelectorAll('.chart-column')];
      return {
        firstByteGap: (chart.querySelector('.first-byte-line').getAttribute('d').match(/M/g) || []).length,
        throughputGap: (chart.querySelector('.throughput-line').getAttribute('d').match(/M/g) || []).length,
        latencyPath: chart.querySelector('.traffic-line').getAttribute('d'),
        emptyMeasured: columns[0].dataset.measured,
        emptyThroughput: columns[0].dataset.throughputValue,
        emptyHasMarker: getComputedStyle(columns[0], '::after').display,
        filledThroughput: columns[1].dataset.throughputValue,
      };
    });
    if (unmeasured.firstByteGap !== 1 || unmeasured.throughputGap !== 1 || unmeasured.latencyPath.includes(' C')) throw new Error(`${project.name}: Performance timeline draws a line for an interval the Provider never measured (${JSON.stringify(unmeasured)})`);
    if (unmeasured.emptyMeasured !== 'false' || unmeasured.emptyThroughput !== 'Not available' || unmeasured.emptyHasMarker !== 'none') throw new Error(`${project.name}: Performance timeline reports a fabricated value or marker for an unmeasured interval (${JSON.stringify(unmeasured)})`);
    if (!unmeasured.filledThroughput.startsWith('41.7') || !unmeasured.filledThroughput.endsWith('tok/s')) throw new Error(`${project.name}: Performance timeline does not report measured generation throughput (${unmeasured.filledThroughput})`);
    const successTones = await page.evaluate(() => [modelSuccessTone(99.4), modelSuccessTone(97), modelSuccessTone(94.9)]);
    if (successTones.join(',') !== 'model-healthy,model-warning,model-critical') throw new Error(`${project.name}: model success-rate severity does not distinguish healthy, warning, and critical rates`);
    const modelTable = page.locator('.model-analysis-table');
    if (!(await modelTable.locator('th', {hasText: 'Input cache hit'}).count()) || !(await modelTable.locator('th', {hasText: 'Usage value'}).count()) || !(await modelTable.locator('th', {hasText: 'Incoming model'}).count())) throw new Error(`${project.name}: per-model analysis omits cache efficiency, usage value, or the incoming model name`);
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
    const modelDimensionPicker = page.locator('#model-dimension-picker');
    if (!(await modelDimensionPicker.isVisible()) || await page.locator('#model-dimension-column').textContent() !== 'Incoming model' || !(await page.locator('#model-analysis-note').textContent()).includes('clients requested')) throw new Error(`${project.name}: Model analysis does not offer grouping by the incoming or outgoing model name`);
    if (await modelDimensionPicker.locator('[data-model-dimension="incoming"]').getAttribute('aria-pressed') !== 'true') throw new Error(`${project.name}: Model analysis does not start grouped by the model name clients requested`);
    // A refresh that was already in flight when the grouping changed must not paint its
    // rows under the newly selected label: the column name, its explanation, and the rows
    // are one answer, so a pending switch keeps describing the grouping it is replacing.
    let holdingOutgoingStats = false;
    const statsRoute = '**/admin/activity/stats*';
    await page.route(statsRoute, async route => {
      if (holdingOutgoingStats && new URL(route.request().url()).searchParams.get('model_dimension') === 'outgoing') await new Promise(resolve => setTimeout(resolve, 750));
      await route.continue();
    });
    const groupingState = () => page.evaluate(() => {
      const column = document.querySelector('#model-dimension-column')?.textContent;
      const rows = [...document.querySelectorAll('#model-stats tr')].map(row => row.querySelector('td[data-label]')?.dataset.label).filter(Boolean);
      return {column, rows: [...new Set(rows)]};
    });
    holdingOutgoingStats = true;
    const outgoingStats = page.waitForResponse(response => response.url().includes('/admin/activity/stats?') && response.url().includes('model_dimension=outgoing'));
    await modelDimensionPicker.locator('[data-model-dimension="outgoing"]').click();
    await page.waitForTimeout(200);
    const pendingGrouping = await groupingState();
    const expectedPendingLabel = pendingGrouping.column === 'Outgoing model' ? 'Outgoing model' : 'Incoming model';
    // The held response is released before anything is asserted, so a failure here still
    // leaves no request waiting behind it.
    holdingOutgoingStats = false;
    await outgoingStats;
    if (!pendingGrouping.rows.length) throw new Error(`${project.name}: Model analysis shows no rows while a grouping switch is loading, so the label cannot be checked against them`);
    if (pendingGrouping.rows.some(label => label !== expectedPendingLabel)) throw new Error(`${project.name}: Model analysis labels rows with a grouping other than the one its column states while a switch is still loading (${JSON.stringify(pendingGrouping)})`);
    await page.waitForFunction(() => document.querySelector('#model-dimension-column')?.textContent === 'Outgoing model' && ![...document.querySelectorAll('#model-stats tr')].some(row => row.querySelector('td[data-label]')?.dataset.label === 'Incoming model'));
    await page.unroute(statsRoute);
    if (await modelDimensionPicker.locator('[data-model-dimension="incoming"]').getAttribute('aria-pressed') !== 'false' || !(await page.locator('#model-analysis-note').textContent()).includes('sent to the Provider')) throw new Error(`${project.name}: Model analysis does not explain grouping by the model sent to the Provider`);
    const outgoingRows = await page.locator('#model-stats tr').allTextContents();
    if (outgoingRows.some(row => row.includes('priced-alias')) || outgoingRows.filter(row => row.includes('alias-sent')).length < 1) throw new Error(`${project.name}: grouping by the sent model keeps the caller-only name or loses the sent model (${JSON.stringify(outgoingRows)})`);
    if (await page.locator('#model-stats tr').count() && !(await page.locator('#model-stats [data-label="Outgoing model"]').count())) throw new Error(`${project.name}: model cells do not carry the selected grouping label`);
    if (project.mobile) {
      const toolsLayout = await page.locator('.activity-model-panel .activity-card-tools').evaluate(element => ({scrollWidth: element.scrollWidth, clientWidth: element.clientWidth}));
      if (toolsLayout.scrollWidth > toolsLayout.clientWidth + 1) throw new Error(`${project.name}: the model grouping control overflows its card on mobile`);
    }
    const incomingStats = page.waitForResponse(response => response.url().includes('/admin/activity/stats?') && response.url().includes('model_dimension=incoming'));
    await modelDimensionPicker.locator('[data-model-dimension="incoming"]').click();
    await incomingStats;
    await page.waitForFunction(() => document.querySelector('#model-dimension-column')?.textContent === 'Incoming model');
    if (!project.mobile) {
      await page.locator('[data-activity-tab="requests"]').click();
      const sevenDayStats = page.waitForResponse(response => response.url().includes('/admin/activity/stats?since=') && response.url().includes('buckets=168'));
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
      const weekStats = page.waitForResponse(response => response.url().includes('/admin/activity/stats?since=') && response.url().includes('buckets=168'));
      await page.locator('#activity-range').selectOption('604800');
      await weekStats;
      await page.waitForFunction(() => document.querySelectorAll('#activity-chart .chart-column').length === 168);
      const weekChart = await page.locator('#activity-chart').evaluate(chart => {
        const labels = [...chart.querySelectorAll('.chart-column small')].map(label => label.textContent).filter(Boolean);
        return {columns: chart.querySelectorAll('.chart-column').length, granularity: document.querySelector('#traffic-granularity').textContent, visibleLabels: labels.length, hourlyLabels: labels.filter(text => /:\d{2}$/.test(text)).length, pointMarkers: chart.querySelectorAll('.traffic-points circle').length};
      });
      if (weekChart.columns !== 168 || !weekChart.granularity.startsWith('1-hour')) throw new Error(`${project.name}: 7-day Activity timeline is not rendered at hourly resolution (${JSON.stringify(weekChart)})`);
      if (weekChart.visibleLabels < 4 || weekChart.visibleLabels > 9 || weekChart.hourlyLabels) throw new Error(`${project.name}: hourly 7-day Activity timeline shows crowded or non-day-boundary labels (${JSON.stringify(weekChart)})`);
      if (weekChart.pointMarkers) throw new Error(`${project.name}: dense hourly Activity timeline still draws per-interval point markers (${JSON.stringify(weekChart)})`);
      const backToDayStats = page.waitForResponse(response => response.url().includes('/admin/activity/stats?since=') && response.url().includes('buckets=48'));
      await page.locator('#activity-range').selectOption('86400');
      await backToDayStats;
      await page.waitForFunction(() => document.querySelectorAll('#activity-chart .chart-column').length === 48);
      const dayMarkers = await page.locator('#activity-chart').evaluate(chart => {
        const nonZero = [...chart.querySelectorAll('.chart-column')].filter(column => Number((column.getAttribute('aria-label') || '').match(/: ([\d,]+) requests/)?.[1]?.replace(/,/g, '') || 0) > 0).length;
        const columns = chart.querySelectorAll('.chart-column').length;
        const plotWidth = Math.max(chart.clientWidth, 320) - 48 - 42;
        return {markers: chart.querySelectorAll('.traffic-points circle').length, expected: plotWidth / Math.max(columns, 1) >= 12 ? nonZero : 0};
      });
      if (dayMarkers.markers !== dayMarkers.expected) throw new Error(`${project.name}: 24-hour Activity timeline point markers do not follow interval density (${JSON.stringify(dayMarkers)})`);
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
      if (!(await activityRow.getByText('Incoming', {exact: true}).isVisible()) || !(await activityRow.getByText('Outgoing', {exact: true}).isVisible()) || !(await activityRow.getByText('Same model ID', {exact: true}).isVisible())) throw new Error(`${project.name}: unchanged Activity model routing is not explicitly identified by incoming and outgoing side (${JSON.stringify(routingVisibility)})`);
      if (!(await mappedActivityRow.getByText('ui-subscription/gpt-fixture', {exact: true}).isVisible()) || !(await mappedActivityRow.getByText('activity-only-model', {exact: true}).isVisible()) || await mappedActivityRow.getByText('Same model ID', {exact: true}).count()) throw new Error(`${project.name}: mapped Activity model routing does not show both distinct model IDs`);
      await activityRow.click();
      await assertDialog(page, '#activity-detail-dialog', project.name);
      await assertNoUpstreamCopy(page, project.name, 'Activity details');
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
      if (!(await page.locator('#activity-detail-model-route').getByText('Incoming model', { exact: true }).isVisible()) || !(await page.locator('#activity-detail-model-route').getByText('Outgoing model', { exact: true }).isVisible())) throw new Error(`${project.name}: request detail does not name the incoming and outgoing sides of model routing`);
      if (!(await page.locator('#activity-detail-model-outcome').getByText('Model ID unchanged', { exact: true }).isVisible())) throw new Error(`${project.name}: request detail does not explain identical client and Provider model IDs`);
      if (!(await page.locator('#activity-detail-api-route').getByText('Client API', { exact: true }).isVisible()) || !(await page.locator('#activity-detail-api-route').getByText('Provider API', { exact: true }).isVisible()) || !(await page.locator('#activity-detail-api-route').getByText('No API conversion', { exact: true }).isVisible())) throw new Error(`${project.name}: request detail does not explain the client and Provider API formats`);
      if (!(await page.locator('#activity-detail-destination').getByText('Provider', { exact: true }).isVisible()) || !(await page.locator('#activity-detail-destination').getByText('Endpoint', { exact: true }).isVisible())) throw new Error(`${project.name}: request detail does not group Provider and Endpoint under the routing destination`);
      if (!(await page.locator('#activity-detail-destination').getByText('Carried the request while cooling down · no eligible identity was left', { exact: true }).isVisible())) throw new Error(`${project.name}: request detail presents an identity that was out of the pool as a healthy carrier`);
      // A failover selection stays explainable after the cooldown it caused has
      // expired, so the summary names the group that carried the request and that
      // a group above it was left behind.
      const routeSummary = await page.locator('#activity-detail-route').textContent();
      if (!routeSummary.includes('Priority 2') || !routeSummary.includes('switched after every destination above it cooled down')) throw new Error(`${project.name}: request detail does not explain a destination chosen by failover (${routeSummary})`);
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
      const incomingModel = await page.locator('#activity-detail-model').textContent();
      await editActivityPricing.click();
      await page.locator('#pricing-view').waitFor({state: 'visible'});
      if (!(await page.locator('#pricing-editor-page').isVisible())) throw new Error(`${project.name}: Activity price action does not open the centralized pricing editor`);
      if (await page.locator('#pricing-model').inputValue() !== incomingModel) throw new Error(`${project.name}: Activity pricing action does not prefill the model name the caller sent`);
      if (!(await page.locator('#central-pricing-form [name="scope"][value="global"]').isChecked()) || !(await page.locator('#central-pricing-form [name="price_name"][value="incoming"]').isChecked())) throw new Error(`${project.name}: a new Activity price does not default to a global price for the caller's model name`);
      await page.locator('#cancel-pricing-edit').click();
      await page.evaluate(() => document.querySelector('[data-view="activity"]').click());
      const pricedActivityRow = recentActivityRows.filter({hasText: 'alias-sent-two'}).first();
      if (await pricedActivityRow.count()) {
        await pricedActivityRow.click();
        await assertDialog(page, '#activity-detail-dialog', project.name);
        const rateSource = await page.locator('#activity-detail-usage .activity-pricing-source').textContent();
        if (!rateSource.includes('Global incoming rule') || !rateSource.includes('priced-alias') || !rateSource.includes('Provider outgoing rule') || !rateSource.includes('alias-sent-two')) throw new Error(`${project.name}: an estimated cost does not explain which pricing rules supplied each rate (${rateSource})`);
        await page.locator('#activity-detail-dialog').evaluate(dialog => dialog.close());
      }
      // The routing destination names the credential that carried the request
      // instead of showing the internal ID: a record from this instance follows
      // the credential's current name, an imported record keeps the name recorded
      // with it, and an identity with neither is shown as its stable ID with the
      // reason stated next to it.
      const credentialDestinations = new Map();
      for (const [model, name, note] of [
        ['credential-name-fixture', 'Second account', 'Identity that carried the request'],
        ['credential-remote-fixture', 'Remote ChatGPT account', 'Identity that carried the request'],
        ['credential-unknown-fixture', 'account-9', 'Identity that carried the request · no matching credential is configured now'],
      ]) {
        const row = recentActivityRows.filter({hasText: model}).first();
        if (!(await row.count())) throw new Error(`${project.name}: Activity credential fixture ${model} is missing from recent requests`);
        await row.click();
        await assertDialog(page, '#activity-detail-dialog', project.name);
        const destination = await page.locator('#activity-detail-destination').textContent();
        credentialDestinations.set(model, destination);
        if (!destination.includes(name) || !destination.includes(note)) throw new Error(`${project.name}: Activity destination does not identify the carrying credential as ${name} (${destination})`);
        await page.locator('#activity-detail-dialog').evaluate(dialog => dialog.close());
      }
      if (credentialDestinations.get('credential-remote-fixture').includes('OpenAI account')) throw new Error(`${project.name}: an imported Activity record borrowed this instance's name for the same credential ID`);
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
    const consoleViews = await page.locator('#console-sidebar [data-view]').evaluateAll(nodes => [...new Set(nodes.map(node => node.dataset.view))]);
    for (const view of consoleViews) {
      await openConsoleView(page, view);
      await assertNoUpstreamCopy(page, project.name, `console view ${view}`);
    }
    await assertNoPageOverflow(page, project.name, 'console');
    assertNoPageErrors();
    console.log(`${project.name}: responsive console and dialogs passed`);
    await context.close();
  } finally {
    await browser.close();
  }
}

async function assertEndpointFieldSpacing(page, project) {
  const dialog = page.locator('#endpoint-dialog');
  // At the end of a small screen's scroll range the sticky actions must leave
  // room for the last field, not cover it or be pulled up by a hidden section.
  await dialog.evaluate(element => { element.scrollTop = element.scrollHeight; });
  const layout = await dialog.evaluate(element => {
    const fields = [...element.querySelector('#endpoint-panel-connection').children]
      .map(field => field.getBoundingClientRect()).filter(box => box.height > 0);
    const button = element.querySelector('button[type="submit"]').getBoundingClientRect();
    return {
      gaps: fields.slice(1).map((field, index) => field.top - fields[index].bottom),
      actionGap: button.top - fields.at(-1).bottom,
      width: element.clientWidth,
      overflow: element.scrollHeight - element.clientHeight,
      editing: Boolean(element.querySelector('form').dataset.endpointId),
    };
  });
  if (layout.gaps.some(gap => gap < 20) || layout.actionGap < 20) throw new Error(`${project.name}: Endpoint fields or actions are crowded (${JSON.stringify(layout)})`);
  if (project.width >= 1024 && (layout.width < 620 || (layout.editing && layout.overflow > 1))) throw new Error(`${project.name}: desktop Endpoint settings do not use available width to fit (${JSON.stringify(layout)})`);
  await dialog.evaluate(element => { element.scrollTop = 0; });
}

async function assertNoPageOverflow(page, name, surface) {
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth > document.documentElement.clientWidth);
  if (overflow) throw new Error(`${name}: ${surface} has horizontal overflow`);
}

async function assertCodeChipsHugContent(page, selector, name, surface) {
  const stretched = await page.locator(selector).evaluateAll(labels => labels.filter(label => {
    const style = getComputedStyle(label);
    if (style.backgroundColor === 'rgba(0, 0, 0, 0)') return false;
    const range = document.createRange();
    range.selectNodeContents(label);
    const contentWidth = range.getBoundingClientRect().width + parseFloat(style.paddingLeft) + parseFloat(style.paddingRight);
    return label.getBoundingClientRect().width > contentWidth + 1;
  }).map(label => label.textContent));
  if (stretched.length) throw new Error(`${name}: ${surface} stretch past their content (${stretched.join(', ')})`);
}

async function assertNoUpstreamCopy(page, name, label) {
  const matches = await page.evaluate(() => {
    const found = [];
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT, {acceptNode: node => node.parentElement && !node.parentElement.closest('script,style') ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_REJECT});
    while (walker.nextNode()) { const text = walker.currentNode.nodeValue; if (/\bupstream\b/i.test(text)) found.push(text.replace(/\s+/g, ' ').trim().slice(0, 120)); }
    for (const element of document.querySelectorAll('[title],[placeholder],[aria-label]')) {
      for (const attribute of ['title', 'placeholder', 'aria-label']) {
        const value = element.getAttribute(attribute);
        if (value && /\bupstream\b/i.test(value)) found.push(`${attribute}="${value.replace(/\s+/g, ' ').trim().slice(0, 100)}"`);
      }
    }
    return [...new Set(found)];
  });
  if (matches.length) throw new Error(`${name}: ${label} still shows ambiguous "upstream" wording: ${JSON.stringify(matches)}`);
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
