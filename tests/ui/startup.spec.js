import { test, expect } from '@playwright/test';

// The startup page is the first thing a user sees and the last thing that shows
// a reason when the backend cannot start. Both are checked here in real Edge:
// CSP blocks inline scripts, so a future "just paste the snippet" edit would
// otherwise fail silently and leave a blank window.

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

test('startup screen animates without sound, CSP errors or page errors', async ({ page }) => {
  const errors = await openStartup(page);
  const state = await page.evaluate(() => ({
    tracks: document.querySelectorAll('.track').length,
    letters: document.querySelectorAll('.jumper span').length,
    media: document.querySelectorAll('audio, video').length,
    statusLine: document.querySelectorAll('.hint, #status-bar, #status-text').length,
    userSelect: getComputedStyle(document.body).userSelect,
    scrolls: document.documentElement.scrollHeight > innerHeight
      || document.documentElement.scrollWidth > innerWidth,
    csp: window.__csp,
  }));
  expect(state.tracks).toBe(17);
  expect(state.letters).toBe(16);
  expect(state.media).toBe(0);
  // The animation is the only progress signal; a status pill broke the picture.
  expect(state.statusLine).toBe(0);
  expect(state.userSelect).toBe('none');
  expect(state.scrolls).toBe(false);
  expect(state.csp).toEqual([]);
  expect(errors).toEqual([]);
});

test('a click spawns one copy and selects no text; phases only set an attribute', async ({ page }) => {
  await openStartup(page);
  await page.mouse.click(180, 300);
  await expect(page.locator('.float')).toHaveCount(1);
  expect(await page.evaluate(() => window.getSelection().toString())).toBe('');

  // What `apply_phase` evaluates: the phase attribute alone.
  await page.evaluate(() => { document.body.dataset.phase = 'backend'; });
  await expect(page.locator('body')).toHaveAttribute('data-phase', 'backend');
  await expect(page.locator('.failure')).toBeHidden();
});

test('every mouse button spawns a copy and the context menu stays suppressed', async ({ page }) => {
  await openStartup(page);
  // Registered after the page's own listener, so it observes the final
  // `defaultPrevented` value rather than the state before it ran.
  await page.evaluate(() => {
    window.__ctx = null;
    document.addEventListener('contextmenu', event => { window.__ctx = event.defaultPrevented; });
  });

  await page.mouse.click(180, 300);
  await expect(page.locator('.float')).toHaveCount(1);

  // This window is an application, not a page: right-click must not open a
  // menu, and middle-click must not start Chromium's autoscroll.
  await page.mouse.click(240, 380, { button: 'right' });
  await expect(page.locator('.float')).toHaveCount(2);
  expect(await page.evaluate(() => window.__ctx)).toBe(true);

  await page.mouse.click(320, 420, { button: 'middle' });
  await expect(page.locator('.float')).toHaveCount(3);
  expect(await page.evaluate(() => window.getSelection().toString())).toBe('');
});

test('the failure state keeps the reason readable over the animation', async ({ page }) => {
  await openStartup(page);
  // What `apply_error` evaluates.
  await page.evaluate(() => {
    document.body.dataset.phase = 'failed';
    document.getElementById('heading').textContent = '启动失败';
    document.querySelector('.subtitle').textContent = '请检查配置或日志后重试。';
    const box = document.getElementById('error-box');
    box.textContent = 'Pi Web 意外退出：code=1\n\n日志：C:\\Temp\\launcher.log';
    box.hidden = false;
  });
  await expect(page.locator('.failure')).toBeVisible();
  await expect(page.locator('#heading')).toHaveText('启动失败');
  await expect(page.locator('#error-box')).toContainText('launcher.log');
  await expect(page.locator('.stage')).toBeHidden();
  // The only surface where selecting text is intentional (copying the log path).
  expect(await page.locator('#error-box').evaluate(node => getComputedStyle(node).userSelect)).toBe('text');
  // Clicking while reading must not paint floating copies over the message.
  await page.mouse.click(40, 40);
  await expect(page.locator('.float')).toHaveCount(0);
});
