const {chromium} = require('playwright');
const http = require('node:http');
const fs = require('node:fs/promises');
const path = require('node:path');
const assert = require('node:assert/strict');

(async () => {
    const root = path.resolve(__dirname, '../web-ui/dist');
    const constants = await fs.readFile(path.resolve(root, '../../src/constants.rs'), 'utf8');
    const appName = constants.match(/^pub const APP_NAME: &str = "([^"]+)";/m)[1];
    const requests = [];
    const svg = '<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32"><style>rect{fill:#23905a}</style><rect width="32" height="32"/><script>parent.iconScriptExecuted=true</script></svg>';
    const server = http.createServer(async (req, res) => {
        if (req.url === '/api/session') { res.writeHead(204).end(); return; }
        if (req.url.startsWith('/api/network-icon?')) {
            requests.push(req.url);
            if (req.url.endsWith('broken')) { res.writeHead(502).end(); return; }
            res.writeHead(200, {'Content-Type':'image/svg+xml', 'Cache-Control':'private, max-age=3600', 'Content-Security-Policy':"sandbox; default-src 'none'; style-src 'unsafe-inline'; img-src data:; font-src data:", 'X-Content-Type-Options':'nosniff'}).end(svg);
            return;
        }
        const name = req.url === '/' ? 'index.html' : req.url.slice(1).split('?')[0];
        if (name.includes('..')) { res.writeHead(404).end(); return; }
        try {
            const content = await fs.readFile(path.join(root, name));
            const mime = name.endsWith('.wasm') ? 'application/wasm' : name.endsWith('.js') ? 'text/javascript' : name.endsWith('.css') ? 'text/css' : 'text/html';
            res.writeHead(200, {'Content-Type':mime}).end(content);
        } catch { res.writeHead(404).end(); }
    });
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
    const base = `http://127.0.0.1:${server.address().port}`;
    let browser;
    try {
        browser = await chromium.launch({headless:true, executablePath:process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE});
        const context = await browser.newContext({viewport:{width:1100,height:800}});
        const page = await context.newPage();
        const errors = [];
        const external = [];
        page.on('pageerror', error => errors.push(String(error)));
        page.on('request', request => { if (!request.url().startsWith(base)) external.push(request.url()); });
        let socket;
        let currentIcon = '/api/network-icon?h=first';
        const snapshot = () => ({type:'SyncInit', buffers:[
            {id:'one/First',connection_id:'one',name:'First',buffer_type:'server',topic:null,unread_count:0,activity:0,nick_count:0,modes:null},
            {id:'two/Second',connection_id:'two',name:'Second',buffer_type:'server',topic:null,unread_count:0,activity:0,nick_count:0,modes:null},
        ],connections:[{id:'one',label:'First network',nick:'me',connected:true,icon_url:currentIcon},{id:'two',label:'Second network',nick:'me',connected:true}],mention_count:0,active_buffer_id:'one/First',timestamp_format:'%H:%M'});
        await page.routeWebSocket('**/ws', ws => { socket=ws; ws.send(JSON.stringify(snapshot())); });
        await page.addInitScript(key => localStorage.setItem(key,'1'), `${appName}-session`);
        await page.goto(base);
        const icons = page.locator('.buffer-list .network-icon:visible');
        await icons.first().waitFor();
        async function decoded() {
            await page.waitForFunction(() => [...document.querySelectorAll('.network-icon')].some(img => img.complete && img.naturalWidth === 32));
        }
        await decoded();
        assert.equal(await icons.count(), 1);
        assert.equal(await page.evaluate(() => window.iconScriptExecuted), undefined);
        const direct = await page.context().newPage();
        await direct.goto(`${base}/api/network-icon?h=direct`);
        assert.equal(await direct.evaluate(() => window.iconScriptExecuted), undefined, 'SVG script executed on direct navigation');
        await direct.close();
        const firstCount = requests.filter(url => url.endsWith('first')).length;
        await page.reload();
        await decoded();
        assert.equal(requests.filter(url => url.endsWith('first')).length, firstCount, 'browser cache was not reused');
        currentIcon = '/api/network-icon?h=second';
        socket.send(JSON.stringify({type:'NetworkIcon',conn_id:'one',icon_url:currentIcon}));
        await page.waitForFunction(() => document.querySelector('.network-icon')?.getAttribute('src')?.endsWith('second'));
        await decoded();
        socket.send(JSON.stringify({type:'NetworkIcon',conn_id:'one',icon_url:'/api/network-icon?h=broken'}));
        await page.waitForFunction(() => document.querySelector('.network-icon')?.hidden);
        assert.ok(await page.getByText('First network', {exact:true}).count());
        socket.send(JSON.stringify({type:'NetworkIcon',conn_id:'one',icon_url:'/api/network-icon?h=recovered'}));
        await page.waitForFunction(() => [...document.querySelectorAll('.network-icon')].some(img => img.getAttribute('src')?.endsWith('recovered') && img.complete && img.naturalWidth === 32 && !img.hidden));
        assert.equal(await icons.count(), 1);

        socket.send(JSON.stringify({type:'NetworkIcon',conn_id:'one',icon_url:null}));
        await page.waitForFunction(() => !document.querySelector('.network-icon'));
        socket.send(JSON.stringify({type:'NetworkIcon',conn_id:'two',icon_url:currentIcon}));
        await decoded();
        await page.screenshot({path:process.env.REPARTEE_ICON_SCREENSHOT || `/tmp/${appName}-network-icons.png`});
        socket.send(JSON.stringify({type:'ConnectionStatus',conn_id:'two',label:'Second network',nick:'me',connected:false}));
        await page.waitForFunction(() => !document.querySelector('.network-icon'));
        assert.deepEqual(errors, []);
        assert.deepEqual(external, []);
        console.log('PASS: compiled WASM icon snapshot, cached reload, updates, removal, failure fallback, network isolation and disconnect. HTTP icon responses and WebSocket events are fixture-controlled.');
    } finally {
        await browser?.close();
        await new Promise(resolve => server.close(resolve));
    }
})().catch(error => { console.error(error); process.exitCode=1; });
