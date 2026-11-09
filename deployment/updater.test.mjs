import test from 'node:test';
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { mkdtemp, writeFile, rm, readFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { createUpdater, githubRepository } from './updater.mjs';

const exec = promisify(execFile);
const token = 'test-update-token-with-at-least-32-characters';

test('repository links support GitHub forks and never expose credentials or local paths', () => {
  for (const remote of ['https://token:secret@github.com/example/fork.git',
    'git@github.com:example/fork.git', 'ssh://git@github.com/example/fork']) {
    assert.equal(githubRepository(remote), 'https://github.com/example/fork');
  }
  for (const remote of ['/srv/private', 'https://github.com.other/example/fork', 'https://github.com/../fork']) {
    assert.equal(githubRepository(remote), null);
  }
});

async function fixture(t) {
  const root = await mkdtemp(`${tmpdir()}/lorevia-updater-`);
  const source = `${root}/source`, repo = `${root}/checkout`, stateDir = `${root}/state`;
  const git = async (cwd, ...args) => (await exec('git', args, { cwd })).stdout.trim();
  await exec('git', ['init', '--initial-branch=main', source]);
  await git(source, 'config', 'user.email', 'test@example.com');
  await git(source, 'config', 'user.name', 'Update Test');
  await writeFile(`${source}/VERSION`, '1.0.0\n');
  await git(source, 'add', 'VERSION'); await git(source, 'commit', '-m', 'initial');
  await exec('git', ['clone', source, repo]);
  let deployed = await git(source, 'rev-parse', 'HEAD');
  const initial = deployed;
  const calls = [];
  let failBuild = false, blockedBuild;
  const run = async (command, args, env) => {
    if (command === 'git') return git(repo, ...args);
    calls.push({ args, env });
    if (args[0] === 'inspect') return args[2].includes('Health') ? 'healthy' : deployed;
    if (args.includes('build')) {
      if (blockedBuild) await blockedBuild;
      if (failBuild) throw new Error('Build failed for test');
    }
    if (args.includes('up')) deployed = env.DEEPTUTOR_BUILD_REVISION;
    return args.includes('ps') ? 'test-container' : '';
  };
  const server = await createUpdater({ repo, token, project: 'existing-server', stateDir, run, sleep: async () => {} });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const request = async (path, method = 'GET', body, auth = token) => {
    const res = await fetch(`http://127.0.0.1:${server.address().port}${path}`, {
      method, headers: { authorization: `Bearer ${auth}`, 'content-type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    return { status: res.status, body: await res.json() };
  };
  const advance = async () => {
    await writeFile(`${source}/code.txt`, `${Math.random()}\n`);
    await git(source, 'add', '.'); await git(source, 'commit', '-m', 'next');
    return git(source, 'rev-parse', 'HEAD');
  };
  const waitJob = async () => {
    for (let attempt = 0; attempt < 200; attempt++) {
      const { body } = await request('/job');
      if (body && ['failed', 'succeeded'].includes(body.status)) return body;
      await new Promise(resolve => setTimeout(resolve, 10));
    }
    throw new Error('Job did not finish');
  };
  t.after(async () => {
    server.closeAllConnections(); await new Promise(resolve => server.close(resolve));
    await rm(root, { recursive: true, force: true });
  });
  return { root, repo, stateDir, initial, calls, request, advance, waitJob, git,
    failBuild: () => { failBuild = true; },
    block: () => { let release; blockedBuild = new Promise(resolve => { release = resolve; }); return release; },
  };
}

test('authenticated check, pinned code deployment, duplicate protection, persisted success', async t => {
  const f = await fixture(t);
  assert.equal((await f.request('/update', 'POST', undefined, 'wrong')).status, 401);
  assert.equal(f.calls.length, 0);
  assert.equal((await f.request('/update', 'POST')).status, 409);
  const target = await f.advance();
  const checked = await f.request('/check', 'POST');
  assert.equal(checked.status, 200); assert.equal(checked.body.update_available, true);
  // VERSION is unchanged; revision still offers an update.
  assert.equal(checked.body.release.version, checked.body.current_version);
  assert.equal(checked.body.release.revision, target);
  assert.deepEqual(checked.body.source, { branch: 'main', repository: null });
  assert.equal((await f.request('/update', 'POST')).status, 409);
  assert.equal((await f.request('/update', 'POST', { target_revision: f.initial })).status, 409);
  const release = f.block();
  assert.equal((await f.request('/update', 'POST', { target_revision: target })).status, 202);
  assert.equal((await f.request('/update', 'POST', { target_revision: target })).status, 409);
  release();
  assert.equal((await f.waitJob()).status, 'succeeded');
  const status = (await f.request('/status')).body;
  assert.equal(status.current_revision, target); assert.equal(status.update_available, false);
  assert.equal(f.calls.filter(c => c.args.includes('up')).length, 1);
  for (const { args } of f.calls.filter(c => c.args[0] === 'compose')) {
    assert.deepEqual(args.slice(1, 3), ['--project-name', 'existing-server']);
  }
  assert.equal(JSON.parse(await readFile(`${f.stateDir}/update.json`, 'utf8')).job.status, 'succeeded');
});

test('build failure leaves deployed application running and reports failure', async t => {
  const f = await fixture(t); const target = await f.advance(); await f.request('/check', 'POST'); f.failBuild();
  await f.request('/update', 'POST', { target_revision: target });
  assert.match((await f.waitJob()).error, /Build failed/);
  assert.equal((await f.request('/status')).body.current_revision, f.initial);
  assert.equal(f.calls.filter(c => c.args.includes('up')).length, 0);
});

test('local edits and changed confirmation target stop deployment', async t => {
  const f = await fixture(t); const target = await f.advance(); await f.request('/check', 'POST');
  await writeFile(`${f.repo}/VERSION`, 'local-edit\n');
  assert.equal((await f.request('/check', 'POST')).status, 409);
  await f.request('/update', 'POST', { target_revision: target });
  assert.match((await f.waitJob()).error, /local changes/);
  await writeFile(`${f.repo}/VERSION`, '1.0.0\n');
  await f.request('/check', 'POST'); await f.advance();
  await f.request('/update', 'POST', { target_revision: target });
  assert.match((await f.waitJob()).error, /upstream changed/);
  assert.equal(f.calls.filter(c => c.args.includes('build')).length, 0);
});

test('settings persist and interrupted jobs are failed on controller restart', async t => {
  const f = await fixture(t);
  assert.equal((await f.request('/settings', 'PUT', { enabled: false })).body.check_enabled, false);
  assert.equal((await f.request('/check', 'POST')).status, 409);
  await writeFile(`${f.stateDir}/update.json`, JSON.stringify({ enabled: true, job: { status: 'running' } }));
  const recovered = await createUpdater({ repo: f.repo, token, stateDir: f.stateDir, run: async () => '' });
  assert.match(JSON.parse(await readFile(`${f.stateDir}/update.json`, 'utf8')).job.error, /restarted/);
  recovered.close();
});
