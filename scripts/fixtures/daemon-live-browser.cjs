const { chromium } = require(process.env.REPARTEE_PLAYWRIGHT_MODULE || 'playwright');
const assert = require('node:assert/strict');
(async () => {
  const browser = await chromium.launch({headless: true, executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE});
  try {
    const context = await browser.newContext({ignoreHTTPSErrors: true, viewport: {width: 1100, height: 800}});
    const page = await context.newPage();
    const cycle = process.env.REPARTEE_DAEMON_TEST_CYCLE;
    const memoryStore = process.env.REPARTEE_MEMORY_HISTORY === '1';
    let connected = false;
    const pages = [];
    const live = [];
    const inserted = [];
    const errors = [];
    page.on('pageerror', error => errors.push(String(error)));
    page.on('websocket', socket => socket.on('framereceived', event => {
      const data = JSON.parse(event.payload);
      if (data.type === 'SyncInit') connected = data.connections.some(connection => connection.id === 'fixture' && connection.connected);
      if (data.type === 'ConnectionStatus' && data.conn_id === 'fixture') connected = data.connected;
      if (data.type === 'Messages') pages.push(data);
      if (data.type === 'NewMessage') live.push(data.message.text);
      if (data.type === 'NewMessage' || data.type === 'InsertMessage') inserted.push(data);
    }));
    await page.goto(process.env.REPARTEE_DAEMON_TEST_URL);
    await page.locator('input[type=password]').fill('fixture-web-password');
    const loggedIn = page.waitForResponse(response => response.url().endsWith('/api/login'));
    await page.locator('button[type=submit]').click();
    assert.equal((await loggedIn).status(), 200);
    const input = page.locator('#chat-input');
    await input.waitFor();
    await page.locator('.buffer-list button:visible').filter({has: page.locator('.name').filter({hasText: /^fixture$/})}).click();
    for (let attempt = 0; attempt < 500 && !connected; attempt++) await page.waitForTimeout(20);
    assert.ok(connected, 'Daemon did not connect to the bouncer');
    const warning = page.locator('.chat-line').filter({hasText: 'This connection does not support CHATHISTORY'});
    if (memoryStore) await warning.waitFor();
    else assert.equal(await warning.count(), 0);
    if (cycle === '1') {
      await page.locator('.buffer-list button:visible').filter({has: page.locator('.name').filter({hasText: /^Alice$/i})}).click();
      await page.locator('.chat-line').filter({hasText: 'fixture-memory-offline'}).waitFor();
      if (!memoryStore) {
        await page.locator('.chat-line').filter({hasText: 'fixture-memory-incoming-0'}).waitFor();
        await page.locator('.chat-line').filter({hasText: 'fixture-memory-outgoing-0'}).waitFor();
      }
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
    pages.length = 0;
    await page.reload();
    await input.waitFor();
    await alice.click();
    await page.locator('.chat-line').filter({hasText: `fixture-memory-incoming-${cycle}`}).waitFor();
    await page.locator('.chat-line').filter({hasText: `fixture-memory-outgoing-${cycle}`}).waitFor();
    const historyPages = () => pages.filter(event => event.buffer_id.toLowerCase() === 'fixture/alice');
    for (let attempt = 0; attempt < 40 && !historyPages().some(event => !event.has_more); attempt++) {
      await page.locator('.chat-messages').hover();
      await page.mouse.wheel(0, -1000);
      await page.waitForTimeout(100);
    }
    assert.ok(historyPages().some(event => !event.has_more), 'Older history did not reach exhaustion');
    const texts = new Set(historyPages().flatMap(event => event.messages.map(message => message.text)));
    if (cycle === '1') assert.ok(texts.has('fixture-memory-offline'), 'Offline replay was lost after browser reload');
    assert.ok(texts.has(`fixture-memory-incoming-${cycle}`));
    assert.ok(texts.has(`fixture-memory-outgoing-${cycle}`));
    for (const text of [`fixture-memory-incoming-${cycle}`, `fixture-memory-outgoing-${cycle}`]) {
      assert.equal(await page.locator('.chat-line').filter({hasText: text}).count(), 1, `Duplicate visible message: ${text}`);
    }
    const command = async text => { await input.fill(text); await input.press('Enter'); };
    const searchButton = page.locator('.buffer-list button:visible').filter({has: page.locator('.name').filter({hasText: /^\*search\*$/})});
    const searchSupported = process.env.REPARTEE_PRESENCE_PROVIDER === 'soju' && !memoryStore;
    const beforeSearch = inserted.length;
    const conversation = await page.locator('.chat-line').filter({hasText: 'fixture-memory-'}).allTextContents();
    await command('/bsearch Alice -from Alice -- fixture-memory-');
    if (searchSupported) {
      await page.locator('.chat-line').filter({hasText: `${Number(cycle) * 2 + 1} search results in Alice`}).waitFor();
      assert.equal(await page.locator('.chat-line').filter({hasText: `fixture-memory-incoming-${cycle}`}).count(), 1);
      assert.equal(await page.locator('.chat-line').filter({hasText: 'fixture-memory-outgoing-'}).count(), 0);
      await command('/bsearch context 1');
      await page.locator('.chat-line').filter({hasText: 'context messages in Alice'}).waitFor();
      assert.equal(await page.locator('.chat-line').filter({hasText: `fixture-memory-outgoing-${cycle}`}).count(), 1);
      await command('/bsearch Alice -- absent-fixture-search-token');
      await page.locator('.chat-line').filter({hasText: '0 search results in Alice'}).waitFor();
      assert.equal(await page.locator('.chat-line').filter({hasText: 'fixture-memory-'}).count(), 0);
      await page.reload();
      await input.waitFor();
      await searchButton.click();
      await page.locator('.chat-line').filter({hasText: '0 search results in Alice'}).waitFor();
      await command('/close');
      await searchButton.waitFor({state: 'detached'});
      await alice.click();
      await page.locator('.chat-line').filter({hasText: `fixture-memory-outgoing-${cycle}`}).waitFor();
      assert.deepEqual(await page.locator('.chat-line').filter({hasText: 'fixture-memory-'}).allTextContents(), conversation, 'Search changed the live conversation messages');
    } else {
      await page.locator('.chat-line').filter({hasText: 'Server search requires a connected bouncer network with acknowledged search'}).waitFor();
      assert.equal(await searchButton.count(), 0);
    }
    assert.equal(inserted.slice(beforeSearch).filter(event => event.buffer_id.toLowerCase() === 'fixture/alice' && event.message.text.startsWith('fixture-memory-')).length, 0,
      'Search results entered the live conversation event stream');
    const control = async action => {
      const response = await fetch(`${process.env.REPARTEE_FAULT_CONTROL_URL}/${action}`, {method: 'POST'});
      assert.equal(response.status, 200);
      return response.json();
    };
    assert.ok((await control('cut')).closed >= 1, 'No established transport was interrupted');
    for (let attempt = 0; attempt < 500 && connected; attempt++) await page.waitForTimeout(20);
    assert.equal(connected, false, 'Daemon did not report transport loss');
    await page.reload();
    await input.waitFor();
    await alice.click();
    await page.locator('.chat-line').filter({hasText: `fixture-memory-outgoing-${cycle}`}).waitFor();
    assert.equal(connected, false);
    await command(`/msg Alice fixture-memory-fault-unsent-${cycle}`);
    await page.locator('.chat-line').filter({hasText: 'Failed to send message:'}).waitFor();
    await control('resume');
    for (let attempt = 0; attempt < 1500 && !connected; attempt++) await page.waitForTimeout(20);
    assert.ok(connected, 'Daemon did not reconnect after transport restoration');
    await command(`/msg Alice fixture-memory-reconnected-${cycle}`);
    await page.locator('.chat-line').filter({hasText: `fixture-memory-reconnected-${cycle}`}).waitFor();
    assert.equal(await page.locator('.chat-line').filter({hasText: `fixture-memory-outgoing-${cycle}`}).count(), 1);
    assert.equal(await page.locator('.chat-line').filter({hasText: `fixture-memory-fault-unsent-${cycle}`}).count(), 0);
    assert.deepEqual(errors, []);
    console.log('PASS: real incoming/outgoing messages, restart history, browser reload, exhausted history page, isolated search, transport failure and reconnect.');
  } finally { await browser.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; });
