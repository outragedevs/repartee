const { chromium } = require('playwright');
const http = require('node:http');
const fs = require('node:fs/promises');
const path = require('node:path');
const assert = require('node:assert/strict');
const crypto = require('node:crypto');

(async () => {
    const key = crypto.generateKeyPairSync('ec', {namedCurve:'prime256v1'}).publicKey.export({format:'jwk'});
    const vapid = Buffer.concat([Buffer.from([4]), Buffer.from(key.x, 'base64url'), Buffer.from(key.y, 'base64url')]).toString('base64url');
    const mutations = [];
    let offline = false;
    const root = path.resolve(__dirname, '../web-ui/push');
    const server = http.createServer(async (request, response) => {
        if (request.url === '/') {
            response.setHeader('Content-Type', 'text/html');
            response.end('<!doctype html><title>Disposable push worker fixture</title>');
            return;
        }
        if (request.url === '/api/webpush') {
            assert.equal(request.headers['x-push-intent'], '1');
            let body = '';
            for await (const chunk of request) body += chunk;
            const input = JSON.parse(body);
            const status = input.action === 'Lookup' ? (offline ? 'Unavailable' : 'Ready') : input.action === 'Register' ? 'Registered' : 'Unregistered';
            if (input.action !== 'Lookup') mutations.push(input.action);
            response.setHeader('Content-Type', 'application/json');
            response.end(JSON.stringify({request_id:input.request_id,status,scope:input.scope,vapid,context:{nick:'Me',label:'Network',chantypes:'#&',statusmsg:'@+',casemapping:'rfc1459'}}));
            return;
        }
        if (!['/push/worker.js', '/push/payload.js', '/push/api.js'].includes(request.url)) {
            response.writeHead(404).end(); return;
        }
        response.setHeader('Content-Type', 'text/javascript');
        response.end(await fs.readFile(path.join(root, path.basename(request.url))));
    });
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
    const origin = `http://127.0.0.1:${server.address().port}`;
    let browser;
    try {
        browser = await chromium.launch({headless: true, executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE});
        const context = await browser.newContext();
        await context.grantPermissions(['notifications'], {origin});
        const page = await context.newPage();
        const cdp = await context.newCDPSession(page);
        const registrations = new Map();
        cdp.on('ServiceWorker.workerRegistrationUpdated', event => {
            for (const row of event.registrations) registrations.set(row.scopeURL, row.registrationId);
        });
        await cdp.send('ServiceWorker.enable');
        await page.goto(origin);
        const scope = 'a'.repeat(64);
        await page.evaluate(async scope => {
            const registration = await navigator.serviceWorker.register('/push/worker.js', {scope: `/push/${scope}/`, type:'module'});
            const worker = registration.installing || registration.waiting || registration.active;
            if (worker.state !== 'activated') await new Promise(resolve => worker.addEventListener('statechange', () => { if (worker.state === 'activated') resolve(); }));
            const channel = new MessageChannel();
            const configured = new Promise(resolve => channel.port1.onmessage = resolve);
            worker.postMessage({type:'configure', config:{scope, nick:'Me', label:'Network', chantypes:'#&', statusmsg:'@+', casemapping:'rfc1459', appName:'Fixture',pending:true}}, [channel.port2]);
            await configured;
        }, scope);
        const registrationId = registrations.get(`${origin}/push/${scope}/`);
        assert.ok(registrationId);
        const worker = context.serviceWorkers()[0];
        await worker.evaluate(() => {
            Object.defineProperty(navigator,'locks',{value:undefined});
            self.fixtureEvents = [];
            self.addEventListener('push', event => self.fixtureEvents.push({push: Boolean(event.data)}));
            const original = self.registration.showNotification.bind(self.registration);
            self.registration.showNotification = async (...args) => {
                self.fixtureEvents.push({show: true});
                if(self.fixtureHoldShow) {self.fixtureHoldShow=false;await new Promise(resolve=>{self.fixtureReleaseShow=resolve;});}
                try { return await original(...args); }
                catch (error) { self.fixtureEvents.push({error: String(error)}); throw error; }
            };
            self.addEventListener('unhandledrejection', event => self.fixtureEvents.push({rejection: String(event.reason)}));
        });
        const push = data => cdp.send('ServiceWorker.deliverPushMessage', {origin, registrationId, data});
        const notifications = () => page.evaluate(async scope => {
            const registration = await navigator.serviceWorker.getRegistration(`/push/${scope}/`);
            return (await registration.getNotifications()).map(item => ({title:item.title, body:item.body, data:item.data}));
        }, scope);
        async function until(predicate) {
            const deadline = Date.now() + 10000;
            while (!await predicate()) {
                if (Date.now() > deadline) throw new Error(`Worker assertion timed out: ${JSON.stringify(await worker.evaluate(() => self.fixtureEvents))}`);
                await new Promise(resolve => setTimeout(resolve, 30));
            }
        }
        async function workerMessage(data) {
            return page.evaluate(async ({scope,data})=>{
                const receiver=await navigator.serviceWorker.getRegistration(`/push/${scope}/`);
                const channel=new MessageChannel();
                const answer=new Promise(resolve=>channel.port1.onmessage=event=>resolve(event.data));
                receiver.active.postMessage(data,[channel.port2]);
                return answer;
            },{scope,data});
        }
        await push('NOTE WEBPUSH REGISTERED :registered');
        await until(async()=>worker.evaluate(async scope=>{
            const db=await new Promise(resolve=>{const open=indexedDB.open(`push-${scope}`,1);open.onsuccess=()=>resolve(open.result);});
            try{return await new Promise(resolve=>{const request=db.transaction('settings').objectStore('settings').get('registrationNote');request.onsuccess=()=>resolve(request.result===true);});}
            finally{db.close();}
        },scope));
        assert.equal((await notifications()).length,0);
        await workerMessage({type:'confirmed'});
        await until(async()=>(await notifications()).some(item=>item.body==='Notifications enabled'));
        await worker.evaluate(()=>{self.fixtureHoldShow=true;});
        await push('@time=2026-09-20T10:00:02.000Z;msgid=inflight :Alice!u@host PRIVMSG Me :in-flight notification');
        await until(async()=>await worker.evaluate(()=>!!self.fixtureReleaseShow));
        const disabling=workerMessage({type:'disable'});
        await worker.evaluate(()=>self.fixtureReleaseShow());
        await disabling;
        assert.deepEqual(await notifications(),[]);
        await workerMessage({type:'confirmed'});
        await page.evaluate(async scope=>{
            const receiver=await navigator.serviceWorker.getRegistration(`/push/${scope}/`);
            for(const item of await receiver.getNotifications())item.close();
        },scope);
        await push('@time=2026-09-20T10:00:00.000Z;msgid=one :Alice!u@host PRIVMSG Me :private fixture message');
        await until(async () => (await notifications()).length === 1);
        const first = (await notifications())[0];
        assert.equal(first.body, 'private fixture message');
        assert.equal(first.data.target, 'Alice');
        assert.equal(first.data.scope, scope);
        await push('MARKREAD Alice :timestamp=2026-09-20T10:00:00.000Z');
        await until(async () => (await notifications()).length === 0);
        for (const [tag, channel] of [['+channel-context', '#Final['], ['+draft/channel-context', '#Draft['], ['+channel-context', '#Colon:scope']]) {
            await push(`@time=2026-09-20T10:00:00.000Z;${tag}=${channel};msgid=context-${channel} :Alice PRIVMSG Me :channel context`);
            await until(async () => (await notifications()).length === 1);
            assert.equal((await notifications())[0].data.target, channel);
            await push(`MARKREAD ${channel.toLowerCase().replace('[', '{')} :timestamp=2026-09-20T10:00:00.000Z`);
            await until(async () => (await notifications()).length === 0);
        }
        await push('@time=2026-09-20T10:00:00.000Z;+channel-context=#Wrong;msgid=public :Alice!u@host PRIVMSG @#Actual :public message');
        await until(async () => (await notifications()).length === 1);
        assert.equal((await notifications())[0].data.target, '#Actual');
        await push('MARKREAD #actual :timestamp=2026-09-20T10:00:00.000Z');
        await until(async () => (await notifications()).length === 0);
        await push('@time=2026-09-20T09:59:59.000Z;msgid=late :Alice!u@host PRIVMSG Me :already read');
        await push('@time=2026-09-20T10:00:01.000Z;msgid=new :Alice!u@host PRIVMSG Me :new unread');
        await until(async () => (await notifications()).some(item => item.body === 'new unread'));
        assert.equal((await notifications()).length, 1);
        await page.evaluate(async scope=>{
            const receiver=await navigator.serviceWorker.getRegistration(`/push/${scope}/`);
            for(const item of await receiver.getNotifications())item.close();
            const {persistSuppression}=await import('/push/api.js');
            await persistSuppression(scope);
        },scope);
        await until(async()=>(await notifications()).length===0);
        const beforeSuppressed=await worker.evaluate(()=>self.fixtureEvents.filter(item=>item.push).length);
        await push('@time=2026-09-20T10:00:02.000Z;msgid=suppressed :Alice!u@host PRIVMSG Me :must remain suppressed');
        await until(async()=>await worker.evaluate(()=>self.fixtureEvents.filter(item=>item.push).length)>beforeSuppressed);
        await workerMessage({type:'confirmed'});
        assert.deepEqual(await notifications(),[]);
        await worker.evaluate(async vapid => {
            const subscription = {endpoint:'https://synthetic.invalid/new',options:{applicationServerKey:Uint8Array.from(atob(vapid.replace(/-/g,'+').replace(/_/g,'/')),ch=>ch.charCodeAt(0))},
                toJSON(){return {endpoint:this.endpoint,keys:{p256dh:'synthetic-public',auth:'synthetic-auth'}};}};
            self.registration.pushManager.getSubscription = async () => subscription;
            const event = new Event('pushsubscriptionchange');
            Object.defineProperty(event, 'oldSubscription', {value:{endpoint:'https://synthetic.invalid/old'}});
            const pending = [];
            event.waitUntil = promise => pending.push(promise);
            self.dispatchEvent(event);
            await Promise.all(pending);
        }, vapid);
        assert.deepEqual(mutations, ['Register', 'Unregister']);
        offline = true;
        await workerMessage({type:'disable',endpoint:'https://synthetic.invalid/new'});
        assert.equal((await workerMessage({type:'cleanup'})).pending,true);
        assert.equal((await workerMessage({type:'status'})).pending,true);
        offline = false;
        assert.equal((await workerMessage({type:'cleanup'})).pending,false);
        assert.equal((await workerMessage({type:'status'})).pending,false);
        assert.deepEqual(mutations,['Register','Unregister','Unregister']);
        console.log('Chromium service worker: offline retirement persists until reconnected cleanup succeeds.');
        console.log('Chromium service worker: mocked renewal registers new endpoint before unregistering old endpoint.');
        console.log('Chromium service worker: injected push display, scoped routing and read-marker ordering passed. This does not test browser push transport.');
    } finally {
        if (browser) await browser.close();
        await new Promise(resolve => server.close(resolve));
    }
})().catch(error => { console.error(error); process.exitCode = 1; });
