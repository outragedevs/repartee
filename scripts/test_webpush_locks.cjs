const {chromium,webkit} = require('playwright');
const http = require('node:http');
const fs = require('node:fs/promises');
const path = require('node:path');
const assert = require('node:assert/strict');

(async () => {
    const server = http.createServer(async (request, response) => {
        if (request.url === '/api.js') {
            response.setHeader('Content-Type','text/javascript');
            response.end(await fs.readFile(path.resolve(__dirname,'../web-ui/push/api.js')));
        } else {
            response.setHeader('Content-Type','text/html');
            response.end('<!doctype html><title>Notification lock fixture</title>');
        }
    });
    await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
    let browser;
    try {
        browser=process.env.REPARTEE_TEST_ENGINE==='webkit' ? await webkit.launch({headless:true})
            : await chromium.launch({headless:true,executablePath:process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE});
        const context=await browser.newContext();
        await context.addInitScript(()=>Object.defineProperty(navigator,'locks',{value:undefined}));
        const first=await context.newPage();
        const second=await context.newPage();
        const origin=`http://127.0.0.1:${server.address().port}`;
        for(const page of [first,second]) {
            await page.goto(origin);
            await page.evaluate(async()=>{window.lock=(await import('/api.js')).withScopeLock;});
        }
        const scope='a'.repeat(64);
        const hold=page=>page.evaluate(scope=>{
            window.entered=false;
            window.operation=window.lock(scope,async()=>{
                window.entered=true;
                await new Promise(resolve=>{window.release=resolve;});
                return 'released';
            });
        },scope);
        const compete=()=>second.evaluate(scope=>{
            window.entered=false;
            window.operation=window.lock(scope,()=>{window.entered=true;return 'acquired';});
        },scope);
        await hold(first);
        await first.waitForFunction(()=>window.entered);
        await compete();
        await second.waitForTimeout(150);
        assert.equal(await second.evaluate(()=>window.entered),false);
        assert.equal(await second.evaluate(()=>window.lock('b'.repeat(64),()=> 'independent')),'independent');
        await first.evaluate(()=>window.release());
        assert.equal(await first.evaluate(()=>window.operation),'released');
        assert.equal(await second.evaluate(()=>window.operation),'acquired');
        await hold(first);
        await first.waitForFunction(()=>window.entered);
        await compete();
        await second.waitForTimeout(150);
        assert.equal(await second.evaluate(()=>window.entered),false);
        await first.close();
        await second.waitForFunction(()=>window.entered);
        assert.equal(await second.evaluate(()=>window.operation),'acquired');
        assert.equal(await second.evaluate(async scope=>{
            try { await window.lock(scope,()=>{throw new Error('fixture');}); }
            catch(error) { return error.message; }
        },scope),'fixture');
        assert.equal(await second.evaluate(scope=>window.lock(scope,()=> 'recovered'),scope),'recovered');
        console.log('IndexedDB fallback: actual tabs serialize the same scope, isolate other scopes, and recover after owner closure or rejection.');
    } finally {
        if(browser)await browser.close();
        await new Promise(resolve=>server.close(resolve));
    }
})().catch(error=>{console.error(error);process.exitCode=1;});
