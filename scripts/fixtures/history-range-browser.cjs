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
    for (const [first, last, expected] of [[10, 20, [11, 12, 13]], [20, 10, [17, 18, 19]]]) {
      await input.fill(`/bsearch between history-peer 2024-01-01T00:00:${first}Z 2024-01-01T00:00:${last}Z 3`);
      await input.press('Enter');
      await page.locator('.buffer-list button:visible').filter({ hasText: '*search*' }).click();
      await page.locator('.chat-line').filter({ hasText: `fixture-history-${expected[2]}` }).waitFor();
      const rows = await page.locator('.chat-line').allTextContents();
      const indices = rows.flatMap(row => Array.from(row.matchAll(/fixture-history-(\d+)/g), match => Number(match[1])));
      assert.deepEqual(indices, expected);
    }
    await page.reload();
    await page.locator('#chat-input').waitFor();
    await page.locator('.buffer-list button:visible').filter({ hasText: '*search*' }).click();
    await page.locator('.chat-line').filter({ hasText: 'fixture-history-19' }).waitFor();
    assert.equal(await page.locator('.chat-line').filter({ hasText: 'fixture-history-19' }).count(), 1);
    assert.deepEqual(errors, []);
    console.log('PASS: actual WebKit bounded history in both directions and reload');
  } finally { await browser.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; });
