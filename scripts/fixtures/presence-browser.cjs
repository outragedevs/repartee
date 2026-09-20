const { chromium } = require(process.env.REPARTEE_PLAYWRIGHT_MODULE || 'playwright');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawn } = require('node:child_process');

async function until(predicate, label) {
  const deadline = Date.now() + 12000;
  while (Date.now() < deadline) {
    if (await predicate()) return;
    await new Promise(resolve => setTimeout(resolve, 30));
  }
  throw new Error(`Timed out: ${label}`);
}

(async () => {
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'bouncer-presence-browser-'));
  const chrome = spawn(process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE || chromium.executablePath(),
    ['--no-first-run', '--no-default-browser-check', `--user-data-dir=${profile}`, '--remote-debugging-port=0', 'about:blank'], { stdio: 'ignore' });
  let browser;
  try {
    const endpoint = path.join(profile, 'DevToolsActivePort');
    await until(() => fs.existsSync(endpoint), 'Chromium debugging endpoint');
    const port = fs.readFileSync(endpoint, 'utf8').split('\n')[0];
    browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`, { noDefaults: true });
    const context = browser.contexts()[0];
    const base = process.env.REPARTEE_BROWSER_FIXTURE_URL;
    await context.addCookies([{ name: process.env.REPARTEE_BROWSER_FIXTURE_COOKIE_NAME,
      value: process.env.REPARTEE_BROWSER_FIXTURE_COOKIE, url: base, httpOnly: true, sameSite: 'Strict' }]);
    await context.addInitScript(key => localStorage.setItem(key, '1'), process.env.REPARTEE_BROWSER_FIXTURE_STORAGE_KEY);
    const errors = [];
    async function client() {
      const page = await context.newPage();
      const reports = [];
      page.on('pageerror', error => errors.push(String(error)));
      page.on('websocket', socket => socket.on('framesent', ({ payload }) => {
        const command = JSON.parse(String(payload));
        if (command.type === 'Presence') reports.push(command.present);
      }));
      await page.goto(base);
      await page.locator('#chat-input').waitFor();
      return { page, reports };
    }
    async function present(client, expected) {
      await until(async () => {
        const actual = await client.page.evaluate(() => ({ hidden: document.hidden, focus: document.hasFocus() }));
        return (!actual.hidden && actual.focus) === expected && client.reports.at(-1) === expected;
      }, `actual document state and WebSocket presence ${expected}`).catch(async error => {
        console.log('presence diagnostic', expected, await client.page.evaluate(() => ({ hidden: document.hidden, focus: document.hasFocus() })), client.reports);
        throw error;
      });
    }
    async function upstream(away) {
      const effective = process.env.REPARTEE_PRESENCE_AUTO_AWAY === 'true' && away;
      await until(() => {
        const rows = fs.readFileSync(process.env.REPARTEE_PRESENCE_EVENTS, 'utf8').trim().split('\n').map(JSON.parse);
        return rows.length && (rows.at(-1).away !== null) === effective;
      }, `upstream AWAY ${effective}`);
    }
    const first = await client();
    await first.page.bringToFront();
    await present(first, true);
    await upstream(false);
    const second = await client();
    await second.page.bringToFront();
    await present(first, false);
    await present(second, true);
    await upstream(false);
    const blank = await context.newPage();
    await blank.goto('about:blank');
    await blank.bringToFront();
    await present(first, false);
    await present(second, false);
    assert.equal(await first.page.evaluate(() => document.hidden), true);
    assert.equal(await second.page.evaluate(() => document.hidden), true);
    await upstream(true);
    await first.page.bringToFront();
    await present(first, true);
    await upstream(false);
    await first.page.close();
    await blank.bringToFront();
    await present(second, false);
    await upstream(true);
    await second.page.bringToFront();
    await present(second, true);
    await upstream(false);
    assert.deepEqual(errors, []);
    console.log('PASS: real headed Chromium tab focus/blur/hidden transitions -> built WASM Presence frames -> App aggregation -> pinned bouncer -> upstream AWAY; two browser clients and closed-tab cleanup.');
  } finally {
    await browser?.close();
    chrome.kill('SIGTERM');
    await until(() => chrome.exitCode !== null || chrome.signalCode !== null, 'Chromium shutdown');
    fs.rmSync(profile, { recursive: true, force: true });
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
