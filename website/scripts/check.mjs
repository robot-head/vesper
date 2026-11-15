import assert from 'node:assert/strict';
import { access, readFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const dist = resolve(root, 'dist');
const html = await readFile(resolve(dist, 'index.html'), 'utf8');
const ids = [...html.matchAll(/\bid="([^"]+)"/g)].map(m => m[1]);
assert.equal(ids.length, new Set(ids).size, 'Duplicate HTML IDs');
for (const [, url] of html.matchAll(/(?:href|src)="([^"]+)"/g)) {
  if (url.startsWith('#')) assert(ids.includes(url.slice(1)), `Missing anchor ${url}`);
  else if (!/^(https?:|data:|mailto:)/.test(url)) await access(resolve(dist, url));
}
for (const [, controls] of html.matchAll(/aria-controls="([^"]+)"/g)) {
  for (const id of controls.split(' ')) assert(ids.includes(id), `Missing controlled element ${id}`);
}
for (const file of ['transaction.ves', 'projections.ves', 'checked_math.ves']) {
  const actual = await readFile(resolve(dist, 'examples', file), 'utf8');
  const original = (await readFile(resolve(root, '..', 'examples', file), 'utf8')).replaceAll('\r\n', '\n').trimEnd() + '\n';
  assert.equal(actual.replace(/\s/g, ''), original.replace(/\s/g, ''), `${file} tokens drifted from the compiler example`);
}
assert.equal((await readFile(resolve(dist, 'CNAME'), 'utf8')).trim(), 'vesper-lang.org');
assert(html.includes('https://vesper-lang.org/'), 'Canonical domain missing');
assert(!/<!-- source:|href="#"/.test(html), 'Unresolved content');
const designActions = [...html.matchAll(/<a\b[^>]*href="([^"]+)"[^>]*>Read the design<\/a>/g)];
assert(designActions.length > 0 && designActions.every(([, url]) => url === 'vesper-specification.pdf'), 'Every design action must open the manuscript');
const specification = await readFile(resolve(dist, 'vesper-specification.pdf'));
assert(specification.subarray(0, 5).equals(Buffer.from('%PDF-')), 'Specification is not a PDF');
console.log('PASS: local links, assets, unique IDs, tabs, source examples, PDF specification actions, and custom domain.');
