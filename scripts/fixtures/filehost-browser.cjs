const { webkit } = require(process.env.REPARTEE_PLAYWRIGHT_MODULE || 'playwright');
const assert = require('node:assert/strict');
const https = require('node:https');
const fs = require('node:fs');
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
    const picker = page.locator('input[type=file]');
    await picker.waitFor({ state: 'attached' });
    const body = Buffer.from('real browser to daemon to bouncer upload\n');
    const response = page.waitForResponse(r => r.url().includes('/api/upload?'));
    await picker.setInputFiles({ name: 'zażółć.txt', mimeType: 'text/plain', buffer: body });
    const uploaded = await response;
    assert.equal(uploaded.status(), 201, await uploaded.text());
    const url = await uploaded.text();
    await page.waitForFunction(url => document.querySelector('#chat-input').value === url, url);
    const downloaded = await new Promise((resolve, reject) => {
      https.get(url, { ca: fs.readFileSync(process.env.REPARTEE_FILEHOST_TEST_CA) }, response => {
        if (response.statusCode !== 200) { reject(new Error(`GET returned ${response.statusCode}`)); response.resume(); return; }
        const chunks = []; response.on('data', chunk => chunks.push(chunk));
        response.on('end', () => resolve(Buffer.concat(chunks))); response.on('error', reject);
      }).on('error', reject);
    });
    assert.deepEqual(downloaded, body);
    assert.deepEqual(errors, []);
    console.log('PASS: built WebKit chooser -> authenticated real Repartee HTTP route -> App -> pinned bouncer HTTPS upload -> verified downloaded bytes. No HTTP or WebSocket mocks.');
  } finally { await browser.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; });
