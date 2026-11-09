import { appendFile, lstat, mkdir, readdir, rename, unlink } from 'node:fs/promises';
import path from 'node:path';
import { randomUUID } from 'node:crypto';

const DAY = 86_400_000;
const knownLog = /^(?:errors-(\d{4}-\d{2}-\d{2})\.jsonl|runtime-(\d{4}-\d{2}-\d{2})\.log)(?:\.[12])?$|^runtime\.log(?:\.previous)?$/;
const ignoreMissing = error => { if (error.code !== 'ENOENT') throw error; };

/** Only our own regular log files are eligible; backups and symlinks are never followed. */
export async function cleanupLogs(directory, { now = new Date(), retentionDays = 14, maxTotalBytes = 64 * 1024 * 1024 } = {}) {
  await mkdir(directory, { recursive: true, mode: 0o700 });
  const today = now.toISOString().slice(0, 10);
  const cutoff = new Date(now.getTime() - retentionDays * DAY).toISOString().slice(0, 10);
  const files = [];
  for (const name of await readdir(directory)) {
    const match = knownLog.exec(name);
    if (!match) continue;
    const filename = path.join(directory, name);
    const info = await lstat(filename).catch(error => { ignoreMissing(error); return null; });
    if (!info?.isFile()) continue;
    const day = match[1] || match[2] || new Date(info.mtimeMs).toISOString().slice(0, 10);
    if (day <= cutoff) { await unlink(filename).catch(ignoreMissing); continue; }
    const active = day === today && name.includes('-') && !/\.[12]$/.test(name);
    files.push({ filename, size: info.size, modified: info.mtimeMs, errorLog: name.startsWith('errors-'), active });
  }
  let total = files.reduce((sum, file) => sum + file.size, 0);
  for (const file of files.filter(file => !file.active).sort((a, b) => Number(a.errorLog) - Number(b.errorLog) || a.modified - b.modified)) {
    if (total <= maxTotalBytes) break;
    await unlink(file.filename).catch(ignoreMissing);
    total -= file.size;
  }
}

/** Daily logs, two size rotations, bounded write backlog, hourly expiry cleanup. */
export function createLogWriter({ directory, kind = 'errors', maxBytes = kind === 'runtime' ? 10 * 1024 * 1024 : 5 * 1024 * 1024,
  retentionDays = 14, maxTotalBytes = 64 * 1024 * 1024, cleanupIntervalMs = 60 * 60_000,
  now = () => new Date(), onError = error => console.error('Desktop log write failed:', error.message) }) {
  if (!['errors', 'runtime'].includes(kind)) throw new Error('Invalid desktop log kind');
  let queue = Promise.resolve();
  let pending = 0;
  const enqueue = work => {
    queue = queue.then(work).catch(error => { try { onError(error); } catch { /* Logging must not stop the app. */ } });
    return queue;
  };
  const cleanup = () => enqueue(() => cleanupLogs(directory, { now: now(), retentionDays, maxTotalBytes }));
  const timer = setInterval(() => void cleanup(), cleanupIntervalMs);
  timer.unref();
  void cleanup();
  return {
    write(value) {
      const data = Buffer.from(value);
      if (pending + data.length > 1024 * 1024) {
        try { onError(new Error('Desktop log backlog exceeded 1 MB; dropping a log chunk')); } catch { /* Best effort. */ }
        return Promise.resolve();
      }
      pending += data.length;
      return enqueue(async () => {
        try {
          const day = now().toISOString().slice(0, 10);
          const filename = path.join(directory, `${kind}-${day}.${kind === 'errors' ? 'jsonl' : 'log'}`);
          await mkdir(directory, { recursive: true, mode: 0o700 });
          const size = await lstat(filename).then(info => {
            if (!info.isFile()) throw new Error('Desktop log path must be a regular file');
            return info.size;
          }).catch(error => { ignoreMissing(error); return 0; });
          if (size + Math.min(data.length, maxBytes) > maxBytes && size) {
            await unlink(`${filename}.2`).catch(ignoreMissing);
            await rename(`${filename}.1`, `${filename}.2`).catch(ignoreMissing);
            await rename(filename, `${filename}.1`);
            await cleanupLogs(directory, { now: now(), retentionDays, maxTotalBytes });
          }
          await appendFile(filename, data.subarray(0, maxBytes), { mode: 0o600 });
        } finally { pending -= data.length; }
      });
    },
    cleanup,
    flush: () => queue,
    async close() { clearInterval(timer); await queue; },
  };
}

function safeText(value) {
  return String(value).slice(0, 8000).replace(/-----BEGIN [^-]*PRIVATE KEY-----[\s\S]*?(?:-----END [^-]*PRIVATE KEY-----|$)/g, '[redacted private key]')
    .replace(/https?:\/\/[^\s)]+/g, value => {
      try { const url = new URL(value); url.username = ''; url.password = ''; url.search = ''; url.hash = ''; return url.href; }
      catch { return '[invalid URL]'; }
    });
}
function safeValue(value, key = '', depth = 0) {
  if (/token|password|secret|authorization|cookie|private.?key|signature|api.?key/i.test(key)) return '[redacted]';
  if (value instanceof Error) return { name: value.name, message: safeText(value.message), code: value.code,
    stack: safeText(value.stack || ''), cause: value.cause && depth < 3 ? safeValue(value.cause, '', depth + 1) : undefined };
  if (typeof value === 'string') return safeText(value);
  if (typeof value === 'bigint') return value.toString();
  if (value && typeof value === 'object') return depth >= 4 ? '[nested]' : Object.fromEntries(Object.entries(value).map(([k, v]) => [k, safeValue(v, k, depth + 1)]));
  return value;
}

export function createErrorLogger(options) {
  const writer = createLogWriter({ ...options, kind: 'errors' });
  return {
    operation(context = {}, operationId = randomUUID()) {
      let stage = 'starting';
      let details = { ...context };
      const started = Date.now();
      return { id: operationId, log: async (event, data = {}) => {
        if (!event.endsWith('.failed')) { stage = event; details = { ...details, ...data }; return; }
        await writer.write(JSON.stringify({ ...safeValue({ ...details, ...data }), timestamp: new Date().toISOString(),
          level: 'error', event, stage, operation_id: operationId, elapsed_ms: Date.now() - started }) + '\n');
      } };
    },
    cleanup: writer.cleanup, flush: writer.flush, close: writer.close,
  };
}
