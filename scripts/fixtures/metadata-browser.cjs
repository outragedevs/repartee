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
      if (event.type === 'NewMessage' && /^Metadata .+: pinned=/.test(event.message.text)) pendingReply?.();
    }));
    page.on('pageerror', error => errors.push(String(error)));
    const base = process.env.REPARTEE_BROWSER_FIXTURE_URL;
    await page.context().addCookies([{ name: process.env.REPARTEE_BROWSER_FIXTURE_COOKIE_NAME,
      value: process.env.REPARTEE_BROWSER_FIXTURE_COOKIE, url: base, httpOnly: true, sameSite: 'Strict' }]);
    await page.addInitScript(key => localStorage.setItem(key, '1'), process.env.REPARTEE_BROWSER_FIXTURE_STORAGE_KEY);
    await page.goto(base);
    const input = page.locator('#chat-input');
    await input.waitFor();
    async function command(text) {
      let completed;
      if (text.startsWith('/bmeta ')) completed = new Promise((resolve, reject) => {
        const timeout = setTimeout(() => { pendingReply = undefined; reject(new Error('Metadata operation did not complete')); }, 10000);
        pendingReply = () => { clearTimeout(timeout); pendingReply = undefined; resolve(); };
      });
      await input.fill(text);
      await input.press('Enter');
      if (completed) await completed;
    }
    const names = () => page.locator('.buffer-list .name:visible').allTextContents();
    await command('/bmeta #search pin on');
    await page.locator('.buffer-list .name:visible').filter({ hasText: '#search [pinned]' }).waitFor();
    await command('/bmeta #search mute on');
    await page.locator('.buffer-list .name:visible').filter({ hasText: '#search [pinned] [muted]' }).waitFor();
    await command('/query Alice');
    await page.locator('.buffer-list .name:visible').filter({ hasText: /^Alice$/ }).waitFor();
    await command('/bmeta #search pin off');
    await page.locator('.buffer-list .name:visible').filter({ hasText: /^#search \[muted\]$/ }).waitFor();
    await command('/bmeta Alice pin on');
    await page.locator('.buffer-list .name:visible').filter({ hasText: /^Alice \[pinned\]$/ }).waitFor();
    let order = await names();
    assert.ok(order.indexOf('Alice [pinned]') < order.indexOf('#search [muted]'));
    await command('/bmeta Alice block on');
    await page.locator('.buffer-list .name:visible').filter({ hasText: /^Alice \[pinned\] \[blocked\]$/ }).waitFor();
    await page.locator('.buffer-list button:visible').filter({ hasText: '#search' }).click();
    await page.locator('.chat-line').filter({ hasText: 'needle second' }).waitFor();
    assert.equal(await page.locator('.chat-line').filter({ hasText: 'needle first 100%' }).count(), 0);
    await command('/quote PRIVMSG FixtureControl :metadata-messages');
    await page.locator('.chat-line').filter({ hasText: 'unblocked live metadata message' }).waitFor();
    assert.equal(await page.locator('.chat-line').filter({ hasText: /: blocked live metadata message/ }).count(), 0);
    await page.reload();
    await page.locator('.buffer-list .name:visible').filter({ hasText: /^Alice \[pinned\] \[blocked\]$/ }).waitFor();
    await page.locator('.buffer-list button:visible').filter({ hasText: '#search' }).click();
    await page.locator('.chat-line').filter({ hasText: 'needle second' }).waitFor();
    assert.equal(await page.locator('.chat-line').filter({ hasText: 'needle first 100%' }).count(), 0);
    await command('/bmeta Alice clear');
    await page.locator('.buffer-list .name:visible').filter({ hasText: /^Alice$/ }).waitFor();
    await command('/bmeta #search clear');
    await page.locator('.buffer-list .name:visible').filter({ hasText: /^#search$/ }).waitFor();
    assert.deepEqual(errors, []);
    console.log('PASS: real WebKit -> Repartee -> pinned Soju metadata, independent flags, ordering, blocked rows and reload. No HTTP or WebSocket mocks.');
  } finally { await browser.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; });
