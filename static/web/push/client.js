import {request, requireStatus, publicKey, subscriptionData, persistSuppression, withScopeLock} from '/push/api.js';
import {fold} from '/push/payload.js';

let snapshot = {connections:[], buffers:[], authenticated:false, appName:''};
let dialog;
let running = false;
let navigationInProgress = false;
let updateTimer;
let sessionGeneration = 0;
const reconciled = new Map();
const busy = new Set();

function preference(scope) { return `${snapshot.appName}-push-${scope}`; }
function repairKey(scope) { return `${preference(scope)}-repair`; }
function retiredKey(scope) { return `${preference(scope)}-retired`; }
function retired(scope) {
    try { return JSON.parse(localStorage.getItem(retiredKey(scope)) || '[]'); } catch (_) { return []; }
}
function retainEndpoint(scope, endpoint) {
    if (!endpoint) return;
    localStorage.setItem(retiredKey(scope), JSON.stringify([...new Set([...retired(scope), endpoint])]));
}
async function cleanupEndpoints(scope) {
    return exclusive(scope, async () => {
        let endpoints = retired(scope);
        if (!endpoints.length) return false;
        try {
            requireStatus(await request({action:'Lookup', scope}), 'Ready');
            const receiver = await navigator.serviceWorker.getRegistration(`/push/${scope}/`);
            const active = enabled(scope) ? await receiver?.pushManager.getSubscription() : null;
            for (const endpoint of [...endpoints]) {
                if (endpoint !== active?.endpoint) requireStatus(await request({action:'Unregister', scope, endpoint}), 'Unregistered');
                endpoints = endpoints.filter(value => value !== endpoint);
                if (endpoints.length) localStorage.setItem(retiredKey(scope), JSON.stringify(endpoints));
                else localStorage.removeItem(retiredKey(scope));
            }
            return false;
        } catch (_) { return true; }
    });
}

function enabled(scope) { return localStorage.getItem(preference(scope)) === 'enabled'; }
function supported() { return isSecureContext && 'serviceWorker' in navigator && 'PushManager' in window && 'Notification' in window; }

async function registration(scope) {
    const result = await navigator.serviceWorker.register('/push/worker.js', {scope:`/push/${scope}/`, type:'module', updateViaCache:'none'});
    const worker = result.installing || result.waiting || result.active;
    if (!worker) throw new Error('Notification receiver is unavailable.');
    if (worker.state !== 'activated') await new Promise((resolve, reject) => {
        const timeout = setTimeout(() => reject(new Error('Notification receiver did not start.')), 15000);
        worker.addEventListener('statechange', () => {
            if (worker.state === 'activated') { clearTimeout(timeout); resolve(); }
            else if (worker.state === 'redundant') { clearTimeout(timeout); reject(new Error('Notification receiver could not start.')); }
        });
    });
    return result;
}

async function message(receiver, data, timeoutMs = 10000) {
    if (!receiver?.active) throw new Error('Notification receiver is unavailable.');
    const channel = new MessageChannel();
    return await new Promise((resolve, reject) => {
        const timeout = setTimeout(() => { channel.port1.close(); reject(new Error('Notification receiver did not respond.')); }, timeoutMs);
        channel.port1.onmessage = event => { clearTimeout(timeout); channel.port1.close(); resolve(event.data); };
        receiver.active.postMessage(data, [channel.port2]);
    });
}

async function suppressReceiver(scope, receiver, subscription) {
    const [stored, worker, removed] = await Promise.allSettled([
        persistSuppression(scope),
        message(receiver, {type:'disable', endpoint:subscription?.endpoint}),
        subscription ? subscription.unsubscribe() : Promise.resolve(true),
        receiver.getNotifications().then(items => { for (const item of items) item.close(); }),
    ]);
    const unsubscribed = removed.status === 'fulfilled' && removed.value === true;
    if (stored.status !== 'fulfilled' && !(worker.status === 'fulfilled' && worker.value?.ok) && !unsubscribed) {
        if (!await receiver.unregister()) throw new Error('Notifications could not be disabled. Try Disable again.');
    }
    await receiver.getNotifications().then(items => { for (const item of items) item.close(); }).catch(() => {});
    return unsubscribed;
}

