// Ignores SIGTERM, so tests can check that stopping falls back to SIGKILL.
process.on('SIGTERM', () => undefined)
console.log('ready')
setInterval(() => undefined, 1_000)
