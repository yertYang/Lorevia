import { readFile, writeFile } from 'node:fs/promises';
import { updateReadme } from './lib/desktop-release.mjs';
const tag = process.env.RELEASE_TAG;
if (!tag?.startsWith('v')) throw new Error('RELEASE_TAG is required');
const readme = await readFile('README.md', 'utf8');
await writeFile('README.md', updateReadme(readme, tag.slice(1), new Date().toISOString().slice(0, 10)));