async function configure(receiver, ready, enabling = false) {
    return message(receiver, {type:'configure', config:{...ready.context, scope:ready.scope, appName:snapshot.appName, pending:enabling}});
}

async function exclusive(scope, operation) {
    if (busy.has(scope)) throw new Error('A notification change is already in progress.');
    busy.add(scope);
    try {
        return await withScopeLock(scope, operation);
    } finally { busy.delete(scope); }
}

async function enable(ready) {
    const generation = sessionGeneration;
    const current = () => generation === sessionGeneration && snapshot.sessionHint;
    const check = () => { if (!current()) throw new Error('Sign in again before enabling notifications.'); };
    return exclusive(ready.scope, async () => {
        let receiver;
        let subscription;
        try {
        check();
        ready = requireStatus(await request({action:'Lookup', scope:ready.scope}), 'Ready');
        check();
        receiver = await registration(ready.scope);
        check();
        await configure(receiver, ready, true);
        subscription = await receiver.pushManager.getSubscription();
        check();
        if (subscription) {
            const actual = new Uint8Array(subscription.options.applicationServerKey || []);
            const expected = publicKey(ready.vapid);
            if (actual.length !== expected.length || actual.some((byte,index) => byte !== expected[index])) {
                requireStatus(await request({action:'Unregister', scope:ready.scope, endpoint:subscription.endpoint}), 'Unregistered');
                if (!await subscription.unsubscribe()) throw new Error('Could not replace the old browser subscription.');
                subscription = null;
            }
        }
        if (!subscription) subscription = await receiver.pushManager.subscribe({userVisibleOnly:true, applicationServerKey:publicKey(ready.vapid)});
        check();
        requireStatus(await request({action:'Register', scope:ready.scope, vapid:ready.vapid, subscription:subscriptionData(subscription)}), 'Registered');
        check();
        localStorage.setItem(preference(ready.scope), 'enabled');
        await message(receiver, {type:'confirmed'});
        localStorage.removeItem(repairKey(ready.scope));
        reconciled.set(ready.scope, ready.vapid);
        } catch (error) {
            if (receiver) {
                localStorage.removeItem(preference(ready.scope));
                const active = subscription || await receiver.pushManager.getSubscription().catch(() => null);
                retainEndpoint(ready.scope, active?.endpoint);
                await suppressReceiver(ready.scope, receiver, active);
            }
            throw error;
        }
    });
}

async function disable(ready) {
    const receiver = await exclusive(ready.scope, async () => {
        const receiver = await navigator.serviceWorker.getRegistration(`/push/${ready.scope}/`);
        const subscription = await receiver?.pushManager.getSubscription();
        retainEndpoint(ready.scope, subscription?.endpoint);

        localStorage.removeItem(preference(ready.scope));
        localStorage.removeItem(repairKey(ready.scope));
        reconciled.delete(ready.scope);
        if (receiver && !await suppressReceiver(ready.scope, receiver, subscription)) throw new Error('Notifications are suppressed, but browser subscription removal failed. Try Disable again.');
        return receiver;
    });
    const pending = await cleanupEndpoints(ready.scope);
    const result = receiver?.active ? await message(receiver, {type:'cleanup'}, 90000).catch(() => ({pending:true})) : {pending:false};
    if (!pending && !result.pending) await exclusive(ready.scope, async () => {
        if (enabled(ready.scope) || await receiver?.pushManager.getSubscription()) throw new Error('Notifications were enabled in another tab. Check / repair to confirm.');
        await receiver?.unregister();
    });
    return pending || result.pending;
}

function element(tag, text) {
    const node = document.createElement(tag);
    if (text !== undefined) node.textContent = text;
    return node;
}

