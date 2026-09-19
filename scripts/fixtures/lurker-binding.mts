import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const [source, readyFile] = process.argv.slice(2);
process.env.DATABASE_PATH = path.join(path.dirname(readyFile), 'lurker.db');
process.env.SESSION_SECRET = 'disposable-bouncer-binding-test-session-secret';
process.env.LURKER_BOUNCER_ENABLED = 'true';
const harnessModule = await import(pathToFileURL(path.join(source, 'server/test-utils/bouncerHarness.ts')).href);
const account = harnessModule.seedAccount({ password: 'fixture-password', nick: 'tester' });
const harness = await harnessModule.startHarness();
fs.writeFileSync(readyFile, JSON.stringify({ port: harness.port, network: account.network.id, user: account.user.username }));
process.on('SIGTERM', () => { harness.stop(); process.exit(0); });
