const { chromium } = require(process.env.REPARTEE_PLAYWRIGHT_MODULE || 'playwright');
const assert = require('node:assert/strict');
async function assertEventually(predicate) {
  for (let attempt = 0; attempt < 500; attempt++) {
    if (predicate()) return;
    await new Promise(resolve => setTimeout(resolve, 20));
  }
  assert.ok(predicate(), 'Daemon did not acknowledge the outgoing command with BufferCreated');
}
(async () => {
  const browser = await chromium.launch({headless: true, executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE});
  try {
    const context = await browser.newContext({ignoreHTTPSErrors: true, viewport: {width: 1100, height: 800}});
    const base = process.env.REPARTEE_DAEMON_TEST_URL;
    const page = await context.newPage();
    const received = new Set();
    const target = `fixture-outgoing-${process.env.REPARTEE_DAEMON_TEST_CYCLE}`;
    const command = `/msg ${target} fixture-browser-outgoing`;
    let outgoingSent = false;
    let commandProcessed = false;
    page.on('websocket', socket => {
      socket.on('framesent', event => {
        const data = JSON.parse(event.payload);
        if (data.type === 'RunCommand' && data.text === command) outgoingSent = true;
      });
      socket.on('framereceived', event => {
        const data = JSON.parse(event.payload);
        if (data.type === 'BufferCreated' && data.buffer.id === `fixture/${target}`) commandProcessed = true;
        if (data.type === 'Messages' && data.buffer_id === 'fixture/history-peer') {
          for (const message of data.messages) received.add(message.text);
        }
      });
    });
    const errors = [];
    page.on('pageerror', error => errors.push(String(error)));
    await page.goto(base);
    await page.locator('input[type=password]').fill('fixture-web-password');
    const loggedIn = page.waitForResponse(response => response.url().endsWith('/api/login'));
    await page.locator('button[type=submit]').click();
    assert.equal((await loggedIn).status(), 200);
    await page.locator('#chat-input').waitFor();
    const peer = page.locator('.buffer-list button:visible').filter({hasText: 'history-peer'});
    await peer.waitFor();
    await peer.click();
    await page.locator('.chat-line').filter({hasText: 'fixture-history-299'}).waitFor();
    for (let attempt = 0; attempt < 12; attempt++) {
      await page.locator('.chat-messages').hover();
      await page.mouse.wheel(0, -4000);
      if (await page.locator('.chat-line').filter({hasText: /^.*fixture-history-0$/}).count()) break;
      await page.waitForTimeout(250);
    }
    await page.locator('.chat-line').filter({hasText: /^.*fixture-history-0$/}).waitFor();
    assert.equal(await page.locator('.chat-line').filter({hasText: 'preserved legacy row'}).count(), 0);
    assert.deepEqual([...received].sort(), Array.from({length: 300}, (_, index) => `fixture-history-${index}`).sort());
    await page.locator('#chat-input').fill(command);
    await page.locator('#chat-input').press('Enter');
    await assertEventually(() => outgoingSent && commandProcessed);
    if (process.env.REPARTEE_HISTORY_FAULT_CONTROL) {
      await require('./daemon-partial-history.cjs')(page);
    }
    assert.deepEqual(errors, []);
    if (process.env.REPARTEE_DAEMON_SCREENSHOT) await page.screenshot({path: process.env.REPARTEE_DAEMON_SCREENSHOT});
    console.log('PASS: actual daemon HTTPS login, compiled Chromium UI and server-backed history scrolling.');
  } finally { await browser.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; });
