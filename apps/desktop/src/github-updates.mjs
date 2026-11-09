import { createWriteStream } from 'node:fs';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { Readable, Transform } from 'node:stream';
import { pipeline } from 'node:stream/promises';
import { verifyManifest, sha256 } from './bundles.mjs';
import { targetId } from './platform.mjs';

const repository = 'https://github.com/yertYang/Lorevia';
const api = 'https://api.github.com/repos/yertYang/Lorevia/releases/latest';
const stable = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;

export function newerVersion(next, current) {
  if (!stable.test(next) || !stable.test(current)) throw new Error('Invalid stable update version');
  const a = next.split('.').map(BigInt), b = current.split('.').map(BigInt);
  for (let i = 0; i < 3; i++) if (a[i] !== b[i]) return a[i] > b[i];
  return false;
}

function asset(release, name) {
  const expected = `${repository}/releases/download/${release.tag_name}/${name}`;
  const row = release.assets?.find(value => value.name === name && value.state === 'uploaded'
    && Number.isSafeInteger(value.size) && value.size > 0 && value.browser_download_url === expected);
  if (!row) throw new Error(`The GitHub release is missing a complete update asset: ${name}`);
  return row;
}

async function json(url, fetchImpl, log) {
  await log('metadata.request', { url });
  const response = await fetchImpl(url, {
    headers: { Accept: 'application/vnd.github+json' }, signal: AbortSignal.timeout(30_000),
    credentials: 'omit', cache: 'no-store',
  });
  await log('metadata.response', { http_status: response.status });
  if (!response.ok) throw new Error(`GitHub update check failed (HTTP ${response.status})`);
  const chunks = [];
  let bytes = 0;
  for await (const chunk of response.body) {
    bytes += chunk.byteLength;
    if (bytes > 1024 * 1024) throw new Error('GitHub update metadata is too large');
    chunks.push(Buffer.from(chunk));
  }
  return JSON.parse(Buffer.concat(chunks).toString('utf8'));
}

export async function checkGithubUpdate({ currentVersion, platform = process.platform, arch = process.arch, fetchImpl = fetch, log = async () => {} }) {
  await log('check.started', { current_version: currentVersion, platform, arch });
  try {
    const release = await json(api, fetchImpl, log);
    if (release.draft !== false || release.prerelease !== false || !Array.isArray(release.assets)
      || typeof release.tag_name !== 'string' || !stable.test(release.tag_name.slice(1)) || !release.tag_name.startsWith('v')
      || release.html_url !== `${repository}/releases/tag/${release.tag_name}`) throw new Error('Invalid GitHub release');
    const version = release.tag_name.slice(1);
    const manifestName = `Lorevia-Core-${targetId(platform, arch)}.json`;
    let compatible = false;
    try { asset(release, manifestName); compatible = true; } catch { /* Older releases provide installers only. */ }
    const available = newerVersion(version, currentVersion);
    await log('check.completed', { target_version: version, available, compatible });
    return { version, available, compatible, url: release.html_url, release, manifestName };
  } catch (error) { await log('check.failed', { error }); throw error; }
}

/** Download only a signed manifest and its hash-bound archive from the same release. */
export async function downloadGithubUpdate({ update, root, publicKey, platform = process.platform, arch = process.arch, fetchImpl = fetch, log = async () => {} }) {
  let directory;
  try {
    await log('manifest.downloading', { target_version: update.version, platform, arch });
    const manifestAsset = asset(update.release, update.manifestName);
    const downloadedManifest = await json(manifestAsset.browser_download_url, fetchImpl, log);
    await log('manifest.verifying');
    const manifest = verifyManifest(downloadedManifest, publicKey, arch, platform);
    if (manifest.coreVersion !== update.version) throw new Error('The signed core version does not match the GitHub release');
    const archive = asset(update.release, manifest.archive);
    await mkdir(root, { recursive: true, mode: 0o700 });
    directory = await mkdtemp(path.join(root, 'github-'));
    await log('archive.downloading', { runtime_id: manifest.runtimeId, archive: manifest.archive, expected_bytes: archive.size, url: archive.browser_download_url });
    const response = await fetchImpl(archive.browser_download_url, { signal: AbortSignal.timeout(30 * 60_000), credentials: 'omit' });
    await log('archive.response', { http_status: response.status });
    if (!response.ok || !response.body) throw new Error(`GitHub update download failed (HTTP ${response.status})`);
    let bytes = 0;
    let percent = -10;
    const limit = new Transform({ transform(chunk, encoding, callback) {
      bytes += chunk.length;
      const nextPercent = Math.floor(bytes / archive.size * 10) * 10;
      if (nextPercent > percent) { percent = nextPercent; void log('archive.downloading', { downloaded_bytes: bytes, percent }); }
      callback(bytes > archive.size ? new Error('Update download exceeds the published file size') : null, chunk);
    } });
    const filename = path.join(directory, manifest.archive);
    await pipeline(Readable.fromWeb(response.body), limit, createWriteStream(filename, { flags: 'wx', mode: 0o600 }));
    const actualHash = await sha256(filename);
    await log('archive.verifying', { downloaded_bytes: bytes, expected_sha256: manifest.sha256, actual_sha256: actualHash });
    if (bytes !== archive.size || actualHash !== manifest.sha256) throw new Error('The GitHub update checksum does not match the signed manifest');
    const manifestFile = path.join(directory, 'runtime.json');
    await writeFile(manifestFile, JSON.stringify(manifest), { mode: 0o600 });
    await log('download.completed');
    return { manifestFile, directory };
  } catch (error) {
    await log('download.failed', { error });
    if (directory) await rm(directory, { recursive: true, force: true }).catch(async cleanupError => { await log('download_cleanup.failed', { error: cleanupError }); });
    throw error;
  }
}