export async function openSettings() {
    if (dialog?.open) return;
    dialog?.remove();
    dialog = element('dialog');
    dialog.className = 'push-settings';
    const heading = element('h2', 'Notifications');
    const description = element('p', 'Receive messages from supported bouncer networks when this page is closed. Enable each network on this browser.');
    const content = element('div');
    const close = element('button', 'Close');
    close.type = 'button';
    close.onclick = () => dialog.close();
    dialog.addEventListener('keydown', event => event.stopPropagation());
    dialog.append(heading, description, content, close);
    document.body.append(dialog);
    dialog.showModal();
    if (!supported()) { content.textContent = 'Push notifications are not available in this browser or context.'; return; }
    content.textContent = 'Checking networks…';
    try {
        const networks = [];
        for (const connection of snapshot.connections.filter(item => item.connected)) {
            try {
                const ready = await request({action:'Get'}, connection.id);
                if (ready.status === 'Ready') networks.push(ready);
            } catch (_) {}
        }
        for (const receiver of await navigator.serviceWorker.getRegistrations()) {
            const scope = new URL(receiver.scope).pathname.match(/^\/push\/([a-f0-9]{64})\/$/)?.[1];
            if (!scope || networks.some(item => item.scope === scope)) continue;
            let context = {label:'Saved notification subscription'};
            if (receiver.active) {
                try {
                    const state = await message(receiver, {type:'status'}, 2000);
                    if (state.config) context = state.config;
                } catch (_) {}
            }
            networks.push({scope, context, offline:true});
        }
        content.replaceChildren();
        if (!networks.length) content.textContent = 'No connected network currently supports push notifications.';
        for (const ready of networks) {
            const row = element('section');
            const title = element('strong', ready.context.label);
            const status = element('p', ready.offline ? 'Network offline; notifications can still be disabled here.' : enabled(ready.scope) ? 'Previously enabled; use Check / repair to confirm' : 'Not enabled on this browser');
            status.setAttribute('role', 'status');
            const on = element('button', enabled(ready.scope) ? 'Check / repair' : 'Enable');
            const off = element('button', 'Disable');
            on.type = off.type = 'button';
            on.disabled = !!ready.offline;
            on.onclick = async () => {
                on.disabled = off.disabled = true;
                try {
                    const permission = await Notification.requestPermission();
                    if (permission !== 'granted') throw new Error('Notification permission was not granted. You can change it in browser settings.');
                    await enable(ready);
                    status.textContent = 'Enabled on this browser';
                    on.textContent = 'Check / repair';
                } catch (error) { localStorage.setItem(repairKey(ready.scope), '1'); status.textContent = error.message; }
                finally { on.disabled = !!ready.offline; off.disabled = false; }
            };
            off.onclick = async () => {
                on.disabled = off.disabled = true;
                try { const pending = await disable(ready); on.textContent = 'Enable'; status.textContent = pending ? 'Disabled on this browser; bouncer cleanup will finish after reconnect.' : 'Disabled on this browser'; }
                catch (error) { status.textContent = error.message; }
                finally { on.disabled = !!ready.offline; off.disabled = false; }
            };
            row.append(title, status, on, off);
            content.append(row);
        }
    } catch (error) { content.textContent = error.message; }
}

async function navigateNotification() {
    if (navigationInProgress || !snapshot.authenticated) return;
    const params = new URLSearchParams(location.hash.slice(1));
    const scope = params.get('push_scope');
    const target = params.get('push_target');
    if (!scope || target === null || !/^[a-f0-9]{64}$/.test(scope) || /[\x00-\x20\x7f]/.test(target) || target.length > 512 || target.includes(',')) return;
    navigationInProgress = true;
    try {
        const ready = requireStatus(await request({action:'Lookup', scope}), 'Ready');
        const matching = snapshot.buffers.find(buffer => buffer.connection_id === ready.connection_id && ['channel','query'].includes(buffer.buffer_type) && fold(buffer.name, ready.context.casemapping) === fold(target, ready.context.casemapping));
        const server = snapshot.buffers.find(buffer => buffer.connection_id === ready.connection_id && buffer.buffer_type === 'server');
        const selected = matching || server;
        if (!selected) { console.warn("Notification target has no available network buffer."); return; }
        window.dispatchEvent(new CustomEvent('push-open', {detail:JSON.stringify({buffer_id:selected.id, target:matching ? '' : target, channel:!matching && (ready.context.chantypes || '#&').includes(target[0])})}));
        history.replaceState(null, '', `${location.pathname}${location.search}`);
    } catch (_) {
        console.warn("Notification target could not be opened.");
    } finally { navigationInProgress = false; }
}

