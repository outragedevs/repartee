const assert = require('node:assert/strict');

async function until(predicate, label) {
  const deadline = Date.now() + 30000;
  while (Date.now() < deadline) {
    if (await predicate()) return;
    await new Promise(resolve => setTimeout(resolve, 30));
  }
  throw new Error(`Timed out: ${label}`);
}

module.exports = async page => {
  let connected = true;
  page.on('websocket', socket => socket.on('framereceived', ({ payload }) => {
    const event = JSON.parse(String(payload));
    if (event.type === 'ConnectionStatus' && event.conn_id === 'fixture') connected = event.connected;
  }));
  await page.reload();
  const input = page.locator('#chat-input');
  await input.waitFor();
  await page.locator('.buffer-list button:visible').filter({ hasText: 'history-peer' }).click();
  const scenarios = [[process.env.REPARTEE_HISTORY_FAULT_CONTROL,
    '/bsearch between history-peer 2024-01-01T00:00:10Z 2024-01-01T00:00:20Z 3', [11, 12, 13]]];
  if (process.env.REPARTEE_SEARCH_FAULT_CONTROL) scenarios.push([process.env.REPARTEE_SEARCH_FAULT_CONTROL,
    '/bsearch history-peer -after 2024-01-01T00:00:10Z -before 2024-01-01T00:00:20Z -limit 100 -- fixture-history',
    Array.from({length: 9}, (_, i) => i + 11)]);
  for (const [control, command, expected] of scenarios) {
    const call = async path => {
      const response = await fetch(control + path, { method: 'POST' });
      assert.equal(response.status, 200);
      return response.json();
    };
    assert.equal((await call('/arm')).armed, true);
    await input.fill(command);
    await input.press('Enter');
    await until(async () => (await call('/status')).faulted && !connected, 'real partial response and disconnect');
    const status = await call('/status');
    assert.equal(status.requested, true);
    assert.equal(status.batch_opened, true);
    assert.equal(status.forwarded_rows, 1);
    await page.locator('.buffer-list button:visible').filter({ hasText: '*search*' }).click();
    const indices = async () => (await page.locator('.chat-line').allTextContents())
      .flatMap(row => Array.from(row.matchAll(/fixture-history-(\d+)/g), match => Number(match[1])));
    assert.deepEqual(await indices(), [], 'partial batch must not appear as complete search results');
    assert.equal((await call('/resume')).resumed, true);
    await until(() => connected, 'automatic transport reconnect');
    await input.fill(command);
    await input.press('Enter');
    await until(async () => (await indices()).length === expected.length, 'retried history results');
    assert.deepEqual(await indices(), expected);
    await page.reload();
    await input.waitFor();
    await page.locator('.buffer-list button:visible').filter({ hasText: '*search*' }).click();
    await until(async () => (await indices()).length === expected.length, 'reloaded transient results');
    assert.deepEqual(await indices(), expected);
  }
  console.log('PASS: actual daemon discarded an interrupted provider batch after one wire row, reconnected and retried without duplicate/partial UI rows; SQLite and TRACE are inspected by the parent.');
};
