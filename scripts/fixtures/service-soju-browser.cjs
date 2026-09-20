require('./service-browser.cjs')('soju').catch(error => { console.error(error); process.exitCode = 1; });
