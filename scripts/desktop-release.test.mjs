import assert from 'node:assert/strict';
import { test } from 'node:test';
import { execFileSync } from 'node:child_process';
import { mkdtemp, mkdir, readFile, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { nextReleaseVersion, updateReadme, downloadSection } from './lib/desktop-release.mjs';

test('automatic versions increment the highest stable tag and respect explicit version bumps', () => {
  assert.equal(nextReleaseVersion('1.7.3', ['v1.7.3', 'v1.7.9', 'v2.0.0rc1']), '1.7.10');
  assert.equal(nextReleaseVersion('2.0.0', ['v1.7.3']), '2.0.0');
  assert.equal(nextReleaseVersion('1.0.0', []), '1.0.0');
  assert.equal(nextReleaseVersion('1.7.3', ['v1.10.0', 'v1.9.99']), '1.10.1');
  assert.throws(() => nextReleaseVersion('1.7.3-rc1', []));
  assert.throws(() => nextReleaseVersion('1.7.3\ninjected', []));
});

test('README updates all four installer links without changing surrounding prose', () => {
  const before = `Introduction\n${downloadSection('1.7.3', '2026-11-05')}\nInstallation`;
  const after = updateReadme(before, '1.7.4', '2026-11-06');
  assert.ok(after.startsWith('Introduction\n'));
  assert.ok(after.endsWith('\nInstallation'));
  assert.ok(!after.includes('1.7.3'));
  assert.equal((after.match(/releases\/download\/v1.7.4\//g) || []).length, 4);
  assert.throws(() => updateReadme('no markers', '1.7.4', '2026-11-06'));
});

test('push preparation publishes an atomic version commit, reuses it on retry and skips stale pushes', async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'desktop-release-'));
  const work = path.join(root, 'work');
  const remote = path.join(root, 'remote.git');
  const output = path.join(root, 'output');
  const script = fileURLToPath(new URL('./prepare-desktop-release.mjs', import.meta.url));
  const git = (...args) => execFileSync('git', args, { cwd: work, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
  const prepare = source => execFileSync(process.execPath, [script], {
    cwd: work, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, GITHUB_SHA: source, GITHUB_EVENT_NAME: 'push', GITHUB_REF: 'refs/heads/master', GITHUB_OUTPUT: output },
  });
  try {
    await mkdir(work);
    git('init', '--bare', remote);
    git('init', '-b', 'master');
    git('config', 'user.name', 'Test');
    git('config', 'user.email', 'test@example.com');
    await writeFile(path.join(work, 'VERSION'), '1.7.3\n');
    await writeFile(path.join(work, 'README.md'), downloadSection('1.7.3', '2026-11-05'));
    git('add', '.');
    git('commit', '-m', 'Source');
    git('remote', 'add', 'origin', remote);
    git('tag', 'v1.7.3');
    git('push', 'origin', 'master', '--tags');
    const source = git('rev-parse', 'HEAD');
    prepare(source);
    const release = git('rev-parse', 'v1.7.4');
    assert.notEqual(release, source);
    assert.equal(git('show', 'v1.7.4:VERSION'), '1.7.4');
    assert.ok((await readFile(output, 'utf8')).includes('publish=true'));
    assert.ok(git('ls-remote', 'origin', 'refs/heads/master').startsWith(release));
    git('checkout', '--detach', source);
    await writeFile(output, '');
    prepare(source);
    assert.ok((await readFile(output, 'utf8')).includes(`ref=${release}`));
    assert.equal(git('tag', '--list', 'v1.7.5'), '');
    // An unprocessed old source must not publish over the latest master.
    await writeFile(path.join(work, 'other.txt'), 'new work');
    git('add', '.');
    git('commit', '-m', 'Obsolete branch work');
    const stale = git('rev-parse', 'HEAD');
    await writeFile(output, '');
    prepare(stale);
    assert.ok((await readFile(output, 'utf8')).includes('skip=true'));
    assert.equal(git('rev-parse', 'v1.7.4'), release);
  } finally { await rm(root, { recursive: true, force: true }); }
});
