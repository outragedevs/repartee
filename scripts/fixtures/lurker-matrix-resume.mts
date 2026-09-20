import fs from 'node:fs';
import path from 'node:path';
import { once } from 'node:events';
import { pathToFileURL } from 'node:url';

const [source, readyFile] = process.argv.slice(2);
const settings = JSON.parse(fs.readFileSync(readyFile, 'utf8'));
process.env.DATABASE_PATH = path.join(path.dirname(readyFile), 'lurker.db');
process.env.SESSION_SECRET = 'disposable-bouncer-binding-test-session-secret';
process.env.LURKER_BOUNCER_ENABLED = 'true';
process.env.LURKER_BOUNCER_TLS = 'off';
const load = (name: string) => import(pathToFileURL(path.join(source, name)).href);
const { FakeUpstream } = await load('server/test-utils/bouncerHarness.ts');
const { default: manager } = await load('server/services/ircManager.ts');
const { findUserByUsername } = await load('server/db/users.ts');
const { listNetworksForUser } = await load('server/db/networks.ts');
const { startBouncer, stopBouncer } = await load('server/services/bouncer.ts');
for (const account of settings.accounts) {
  const user = findUserByUsername(account.user);
  for (const network of listNetworksForUser(user.id)) {
    const upstream = new FakeUpstream('tester');
    upstream.network = network;
    manager.connectionsForUser(user.id).set(network.id, upstream);
  }
}
const server = await startBouncer(settings.port, '127.0.0.1');
if (!server.listening) await once(server, 'listening');
fs.writeFileSync(path.join(path.dirname(readyFile), 'resume-ready.json'), '{}');
process.on('SIGTERM', () => { stopBouncer(); process.exit(0); });
