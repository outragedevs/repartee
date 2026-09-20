import {notification, navigation, fold} from './payload.js';
import {request, requireStatus, publicKey, subscriptionData, withScopeLock} from './api.js';

const scope = new URL(self.registration.scope).pathname.match(/^\/push\/([a-f0-9]{64})\/$/)?.[1];

async function database() {
    if (!scope) throw new Error('Invalid notification scope');
    return new Promise((resolve, reject) => {
        const request = indexedDB.open(`push-${scope}`, 1);
        request.onupgradeneeded = () => {
            request.result.createObjectStore('settings');
            request.result.createObjectStore('read');
        };
        request.onsuccess = () => resolve(request.result);
        request.onerror = () => reject(new Error('Notification storage unavailable'));
    });
}

async function stored(store, key, value) {
    const db = await database();
    try {
        return await new Promise((resolve, reject) => {
            const transaction = db.transaction(store, value === undefined ? 'readonly' : 'readwrite');
            const objectStore = transaction.objectStore(store);
            const request = value === undefined ? objectStore.get(key) : objectStore.put(value, key);
            transaction.oncomplete = () => resolve(request.result);
            transaction.onerror = () => reject(new Error('Notification storage failed'));
            transaction.onabort = () => reject(new Error('Notification storage aborted'));
        });
    } finally { db.close(); }
}

let ordered = Promise.resolve();
function serialize(operation) {
    const next = ordered.catch(() => {}).then(operation);
    ordered = next;
    return next;
}

async function retire(endpoint) {
    if (typeof endpoint !== 'string' || endpoint.length > 2048 || !endpoint.startsWith('https://')) return;
    const endpoints = await stored('settings', 'retired') || [];
    if (!endpoints.includes(endpoint)) endpoints.push(endpoint);
    await stored('settings', 'retired', endpoints);
}

async function cleanupRetired() {
    let endpoints = await stored('settings', 'retired') || [];
    if (!endpoints.length) return {pending:false};
    try {
        const ready = await request({action:'Lookup', scope});
        if (ready.status !== 'Ready') return {pending:true};
        const active = await stored('settings', 'enabled') ? await self.registration.pushManager.getSubscription() : null;
        for (const endpoint of [...endpoints]) {
            if (endpoint !== active?.endpoint) requireStatus(await request({action:'Unregister', scope, endpoint}), 'Unregistered');
            endpoints = endpoints.filter(value => value !== endpoint);
            await stored('settings', 'retired', endpoints);
        }
        return {pending:false};
    } catch (_) { return {pending:true}; }
}

self.addEventListener('install', event => event.waitUntil(self.skipWaiting()));
self.addEventListener('activate', event => event.waitUntil(self.clients.claim()));

self.addEventListener('message', event => {
    if (!event.source?.url) return;
    const source = new URL(event.source.url);
    if (source.origin !== self.location.origin || source.pathname !== '/') return;
    if (event.data?.type === 'status') {
        event.waitUntil((async () => { event.ports[0]?.postMessage({status: await stored('settings', 'status') || 'ready', config:await stored('settings', 'config'), pending:(await stored('settings', 'retired') || []).length > 0}); })());
        return;
    }
    if (event.data?.type === 'cleanup') {
        event.waitUntil((async () => {
            const result = await withScopeLock(scope, cleanupRetired);
            event.ports[0]?.postMessage(result);
        })());
        return;
    }
    if (event.data?.type === 'confirmed') {
        event.waitUntil(serialize(async () => {
            await stored('settings', 'enabled', true);
            await stored('settings', 'pending', false);
            await stored('settings', 'status', 'ready');
            if (await stored('settings', 'registrationNote')) {
                const config = await stored('settings', 'config');
                await self.registration.showNotification(config.label || config.appName, {body:'Notifications enabled',tag:`${scope}:id:registration`,data:{scope,target:'',targetKey:'',time:Date.now()}});
                await stored('settings', 'registrationNote', false);
            }
            event.ports[0]?.postMessage({ok:true});
        }));
        return;
    }
    if (event.data?.type === 'disable') {
        event.waitUntil(serialize(async () => {
            await stored('settings', 'enabled', false);
            await stored('settings', 'pending', false);
            await stored('settings', 'registrationNote', false);
            await retire(event.data.endpoint);
            for (const item of await self.registration.getNotifications()) item.close();
            event.ports[0]?.postMessage({ok:true});
        }));
        return;
    }
    if (event.data?.type !== 'configure') return;
    const config = event.data.config;
    if (config?.scope !== scope || !['label', 'nick', 'chantypes', 'statusmsg', 'casemapping', 'appName'].every(key => typeof config[key] === 'string' && config[key].length <= 512)) return;
    event.waitUntil((async () => {
        await stored('settings', 'config', {
            scope, label: config.label, nick: config.nick, chantypes: config.chantypes,
            statusmsg: config.statusmsg, casemapping: config.casemapping, appName: config.appName,
        });
        if (config.pending === true) {
            await stored('settings', 'pending', true);
            await stored('settings', 'enabled', false);
        } else if (await stored('settings', 'enabled') === undefined) await stored('settings', 'enabled', false);
        event.ports[0]?.postMessage({ok: true, status: await stored('settings', 'status') || 'ready'});
    })());
});

