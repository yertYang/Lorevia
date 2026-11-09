// The application never receives the Docker socket. This private service owns
// the checkout/build lifecycle and survives replacement of the app container.
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { createServer } from 'node:http';
import { mkdir, readFile, rename, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { randomUUID, timingSafeEqual } from 'node:crypto';

const exec = promisify(execFile);
const active = job => job && ['pending', 'running', 'restarting'].includes(job.status);
const now = () => new Date().toISOString();
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));

// Return only the public repository identity. A remote may contain an HTTPS
// token, SSH user, or a private filesystem path; never send its raw URL to a client.
export function githubRepository(remote) {
  const match = remote.match(/^(?:https:\/\/(?:[^/]*@)?github\.com\/|git@github\.com:|ssh:\/\/git@github\.com\/)([\w.-]+)\/([\w.-]+?)(?:\.git)?\/?$/i);
  return match && [match[1], match[2]].every(part => part !== '.' && part !== '..')
    ? `https://github.com/${match[1]}/${match[2]}` : null;
}

export async function createUpdater({ repo, token, project = 'lorevia', stateDir = '/state', run, sleep = pause } = {}) {
  if (!repo || !token || token.length < 32) throw new Error('Absolute DEEPTUTOR_REPO and update token (32+ characters) are required.');
  if (resolve(repo) !== repo) throw new Error('DEEPTUTOR_REPO must be absolute.');
  run ??= async (command, args, env = {}) => {
    try {
      const result = await exec(command, args, {
        cwd: repo, env: { ...process.env, ...env, GIT_TERMINAL_PROMPT: '0' },
        timeout: 30 * 60_000, maxBuffer: 8 * 1024 * 1024,
      });
      return result.stdout.trim();
    } catch (error) {
      console.error(`${command} ${args.join(' ')} failed`, error.stderr || error.message);
      throw new Error(`${command} ${args.slice(0, 3).join(' ')} failed. See the updater container logs.`);
    }
  };
  const git = (...args) => run('git', ['-c', `safe.directory=${repo}`, ...args]);
  const compose = (args, env) => run('docker', ['compose', '--project-name', project,
    '--project-directory', repo, '-f', `${repo}/compose.yaml`, '-f', `${repo}/compose.server.yaml`, ...args], env);
  await mkdir(stateDir, { recursive: true });
  const statePath = `${stateDir}/update.json`;
  let state;
  try { state = JSON.parse(await readFile(statePath, 'utf8')); }
  catch (error) { if (error.code !== 'ENOENT') throw error; }
  state ??= { enabled: true, checked_at: '', target: '', job: null };
  const save = async () => {
    await writeFile(`${statePath}.tmp`, JSON.stringify(state), { mode: 0o600 });
    await rename(`${statePath}.tmp`, statePath);
  };
  if (active(state.job)) {
    state.job.status = 'failed';
    state.job.error = 'Updater restarted during deployment. Check container health and retry.';
    state.job.finished_at = now();
    await save();
  }
  let busy = false;
  async function deployed() {
    const id = await compose(['ps', '-q', 'deeptutor']);
    if (!id) throw new Error('Application container is not running.');
    const revision = await run('docker', ['inspect', '--format', '{{ index .Config.Labels "org.opencontainers.image.revision" }}', id]);
    if (!/^[a-f0-9]{40}$/.test(revision)) throw new Error('Redeploy with DEEPTUTOR_BUILD_REVISION=$(git rev-parse HEAD) before enabling code updates.');
    return revision;
  }
  async function status() {
    const current = await deployed();
    const target = state.target || current;
    const version = (await git('show', `${current}:VERSION`)).trim();
    const targetVersion = (await git('show', `${target}:VERSION`)).trim();
    return {
      current_version: version, current_revision: current, check_enabled: state.enabled,
      checked_at: state.checked_at, cached: true, check_error: state.error || '',
      update_available: target !== current,
      release: state.checked_at ? { version: targetVersion, revision: target, name: `Server code ${target.slice(0, 12)}`,
        published_at: '', url: '', excerpt: 'Updates the configured Git tracking branch on this server.', migration_warning: false } : null,
      installation: { mode: 'docker', automatic_update: true, command: '', reason: 'Server code updates are managed by Docker Compose.' },
      source: state.source || null, launcher_managed: true, is_admin: true, job: state.job,
    };
  }
  async function check() {
    if (!state.enabled) throw new Error('Version checks are disabled.');
    if ((await git('status', '--porcelain', '--untracked-files=no')).trim()) throw new Error('Server checkout has local changes. Commit or remove them before updating.');
    const branch = await git('symbolic-ref', '--short', 'HEAD');
    const remote = await git('config', '--get', `branch.${branch}.remote`);
    const tracking = await git('config', '--get', `branch.${branch}.merge`);
    if (remote === '.' || !tracking.startsWith('refs/heads/')) throw new Error('Configure a remote tracking branch for server updates.');
    const repository = githubRepository(await git('remote', 'get-url', '--', remote));
    await git('fetch', '--prune', '--', remote);
    // A detached checkout or missing upstream fails here, without changing code.
    const target = await git('rev-parse', '@{upstream}');
    await git('merge-base', '--is-ancestor', 'HEAD', target);
    state.target = target;
    state.source = { branch: tracking.slice('refs/heads/'.length), repository };
    state.checked_at = now();
    state.error = '';
    await save();
  }
  async function perform() {
    try {
      state.job.status = 'running'; state.job.started_at = now(); await save();
      // Recheck even if the browser inspected a cached revision. Never reset or
      // discard server changes; merge only to the exact inspected upstream SHA.
      await check();
      if (state.job.target_version !== state.target) throw new Error('The upstream changed since the last check. Check and confirm the new revision before retrying.');
      await git('merge', '--ff-only', state.target);
      const env = { DEEPTUTOR_BUILD_REVISION: state.target };
      await compose(['build', 'deeptutor'], env);
      state.job.status = 'restarting'; await save();
      await compose(['up', '-d', '--no-deps', '--no-build', '--force-recreate', 'deeptutor'], env);
      for (let attempt = 0; attempt < 120; attempt++) {
        const id = await compose(['ps', '-q', 'deeptutor']);
        if (id) {
          const health = await run('docker', ['inspect', '--format', '{{.State.Health.Status}}', id]);
          if (health === 'healthy' && await deployed() === state.target) {
            state.job.status = 'succeeded'; state.job.restart_count = 1;
            state.job.finished_at = now(); await save(); return;
          }
        }
        await sleep(2000);
      }
      throw new Error('New application did not become healthy within four minutes. Inspect the container logs.');
    } catch (error) {
      state.job.status = 'failed'; state.job.error = error.message;
      state.job.finished_at = now(); await save();
    } finally { busy = false; }
  }
  const server = createServer(async (req, res) => {
    const send = (code, value) => { res.writeHead(code, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' }); res.end(JSON.stringify(value)); };
    const supplied = Buffer.from(req.headers.authorization || '');
    const expected = Buffer.from(`Bearer ${token}`);
    if (supplied.length !== expected.length || !timingSafeEqual(supplied, expected)) return send(401, { detail: 'Unauthorized' });
    try {
      const path = req.url;
      if (req.method === 'GET' && path === '/job') return send(200, state.job);
      if (req.method === 'GET' && path === '/status') return send(200, await status());
      if (busy) return send(409, { detail: 'A server update or check is already running.' });
      if (req.method === 'POST' && path === '/check') {
        busy = true;
        try { await check(); return send(200, await status()); }
        catch (error) { state.error = error.message; await save(); throw error; }
        finally { busy = false; }
      }
      if (req.method === 'PUT' && path === '/settings') {
        const chunks = []; let size = 0;
        for await (const chunk of req) { size += chunk.length; if (size > 1024) return send(413, { detail: 'Request too large' }); chunks.push(chunk); }
        const body = JSON.parse(Buffer.concat(chunks).toString());
        if (typeof body.enabled !== 'boolean') return send(400, { detail: 'enabled must be a boolean' });
        if (busy) return send(409, { detail: 'A server update or check is already running.' });
        busy = true;
        try { state.enabled = body.enabled; await save(); return send(200, await status()); }
        finally { busy = false; }
      }
      if (req.method === 'POST' && path === '/update') {
        const chunks = []; let size = 0;
        for await (const chunk of req) { size += chunk.length; if (size > 1024) return send(413, { detail: 'Request too large' }); chunks.push(chunk); }
        const body = JSON.parse(Buffer.concat(chunks).toString() || '{}');
        if (busy) return send(409, { detail: 'A server update or check is already running.' });
        // Set the lock before the first await so double clicks cannot race.
        busy = true;
        try {
          const current = await deployed();
          if (!state.enabled) throw new Error('Version checks are disabled.');
          if (!state.target) throw new Error('Check for updates before starting a deployment.');
          if (body.target_revision !== state.target) throw new Error('The checked revision changed. Check and confirm the update again.');
          if (state.target === current) throw new Error('The server code is already up to date.');
          state.job = { id: randomUUID(), status: 'pending', current_version: current, target_version: state.target,
            created_at: now(), started_at: null, finished_at: null, error: null, restart_count: 0 };
          await save(); send(202, state.job);
          void perform(); return;
        } catch (error) { busy = false; throw error; }
      }
      send(404, { detail: 'Unknown update operation' });
    } catch (error) { send(409, { detail: error.message }); }
  });
  return server;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const server = await createUpdater({ repo: process.env.DEEPTUTOR_REPO, token: process.env.DEEPTUTOR_UPDATE_TOKEN,
    project: process.env.COMPOSE_PROJECT_NAME });
  server.listen(9090, '0.0.0.0');
}
