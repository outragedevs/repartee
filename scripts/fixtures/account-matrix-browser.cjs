const { chromium } = require(process.env.REPARTEE_PLAYWRIGHT_MODULE || 'playwright');
const assert = require('node:assert/strict');

(async () => {
  const browser = await chromium.launch({headless: true, executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE});
  try {
    const context = await browser.newContext({viewport: {width: 1200, height: 1000}});
    const base = process.env.REPARTEE_BROWSER_FIXTURE_URL;
    await context.addCookies([{name: process.env.REPARTEE_BROWSER_FIXTURE_COOKIE_NAME,
      value: process.env.REPARTEE_BROWSER_FIXTURE_COOKIE, url: base, httpOnly: true, sameSite: 'Strict'}]);
    await context.addInitScript(key => localStorage.setItem(key, '1'), process.env.REPARTEE_BROWSER_FIXTURE_STORAGE_KEY);
    const page = await context.newPage();
    let selected;
    const errors = [];
    page.on('pageerror', error => errors.push(String(error)));
    page.on('websocket', socket => socket.on('framesent', ({payload}) => {
      const command = JSON.parse(String(payload));
      if (command.type === 'SwitchBuffer') selected = command.buffer_id;
    }));
    await page.goto(base);
    await page.locator('#chat-input').waitFor();
    for (let reload = 0; reload < 2; reload++) {
      const seen = new Set();
      const buttons = page.locator('.buffer-list button:visible').filter({has: page.locator('.name').filter({hasText: /^Alice(?: \[pinned\])?$/i})});
      assert.equal(await buttons.count(), 4, JSON.stringify(await page.locator('.buffer-list .name').allTextContents()));
      for (let index = 0; index < 4; index++) {
        selected = undefined;
        await buttons.nth(index).click();
        for (let attempt = 0; attempt < 500 && !selected; attempt++) await page.waitForTimeout(20);
        assert.ok(selected?.toLowerCase().endsWith('/alice'));
        seen.add(selected);
        const text = `matrix-incoming-after-restart-${selected.slice(0, -'/Alice'.length)}`;
        await page.locator('.chat-line').filter({hasText: text}).waitFor();
        const rows = await page.locator('.chat-line').filter({hasText: 'matrix-incoming-after-restart-'}).allTextContents();
        assert.equal(rows.length, 1);
        assert.ok(rows[0].includes(text));
      }
      assert.equal(seen.size, 4);
      if (reload === 0) { await page.reload(); await page.locator('#chat-input').waitFor(); }
    }
    assert.deepEqual(errors, []);
    console.log('PASS: compiled browser UI keeps four identical query names isolated after provider restart and browser reload');
  } finally { await browser.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; });
