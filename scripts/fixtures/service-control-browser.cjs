require('./service-browser.cjs')('control').catch(error => { console.error(error); process.exitCode = 1; });
