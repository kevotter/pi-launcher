import fs from 'node:fs';
import { test, expect } from '@playwright/test';

// The startup page is the first thing a user sees and the last thing that shows
// a reason when the backend cannot start. Both are checked here in real Edge:
// CSP blocks inline scripts, so a future "just paste the snippet" edit would
// otherwise fail silently and leave a blank window.

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
    // The embedded subset must win over whatever serif the machine happens to
    // have, so both weights are checked as loaded rather than merely declared.
    embedded: [...document.fonts].map(face => `${face.family} ${face.weight} ${face.status}`).sort(),
    boldUsable: document.fonts.check('700 40px "Ciallo Serif"'),
    heavyUsable: document.fonts.check('800 40px "Ciallo Serif"'),
    boardFamily: getComputedStyle(document.querySelector('.ciallo')).fontFamily,
  }));
  expect(state.tracks).toBe(17);
  expect(state.letters).toBe(16);
  expect(state.media).toBe(0);
  // The animation is the only progress signal; a status pill broke the picture.
  expect(state.statusLine).toBe(0);
  expect(state.userSelect).toBe('none');
  expect(state.scrolls).toBe(false);
  expect(state.csp).toEqual([]);
  expect(state.embedded).toEqual(['Ciallo Serif 700 loaded', 'Ciallo Serif 800 loaded']);
  expect(state.boldUsable).toBe(true);
  expect(state.heavyUsable).toBe(true);
  expect(state.boardFamily.startsWith('"Ciallo Serif"')).toBe(true);
  expect(errors).toEqual([]);
});

// Everything the page needs must survive the *shipped* header policy, not just
// the meta tag a browser preview obeys. Serving the same build with that header
// reproduced a real regression: fonts were blocked only in the installed app.
test('the packaged header policy still permits the fonts and styles it ships', async ({ page }) => {
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
    embedded: [...document.fonts].map(face => `${face.family} ${face.weight} ${face.status}`).sort(),
    boldUsable: document.fonts.check('700 40px "Ciallo Serif"'),
    heavyUsable: document.fonts.check('800 40px "Ciallo Serif"'),
  }));
  expect(state.csp).toEqual([]);
  expect(state.embedded).toEqual(['Ciallo Serif 700 loaded', 'Ciallo Serif 800 loaded']);
  expect(state.boldUsable).toBe(true);
  expect(state.heavyUsable).toBe(true);
  expect(errors).toEqual([]);
});

test('a click does nothing and selects no text; phases only set an attribute', async ({ page }) => {
  await openStartup(page);
  const before = await page.evaluate(() => document.body.childElementCount);
  await page.mouse.click(180, 300);
  await page.mouse.dblclick(220, 340);
  expect(await page.evaluate(() => document.body.childElementCount)).toBe(before);
  expect(await page.locator('.float, #floats').count()).toBe(0);
  expect(await page.evaluate(() => window.getSelection().toString())).toBe('');

  // What `apply_phase` evaluates: the phase attribute alone.
  await page.evaluate(() => { document.body.dataset.phase = 'backend'; });
  await expect(page.locator('body')).toHaveAttribute('data-phase', 'backend');
  await expect(page.locator('.failure')).toBeHidden();
});

test('the context menu and middle-click autoscroll stay suppressed', async ({ page }) => {
  await openStartup(page);
  // Registered after the page's own listener, so it observes the final
  // `defaultPrevented` value rather than the state before it ran.
  await page.evaluate(() => {
    window.__ctx = null;
    document.addEventListener('contextmenu', event => { window.__ctx = event.defaultPrevented; });
  });

  // This window is an application, not a page: right-click must not open a
  // menu, and middle-click must not start Chromium's autoscroll.
  await page.mouse.click(240, 380, { button: 'right' });
  expect(await page.evaluate(() => window.__ctx)).toBe(true);

  await page.mouse.click(320, 420, { button: 'middle' });
  expect(await page.evaluate(() => window.getSelection().toString())).toBe('');
  expect(await page.locator('.float, #floats').count()).toBe(0);
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
  // ...and the only surface that keeps its context menu: the log path has to be
  // copyable here, everywhere else the window suppresses it.
  await page.evaluate(() => {
    window.__ctxOnFailure = null;
    document.addEventListener('contextmenu', event => { window.__ctxOnFailure = event.defaultPrevented; });
  });
  await page.click('#error-box', { button: 'right' });
  expect(await page.evaluate(() => window.__ctxOnFailure)).toBe(false);
});
