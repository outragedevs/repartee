const {chromium} = require('playwright');
const http = require('node:http');
const fs = require('node:fs/promises');
const path = require('node:path');

(async () => {
    const root = path.resolve(process.argv[2]);
    const sources = path.resolve(__dirname, '../web-ui/push');
    const server = http.createServer(async (request, response) => {
        if (request.url === '/') {
            response.setHeader('Content-Type','text/html');
            response.end('<!doctype html><title>Disposable browser push receiver</title>'); return;
        }
        if (!['/push/worker.js','/push/api.js','/push/payload.js'].includes(request.url)) { response.writeHead(404).end(); return; }
        response.setHeader('Content-Type','text/javascript');
        response.end(await fs.readFile(path.join(sources, path.basename(request.url))));
    });
    await new Promise(resolve => server.listen(0,'127.0.0.1',resolve));
    const origin = `http://127.0.0.1:${server.address().port}`;
    let context;
    let stopping = false;
    process.on('SIGTERM', () => { stopping = true; });
    try {
        context = await chromium.launchPersistentContext(path.join(root,'browser-profile'), {headless:true, executablePath:process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE});
        await context.grantPermissions(['notifications'], {origin});
        const page = await context.newPage();
        await page.goto(origin);
        let config;
        const deadline = Date.now() + 120000;
        while (!stopping) {
            try { config = JSON.parse(await fs.readFile(path.join(root,'browser-config.json'),'utf8')); break; } catch (_) {}
            if (Date.now() > deadline) throw new Error('Provider did not supply browser configuration');
            await new Promise(resolve => setTimeout(resolve,50));
        }
        if (!config) return;
        const subscription = await page.evaluate(async config => {
            const receiver = await navigator.serviceWorker.register('/push/worker.js',{scope:`/push/${config.scope}/`,type:'module'});
            const worker = receiver.installing || receiver.waiting || receiver.active;
            if (worker.state !== 'activated') await new Promise(resolve => worker.addEventListener('statechange',()=>{if(worker.state==='activated')resolve();}));
            const channel = new MessageChannel();
            const configured = new Promise(resolve => channel.port1.onmessage = resolve);
            worker.postMessage({type:'configure',config:{...config.context,scope:config.scope,appName:'Fixture',enabled:true}},[channel.port2]);
            await configured;
            const confirmChannel=new MessageChannel();
            const confirmed=new Promise(resolve=>confirmChannel.port1.onmessage=resolve);
            worker.postMessage({type:'confirmed'},[confirmChannel.port2]);
            await confirmed;
            const key = Uint8Array.from(atob(config.vapid.replace(/-/g,'+').replace(/_/g,'/')),ch=>ch.charCodeAt(0));
            const subscription = await receiver.pushManager.subscribe({userVisibleOnly:true,applicationServerKey:key});
            const value = subscription.toJSON();
            return {endpoint:value.endpoint,p256dh:value.keys.p256dh,auth:value.keys.auth};
        }, config);
        const subscriptionFile = path.join(root,'browser-subscription.json');
        await fs.writeFile(subscriptionFile+'.tmp', JSON.stringify(subscription), {mode:0o600});
        await fs.rename(subscriptionFile+'.tmp',subscriptionFile);
        console.log('Real browser push subscription created; credentials remain in disposable fixture storage.');
        const worker = context.serviceWorkers().find(item => new URL(item.url()).pathname === '/push/worker.js');
        if (!worker) throw new Error('Notification worker is unavailable');
        let pageClosed = false;
        while (!stopping) {
            const delivered = await worker.evaluate(async () => {
                const notifications = await self.registration.getNotifications();
                return {registration:notifications.some(item=>item.body==='Notifications enabled'),
                    message:notifications.some(item=>item.body==='private needle incoming'),count:notifications.length};
            });
            if (delivered.registration && !pageClosed) {
                await page.close();
                pageClosed = true;
                console.log('Application page closed before the upstream private-message trigger.');
            }
            delivered.pageClosed = pageClosed;
            const file = path.join(root,'browser-delivery.json');
            await fs.writeFile(file+'.tmp',JSON.stringify(delivered));
            await fs.rename(file+'.tmp',file);
            await new Promise(resolve=>setTimeout(resolve,50));
        }
    } finally {
        if(context) await context.close();
        await new Promise(resolve=>server.close(resolve));
    }
})().catch(error=>{console.error(error.name + ': browser push receiver failed');process.exitCode=1;});
