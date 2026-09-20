const { webkit } = require(process.env.REPARTEE_PLAYWRIGHT_MODULE || 'playwright');
const assert = require('node:assert/strict');

module.exports = async function run(mode) {
  const browser = await webkit.launch({ headless: true });
  try {
    const page = await browser.newPage({ viewport: { width: 1100, height: 800 } });
    const errors = [];
    let pending;
    page.on('pageerror', error => errors.push(String(error)));
    page.on('websocket', socket => socket.on('framereceived', ({ payload }) => {
      const event = JSON.parse(String(payload));
      if (event.type === 'NewMessage') pending?.(event);
    }));
    const base = process.env.REPARTEE_BROWSER_FIXTURE_URL;
    await page.context().addCookies([{ name: process.env.REPARTEE_BROWSER_FIXTURE_COOKIE_NAME,
      value: process.env.REPARTEE_BROWSER_FIXTURE_COOKIE, url: base, httpOnly: true, sameSite: 'Strict' }]);
    await page.addInitScript(key => localStorage.setItem(key, '1'), process.env.REPARTEE_BROWSER_FIXTURE_STORAGE_KEY);
    await page.goto(base);
    const input = page.locator('#chat-input');
    await input.waitFor();
    async function submit(text, expected) {
      const completed = new Promise((resolve, reject) => {
        const timeout = setTimeout(() => { pending = undefined; reject(new Error(`Missing reply: ${expected}`)); }, 15000);
        pending = event => {
          if (event.message.text.includes(expected) && (mode === 'control' || event.message.nick === 'BouncerServ')) {
            clearTimeout(timeout); pending = undefined; resolve(event);
          }
        };
      });
      await input.fill(`/msg BouncerServ ${text}`);
      await input.press('Enter');
      return completed;
    }
    if (mode === 'soju') {
      await submit(`network create -name 'Browser 100%; test' -addr irc+insecure://127.0.0.1:1 -enabled false`, 'created network "Browser 100%; test"');
      await submit('network status', 'Browser 100%; test (irc+insecure://127.0.0.1:1) [disabled]');
      await submit('nonexistent-browser-command', 'error: command "nonexistent-browser-command" not found');
      await submit("network delete 'unterminated", 'unterminated quoted string');
      await submit("network delete 'Browser 100%; test'", 'deleted network "Browser 100%; test"');
    } else if (mode === 'control') {
      await submit("browser-control '100%; test'", 'Cannot interact with channels and users on the bouncer connection');
    } else {
      await submit(String.raw`browser '100%; test' A\B`, String.raw`upstream received: browser '100%; test' A\B`);
    }
    if (mode !== 'control') {
      await page.locator('.buffer-list button:visible').filter({ hasText: 'BouncerServ' }).click();
      const expected = mode === 'soju' ? 'deleted network "Browser 100%; test"' : String.raw`upstream received: browser '100%; test' A\B`;
      await page.locator('.chat-line').filter({ hasText: expected }).waitFor();
      await page.reload();
      await page.locator('#chat-input').waitFor();
      await page.locator('.buffer-list button:visible').filter({ hasText: 'BouncerServ' }).click();
      await page.locator('.chat-line').filter({ hasText: expected }).waitFor();
      assert.equal(await page.locator('.chat-line').filter({ hasText: expected }).count(), 1);
    }
    assert.deepEqual(errors, []);
    console.log(`PASS: actual WebKit service ${mode} commands and responses${mode === 'control' ? '' : ' with browser reload'}`);
  } finally { await browser.close(); }
};
