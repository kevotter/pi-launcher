import { test, expect } from '@playwright/test';

// Smoke only. Does the document load, does its script run without throwing,
// and did the CSP block anything it needs. Nothing here describes how a screen
// should look or behave — that belongs in `tests/feature`, which is disposable
// once the feature has been checked by hand.
//
// Run with `npm run test:smoke`. This file is meant to survive; feature specs
// are not.
//
// One project is enough: smoke asks whether the document boots, not how it
// looks at a given DPI, so the colour/scale variants would only triple the run.
for (const path of ['/index.html', '/tray.html']) {
  test(`${path} opens with no page error and no blocked request`, async ({ page }, testInfo) => {
    test.skip(testInfo.project.name !== 'light', 'smoke runs once');
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.addInitScript(() => {
      window.__blocked = [];
      document.addEventListener('securitypolicyviolation', event => {
        window.__blocked.push(`${event.violatedDirective} ${event.blockedURI}`);
      });
    });

    const response = await page.goto(path);
    expect(response?.status()).toBe(200);

    const state = await page.evaluate(() => ({
      blocked: window.__blocked,
      // A document that threw while booting renders an empty body; this is the
      // cheapest signal that the script actually got somewhere.
      painted: document.body.childElementCount > 0,
    }));
    expect(state.blocked).toEqual([]);
    expect(state.painted).toBe(true);
    expect(errors).toEqual([]);
  });
}
