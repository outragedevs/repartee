const { chromium } = require(process.env.REPARTEE_PLAYWRIGHT_MODULE || 'playwright');
const assert = require('node:assert/strict');
(async () => {
  const browser = await chromium.launch({headless: true, executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE});
  try {
    const context = await browser.newContext({ignoreHTTPSErrors: true, viewport: {width: 1100, height: 800}});
    const page = await context.newPage();
    const cycle = process.env.REPARTEE_DAEMON_TEST_CYCLE;
    const pages = [];
    const live = [];
    const errors = [];
    page.on('pageerror', error => errors.push(String(error)));
    page.on('websocket', socket => socket.on('framereceived', event => {
      const data = JSON.parse(event.payload);
      if (data.type === 'Messages') pages.push(data);
      if (data.type === 'NewMessage') live.push(data.message.text);
    }));
    await page.goto(process.env.REPARTEE_DAEMON_TEST_URL);
    await page.locator('input[type=password]').fill('fixture-web-password');
    const loggedIn = page.waitForResponse(response => response.url().endsWith('/api/login'));
    await page.locator('button[type=submit]').click();
    assert.equal((await loggedIn).status(), 200);
    const input = page.locator('#chat-input');
    await input.waitFor();
    await page.locator('.buffer-list button:visible').filter({has: page.locator('.name').filter({hasText: /^fixture$/})}).click();
    await page.locator('.chat-line').filter({hasText: 'This connection does not support CHATHISTORY'}).waitFor();
    if (cycle === '1') {
      await page.locator('.buffer-list button:visible').filter({has: page.locator('.name').filter({hasText: /^Alice$/i})}).click();
      await page.locator('.chat-line').filter({hasText: 'fixture-memory-offline'}).waitFor();
    }
    await input.fill(`/msg FixtureControl memory-history-${cycle}`);
    await input.press('Enter');
    for (let attempt = 0; attempt < 500 && !live.includes(`fixture-memory-incoming-${cycle}`); attempt++) {
      await page.waitForTimeout(20);
    }
    assert.ok(live.includes(`fixture-memory-incoming-${cycle}`), 'Incoming message did not reach the browser');
    const alice = page.locator('.buffer-list button:visible').filter({has: page.locator('.name').filter({hasText: /^Alice$/i})});
    await alice.waitFor();
    await alice.click();
    await page.locator('.chat-line').filter({hasText: `fixture-memory-incoming-${cycle}`}).waitFor();
    await input.fill(`fixture-memory-outgoing-${cycle}`);
    await input.press('Enter');
    await page.locator('.chat-line').filter({hasText: `fixture-memory-outgoing-${cycle}`}).waitFor();
    await page.reload();
    await input.waitFor();
    await alice.click();
    await page.locator('.chat-line').filter({hasText: `fixture-memory-incoming-${cycle}`}).waitFor();
    await page.locator('.chat-line').filter({hasText: `fixture-memory-outgoing-${cycle}`}).waitFor();
    const history = pages.filter(event => event.buffer_id.toLowerCase() === 'fixture/alice').at(-1);
    assert.ok(history, 'Browser did not fetch in-memory messages');
    if (cycle === '1') assert.ok(history.messages.some(message => message.text === 'fixture-memory-offline'), 'Offline replay was lost after browser reload');
    assert.equal(history.has_more, false, 'No-CHATHISTORY connection offers unavailable older history');
    assert.ok(history.messages.some(message => message.text === `fixture-memory-incoming-${cycle}`));
    assert.ok(history.messages.some(message => message.text === `fixture-memory-outgoing-${cycle}`));
    assert.deepEqual(errors, []);
    console.log('PASS: no-CHATHISTORY warning, real incoming/outgoing messages, browser reload, exhausted history page.');
  } finally { await browser.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; });
