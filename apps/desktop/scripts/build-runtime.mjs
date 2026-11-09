import { cp, mkdir, rm, readFile, writeFile } from 'node:fs/promises';
import { createWriteStream } from 'node:fs';
import { Readable } from 'node:stream';
import { pipeline } from 'node:stream/promises';
import { execFileSync } from 'node:child_process';
import path from 'node:path';
import { sha256 } from '../src/bundles.mjs';
import { systemTar } from '../src/platform.mjs';
import { appRoot, loadTarget } from './lib/target.mjs';
import { rustTarget, snapshotWorkingTree } from './lib/source-snapshot.mjs';

// Finder and Spotlight may write .DS_Store into a folder while it is being removed.
const clear = folder => rm(folder, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });

// Usage: node scripts/build-runtime.mjs [--target macos-arm64|macos-x64|windows-x64] [--assemble] [--new-key] [--working-tree]
const target = await loadTarget();
// Builds from the committed HEAD of the Lorevia checkout this app lives in.
const source = path.resolve(process.env.DEEPTUTOR_SOURCE || path.join(appRoot, '../..'));
const build = path.join(target.local, 'build');
const runtime = target.runtime;
const nodeArchive = path.join(target.local, target.node.file);
const run = (command, args, options = {}) => execFileSync(command, args, { stdio: 'inherit', ...options });
const npm = (args, cwd) => run(target.nodeBin, [target.npmCli, ...args], { cwd, env: {
  ...process.env, PATH: [path.dirname(target.nodeBin), process.env.PATH].join(path.delimiter),
  NEXT_PUBLIC_API_BASE: '__NEXT_PUBLIC_API_BASE_PLACEHOLDER__',
  NEXT_PUBLIC_AUTH_ENABLED: '__NEXT_PUBLIC_AUTH_ENABLED_PLACEHOLDER__', NEXT_TELEMETRY_DISABLED: '1',
} });
const assembleOnly = process.argv.includes('--assemble');
const workingTree = process.argv.includes('--working-tree');
console.log(`Building the ${target.id} core bundle`);

async function fetchNode() {
  try { if (await sha256(nodeArchive) === target.node.sha256) return; }
  catch (error) { if (error.code !== 'ENOENT') throw error; }
  const response = await fetch(`https://nodejs.org/dist/v${target.node.version}/${target.node.file}`);
  if (!response.ok) throw new Error(`Node download failed: ${response.status}`);
  await pipeline(Readable.fromWeb(response.body), createWriteStream(nodeArchive));
}

await mkdir(target.local, { recursive: true });
if (!assembleOnly) {
  const dirty = execFileSync('git', ['-C', source, 'status', '--porcelain', '--untracked-files=normal'], { encoding: 'utf8' }).trim();
  if (dirty && !workingTree) throw new Error('Source checkout has uncommitted changes. Commit them for a release build, or use --working-tree for a labelled local preview.');
  await fetchNode();
  if (await sha256(nodeArchive) !== target.node.sha256) throw new Error('Node runtime checksum mismatch');
  await clear(path.join(target.local, 'node'));
  await mkdir(path.join(target.local, 'node'));
  run(systemTar(), ['-xf', nodeArchive, '-C', path.join(target.local, 'node'), '--strip-components', '1']);

  const commit = execFileSync('git', ['-C', source, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
  await clear(build);
  await mkdir(build, { recursive: true });
  if (workingTree) {
    await snapshotWorkingTree(source, build);
  } else {
    run('git', ['-C', source, 'archive', '--output', path.join(target.local, 'source.tar'), commit]);
    run(systemTar(), ['-xf', path.join(target.local, 'source.tar'), '-C', build]);
  }
  await writeFile(path.join(build, 'BUILD_COMMIT'), commit + '\n');
  await writeFile(path.join(build, 'BUILD_SOURCE.json'), JSON.stringify({ commit, dirty: Boolean(dirty), mode: workingTree ? 'working-tree' : 'commit' }));
  run(process.env.CARGO || 'cargo', ['run', '--locked', '--release', '-p', 'deeptutor-build', '--', '--target', rustTarget(target), '--output', path.join(build, 'native'), '--target-dir', path.join(target.local, 'cargo-target')], { cwd: build });
  npm(['ci', '--legacy-peer-deps'], path.join(build, 'web'));
  npm(['run', 'build'], path.join(build, 'web'));
  npm(['ci', '--omit=dev'], path.join(build, 'packages/video-lesson'));
  const browser = path.join(build, 'packages/browser-testing');
  npm(['ci', '--omit=dev', '--ignore-scripts'], browser);
  run(target.nodeBin, [path.join(browser, 'node_modules/playwright/cli.js'), 'install', '--no-shell', 'chromium'], {
    cwd: browser, env: { ...process.env, PLAYWRIGHT_BROWSERS_PATH: path.join(browser, 'browsers') },
  });
}
if (await sha256(nodeArchive) !== target.node.sha256) throw new Error('Node runtime checksum mismatch');
await clear(runtime);
await mkdir(path.join(runtime, 'core'), { recursive: true });
for (const name of ['native', 'VERSION', 'LICENSE', 'THIRD_PARTY_NOTICES.txt']) {
  await cp(path.join(build, name), path.join(runtime, 'core', name), { recursive: true });
}
await cp(path.join(build, 'packages/video-lesson'), path.join(runtime, 'core/video-lesson'), { recursive: true, verbatimSymlinks: true });
await cp(path.join(build, 'packages/browser-testing'), path.join(runtime, 'core/browser-testing'), {
  recursive: true, verbatimSymlinks: true,
  filter: file => !path.relative(path.join(build, 'packages/browser-testing'), file).split(path.sep).some(part => part === 'test'),
});
await cp(path.join(target.local, 'node'), path.join(runtime, 'node'), { recursive: true, verbatimSymlinks: true });
await cp(path.join(build, 'web/.next/standalone'), path.join(runtime, 'web'), { recursive: true, verbatimSymlinks: true });
await cp(path.join(build, 'BUILD_COMMIT'), path.join(runtime, 'BUILD_COMMIT'));
await writeFile(path.join(runtime, 'BUILD_INFO.json'), JSON.stringify({
  target: target.id, coreCommit: (await readFile(path.join(build, 'BUILD_COMMIT'), 'utf8')).trim(),
  backend: 'rust', node: target.node.version, platform: target.platform, arch: target.arch,
  source: JSON.parse(await readFile(path.join(build, 'BUILD_SOURCE.json'), 'utf8')),
  native: JSON.parse(await readFile(path.join(build, 'native/manifest.json'), 'utf8')),
}, null, 2));
// --new-key passes through for throwaway builds, such as pull requests without the release key.
run(process.execPath, [path.join(appRoot, 'scripts/seal-runtime.mjs'), '--target', target.id,
  ...process.argv.filter(arg => arg === '--new-key')]);
