const {chromium} = require('playwright');
const http = require('node:http');
const fs = require('node:fs/promises');
const path = require('node:path');
const assert = require('node:assert/strict');
const crypto = require('node:crypto');

async function waitForBrowser(page, predicate, argument) {
    const deadline = Date.now() + 30000;
    while (Date.now() < deadline) {
        const result = await page.evaluate(predicate, argument);
        if (result) return result;
        await new Promise(resolve => setTimeout(resolve, 50));
    }
    throw new Error('Browser condition timed out');
}

(async () => {
    const root = path.resolve(__dirname, '../web-ui/push');
    const key = crypto.generateKeyPairSync('ec', {namedCurve:'prime256v1'}).publicKey.export({format:'jwk'});
    const vapid = Buffer.concat([Buffer.from([4]), Buffer.from(key.x, 'base64url'), Buffer.from(key.y, 'base64url')]).toString('base64url');
    const scopes = {'one':'a'.repeat(64), 'two':'b'.repeat(64)};
    const mutations = [];
    let gets = 0;
    let failNext = false;
    let sessionStatus = 204;
    let remoteOffline = false;
    let failLookupOnce = false;
    const server = http.createServer(async (req, res) => {
        if (req.url === '/api/session') { res.writeHead(sessionStatus).end(); return; }
        if (req.url === '/') {
            res.setHeader('Content-Type','text/html');
            res.end('<!doctype html><button id="settings">Notifications</button><script type="module">import * as push from "/push/client.js"; window.push = push; document.querySelector("button").onclick = push.openSettings;</script>'); return;
        }
        if (req.url === '/api/webpush') {
            assert.equal(req.headers['x-push-intent'], '1');
            let body = '';
            for await (const chunk of req) body += chunk;
            const input = JSON.parse(body);
            if (input.action === "Get") gets++;
            const connection = input.connection_id || Object.keys(scopes).find(id => scopes[id] === input.scope);
            let status = connection && !remoteOffline ? 'Ready' : 'Unavailable';
            if (input.action==='Lookup' && failLookupOnce) { failLookupOnce=false; status='Unavailable'; }
            if (['Register','Unregister'].includes(input.action)) {
                mutations.push({action:input.action, scope:input.scope});
                status = failNext ? 'Unknown' : (input.action === 'Register' ? 'Registered' : 'Unregistered');
                failNext = false;
            }
            res.setHeader('Content-Type','application/json');
            res.end(JSON.stringify({type:'WebPush', request_id:input.request_id, connection_id:connection, scope:scopes[connection], vapid, status,
                context:{nick:'Me', label:connection, chantypes:'#&', statusmsg:'@+', casemapping:'rfc1459'}})); return;
        }
        if (['client.js','api.js','payload.js'].some(name => req.url === `/push/${name}`)) {
            res.setHeader('Content-Type','text/javascript');
            res.end(await fs.readFile(path.join(root, path.basename(req.url)))); return;
        }
        res.writeHead(404).end();
    });
    await new Promise(resolve => server.listen(0,'127.0.0.1',resolve));
    let browser;
    try {
        browser = await chromium.launch({headless:true, executablePath:process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE});
        const page = await browser.newPage();
        await page.addInitScript(() => {
            window.fixturePermission = 'granted';
            window.fixtureSubscribed = 0;
            window.fixtureOpened = [];
            const receivers = new Map();
            Object.defineProperty(Notification, 'requestPermission', {value:async () => window.fixturePermission});
            Object.defineProperty(navigator, 'serviceWorker', {value:{
                addEventListener() {},
                async getRegistrations() { return [...receivers.values()]; },
                async getRegistration(scope) { return receivers.get(scope); },
                async register(script, {scope}) {
                    if (!receivers.has(scope)) {
                        let subscription;
                        let config;
                        let pending = [];
                        const worker = {async postMessage(data, ports) {
                            if (data.type === 'confirmed') window.fixturePreferenceAtConfirmation = localStorage.getItem(`fixture-push-${config.scope}`);
                            if (data.type === 'confirmed' && window.fixtureBreakConfirmation) { receiver.active=null; return; }
                            if (data.type === 'configure') config = data.config;
                            if (data.type === 'disable' && data.endpoint) pending.push(data.endpoint);
                            if (data.type === 'cleanup' && !window.fixtureOffline) {
                                for (const endpoint of pending) await fetch('/api/webpush', {method:'POST',headers:{'X-Push-Intent':'1'},body:JSON.stringify({request_id:crypto.randomUUID(),action:'Unregister',scope:config.scope,endpoint})});
                                pending = [];
                                if (window.fixtureConcurrentEnable) {
                                    window.fixtureConcurrentEnable=false;
                                    await receiver.pushManager.subscribe(receiver.fixtureOptions);
                                    localStorage.setItem(`fixture-push-${config.scope}`,'enabled');
                                }
                            }
                            ports?.[0]?.postMessage({ok:true,status:'ready',config,pending:pending.length>0});
                        }};
                        const receiver = {scope:new URL(scope, location.href).href, active:worker,
                            async unregister() { receivers.delete(scope); return true; }, async getNotifications() { return []; },
                            pushManager:{async getSubscription() { return subscription; }, async subscribe(options) {
                                receiver.fixtureOptions=options;
                                window.fixtureSubscribed++;
                                if(window.fixtureDelaySubscribe) await new Promise(resolve=>{window.fixtureReleaseSubscribe=resolve;});
                                subscription = {endpoint:'https://synthetic.invalid/'+scope, options,
                                    toJSON() { return {endpoint:this.endpoint, keys:{p256dh:'synthetic-public', auth:'synthetic-auth'}}; },
                                    async unsubscribe() { if(window.fixtureUnsubscribeFails)return false; subscription = null; return true; }};
                                return subscription;
                            }}};
                        worker.state = 'activated';
                        receivers.set(scope, receiver);
                    }
                    return receivers.get(scope);
                }
            }});
            window.addEventListener('push-open', event => window.fixtureOpened.push(JSON.parse(event.detail)));
        });
        await page.goto(`http://127.0.0.1:${server.address().port}`);
        await page.waitForFunction(() => window.push);
        const snapshot = {appName:'fixture', authenticated:true, sessionHint:true,
            connections:[{id:'one',connected:true},{id:'two',connected:true}],
            buffers:[{id:'one/server',connection_id:'one',name:'one',buffer_type:'server'}, {id:'two/server',connection_id:'two',name:'Alice',buffer_type:'server'},
                {id:'two/Alice',connection_id:'two',name:'Alice',buffer_type:'query'}]};
        await page.evaluate(snapshot => window.push.update(JSON.stringify(snapshot)), snapshot);
        await page.click('#settings');
        const one = page.locator('dialog section').filter({has:page.locator('strong', {hasText:'one'})});
        const two = page.locator('dialog section').filter({has:page.locator('strong', {hasText:'two'})});
        await one.getByRole('button',{name:'Enable', exact:true}).waitFor();
        assert.equal(mutations.length,0);
        await page.waitForTimeout(500);
        const beforeUnread = gets;
        for(let unread=1;unread<=3;unread++) {
            await page.evaluate(({snapshot,unread})=>window.push.update(JSON.stringify({...snapshot,buffers:snapshot.buffers.map(buffer=>({...buffer,unread_count:unread}))})),{snapshot,unread});
            await page.waitForTimeout(350);
        }
        assert.equal(gets,beforeUnread);

        await one.getByRole('button',{name:'Enable', exact:true}).click();
        await one.getByRole('status').filter({hasText:/^Enabled on this browser$/}).waitFor();
        assert.deepEqual(mutations,[{action:'Register',scope:scopes.one}]);
        assert.equal(await page.evaluate(()=>window.fixturePreferenceAtConfirmation),'enabled');
        failLookupOnce=true;
        await one.getByRole('button',{name:'Check / repair',exact:true}).click();
        await one.getByRole('status').filter({hasText:'Notifications are unavailable'}).waitFor();
        assert.equal(await page.evaluate(scope=>localStorage.getItem(`fixture-push-${scope}`),scopes.one),'enabled');
        assert.equal(await page.evaluate(async scope=>!!await (await navigator.serviceWorker.getRegistration(`/push/${scope}/`)).pushManager.getSubscription(),scopes.one),true);
        assert.equal(mutations.length,1);

        await page.evaluate(() => { window.fixturePermission = 'denied'; });
        await two.getByRole('button',{name:'Enable', exact:true}).click();
        await two.getByRole('status').filter({hasText:'permission was not granted'}).waitFor();
        assert.equal(mutations.length,1);
        await page.evaluate(() => { window.fixturePermission = 'granted'; });
        failNext = true;
        await two.getByRole('button',{name:'Enable', exact:true}).click();
        await two.getByRole('status').filter({hasText:'outcome is unknown'}).waitFor();
        assert.equal(await page.evaluate(scope => localStorage.getItem(`fixture-push-${scope}`), scopes.two),null);
        assert.equal(await page.evaluate(async()=>{
            let count=0;for(const receiver of await navigator.serviceWorker.getRegistrations())if(await receiver.pushManager.getSubscription())count++;
            return count;
        }),1);
        await page.evaluate(()=>{window.fixtureBreakConfirmation=true;});
        await two.getByRole('button',{name:'Enable',exact:true}).click();
        await two.getByRole('status').filter({hasText:'Notification receiver did not respond'}).waitFor();
        assert.equal(await page.evaluate(async scope=>!!await (await navigator.serviceWorker.getRegistration(`/push/${scope}/`)).pushManager.getSubscription(),scopes.two),false);
        assert.equal(await page.evaluate(scope=>localStorage.getItem(`fixture-push-${scope}`),scopes.two),null);
        await page.evaluate(async scope=>{
            window.fixtureBreakConfirmation=false;
            await (await navigator.serviceWorker.getRegistration(`/push/${scope}/`)).unregister();
        },scopes.two);
        await page.evaluate(()=>{window.fixtureConcurrentEnable=true;});
        await one.getByRole('button',{name:'Disable',exact:true}).click();
        await one.getByRole('status').filter({hasText:'enabled in another tab'}).waitFor();
        assert.equal(await page.evaluate(async scope=>!!await (await navigator.serviceWorker.getRegistration(`/push/${scope}/`)).pushManager.getSubscription(),scopes.one),true);
        await one.getByRole('button',{name:'Disable', exact:true}).click();
        await one.getByRole('status').filter({hasText:/^Disabled on this browser$/}).waitFor();
        await page.getByRole('button',{name:'Close', exact:true}).click();
        await page.evaluate(scope => { location.hash = new URLSearchParams({push_scope:scope,push_target:'Alice'}).toString(); }, scopes.two);
        await page.waitForFunction(() => window.fixtureOpened.length === 1);
        assert.deepEqual(await page.evaluate(() => window.fixtureOpened[0]),{buffer_id:'two/Alice',target:'',channel:false});
        await page.evaluate(scope=>{location.hash=new URLSearchParams({push_scope:scope,push_target:'#invited'}).toString();},scopes.two);
        await page.waitForFunction(()=>window.fixtureOpened.length===2);
        assert.deepEqual(await page.evaluate(()=>window.fixtureOpened[1]),{buffer_id:'two/server',target:'#invited',channel:true});

        await page.click('#settings');
        await one.getByRole('button',{name:'Enable',exact:true}).click();
        await one.getByRole('status').filter({hasText:/^Enabled on this browser$/}).waitFor();
        await page.getByRole('button',{name:'Close',exact:true}).click();
        for (const status of [204,503]) {
            sessionStatus = status;
            await page.evaluate(snapshot=>window.push.update(JSON.stringify({...snapshot,authenticated:false,sessionHint:false})),snapshot);
            await page.waitForTimeout(300);
            assert.equal(await page.evaluate(scope=>localStorage.getItem(`fixture-push-${scope}`),scopes.one),'enabled');
            assert.equal(await page.evaluate(async scope=>!!await (await navigator.serviceWorker.getRegistration(`/push/${scope}/`)).pushManager.getSubscription(),scopes.one),true);
            await page.evaluate(snapshot=>window.push.update(JSON.stringify(snapshot)),snapshot);
        }
        sessionStatus = 401;
        remoteOffline = true;
        const offline = {...snapshot,connections:[]};
        await page.evaluate(snapshot=>{window.fixtureOffline=true;window.push.update(JSON.stringify(snapshot));},offline);
        await page.click('#settings');
        await one.getByRole('status').filter({hasText:'Network offline'}).waitFor();
        await one.getByRole('button',{name:'Disable',exact:true}).click();
        await one.getByRole('status').filter({hasText:'cleanup will finish after reconnect'}).waitFor();
        assert.equal(await page.evaluate(async()=>{
            let count=0;for(const receiver of await navigator.serviceWorker.getRegistrations())if(await receiver.pushManager.getSubscription())count++;
            return count;
        }),0);
        remoteOffline = false;
        const beforeCleanup = mutations.length;
        await page.evaluate(snapshot=>{window.fixtureOffline=false;window.push.update(JSON.stringify(snapshot));},snapshot);
        const deadline = Date.now()+5000;
        while(mutations.length===beforeCleanup){if(Date.now()>deadline)throw new Error('Reconnect cleanup timed out');await new Promise(resolve=>setTimeout(resolve,30));}
        assert.equal(mutations.at(-1).action,'Unregister');
        await waitForBrowser(page, async scope=>!await navigator.serviceWorker.getRegistration(`/push/${scope}/`),scopes.one);
        await page.getByRole('button',{name:'Close',exact:true}).click();
        await page.click('#settings');
        await two.getByRole('button',{name:'Enable',exact:true}).click();
        await two.getByRole('status').filter({hasText:/^Enabled on this browser$/}).waitFor();
        await page.evaluate(async scope=>{(await navigator.serviceWorker.getRegistration(`/push/${scope}/`)).active=null;},scopes.two);
        remoteOffline=true;
        await page.evaluate(snapshot=>window.push.update(JSON.stringify(snapshot)),offline);
        await page.evaluate(()=>{window.fixtureUnsubscribeFails=true;});
        await two.getByRole('button',{name:'Disable',exact:true}).click();
        await two.getByRole('status').filter({hasText:'browser subscription removal failed'}).waitFor();
        assert.equal(await page.evaluate(async scope=>{
            const db=await new Promise(resolve=>{const request=indexedDB.open(`push-${scope}`,1);request.onsuccess=()=>resolve(request.result);});
            try{return await new Promise(resolve=>{const request=db.transaction('settings').objectStore('settings').get('enabled');request.onsuccess=()=>resolve(request.result);});}
            finally{db.close();}
        },scopes.two),false);
        await page.evaluate(()=>{window.fixtureUnsubscribeFails=false;});
        await two.getByRole('button',{name:'Disable',exact:true}).click();
        await two.getByRole('status').filter({hasText:'cleanup will finish after reconnect'}).waitFor();
        assert.equal(await page.evaluate(scope=>JSON.parse(localStorage.getItem(`fixture-push-${scope}-retired`)).length,scopes.two),1);
        assert.equal(await page.evaluate(async scope=>!!await (await navigator.serviceWorker.getRegistration(`/push/${scope}/`)).pushManager.getSubscription(),scopes.two),false);
        remoteOffline=false;
        await page.evaluate(snapshot=>window.push.update(JSON.stringify(snapshot)),snapshot);
        await waitForBrowser(page, async scope=>!localStorage.getItem(`fixture-push-${scope}-retired`) && !await navigator.serviceWorker.getRegistration(`/push/${scope}/`),scopes.two);
        await page.evaluate(snapshot=>window.push.update(JSON.stringify({...snapshot,authenticated:false,sessionHint:false})),snapshot);
        await waitForBrowser(page, async()=>{
            for(const receiver of await navigator.serviceWorker.getRegistrations())if(await receiver.pushManager.getSubscription())return false;
            return true;
        });
        await page.evaluate(snapshot=>{window.fixtureDelaySubscribe=true;window.push.update(JSON.stringify(snapshot));},snapshot);
        await page.click('#settings');
        await one.getByRole('button',{name:'Enable',exact:true}).click();
        await page.waitForFunction(()=>window.fixtureReleaseSubscribe);
        await page.evaluate(snapshot=>{
            window.push.update(JSON.stringify({...snapshot,authenticated:false,sessionHint:false}));
            window.fixtureReleaseSubscribe();
        },snapshot);
        await page.waitForFunction(()=>document.querySelector('dialog').textContent.includes('Sign in again before enabling'));
        await waitForBrowser(page, async()=>{
            for(const receiver of await navigator.serviceWorker.getRegistrations())if(await receiver.pushManager.getSubscription())return false;
            return true;
        });
        assert.equal(await page.evaluate(scope=>localStorage.getItem(`fixture-push-${scope}`),scopes.one),null);
        await page.evaluate(snapshot=>{window.fixtureDelaySubscribe=false;window.push.update(JSON.stringify(snapshot));},snapshot);
        await page.click('#settings');
        for (const label of ['one','two']) {
            const row = page.locator('dialog section').filter({has:page.locator('strong').filter({hasText:new RegExp(`^${label}$`)})});
            await row.getByRole('button',{name:'Enable',exact:true}).click();
            await row.getByRole('status').filter({hasText:/^Enabled on this browser$/}).waitFor();
        }
        await page.evaluate(async snapshot=>{
            for(const receiver of await navigator.serviceWorker.getRegistrations()) receiver.active=null;
            Object.defineProperty(navigator,'locks',{value:undefined,configurable:true});
            window.push.update(JSON.stringify({...snapshot,authenticated:false,sessionHint:false}));
        },snapshot);
        await waitForBrowser(page, async()=>{
            for(const receiver of await navigator.serviceWorker.getRegistrations())if(await receiver.pushManager.getSubscription())return false;
            return true;
        });
        for(const scope of Object.values(scopes)) assert.equal(await page.evaluate(scope=>localStorage.getItem(`fixture-push-${scope}`),scope),null);
        await page.evaluate(async snapshot=>{
            window.push.update(JSON.stringify(snapshot));
            const receiver=await navigator.serviceWorker.register('/push/worker.js',{scope:`/push/${'c'.repeat(64)}/`});
            receiver.active={postMessage(){throw new Error('Fixture worker unavailable');}};
        },snapshot);
        await page.click('#settings');
        await one.getByRole('button',{name:'Enable',exact:true}).waitFor();
        await two.getByRole('button',{name:'Enable',exact:true}).waitFor();
        const broken=page.locator('dialog section').filter({hasText:'Saved notification subscription'});
        await broken.getByRole('button',{name:'Disable',exact:true}).click();
        await broken.getByRole('status').filter({hasText:'Disabled on this browser'}).waitFor();
        console.log('Chromium settings UI: confirmed logout cleans every subscription without Web Locks or active workers; delayed cleanup removes registrations.');
        console.log('Chromium settings UI: logout during pending subscribe cannot re-enable notifications.');
        console.log('Chromium settings UI: offline disable, reconnect cleanup and logout unsubscribe passed.');
        console.log('Chromium settings UI: explicit enable/disable, permission refusal, unknown outcome and correct-account navigation passed with mocked subscriptions/backend.');
    } finally { if (browser) await browser.close(); await new Promise(resolve => server.close(resolve)); }
})().catch(error => { console.error(error); process.exitCode = 1; });