self.addEventListener('push', event => {
    event.waitUntil(serialize(async () => {
        const config = await stored('settings', 'config');
        if (!config || !event.data) return;
        const item = notification(event.data.text(), config);
        if (!item) return;
        if (!await stored('settings', 'enabled')) {
            if (item.msgid === 'registration' && await stored('settings', 'pending')) await stored('settings', 'registrationNote', true);
            return;
        }
        const target = fold(item.target, config.casemapping);
        if (item.kind === 'read') {
            const previous = await stored('read', target) || 0;
            const readAt = Math.max(previous, item.time);
            await stored('read', target, readAt);
            for (const displayed of await self.registration.getNotifications()) {
                if (displayed.data?.scope === scope && displayed.data?.targetKey === target && displayed.data?.time <= readAt) displayed.close();
            }
            return;
        }
        if (target && (await stored('read', target) || 0) >= item.time) return;
        const tag = `${scope}:${item.msgid ? `id:${item.msgid}` : `target:${target}:${item.time}`}`;
        await self.registration.showNotification(item.title || config.appName, {
            body: item.body,
            tag,
            timestamp: item.time,
            data: {scope, target: item.target, targetKey: target, time: item.time},
        });
    }));
});

self.addEventListener('notificationclick', event => {
    event.notification.close();
    event.waitUntil((async () => {
        const data = event.notification.data;
        if (data?.scope !== scope) return;
        const destination = navigation(self.location.origin, scope, data.target);
        if (!destination) return;
        const windows = await self.clients.matchAll({type: 'window', includeUncontrolled: true});
        const client = windows.find(candidate => {
            const url = new URL(candidate.url);
            return url.origin === self.location.origin && url.pathname === '/';
        });
        if (client) {
            await client.navigate(destination);
            await client.focus();
        } else await self.clients.openWindow(destination);
    })());
});

self.addEventListener('pushsubscriptionchange', event => {
    event.waitUntil((async () => {
        const renew = async () => {
            const config = await stored('settings', 'config');
            if (!config || !await stored('settings', 'enabled')) return;
            await stored('settings', 'status', 'renewing');
            await retire(event.oldSubscription?.endpoint);
            try {
                const ready = requireStatus(await request({action:'Lookup', scope}), 'Ready');
                let subscription = await self.registration.pushManager.getSubscription();
                if (subscription) {
                    const actual = new Uint8Array(subscription.options.applicationServerKey || []);
                    const expected = publicKey(ready.vapid);
                    if (actual.length !== expected.length || actual.some((value, index) => value !== expected[index])) {
                        requireStatus(await request({action:'Unregister', scope, endpoint:subscription.endpoint}), 'Unregistered');
                        if (!await subscription.unsubscribe()) throw new Error('Browser subscription could not be replaced.');
                        subscription = null;
                    }
                }
                if (!subscription) subscription = await self.registration.pushManager.subscribe({userVisibleOnly:true, applicationServerKey:publicKey(ready.vapid)});
                requireStatus(await request({action:'Register', scope, vapid:ready.vapid, subscription:subscriptionData(subscription)}), 'Registered');
                await cleanupRetired();
                await stored('settings', 'config', {...config, ...ready.context});
                await stored('settings', 'status', 'ready');
            } catch (_) {
                await stored('settings', 'status', 'repair');
            }
            for (const client of await self.clients.matchAll({type:'window', includeUncontrolled:true})) client.postMessage({type:'push-renewal', scope, status: await stored('settings', 'status')});
        };
        await withScopeLock(scope, renew);
    })());
});
