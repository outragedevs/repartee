const { webkit } = require(process.env.REPARTEE_PLAYWRIGHT_MODULE || 'playwright');
const assert = require('node:assert/strict');
(async () => {
  const browser = await webkit.launch({ headless: true });
  try {
    const page = await browser.newPage({ viewport: { width: 1100, height: 800 } });
    const errors = [];
    let pendingReply;
    page.on('websocket', socket => socket.on('framereceived', ({ payload }) => {
      const event = JSON.parse(String(payload));
      if (event.type === 'NewMessage') pendingReply?.(event.message.text);
    }));
    page.on('pageerror', error => errors.push(String(error)));
    const base = process.env.REPARTEE_BROWSER_FIXTURE_URL;
    await page.context().addCookies([{ name: process.env.REPARTEE_BROWSER_FIXTURE_COOKIE_NAME,
      value: process.env.REPARTEE_BROWSER_FIXTURE_COOKIE, url: base, httpOnly: true, sameSite: 'Strict' }]);
    await page.addInitScript(key => localStorage.setItem(key, '1'), process.env.REPARTEE_BROWSER_FIXTURE_STORAGE_KEY);
    await page.goto(base);
    const input = page.locator('#chat-input');
    await input.waitFor();
    async function command(text, expected) {
      const completed = new Promise((resolve, reject) => {
        const timeout = setTimeout(() => { pendingReply = undefined; reject(new Error('Certificate operation did not complete')); }, 10000);
        pendingReply = message => {
          if (expected.test(message)) { clearTimeout(timeout); pendingReply = undefined; resolve(); }
        };
      });
      await input.fill(text);
      await input.press('Enter');
      await completed;
    }
    await command('/bcert list', /^Pinned certificates: 1$/);
    await command('/bcert delete', /^Certificate removed/);
    await command('/bcert list', /^Pinned certificates: 0$/);
    await command('/bcert create Web device 100% ; test', /^Current TLS client certificate pinned/);
    await command('/bcert list', /^Pinned certificates: 1$/);
    await page.locator('.chat-line').filter({ hasText: 'name=Web device 100% ; test' }).waitFor();
    await page.reload();
    await page.locator('#chat-input').waitFor();
    await page.locator('.chat-line').filter({ hasText: 'name=Web device 100% ; test' }).waitFor();
    assert.deepEqual(errors, []);
    console.log('PASS: real WebKit -> Repartee -> pinned Soju certificate list/create/delete, literal names and reload. No HTTP or WebSocket mocks.');
  } finally { await browser.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; });
