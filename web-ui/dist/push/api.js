export async function request(action, connectionId = '') {
    const requestId = crypto.randomUUID();
    try {
        const response = await fetch('/api/webpush', {
            method: 'POST', credentials: 'same-origin', cache: 'no-store',
            headers: {'Content-Type':'application/json', 'X-Push-Intent':'1'},
            body: JSON.stringify({connection_id:connectionId, request_id:requestId, ...action}),
            signal: AbortSignal.timeout(['Get','Lookup'].includes(action.action) ? 6000 : 40000),
        });
        if (response.status === 401) throw new Error('Sign in to manage notifications.');
        if (!response.ok) throw new Error('The notification change could not be confirmed. Reconnect and check its status.');
        const result = await response.json();
        if (result.request_id !== requestId) throw new Error('Unexpected notification response.');
        return result;
    } catch (error) {
        if (error instanceof Error && ['Sign in to manage notifications.', 'The notification change could not be confirmed. Reconnect and check its status.', 'Unexpected notification response.'].includes(error.message)) throw error;
        throw new Error('The notification change could not be confirmed. Reconnect and check its status.');
    }
}

export function requireStatus(response, status) {
    if (response.status === status) return response;
    const messages = {
        Unavailable:'Notifications are unavailable for this network or connection.',
        Invalid:'This browser subscription is not compatible with the bouncer.',
        Busy:'Another notification change is still pending. Try again after it finishes.',
        Failed:'The bouncer rejected this notification change.',
        Unknown:'The outcome is unknown. Reconnect and check notification status before trying again.',
    };
    throw new Error(messages[response.status] || 'Unexpected notification response.');
}

export function publicKey(value) {
    return Uint8Array.from(atob(value.replace(/-/g, '+').replace(/_/g, '/')), character => character.charCodeAt(0));
}

export function subscriptionData(subscription) {
    const json = subscription.toJSON();
    if (!json.endpoint || !json.keys?.p256dh || !json.keys?.auth) throw new Error('Browser did not provide subscription keys.');
    return {endpoint:json.endpoint, p256dh:json.keys.p256dh, auth:json.keys.auth};
}

export async function persistSuppression(scope) {
    if (!/^[a-f0-9]{64}$/.test(scope)) throw new Error('Invalid notification scope');
    const db = await new Promise((resolve, reject) => {
        const request = indexedDB.open(`push-${scope}`, 1);
        request.onupgradeneeded = () => {
            request.result.createObjectStore('settings');
            request.result.createObjectStore('read');
        };
        request.onsuccess = () => resolve(request.result);
        request.onerror = () => reject(new Error('Notification storage unavailable'));
    });
    try {
        await new Promise((resolve, reject) => {
            const transaction = db.transaction('settings', 'readwrite');
            const settings = transaction.objectStore('settings');
            for (const key of ['enabled','pending','registrationNote']) settings.put(false, key);
            transaction.oncomplete = resolve;
            transaction.onerror = transaction.onabort = () => reject(new Error('Notification storage failed'));
        });
    } finally { db.close(); }
}

export async function withScopeLock(scope, operation) {
    if (!/^[a-f0-9]{64}$/.test(scope)) throw new Error('Invalid notification scope');
    if (navigator.locks) return navigator.locks.request(`push-${scope}`, operation);
    const db = await new Promise((resolve, reject) => {
        const request = indexedDB.open(`push-lock-${scope}`, 1);
        request.onupgradeneeded = () => request.result.createObjectStore('lock');
        request.onsuccess = () => resolve(request.result);
        request.onerror = () => reject(new Error('Notification lock unavailable'));
    });
    try {
        return await new Promise((resolve, reject) => {
            const transaction = db.transaction('lock', 'readwrite');
            const store = transaction.objectStore('lock');
            let started = false;
            let finished = false;
            let succeeded = false;
            let result;
            transaction.oncomplete = () => succeeded ? resolve(result) : reject(result);
            transaction.onerror = transaction.onabort = () => reject(new Error('Notification lock interrupted'));
            const keepAlive = () => {
                const request = store.get('held');
                request.onsuccess = () => {
                    if (!started) {
                        started = true;
                        Promise.resolve().then(operation).then(value => {
                            result = value; succeeded = true; finished = true;
                        }, error => { result = error; finished = true; });
                    }
                    if (!finished) keepAlive();
                };
            };
            keepAlive();
        });
    } finally { db.close(); }
}
