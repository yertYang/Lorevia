import { execFileSync } from 'node:child_process';
import { copyFile, lstat, mkdir } from 'node:fs/promises';
import path from 'node:path';

const roots = new Set(['assets', 'web', 'rust']);
const files = new Set(['Cargo.toml', 'Cargo.lock', 'VERSION', 'LICENSE', 'THIRD_PARTY_NOTICES.txt']);

export function includeSource(relative) {
  const parts = relative.replaceAll('\\', '/').split('/');
  if (parts.some(part => !part || part === '.' || part === '..' || part.startsWith('.env') || ['node_modules', 'target', '__pycache__', '.next', '.local'].includes(part) || part.startsWith('.next-'))) return false;
  return parts.length === 1 ? files.has(parts[0]) : roots.has(parts[0]) || (parts[0] === 'packages' && ['browser-testing', 'video-lesson'].includes(parts[1]));
}

/** Explicit local-development mode; never copies data/, credentials or builds. */
export async function snapshotWorkingTree(source, destination) {
  const names = execFileSync('git', ['-C', source, 'ls-files', '--cached', '--others', '--exclude-standard', '-z'], { encoding: 'utf8' }).split('\0');
  for (const name of new Set(names.filter(includeSource))) {
    const from = path.join(source, name);
    let info;
    try { info = await lstat(from); }
    catch (error) { if (error.code === 'ENOENT') continue; throw error; }
    if (!info.isFile() || info.isSymbolicLink()) throw new Error(`Working-tree source must be a regular file: ${name}`);
    const to = path.join(destination, name);
    await mkdir(path.dirname(to), { recursive: true });
    await copyFile(from, to);
  }
}

export function rustTarget(target) {
  const triple = {
    'darwin:arm64': 'aarch64-apple-darwin',
    'darwin:x64': 'x86_64-apple-darwin',
    'win32:x64': 'x86_64-pc-windows-msvc',
  }[`${target.platform}:${target.arch}`];
  if (!triple) throw new Error(`Unsupported native desktop target: ${target.id}`);
  return triple;
}
