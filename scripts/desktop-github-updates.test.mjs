import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createHash, generateKeyPairSync, sign } from 'node:crypto';
import { mkdtemp, readFile, readdir, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { canonicalManifest } from '../apps/desktop/src/bundles.mjs';
import { checkGithubUpdate, downloadGithubUpdate, newerVersion } from '../apps/desktop/src/github-updates.mjs';
import { createErrorLogger } from '../apps/desktop/src/logging.mjs';

const repo = 'https://github.com/yertYang/Lorevia';
const api = 'https://api.github.com/repos/yertYang/Lorevia/releases/latest';
const keys = generateKeyPairSync('ed25519');
const bytes = Buffer.from('A signed update archive fixture');
function fixture(platform = 'darwin', arch = 'arm64') {
  const target = `${platform === 'win32' ? 'windows' : 'macos'}-${arch}`;
  const manifestName = `Lorevia-Core-${target}.json`;
  const manifest = { formatVersion: 1, protocolVersion: 1, platform, arch,
    runtimeId: `1.7.4-commit-${target}`, coreVersion: '1.7.4', commit: 'a'.repeat(40),
    archive: `open-deeptutor-core-1.7.4-commit-${target}.tar.gz`, sha256: createHash('sha256').update(bytes).digest('hex') };
  manifest.signature = sign(null, Buffer.from(canonicalManifest(manifest)), keys.privateKey).toString('base64');
  const asset = (name, size) => ({ name, size, state: 'uploaded', browser_download_url: `${repo}/releases/download/v1.7.4/${name}` });
  const release = { tag_name: 'v1.7.4', html_url: `${repo}/releases/tag/v1.7.4`, draft: false, prerelease: false,
    assets: [asset(manifestName, 500), asset(manifest.archive, bytes.length)] };
  const fetchImpl = async url => {
    if (url === api) return Response.json(release);
    if (url === release.assets[0].browser_download_url) return Response.json(manifest);
    if (url === release.assets[1].browser_download_url) return new Response(bytes);
    throw new Error(`Unexpected URL: ${url}`);
  };
  return { release, manifest, fetchImpl, options: { currentVersion: '1.7.3', platform, arch, fetchImpl } };
}

test('stable version comparison never downgrades or uses lexical ordering', () => {
  assert.equal(newerVersion('1.10.0', '1.9.99'), true);
  assert.equal(newerVersion('1.7.4', '1.7.4'), false);
  assert.equal(newerVersion('1.7.3', '1.7.4'), false);
  assert.throws(() => newerVersion('1.7.4rc1', '1.7.3'));
});

test('downloads and verifies the matching signed GitHub core for all supported targets', async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'github-update-'));
  try {
    for (const [platform, arch] of [['darwin', 'arm64'], ['darwin', 'x64'], ['win32', 'x64']]) {
      const { options } = fixture(platform, arch);
      const update = await checkGithubUpdate(options);
      assert.equal(update.available, true);
      assert.equal(update.compatible, true);
      const result = await downloadGithubUpdate({ ...options, update, root, publicKey: keys.publicKey });
      const saved = JSON.parse(await readFile(result.manifestFile, 'utf8'));
      assert.equal(saved.platform, platform);
      assert.equal(saved.arch, arch);
      assert.deepEqual(await readFile(path.join(result.directory, saved.archive)), bytes);
      await rm(result.directory, { recursive: true });
    }
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('older installer-only releases are offered as a manual installer update', async () => {
  const { options, release } = fixture();
  release.assets = [];
  const update = await checkGithubUpdate(options);
  assert.equal(update.available, true);
  assert.equal(update.compatible, false);
});

test('drafts, previews, foreign release links and invalid tags never become updates', async () => {
  for (const change of [{ draft: true }, { prerelease: true }, { tag_name: 'v1.7.4\ninjected' },
    { html_url: 'https://evil.example/release' }, { tag_name: 'v1.7.4rc1' }]) {
    const { options, release } = fixture();
    Object.assign(release, change);
    await assert.rejects(checkGithubUpdate(options));
  }
});

test('forged, cross-platform, incomplete and corrupted downloads are rejected without retaining partial files', async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'github-update-rejected-'));
  try {
    for (const change of ['signature', 'version', 'architecture', 'external', 'incomplete', 'size', 'hash', 'truncated', 'oversized']) {
      const { options, manifest } = fixture();
      const update = await checkGithubUpdate(options);
      const { release } = update;
      if (change === 'signature') manifest.signature = 'invalid';
      if (change === 'version') manifest.coreVersion = '1.7.5';
      if (change === 'architecture') manifest.arch = 'x64';
      if (['version', 'architecture', 'hash'].includes(change)) {
        if (change === 'hash') manifest.sha256 = '0'.repeat(64);
        manifest.signature = sign(null, Buffer.from(canonicalManifest(manifest)), keys.privateKey).toString('base64');
      }
      if (change === 'external') release.assets[1].browser_download_url = 'https://evil.example/core.tar.gz';
      if (change === 'incomplete') release.assets[1].state = 'new';
      if (change === 'size') release.assets[1].size = 0;
      if (change === 'truncated') release.assets[1].size = bytes.length + 1;
      if (change === 'oversized') release.assets[1].size = bytes.length - 1;
      await assert.rejects(downloadGithubUpdate({ ...options, update, root, publicKey: keys.publicKey }), undefined, change);
      assert.deepEqual(await readdir(root), [], change);
    }
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('GitHub failures are reported rather than mistaken for an up-to-date installation', async () => {
  await assert.rejects(checkGithubUpdate({ currentVersion: '1.7.3', fetchImpl: async () => new Response('', { status: 403 }) }), /HTTP 403/);
});

test('real failed update paths persist the HTTP or checksum failure with the matching stage and operation ID', async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'github-error-context-'));
  const logger = createErrorLogger({ directory: root, now: () => new Date('2026-11-06T12:00:00Z') });
  try {
    const check = logger.operation({}, 'failed-check');
    await assert.rejects(checkGithubUpdate({ currentVersion: '1.7.3', log: check.log, fetchImpl: async () => new Response('', { status: 403 }) }));
    const download = logger.operation({}, 'failed-download');
    const { options, manifest } = fixture();
    manifest.sha256 = '0'.repeat(64);
    manifest.signature = sign(null, Buffer.from(canonicalManifest(manifest)), keys.privateKey).toString('base64');
    const update = await checkGithubUpdate({ ...options, log: download.log });
    await assert.rejects(downloadGithubUpdate({ ...options, update, publicKey: keys.publicKey, root: path.join(root, 'downloads'), log: download.log }));
    const records = (await readFile(path.join(root, 'errors-2026-11-06.jsonl'), 'utf8')).trim().split('\n').map(line => JSON.parse(line));
    assert.equal(records.length, 2);
    assert.equal(records[0].operation_id, 'failed-check');
    assert.equal(records[0].stage, 'metadata.response');
    assert.equal(records[0].http_status, 403);
    assert.equal(records[1].operation_id, 'failed-download');
    assert.equal(records[1].stage, 'archive.verifying');
    assert.equal(records[1].target_version, '1.7.4');
    assert.notEqual(records[1].expected_sha256, records[1].actual_sha256);
    assert.equal(records[1].downloaded_bytes, bytes.length);
  } finally { await logger.close(); await rm(root, { recursive: true, force: true }); }
});
