const {chromium} = require(process.env.REPARTEE_PLAYWRIGHT_MODULE || 'playwright');
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
let stage = 'start';
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
    const profile = await fs.mkdtemp(path.join(os.tmpdir(),'webpush-ui-'));
    let context;
    try {
        context = await chromium.launchPersistentContext(profile, {headless:true, executablePath:process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE, viewport:{width:1100,height:800}});
        const base = process.env.REPARTEE_BROWSER_FIXTURE_URL;
        await context.grantPermissions(['notifications'], {origin:base});
        await context.addCookies([{name:process.env.REPARTEE_BROWSER_FIXTURE_COOKIE_NAME,value:process.env.REPARTEE_BROWSER_FIXTURE_COOKIE,url:base,httpOnly:true,sameSite:'Strict'}]);
        await context.addInitScript(key=>localStorage.setItem(key,'1'),process.env.REPARTEE_BROWSER_FIXTURE_STORAGE_KEY);
        await context.addInitScript(() => {
            const subscribe = PushManager.prototype.subscribe;
            PushManager.prototype.subscribe = async function(options) {
                console.info('Push fixture: browser subscription started');
                try {
                    const result = await subscribe.call(this, options);
                    console.info('Push fixture: browser subscription completed');
                    return result;
                } catch (error) {
                    console.info('Push fixture: browser subscription failed');
                    throw error;
                }
            };
        });
        const page = await context.newPage();
        page.on('console', event => {
            if (['Push fixture: browser subscription started','Push fixture: browser subscription completed','Push fixture: browser subscription failed','Notification target has no available network buffer.','Notification target could not be opened.'].includes(event.text())) console.log(event.text());
        });
        page.on('response', async response => {
            if (new URL(response.url()).pathname !== '/api/webpush') return;
            try {
                const body = await response.json();
                const action = response.request().postDataJSON()?.action;
                if (['Get','Lookup','Register','Unregister'].includes(action) && ['Ready','Registered','Unregistered','Unavailable','Invalid','Busy','Failed','Unknown'].includes(body.status)) console.log('WebPush API '+action+': '+body.status);
            } catch (_) {}
        });
        page.setDefaultTimeout(90000);
        const cdp = await context.newCDPSession(page);
        const errors = [];
        page.on('pageerror', error=>errors.push(error.name));
        stage='load application'; console.log('WebPush UI stage: '+stage);
        await page.goto(base);
        const input = page.locator('#chat-input');
        stage='wait for input'; console.log('WebPush UI stage: '+stage);
        await input.waitFor();
        stage='open notifications'; console.log('WebPush UI stage: '+stage);
        await page.locator('.desktop-tools').getByRole('button',{name:'Notifications',exact:true}).click();
        const settings = page.locator('dialog.push-settings');
        stage='click Enable'; console.log('WebPush UI stage: '+stage);
        await settings.getByRole('button',{name:'Enable',exact:true}).click();
        stage='confirm registration'; console.log('WebPush UI stage: '+stage);
        try { await settings.getByRole('status').filter({hasText:/^Enabled on this browser$/}).waitFor(); }
        catch(error) {
            const status=await settings.getByRole('status').innerText();
            const category=['Sign in again','receiver','compatible','pending','unknown','rejected','permission'].find(value=>status.includes(value)) || 'other';
            console.log('Registration status category: '+category);
            if(process.env.REPARTEE_WEBPUSH_SCREENSHOT) await page.screenshot({path:process.env.REPARTEE_WEBPUSH_SCREENSHOT+'.failure.png'});
            throw error;
        }
        if(process.env.REPARTEE_WEBPUSH_SCREENSHOT) await page.screenshot({path:process.env.REPARTEE_WEBPUSH_SCREENSHOT});
        stage='wait for notification'; console.log('WebPush UI stage: '+stage);
        await waitForBrowser(page, async()=>{
            const receivers=await navigator.serviceWorker.getRegistrations();
            for(const receiver of receivers) if((await receiver.getNotifications()).some(item=>item.body==='Notifications enabled'))return true;
            return false;
        });
        await settings.getByRole('button',{name:'Close',exact:true}).click();
        stage='send upstream trigger'; console.log('WebPush UI stage: '+stage);
        await input.fill('/msg FixtureControl search-private');
        stage='background application'; console.log('WebPush UI stage: '+stage);
        await cdp.send('Emulation.setFocusEmulationEnabled',{enabled:false});
        const away = await context.newPage();
        await away.goto('about:blank');
        await away.bringToFront();
        await page.waitForFunction(()=>!document.hasFocus());
        stage='dispatch background trigger'; console.log('WebPush UI stage: '+stage);
        await page.evaluate(()=>document.querySelector('#chat-input').dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',bubbles:true,cancelable:true})));
        const destination = await waitForBrowser(page, async()=>{
            for(const receiver of await navigator.serviceWorker.getRegistrations()) {
                const notification=(await receiver.getNotifications()).find(item=>item.body==='private needle incoming');
                if(notification) return {scope:notification.data.scope,target:notification.data.target};
            }
            return false;
        });
        stage='navigate private notification'; console.log('WebPush UI stage: '+stage);
        assert.match(destination.scope, /^[a-f0-9]{64}$/);
        assert.equal(destination.target, "Alice");
        await page.bringToFront();
        await cdp.send('Emulation.setFocusEmulationEnabled',{enabled:true});
        await page.evaluate(({scope,target})=>{location.hash=new URLSearchParams({push_scope:scope,push_target:target}).toString();},destination);
        stage='resolve notification route'; console.log('WebPush UI stage: '+stage);
        await page.waitForFunction(()=>location.hash==='');
        stage='dismiss read notification'; console.log('WebPush UI stage: '+stage);
        await waitForBrowser(page, async()=>{
            for(const receiver of await navigator.serviceWorker.getRegistrations())if((await receiver.getNotifications()).some(item=>item.body==='private needle incoming'))return false;
            return true;
        });
        stage='reload'; console.log('WebPush UI stage: '+stage);
        await page.reload();
        await page.locator('#chat-input').waitFor();
        await page.locator('.desktop-tools').getByRole('button',{name:'Notifications',exact:true}).click();
        stage='disable'; console.log('WebPush UI stage: '+stage);
        await settings.getByRole('button',{name:'Disable',exact:true}).click();
        await settings.getByRole('status').filter({hasText:/^Disabled on this browser$/}).waitFor();
        const remaining = await page.evaluate(async()=>{
            const receivers=await navigator.serviceWorker.getRegistrations();
            return (await Promise.all(receivers.map(receiver=>receiver.pushManager.getSubscription()))).filter(Boolean).length;
        });
        assert.equal(remaining,0);
        assert.deepEqual(errors,[]);
        console.log('PASS: actual WASM settings -> authenticated HTTP -> Repartee -> pinned Soju -> real browser push service -> Chromium notification, then reload and disable. No HTTP/WebSocket/push mocks.');
    } finally {
        if(context)await context.close();
        await fs.rm(profile,{recursive:true,force:true});
    }
})().catch(error=>{console.error(error.name + ': WebPush UI fixture failed at ' + stage);process.exitCode=1;});
