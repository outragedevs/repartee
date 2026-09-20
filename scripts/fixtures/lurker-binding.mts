import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const [source, readyFile] = process.argv.slice(2);
process.env.DATABASE_PATH = path.join(path.dirname(readyFile), 'lurker.db');
process.env.SESSION_SECRET = 'disposable-bouncer-binding-test-session-secret';
process.env.LURKER_BOUNCER_ENABLED = 'true';
const harnessModule = await import(pathToFileURL(path.join(source, 'server/test-utils/bouncerHarness.ts')).href);
const account = harnessModule.seedAccount({ password: 'fixture-password', nick: 'tester' });
if (process.env.REPARTEE_BOUNCER_DAEMON_FIXTURE === '1') account.upstream.state = 'connecting';
const messages = await import(pathToFileURL(path.join(source, 'server/db/messages.ts')).href);
const tie = process.env.REPARTEE_BOUNCER_TARGET_TIE === '1';
const targets = tie ? Array.from({length: 1001}, (_, index) => `peer-${String(index).padStart(4, '0')}`) : ['history-peer', '#history-channel'];
for (const target of targets) {
for (let index = 0; index < (tie ? 1 : 300); index += 1) {
  messages.insertMessage({
    networkId: account.network.id,
    target,
    time: new Date(Date.UTC(2024, 0, 1) + index * 1000).toISOString(),
    type: 'message',
    nick: tie ? target : 'history-peer',
    userhost: 'user@fixture.local',
    text: `fixture-history-${index}`,
    msgid: `fixture-${target}-${index}`,
  });
}
}
const harness = await harnessModule.startHarness();
fs.writeFileSync(readyFile, JSON.stringify({ port: harness.port, network: account.network.id, user: account.user.username }));
process.on('SIGTERM', () => { harness.stop(); process.exit(0); });
