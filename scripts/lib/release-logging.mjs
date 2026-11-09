export function releaseLog(event, details = {}) {
  console.log(JSON.stringify({ timestamp: new Date().toISOString(), event,
    run_id: process.env.GITHUB_RUN_ID, run_attempt: process.env.GITHUB_RUN_ATTEMPT,
    source_commit: process.env.GITHUB_SHA, ref: process.env.GITHUB_REF, ...details }));
}

export function logReleaseErrors() {
  process.on('uncaughtExceptionMonitor', error => {
    releaseLog('release.failed', { level: 'error', tag: process.env.RELEASE_TAG,
      error: { name: error.name, message: error.message, code: error.code, stack: error.stack } });
  });
}
