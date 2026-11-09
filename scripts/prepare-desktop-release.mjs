import { execFileSync } from 'node:child_process';
import { readFile, writeFile, appendFile } from 'node:fs/promises';
import { nextReleaseVersion } from './lib/desktop-release.mjs';
import { releaseImageTag } from './lib/release-tag.mjs';
import { releaseLog, logReleaseErrors } from './lib/release-logging.mjs';

logReleaseErrors();
releaseLog('release.prepare.started');

const git = (...args) => execFileSync('git', args, { encoding: 'utf8' }).trim();
if (!process.env.GITHUB_OUTPUT) throw new Error('GITHUB_OUTPUT is required');
const source = process.env.GITHUB_SHA;
const automatic = process.env.GITHUB_EVENT_NAME === 'push' && process.env.GITHUB_REF === 'refs/heads/master';
let tag = process.env.RELEASE_TAG || '';
let ref = tag || source;
let publish = process.env.PUBLISH === 'true';
let skip = false;

if (automatic) {
  if (!/^[a-f0-9]{40}$/.test(source ?? '')) throw new Error('Invalid source commit');
  git('fetch', 'origin', 'master', '--tags');
  const tags = git('tag', '--list', 'v*').split('\n').filter(Boolean);
  // Re-running the original push must reuse its immutable release commit.
  const existing = tags.find(candidate => {
    if (!/^v\d+\.\d+\.\d+$/.test(candidate)) return false;
    const message = git('show', '-s', '--format=%B', `${candidate}^{commit}`);
    return message.split('\n').includes(`Release-Source: ${source}`)
      && git('show', '-s', '--format=%P', `${candidate}^{commit}`) === source;
  });
  if (existing) {
    releaseLog('release.prepare.reusing', { tag: existing });
    tag = existing;
    ref = git('rev-parse', `${tag}^{commit}`);
    publish = true;
  } else if (git('rev-parse', 'FETCH_HEAD') !== source) {
    console.log('A newer master commit is queued; skipping this superseded push.');
    skip = true;
    releaseLog('release.prepare.skipped', { reason: 'superseded_push' });
  } else {
    if (git('rev-parse', 'HEAD') !== source || git('status', '--porcelain')) throw new Error('Release checkout must be clean and match the push');
    const version = nextReleaseVersion((await readFile('VERSION', 'utf8')).trim(), tags);
    tag = `v${version}`;
    releaseLog('release.prepare.version', { tag });
    await writeFile('VERSION', `${version}\n`);
    git('add', 'VERSION');
    git('-c', 'user.name=github-actions[bot]', '-c', 'user.email=41898282+github-actions[bot]@users.noreply.github.com',
      'commit', '--allow-empty', '-m', `chore(release): prepare Lorevia ${tag} [skip ci]\n\nRelease-Source: ${source}`);
    ref = git('rev-parse', 'HEAD');
    git('tag', tag);
    // Atomic, fast-forward only: a concurrent human push can never be overwritten.
    // GITHUB_TOKEN does not recursively trigger push/release workflows.
    git('push', '--atomic', 'origin', 'HEAD:refs/heads/master', `refs/tags/${tag}`);
    publish = true;
  }
}
if (publish) {
  releaseImageTag(tag);
  if (!automatic && git('show', `${tag}:VERSION`) !== tag.slice(1)) throw new Error('Release tag does not match VERSION');
}
await appendFile(process.env.GITHUB_OUTPUT, `tag=${tag}\nref=${ref}\npublish=${publish}\nskip=${skip}\n`);
releaseLog('release.prepare.completed', { tag, build_commit: ref, publish, skip });
