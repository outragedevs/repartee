import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const [source, readyFile] = process.argv.slice(2);
process.env.DATABASE_PATH = path.join(path.dirname(readyFile), 'lurker.db');
process.env.SESSION_SECRET = 'disposable-bouncer-binding-test-session-secret';
process.env.LURKER_BOUNCER_ENABLED = 'true';
const harnessModule = await import(pathToFileURL(path.join(source, 'server/test-utils/bouncerHarness.ts')).href);
const account = harnessModule.seedAccount({ password: 'fixture-password', nick: 'tester' });
const matrix = process.env.REPARTEE_BOUNCER_ACCOUNT_MATRIX === '1';
const accounts = [account];
const matrixNetworks = [{ account: 0, ...account }];
if (matrix) {
  accounts.push(harnessModule.seedAccount({ password: 'fixture-password', nick: 'tester' }));
  for (const [index, owner] of accounts.entries()) {
    if (index > 0) matrixNetworks.push({ account: index, ...owner });
    matrixNetworks.push({ account: index, ...harnessModule.seedNetwork(owner.user, { networkName: 'second', nick: 'tester' }) });
  }
}
if (process.env.REPARTEE_BOUNCER_DAEMON_FIXTURE === '1') account.upstream.state = 'connecting';
const messages = await import(pathToFileURL(path.join(source, 'server/db/messages.ts')).href);
const tie = process.env.REPARTEE_BOUNCER_TARGET_TIE === '1';
const targets = tie ? Array.from({length: 1001}, (_, index) => `peer-${String(index).padStart(4, '0')}`) : ['history-peer', '#history-channel'];
for (const entry of matrix ? matrixNetworks : [matrixNetworks[0]]) {
for (const target of targets) {
for (let index = 0; index < (tie ? 1 : 300); index += 1) {
  messages.insertMessage({
    networkId: entry.network.id,
    target,
    time: new Date(Date.UTC(2024, 0, 1) + index * 1000).toISOString(),
    type: 'message',
    nick: tie ? target : 'history-peer',
    userhost: 'user@fixture.local',
    text: matrix ? `matrix-${entry.account}-${entry.network.id}-${index}` : `fixture-history-${index}`,
    msgid: `fixture-${target}-${index}`,
  });
}
}
}
const harness = await harnessModule.startHarness({ tls: process.env.REPARTEE_BOUNCER_TLS_FIXTURE === '1' });
fs.writeFileSync(readyFile, JSON.stringify({ port: harness.port, network: account.network.id, user: account.user.username, accounts: accounts.map((owner, index) => ({ user: owner.user.username, networks: matrixNetworks.filter(entry => entry.account === index).map(entry => entry.network.id) })) }));
process.on('SIGTERM', () => { harness.stop(); process.exit(0); });

if (matrix) {
  const control = path.join(path.dirname(readyFile), 'matrix-control.json');
  const networks = await import(pathToFileURL(path.join(source, 'server/db/networks.ts')).href);
  const { default: manager } = await import(pathToFileURL(path.join(source, 'server/services/ircManager.ts')).href);
  setInterval(() => {
    if (!fs.existsSync(control)) return;
    let response;
    try {
      const request = JSON.parse(fs.readFileSync(control, 'utf8'));
      fs.unlinkSync(control);
      const owner = accounts[request.account];
      let id = request.network;
      if (request.action === 'online') {
        manager.disposeNetwork(owner.user.id, id, 'fixture activation');
        networks.updateNetwork(id, owner.user.id, { host: '127.0.0.1', port: request.port, tls: false, nick: request.nick });
        if (!manager.startNetwork(owner.user.id, id)) throw new Error('Matrix upstream activation refused');
      } else if (request.action === 'offline') manager.stopNetwork(owner.user.id, id, 'fixture offline');
      else if (request.action === 'rename') networks.updateNetwork(id, owner.user.id, { name: 'renamed' });
      else if (request.action === 'delete') {
        manager.disposeNetwork(owner.user.id, id, 'fixture deletion');
        networks.deleteNetwork(id, owner.user.id);
      } else if (request.action === 'create') {
        id = harnessModule.seedNetwork(owner.user, { networkName: 'renamed', nick: 'tester' }).network.id;
        manager.disposeNetwork(owner.user.id, id, 'fixture recreation');
        networks.updateNetwork(id, owner.user.id, { host: '127.0.0.1', port: request.port, tls: false, nick: 'matrix-recreated' });
        if (!manager.startNetwork(owner.user.id, id)) throw new Error('Recreated upstream activation refused');
      } else throw new Error('Unknown matrix action');
      manager.networkChanged(owner.user.id, id);
      response = { network: id };
    } catch (error) { response = { error: String(error) }; }
    const output = control.replace(/\.json$/, '.response');
    fs.writeFileSync(output + '.pending', JSON.stringify(response));
    fs.renameSync(output + '.pending', output);
  }, 20);
}
