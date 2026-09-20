import fs from 'node:fs';
import path from 'node:path';
import {pathToFileURL} from 'node:url';
const [source, directory] = process.argv.slice(2);
process.env.DATABASE_PATH = path.join(directory, 'lurker.db');
process.env.SESSION_SECRET = 'disposable-filtered-target-probe';
process.env.LURKER_BOUNCER_ENABLED = 'true';
process.env.LURKER_BOUNCER_TLS = 'off';
const load = (name: string) => import(pathToFileURL(path.join(source, name)).href);
const harnessModule = await load('server/test-utils/bouncerHarness.ts');
const {insertMessage} = await load('server/db/messages.ts');
const buffers = await load('server/db/buffers.ts');
const account = harnessModule.seedAccount({password:'fixture-password',nick:'fixture',networkName:'fixture'});
for (let i = 0; i < 1001; i++) {
 const target = i < 1000 ? `hidden-${i}` : 'older-visible';
 insertMessage({networkId:account.network.id,target,time:i < 1000 ? '2024-01-01T00:00:00.000Z' : '2023-01-01T00:00:00.000Z',type:'message',nick:target,userhost:'u@fixture',text:'fixture-filtered-row',msgid:`filtered-${i}`});
 if (i < 1000 && !buffers.close(account.user.id,account.network.id,target)) throw new Error('Target not closed');
}
const harness = await harnessModule.startHarness();
fs.writeFileSync(path.join(directory,'ready.json'),JSON.stringify({port:harness.port}));
process.on('SIGTERM',()=>{harness.stop();process.exit(0);});
