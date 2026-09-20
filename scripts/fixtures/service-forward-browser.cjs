require('./service-browser.cjs')('forward').catch(error => { console.error(error); process.exitCode = 1; });
