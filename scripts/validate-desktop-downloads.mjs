import path from 'node:path';
import { readFile } from 'node:fs/promises';
import { verifyManifest, sha256 } from '../apps/desktop/src/bundles.mjs';
import { releaseLog, logReleaseErrors } from './lib/release-logging.mjs';
logReleaseErrors();
const publicKey = await readFile('apps/desktop/assets/runtime-public.pem');
for (const [target, platform, arch] of [['macos-arm64', 'darwin', 'arm64'], ['macos-x64', 'darwin', 'x64'], ['windows-x64', 'win32', 'x64']]) {
  releaseLog('release.verify.started', { target, tag: process.env.RELEASE_TAG });
  const manifest = verifyManifest(JSON.parse(await readFile(`downloads/Lorevia-Core-${target}.json`, 'utf8')), publicKey, arch, platform);
  if (`v${manifest.coreVersion}` !== process.env.RELEASE_TAG) throw new Error('Signed update version does not match the release');
  if (await sha256(path.join('downloads', manifest.archive)) !== manifest.sha256) throw new Error(`Invalid core archive for ${target}`);
  releaseLog('release.verify.completed', { target, runtime_id: manifest.runtimeId, archive: manifest.archive, sha256: manifest.sha256 });
}
