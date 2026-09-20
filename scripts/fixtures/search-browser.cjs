const { webkit } = require(process.env.REPARTEE_PLAYWRIGHT_MODULE || 'playwright');
const assert = require('node:assert/strict');
(async () => {
  const browser = await webkit.launch({ headless: true });
  try {
    const page = await browser.newPage({ viewport: { width: 1100, height: 800 } });
    const errors = [];
    page.on('pageerror', error => errors.push(String(error)));
    const base = process.env.REPARTEE_BROWSER_FIXTURE_URL;
    await page.context().addCookies([{ name: process.env.REPARTEE_BROWSER_FIXTURE_COOKIE_NAME,
      value: process.env.REPARTEE_BROWSER_FIXTURE_COOKIE, url: base, httpOnly: true, sameSite: 'Strict' }]);
    await page.addInitScript(key => localStorage.setItem(key, '1'), process.env.REPARTEE_BROWSER_FIXTURE_STORAGE_KEY);
    await page.goto(base);
    const input = page.locator('#chat-input');
    await input.waitFor();
    await input.fill('/bsearch #search -from Alice -- needle');
    await input.press('Enter');
    await page.locator('.chat-line').filter({ hasText: '1 search results in #search' }).waitFor();
    assert.equal(await page.locator('.chat-line').filter({ hasText: 'needle first 100%' }).count(), 1);
    assert.equal(await page.locator('.chat-line').filter({ hasText: 'needle second' }).count(), 0);
    await input.fill('/bsearch context 1');
    await input.press('Enter');
    await page.locator('.chat-line').filter({ hasText: 'context messages in #search' }).waitFor();
    assert.equal(await page.locator('.chat-line').filter({ hasText: 'needle second' }).count(), 1);
    await input.fill('/bsearch #search -- nonexistent-token');
    await input.press('Enter');
    await page.locator('.chat-line').filter({ hasText: '0 search results in #search' }).waitFor();
    assert.equal(await page.locator('.chat-line').filter({ hasText: 'needle first 100%' }).count(), 0);
    await page.reload();
    await page.locator('.buffer-list button:visible').filter({ hasText: '*search*' }).click();
    await page.locator('.chat-line').filter({ hasText: '0 search results in #search' }).waitFor();
    await input.fill('/close');
    await input.press('Enter');
    await page.waitForFunction(() => !Array.from(document.querySelectorAll('.buffer-list .name')).some(node => node.textContent === '*search*'));
    assert.ok(await page.locator('.buffer-list button').filter({ hasText: '#search' }).count());
    assert.deepEqual(errors, []);
    console.log('PASS: real WebKit command -> Repartee WebSocket -> pinned Soju SEARCH -> dedicated result view, replacement, empty results and reload. No HTTP or WebSocket mocks.');
  } finally { await browser.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; });
