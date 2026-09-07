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
    await page.goto(base, { waitUntil: 'networkidle' });
    if (await page.locator('#login-screen').isVisible()) {
      await assertNoPageOverflow(page, project.name, 'login page');
      console.log(`${project.name}: login layout checked (authenticated dialog checks skipped)`);
      await context.close();
      continue;
    }

    await page.locator('#open-help').click();
    await assertDialog(page, '#help-dialog', project.name);
    await page.locator('#help-dialog .close-help').first().click();
    await page.evaluate(() => document.querySelector('.open-about').click());
    await assertDialog(page, '#about-dialog', project.name);
    await page.locator('#about-dialog .close-about').first().click();
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
    await page.evaluate(() => document.querySelector('[data-view="activity"]').click());
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
