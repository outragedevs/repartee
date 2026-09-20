import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
const [source, ready, upstream, httpPort] = process.argv.slice(2);
process.env.DATABASE_PATH = path.join(path.dirname(ready), 'lurker.db');
process.env.SESSION_SECRET = 'disposable-presence-fixture-secret';
process.env.LURKER_BOUNCER_ENABLED = 'true';
if (httpPort) {
  process.env.PUBLIC_BASE_URL = `https://127.0.0.1:${httpPort}`;
  process.env.LOCAL_UPLOADS_DIR = path.join(path.dirname(ready), 'uploads');
}
const load = (name: string) => import(pathToFileURL(path.join(source, name)).href);
const harnessModule = await load('server/test-utils/bouncerHarness.ts');
const account = harnessModule.seedAccount({ password: 'fixture-password', nick: 'fixture' });
const { default: manager } = await load('server/services/ircManager.ts');
const networks = await load('server/db/networks.ts');
const settings = await load('server/db/settings.ts');
manager.connectionsForUser(account.user.id).delete(account.network.id);
networks.updateNetwork(account.network.id, account.user.id, { host: '127.0.0.1', port: Number(upstream), tls: false });
settings.setUserSetting(account.user.id, 'away.auto.enabled', true);
settings.setUserSetting(account.user.id, 'away.auto.delay_seconds', 1);
if (httpPort) {
  const { listInstanceUploaders } = await load('server/db/uploaderConfig.ts');
  settings.setUserSetting(account.user.id, 'uploads.uploader_id', listInstanceUploaders().find((row: any) => row.driver === 'local').id);
  const { buildApp } = await load('server/app.ts');
  const { createServer } = await import('node:https');
  const clientDist = path.join(path.dirname(ready), 'client');
  fs.mkdirSync(clientDist);
  fs.writeFileSync(path.join(clientDist, 'index.html'), '<!doctype html>');
  const server = createServer({ key: fs.readFileSync(path.join(path.dirname(ready), 'key.pem')),
    cert: fs.readFileSync(path.join(path.dirname(ready), 'cert.pem')) }, buildApp(process.env.SESSION_SECRET, { clientDist }));
  await new Promise<void>(resolve => server.listen(Number(httpPort), '127.0.0.1', resolve));
}
const harness = await harnessModule.startHarness();
const connection = manager.startNetwork(account.user.id, account.network.id);
if (!connection) throw new Error('Connection refused by test account configuration');
fs.writeFileSync(ready, JSON.stringify({ port: harness.port, user: account.user.username }));
process.on('SIGTERM', () => { harness.stop(); connection.dispose(); process.exit(0); });
