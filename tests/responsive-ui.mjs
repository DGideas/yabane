import { chromium, webkit } from 'playwright';

const base = process.env.YABANE_UI_BASE || 'http://127.0.0.1:8080';
const sessionCookie = process.env.YABANE_SESSION_COOKIE;
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
    if (sessionCookie) {
      await context.addCookies([{ name: 'yabane_session', value: sessionCookie, url: base, httpOnly: true, sameSite: 'Strict' }]);
    }
    const page = await context.newPage();
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
      });
      await route.fulfill({response, json: body});
    });
    const testLiveRefresh = project.name === 'desktop-chrome';
    if (testLiveRefresh) await page.clock.install();
    await page.goto(base, { waitUntil: 'domcontentloaded' });
    await page.waitForFunction(() => !document.querySelector('#login-screen')?.hidden || !document.querySelector('#admin-app')?.hidden);
    if (await page.locator('#login-screen').isVisible()) {
      await page.locator('#login-form [name="username"]').waitFor();
      if (!(await page.locator('#login-form [name="username"]').evaluate(element => element === document.activeElement))) throw new Error(`${project.name}: login does not initially focus the username field`);
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
    if (!(await requestDefaultsExtension.getByText('Native Rust', {exact: true}).isVisible())) throw new Error(`${project.name}: extension implementation type is not visible`);
    if (!(await requestDefaultsExtension.getByText('v1', {exact: true}).isVisible())) throw new Error(`${project.name}: Extension API version is not visible`);
    const extensionToggle = requestDefaultsExtension.locator('[data-extension-toggle="request-defaults"]');
    if (!(await extensionToggle.isChecked()) || !(await extensionToggle.isEnabled())) throw new Error(`${project.name}: Request Defaults does not expose its enabled runtime setting`);
    if (project.name === 'desktop-chrome') {
      await extensionToggle.evaluate(input => input.click());
      await requestDefaultsExtension.getByText('Disabled', {exact: true}).waitFor();
      await requestDefaultsExtension.locator('[data-extension-toggle="request-defaults"]').evaluate(input => input.click());
      await requestDefaultsExtension.getByText('Enabled', {exact: true}).waitFor();
    }
    await assertNoPageOverflow(page, project.name, 'Extensions page');
    await page.evaluate(() => document.querySelector('[data-view="providers"]').click());
    const stretchedProviderLabels = await page.locator('.provider-list-main code').evaluateAll(labels => labels.filter(label => {
      const range = document.createRange();
      range.selectNodeContents(label);
      const style = getComputedStyle(label);
      const contentWidth = range.getBoundingClientRect().width + parseFloat(style.paddingLeft) + parseFloat(style.paddingRight);
      return label.getBoundingClientRect().width > contentWidth + 1;
    }).map(label => label.textContent));
    if (stretchedProviderLabels.length) throw new Error(`${project.name}: Provider model labels stretch past their content (${stretchedProviderLabels.join(', ')})`);
    await page.evaluate(() => document.querySelector('[data-view="home"]').click());
    const homeHero = page.locator('#home-view .home-hero');
    if (!(await homeHero.isVisible()) || !(await page.locator('#home-traffic-chart').isVisible())) throw new Error(`${project.name}: Home is missing its gateway hero or traffic visualization`);
    const homeArtwork = await page.locator('.home-hero-motion').evaluate(element => ({pointerEvents: getComputedStyle(element).pointerEvents, ariaHidden: element.getAttribute('aria-hidden')}));
    if (homeArtwork.pointerEvents !== 'none' || homeArtwork.ariaHidden !== 'true') throw new Error(`${project.name}: Home hero artwork can interfere with interaction or accessibility`);
    await page.emulateMedia({ reducedMotion: 'reduce' });
    const homeAnimations = await page.locator('.home-hero-motion g').evaluateAll(groups => groups.map(group => getComputedStyle(group).animationName));
    if (homeAnimations.some(name => name !== 'none')) throw new Error(`${project.name}: Home hero ignores reduced-motion preference`);
    await page.emulateMedia({ reducedMotion: 'no-preference' });

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
    const logoPaths = await page.locator('#about-dialog .about-logo path').evaluateAll(paths => paths.map(path => path.getAttribute('d')));
    if (logoPaths.join('|') !== 'M14 4h32l14 14v32c0 5.5-4.5 10-10 10H14C8.5 60 4 55.5 4 50V14C4 8.5 8.5 4 14 4Z|m13 18 13 14-13 14h8l13-14-13-14Z|m31 18 13 14-13 14h8l13-14-13-14Z') throw new Error(`${project.name}: About dialog does not use the Yabane mark`);
    const brandLoaded = await page.locator('.topbar .brand-mark').evaluate(image => image.complete && image.naturalWidth > 0);
    if (!brandLoaded) throw new Error(`${project.name}: Yabane application icon did not load`);
    await page.locator('#about-dialog .close-about').first().click();
    await page.evaluate(() => document.querySelector('#open-provider').click());
    await page.locator('#display-name').fill('OpenAI subscription');
    await page.locator('#next-step').click();
    await page.locator('#api-type-choices input[value="openai_codex"]').check();
    await assertDialog(page, '#provider-dialog', project.name);
    if (await page.locator('#initial-endpoint-id').inputValue() !== 'chatgpt') throw new Error(`${project.name}: first endpoint does not expose the API-type default ID`);
    if (await page.locator('#base-url').isVisible()) throw new Error(`${project.name}: subscription setup exposes Base URL`);
    if (!(await page.locator('#provider-form [name="socks5_proxy"]').isVisible())) throw new Error(`${project.name}: subscription setup hides SOCKS5 proxy`);
    await page.locator('#provider-form [name="socks5_proxy"]').fill('socks5h://127.0.0.1:1080');
    if (await page.locator('#api-key').isVisible()) throw new Error(`${project.name}: subscription setup exposes API key input`);
    if (await page.locator('#create-provider').textContent() !== 'Connect OpenAI') throw new Error(`${project.name}: subscription setup has the wrong primary action`);
    await page.locator('#provider-dialog .close-dialog').first().click();
    await page.evaluate(() => document.querySelector('[data-view="providers"]').click());
    const subscriptionProvider = page.locator('#providers .provider-list-item').filter({has: page.locator('code', {hasText: 'ui-subscription/model-id'})});
    await subscriptionProvider.click();
    const credential = page.locator('.subscription-credential');
    await credential.waitFor({state: 'visible'});
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
        if (!(await defaultsCard.getByRole('button', {name: 'Configure'}).isVisible())) throw new Error(`${project.name}: included Request Defaults cannot be configured`);
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
      await page.locator('.add-endpoint').click();
      await assertDialog(page, '#endpoint-dialog', project.name);
      if (!(await page.locator('#endpoint-form [name="id"]').inputValue())) throw new Error(`${project.name}: additional endpoint ID is not suggested`);
      await page.locator('#endpoint-dialog .close-endpoint').first().click();
    }
    await page.evaluate(() => document.querySelector('[data-view="models"]').click());
    const renderedRoute = page.locator('#routes .route-destination').first();
    if (await renderedRoute.count()) {
      if (!(await renderedRoute.locator('.route-upstream').isVisible()) || !(await renderedRoute.locator('.route-status').isVisible()) || !(await renderedRoute.locator('.route-target-state > strong').isVisible())) throw new Error(`${project.name}: route destination does not visually separate its upstream, status, and traffic share`);
      const routeOverflow = await page.locator('#routes-table').evaluate(element => element.scrollWidth > element.clientWidth + 1);
      if (routeOverflow) throw new Error(`${project.name}: structured route summary overflows its table viewport`);
    }
    await page.evaluate(() => document.querySelector('#open-route').click());
    await assertDialog(page, '#route-dialog', project.name);
    if (await page.locator('#route-targets .route-weight-field').first().isVisible()) throw new Error(`${project.name}: traffic share is visible for a simple alias`);
    await page.locator('#add-route-target').click();
    await page.locator('#route-split-head').waitFor({ state: 'visible' });
    const shares = await page.locator('#route-targets [name="target_weight"]').evaluateAll(inputs => inputs.map(input => input.value));
    if (shares.join(',') !== '50,50') throw new Error(`${project.name}: initial traffic split is not 50/50 (${shares.join(',')})`);
    await page.locator('#route-targets [name="target_weight"]').first().fill('60');
    if (!(await page.locator('#save-route').isDisabled())) throw new Error(`${project.name}: invalid traffic total does not disable saving`);
    if (await page.locator('#route-split-total').textContent() !== '110%') throw new Error(`${project.name}: invalid traffic total is not explained`);
    await page.locator('#route-targets [name="target_enabled"]').first().uncheck();
    const switchedShares = await page.locator('#route-targets [name="target_weight"]').evaluateAll(inputs => inputs.map(input => ({value: input.value, disabled: input.disabled})));
    if (!switchedShares[0].disabled || switchedShares[1].value !== '100') throw new Error(`${project.name}: disabling a route target does not move all traffic to the active target`);
    if (await page.locator('#save-route').isDisabled()) throw new Error(`${project.name}: one active 100% target cannot be saved`);
    await page.locator('#route-targets [name="target_enabled"]').nth(1).uncheck();
    if (!(await page.locator('#save-route').isDisabled()) || await page.locator('#route-split-total').textContent() !== '0%') throw new Error(`${project.name}: route permits every target to be disabled`);
    await page.locator('#route-dialog .close-route').first().click();
    const initialActivityLoad = testLiveRefresh ? Promise.all([
      page.waitForResponse(response => response.url().includes('/admin/activity/stats?since=') && response.url().includes('buckets=24') && response.url().includes('until=')),
      page.waitForResponse(response => response.url().includes('/admin/activity/logs?since=') && response.url().includes('limit=100')),
    ]) : null;
    await page.evaluate(() => document.querySelector('[data-view="activity"]').click());
    if (initialActivityLoad) await initialActivityLoad;
    if (testLiveRefresh) {
      const activityRefresh = Promise.all([
        page.waitForResponse(response => response.url().includes('/admin/activity/stats?since=') && response.url().includes('buckets=24') && response.url().includes('until=')),
        page.waitForResponse(response => response.url().includes('/admin/activity/logs?since=') && response.url().includes('limit=100')),
      ]);
      await page.clock.fastForward(30000);
      await activityRefresh;
    }
    const timelineColumns = page.locator('#activity-chart .chart-column');
    await page.waitForFunction(() => document.querySelectorAll('#activity-chart .chart-column').length === 24);
    if (await timelineColumns.count() !== 24) throw new Error(`${project.name}: 24-hour Activity timeline does not expose every interval`);
    const inspectorBefore = await page.locator('#chart-inspector-time').textContent();
    if (project.mobile) await timelineColumns.first().click(); else await timelineColumns.first().hover();
    const inspectorAfter = await page.locator('#chart-inspector-time').textContent();
    const firstColumnClass = await timelineColumns.first().getAttribute('class');
    if (!inspectorAfter || inspectorAfter === '—' || (inspectorAfter === inspectorBefore && !firstColumnClass.includes('selected'))) throw new Error(`${project.name}: Activity timeline does not respond to interval interaction`);
    await page.locator('[data-chart-metric="latency"]').click();
    if (await page.locator('[data-chart-metric="latency"]').getAttribute('aria-pressed') !== 'true') throw new Error(`${project.name}: Activity timeline metric cannot be changed`);
    if (!(await page.locator('#chart-inspector-values').getByText('Avg latency', {exact: true}).isVisible())) throw new Error(`${project.name}: Activity timeline inspector omits latency`);
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
      if (!(await explorerRow.locator('.activity-model').isVisible()) || !(await explorerRow.locator('.route-cell').isVisible()) || !(await explorerRow.locator('.activity-output').isVisible())) throw new Error(`${project.name}: Request explorer does not emphasize model, route, and output usage`);
    }
    await page.locator('[data-activity-tab="overview"]').click();
    const activityRow = page.locator('#recent-activity-logs .activity-request-row').first();
    if (await activityRow.count()) {
      await activityRow.click();
      await assertDialog(page, '#activity-detail-dialog', project.name);
      if (!(await page.locator('#activity-detail-request').getByText('Request ID', { exact: true }).isVisible())) throw new Error(`${project.name}: request detail dialog omits request metadata`);
      if (!(await page.locator('#activity-detail-timing').getByText('Total', { exact: true }).isVisible())) throw new Error(`${project.name}: request detail dialog omits timing`);
      if (!(await page.locator('#activity-detail-failure').count())) throw new Error(`${project.name}: request detail dialog omits the failure diagnosis region`);
      if (!(await page.locator('#activity-detail-dialog').getByText('Prompt and response content are not retained.').isVisible())) throw new Error(`${project.name}: request detail dialog omits the content-retention notice`);
      await page.locator('#activity-detail-dialog .close-activity-detail').first().click();
    }
    await page.locator('#manage-activity-data').click();
    await assertDialog(page, '#activity-data-dialog', project.name);
    await page.locator('[data-activity-data-tab="import"]').click();
    const previewRecord = {
      timestamp: Math.floor(Date.now() / 1000), request_id: `responsive-preview-${project.name}`,
      path: '/v1/responses', model: 'preview/model', provider: 'preview', endpoint: 'preview',
      status: 200, latency_ms: 1200, gateway_ms: 4, upstream_response_ms: 180, first_byte_ms: 250, generation_ms: 950,
      input_tokens: 120, output_tokens: 40, cached_tokens: 20, cost: null, streaming: false,
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
  const box = await page.locator(selector).boundingBox();
  const viewport = page.viewportSize();
  if (!box || !viewport) throw new Error(`${name}: ${selector} is not visible`);
  const tolerance = 1;
  if (box.x < -tolerance || box.y < -tolerance || box.x + box.width > viewport.width + tolerance || box.y + box.height > viewport.height + tolerance) {
    throw new Error(`${name}: ${selector} escapes viewport (${JSON.stringify(box)})`);
  }
  const close = await page.locator(`${selector} .icon-button`).first().boundingBox();
  if (!close || close.width < 40 || close.height < 40) throw new Error(`${name}: ${selector} close target is too small`);
}
