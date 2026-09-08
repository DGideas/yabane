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
    const testLiveRefresh = project.name === 'desktop-chrome';
    if (testLiveRefresh) await page.clock.install();
    await page.goto(base, { waitUntil: 'networkidle' });
    if (await page.locator('#login-screen').isVisible()) {
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
    } else if (await mobileNavToggle.isVisible()) throw new Error(`${project.name}: mobile navigation trigger is visible on a wide layout`);

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
    await page.evaluate(() => document.querySelector('[data-view="home"]').click());

    if (testLiveRefresh) {
      const homeRefresh = page.waitForResponse(response => response.url().includes('/admin/activity/stats?since='));
      await page.clock.fastForward(10000);
      await homeRefresh;
    }

    await page.locator('#open-help').click();
    await assertDialog(page, '#help-dialog', project.name);
    await page.locator('#help-dialog .close-help').first().click();
    await page.evaluate(() => document.querySelector('.open-about').click());
    await assertDialog(page, '#about-dialog', project.name);
    if (await page.getByText('Reproducible from Git').count()) throw new Error(`${project.name}: About dialog still shows redundant build text`);
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
    if (await page.locator('#base-url').isVisible()) throw new Error(`${project.name}: subscription setup exposes Base URL`);
    if (!(await page.locator('#provider-form [name="socks5_proxy"]').isVisible())) throw new Error(`${project.name}: subscription setup hides SOCKS5 proxy`);
    await page.locator('#provider-form [name="socks5_proxy"]').fill('socks5h://127.0.0.1:1080');
    if (await page.locator('#api-key').isVisible()) throw new Error(`${project.name}: subscription setup exposes API key input`);
    if (await page.locator('#create-provider').textContent() !== 'Connect OpenAI') throw new Error(`${project.name}: subscription setup has the wrong primary action`);
    await page.locator('#provider-dialog .close-dialog').first().click();
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
    await page.locator('#route-dialog .close-route').first().click();
    const initialActivityLoad = testLiveRefresh ? Promise.all([
      page.waitForResponse(response => response.url().includes('/admin/activity/stats?since=')),
      page.waitForResponse(response => response.url().includes('/admin/activity/logs?since=')),
    ]) : null;
    await page.evaluate(() => document.querySelector('[data-view="activity"]').click());
    if (initialActivityLoad) await initialActivityLoad;
    if (testLiveRefresh) {
      const activityRefresh = Promise.all([
        page.waitForResponse(response => response.url().includes('/admin/activity/stats?since=')),
        page.waitForResponse(response => response.url().includes('/admin/activity/logs?since=')),
      ]);
      await page.clock.fastForward(10000);
      await activityRefresh;
    }
    await page.locator('#manage-activity-data').click();
    await assertDialog(page, '#activity-data-dialog', project.name);
    await page.locator('[data-activity-data-tab="import"]').click();
    const previewRecord = {
      timestamp: Math.floor(Date.now() / 1000), request_id: `responsive-preview-${project.name}`,
      path: '/v1/responses', model: 'preview/model', provider: 'preview', endpoint: 'preview',
      status: 200, latency_ms: 1, input_tokens: 0, output_tokens: 0, cached_tokens: 0,
      cost: null, streaming: false,
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
