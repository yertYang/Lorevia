import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdtemp, readFile, readdir, rm, writeFile, symlink, lstat, utimes } from 'node:fs/promises';
import path from 'node:path';
import { tmpdir } from 'node:os';
import { cleanupLogs, createErrorLogger, createLogWriter } from '../apps/desktop/src/logging.mjs';

test('only failures reach disk, with correlated stage, versions, stack and redacted credentials', async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'desktop-error-log-'));
  const logger = createErrorLogger({ directory: root, now: () => new Date('2026-11-06T12:00:00Z') });
  try {
    const operation = logger.operation({ current_version: '1.7.3', platform: 'darwin', arch: 'arm64' }, 'same-update');
    await operation.log('archive.downloading', { target_version: '1.7.4', downloaded_bytes: 100 });
    await logger.flush();
    assert.deepEqual(await readdir(root), []);
    const error = new Error('Download failed at https://user:password@example.com/file?token=sensitive#fragment', { cause: new Error('Connection reset') });
    error.code = 'ECONNRESET';
    await operation.log('download.failed', { error, authorization: 'Bearer sensitive', private_key: 'sensitive' });
    const raw = await readFile(path.join(root, 'errors-2026-11-06.jsonl'), 'utf8');
    const record = JSON.parse(raw);
    assert.equal(record.stage, 'archive.downloading');
    assert.equal(record.operation_id, 'same-update');
    assert.equal(record.current_version, '1.7.3');
    assert.equal(record.target_version, '1.7.4');
    assert.equal(record.error.code, 'ECONNRESET');
    assert.match(record.error.stack, /Download failed/);
    assert.equal(record.error.cause.message, 'Connection reset');
    assert.ok(!raw.includes('sensitive'));
    assert.ok(!raw.includes('user:password'));
    assert.equal(raw.trim().split('\n').length, 1);
  } finally { await logger.close(); await rm(root, { recursive: true, force: true }); }
});

test('retention deletes expired known logs while preserving unrelated files, symlinks and backups', async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'desktop-log-retention-'));
  try {
    for (const name of ['errors-2026-10-01.jsonl', 'runtime-2026-10-20.log.1', 'runtime.log.previous', 'notes.json', 'errors-2026-11-06.jsonl']) {
      await writeFile(path.join(root, name), 'keep or expire');
    }
    const old = new Date('2026-10-01T12:00:00Z');
    await utimes(path.join(root, 'runtime.log.previous'), old, old);
    await symlink(path.join(root, 'notes.json'), path.join(root, 'runtime-2026-10-01.log'));
    await cleanupLogs(root, { now: new Date('2026-11-06T12:00:00Z') });
    assert.deepEqual((await readdir(root)).sort(), ['errors-2026-11-06.jsonl', 'notes.json', 'runtime-2026-10-01.log']);
    assert.ok((await lstat(path.join(root, 'runtime-2026-10-01.log'))).isSymbolicLink());
    assert.equal(await readFile(path.join(root, 'notes.json'), 'utf8'), 'keep or expire');
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('concurrent writes rotate at the size limit, retain two archives and preserve the newest entries', async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'desktop-log-rotation-'));
  const writer = createLogWriter({ directory: root, kind: 'runtime', maxBytes: 100, now: () => new Date('2026-11-06T12:00:00Z') });
  try {
    await Promise.all([1, 2, 3, 4].map(i => writer.write(`${i}${'x'.repeat(78)}\n`)));
    const files = (await readdir(root)).sort();
    assert.equal(files.length, 3);
    const records = await Promise.all(files.map(file => readFile(path.join(root, file), 'utf8')));
    assert.equal(records[0][0], '4');
    assert.equal(records[1][0], '3');
    assert.equal(records[2][0], '2');
    for (const file of files) assert.ok((await lstat(path.join(root, file))).size <= 100);
  } finally { await writer.close(); await rm(root, { recursive: true, force: true }); }
});

test('hourly cleanup also runs in a long-lived app without requiring another update or restart', async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'desktop-log-scheduled-'));
  let now = new Date('2026-11-06T12:00:00Z');
  const writer = createLogWriter({ directory: root, kind: 'runtime', now: () => now, cleanupIntervalMs: 10 });
  try {
    await writer.write('old runtime error');
    now = new Date('2026-11-21T12:00:00Z');
    for (let i = 0; i < 30 && (await readdir(root)).length; i++) await new Promise(resolve => setTimeout(resolve, 10));
    await writer.flush();
    assert.deepEqual(await readdir(root), []);
  } finally { await writer.close(); await rm(root, { recursive: true, force: true }); }
});

test('disk budget removes older archives and logging failures never fail an update', async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'desktop-log-budget-'));
  try {
    for (const name of ['runtime-2026-11-04.log', 'runtime-2026-11-05.log', 'runtime-2026-11-06.log']) await writeFile(path.join(root, name), 'x'.repeat(100));
    await cleanupLogs(root, { now: new Date('2026-11-06T12:00:00Z'), maxTotalBytes: 100 });
    assert.deepEqual(await readdir(root), ['runtime-2026-11-06.log']);
    await writeFile(path.join(root, 'errors-2026-11-04.jsonl'), 'e'.repeat(100));
    await writeFile(path.join(root, 'runtime-2026-11-05.log'), 'r'.repeat(100));
    await writeFile(path.join(root, 'runtime.log.previous'), 'legacy'.repeat(100));
    const today = new Date('2026-11-06T12:00:00Z');
    await utimes(path.join(root, 'runtime.log.previous'), today, today);
    await cleanupLogs(root, { now: new Date('2026-11-06T12:00:00Z'), maxTotalBytes: 200 });
    assert.deepEqual((await readdir(root)).sort(), ['errors-2026-11-04.jsonl', 'runtime-2026-11-06.log']);
    const invalidDirectory = path.join(root, 'a-file');
    await writeFile(invalidDirectory, 'not a directory');
    const errors = [];
    const logger = createErrorLogger({ directory: invalidDirectory, onError: error => errors.push(error) });
    try { await assert.doesNotReject(logger.operation().log('install.failed', { error: new Error('original error') })); }
    finally { await logger.close(); }
    assert.ok(errors.length > 0);
  } finally { await rm(root, { recursive: true, force: true }); }
});