export function update(value) {
    const previous = snapshot;
    snapshot = JSON.parse(value);
    if (((previous.sessionHint && !snapshot.sessionHint) || (previous.authenticated && !snapshot.authenticated)) && supported()) {
        const disconnected = snapshot;
        void (async () => {
            const response = await fetch('/api/session', {credentials:'same-origin', cache:'no-store', signal:AbortSignal.timeout(5000)});
            if (response.status !== 401 || snapshot !== disconnected) return;
            sessionGeneration++;
            for (const receiver of await navigator.serviceWorker.getRegistrations()) {
                const scope = new URL(receiver.scope).pathname.match(/^\/push\/([a-f0-9]{64})\/$/)?.[1];
                if (!scope) continue;
                const cleanup = async () => {
                    if (snapshot.authenticated) return;
                    localStorage.removeItem(preference(scope));
                    localStorage.removeItem(repairKey(scope));
                    const subscription = await receiver.pushManager.getSubscription().catch(() => null);
                    retainEndpoint(scope, subscription?.endpoint);
                    await suppressReceiver(scope, receiver, subscription);
                };
                try {
                    await withScopeLock(scope, cleanup);
                } catch (_) {}
            }
        })().catch(() => {});
    }
    if (!snapshot.authenticated) { dialog?.close(); reconciled.clear(); return; }
    void navigateNotification();
    if (previous.authenticated !== snapshot.authenticated || JSON.stringify(previous.connections) !== JSON.stringify(snapshot.connections)) {
        clearTimeout(updateTimer);
        updateTimer = setTimeout(refresh, 250);
    }
}

async function refresh() {
    if (!snapshot.authenticated || !supported() || running) return;
    running = true;
    (async () => {
        try {
            const prefix = `${snapshot.appName}-push-`;
            for (const key of Object.keys(localStorage)) {
                if (!key.startsWith(prefix)) continue;
                const match = key.slice(prefix.length).match(/^([a-f0-9]{64})-retired$/);
                if (match) await cleanupEndpoints(match[1]);
            }
            for (const receiver of await navigator.serviceWorker.getRegistrations()) {
                try {
                const scope = new URL(receiver.scope).pathname.match(/^\/push\/([a-f0-9]{64})\/$/)?.[1];
                if (!scope || retired(scope).length) continue;
                if (!receiver.active) {
                    await exclusive(scope, async () => {
                        if (!enabled(scope) && !await receiver.pushManager.getSubscription()) await receiver.unregister();
                    });
                    continue;
                }
                const state = await message(receiver, {type:'status'});
                const result = state.pending ? await message(receiver, {type:'cleanup'}, 90000) : state;
                if (!result.pending && !enabled(scope)) await exclusive(scope, async () => {
                    if (!enabled(scope) && !await receiver.pushManager.getSubscription()) await receiver.unregister();
                });
                } catch (_) {}
            }
            for (const connection of snapshot.connections.filter(item => item.connected)) {
                try {
                const ready = await request({action:'Get'}, connection.id);
                if (!snapshot.authenticated) return;
                if (ready.status !== 'Ready' || !enabled(ready.scope)) continue;
                const receiver = await navigator.serviceWorker.getRegistration(`/push/${ready.scope}/`);
                if (receiver?.active) {
                    const state = await configure(receiver, ready);
                    if (state.status === 'repair') localStorage.setItem(repairKey(ready.scope), '1');
                }
                if (reconciled.get(ready.scope) !== ready.vapid && !localStorage.getItem(repairKey(ready.scope))) {
                    reconciled.set(ready.scope, ready.vapid);
                    try { await enable(ready); } catch (_) { localStorage.setItem(repairKey(ready.scope), '1'); }
                }
                } catch (_) {}
            }
        } catch (_) {} finally { running = false; }
    })();
}

window.addEventListener('hashchange', () => void navigateNotification());
setInterval(() => void refresh(), 30000);
if ('serviceWorker' in navigator) navigator.serviceWorker.addEventListener('message', event => {
    if (event.data?.type === 'push-renewal' && event.data.status === 'repair') localStorage.setItem(repairKey(event.data.scope), '1');
});
