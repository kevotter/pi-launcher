import fs from 'node:fs';
import { test, expect } from '@playwright/test';

// The startup page is the first thing a user sees and the last thing that shows
// a reason when the backend cannot start. Both are checked here in real Edge.

// The webview applies `app.security.csp` from tauri.conf.json as a response
// header *on top of* the page's meta tag, and the two are intersected. A
// directive missing from the conf therefore blocks a request that meta allows —
// and only inside an installed build, which is exactly where nobody looks while
// developing. Reading it from the conf keeps this test honest about what ships.
const HEADER_CSP = JSON.parse(fs.readFileSync('src-tauri/tauri.conf.json', 'utf8')).app.security.csp;

async function openStartup(page) {
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.addInitScript(() => {
    window.__csp = [];
    document.addEventListener('securitypolicyviolation', event => {
      window.__csp.push(`${event.violatedDirective} ${event.blockedURI}`);
    });
  });
  await page.goto('/index.html');
  await page.evaluate(() => document.fonts.ready);
  return errors;
}

test('startup screen shows progress without media, CSP errors or page errors', async ({ page }) => {
  const errors = await openStartup(page);
  const state = await page.evaluate(() => ({
    cards: document.querySelectorAll('.card').length,
    spinners: document.querySelectorAll('.spinner').length,
    tracks: document.querySelectorAll('.track').length,
    steps: document.querySelectorAll('[data-step]').length,
    statusText: document.getElementById('status-text').textContent,
    endpoint: document.getElementById('endpoint').textContent,
    media: document.querySelectorAll('audio, video').length,
    scrolls: document.documentElement.scrollHeight > innerHeight
      || document.documentElement.scrollWidth > innerWidth,
    csp: window.__csp,
  }));
  expect(state.cards).toBe(1);
  expect(state.spinners).toBe(1);
  expect(state.tracks).toBe(1);
  expect(state.steps).toBe(3);
  // The first phase is painted by Rust, so the markup ships the same wording
  // instead of an empty line that only looks right in the app.
  expect(state.statusText).toBe('正在检查运行环境');
  expect(state.endpoint).toBe('127.0.0.1');
  expect(state.media).toBe(0);
  expect(state.scrolls).toBe(false);
  expect(state.csp).toEqual([]);
  expect(errors).toEqual([]);
});

// Everything the page needs must survive the *shipped* header policy, not just
// the meta tag a browser preview obeys.
test('the packaged header policy still permits what the page ships', async ({ page }) => {
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.addInitScript(() => {
    window.__csp = [];
    document.addEventListener('securitypolicyviolation', event => {
      window.__csp.push(`${event.violatedDirective} ${event.blockedURI}`);
    });
  });
  // Only documents carry the policy; adding it to subresources changes nothing.
  await page.route('**/*', async route => {
    if (route.request().resourceType() !== 'document') return route.continue();
    const response = await route.fetch();
    await route.fulfill({
      response,
      headers: { ...response.headers(), 'content-security-policy': HEADER_CSP },
    });
  });

  await page.goto('/index.html');
  await page.evaluate(() => document.fonts.ready);
  const state = await page.evaluate(() => ({
    csp: window.__csp,
    // A blocked image or webfont shows up as a zero-width load, not an error.
    iconLoaded: [...document.images].every(image => image.naturalWidth > 0),
  }));
  expect(state.csp).toEqual([]);
  expect(state.iconLoaded).toBe(true);
  expect(errors).toEqual([]);
});

test('apply_phase writes the status line, endpoint and current step', async ({ page }) => {
  await openStartup(page);
  // The same payload and script `emit_phase`/`apply_phase` evaluate.
  await page.evaluate(() => {
    const state = { phase: 'backend', status: '正在连接本地服务', endpoint: '127.0.0.1:30141' };
    document.body.dataset.phase = state.phase;
    document.getElementById('status-text').textContent = state.status;
    document.getElementById('endpoint').textContent = state.endpoint;
    const steps = document.querySelectorAll('[data-step]');
    for (const step of steps) {
      if (step.dataset.step === state.phase) step.setAttribute('aria-current', 'step');
      else step.removeAttribute('aria-current');
    }
  });
  await expect(page.locator('#status-text')).toHaveText('正在连接本地服务');
  await expect(page.locator('#endpoint')).toHaveText('127.0.0.1:30141');
  await expect(page.locator('body')).toHaveAttribute('data-phase', 'backend');
  await expect(page.locator('[data-step="backend"]')).toHaveAttribute('aria-current', 'step');
  await expect(page.locator('[data-step="environment"]')).not.toHaveAttribute('aria-current', 'step');
  // Earlier steps read as done rather than current: the dot swaps its number for
  // a check mark, which is the only thing the CSS keys on.
  expect(await page.locator('[data-step="environment"] .step-dot').evaluate(node => getComputedStyle(node, '::after').content)).toContain('✓');
});

test('the failure state stops the spinner and keeps the reason readable', async ({ page }) => {
  await openStartup(page);
  // What `apply_error` evaluates.
  await page.evaluate(() => {
    const state = {
      message: 'Pi Web 意外退出：code=1\n\n日志：C:\\Temp\\launcher.log',
      heading: '启动失败',
      hint: '请检查配置或日志后重试。',
    };
    document.body.dataset.phase = 'failed';
    document.getElementById('heading').textContent = state.heading;
    document.querySelector('.subtitle').textContent = state.hint;
    const box = document.getElementById('error-box');
    box.textContent = state.message;
    box.hidden = false;
  });
  await expect(page.locator('#heading')).toHaveText('启动失败');
  await expect(page.locator('#error-box')).toContainText('launcher.log');
  // Nothing keeps implying progress once the start has failed.
  await expect(page.locator('.status')).toBeHidden();
  await expect(page.locator('.track')).toBeHidden();
  await expect(page.locator('.steps')).toBeHidden();
  // The log path has to stay copyable: this card never disables selection.
  expect(await page.locator('#error-box').evaluate(node => getComputedStyle(node).userSelect)).not.toBe('none');
});
