import assert from 'node:assert/strict';
import { access, readFile, readdir } from 'node:fs/promises';
import { dirname, posix, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const dist = resolve(root, 'dist');
const pages = new Map();
const docs = (await readdir(resolve(dist, 'docs'))).filter(name => name.endsWith('.html')).map(name => `docs/${name}`);
const manuscript = await readFile(resolve(root, '../docs/manuscript/content.typ'), 'utf8');
const destinations = new Set([...manuscript.matchAll(/^<([\w-]+)>\s*$/gm)].map(m => m[1]));
for (const file of ['index.html', 'language.html', ...docs]) {
  const source = await readFile(resolve(dist, file), 'utf8');
  const ids = [...source.matchAll(/\bid="([^"]+)"/g)].map(m => m[1]);
  assert.equal(ids.length, new Set(ids).size, `Duplicate HTML IDs in ${file}`);
  pages.set(file, { source, ids: new Set(ids) });
}
for (const [file, { source, ids }] of pages) {
  for (const [, url] of source.matchAll(/(?:href|src)="([^"]+)"/g)) {
    if (/^(https?:|data:|mailto:)/.test(url)) continue;
    const [path, anchor] = url.split('#');
    const target = path ? posix.join(posix.dirname(file), path) : file;
    if (path) await access(resolve(dist, target));
    if (anchor !== undefined && target === 'vesper-specification.pdf') {
      assert(anchor.startsWith('nameddest=') && destinations.has(anchor.slice(10)), `Unknown manuscript destination ${url}`);
    } else if (anchor !== undefined) {
      assert(pages.get(target)?.ids.has(anchor), `Missing anchor ${url} in ${file}`);
    }
  }
  for (const [, controls] of source.matchAll(/aria-controls="([^"]+)"/g)) {
    for (const id of controls.split(' ')) assert(ids.has(id), `Missing controlled element ${id}`);
  }
}
const catalog = pages.get('docs/index.html').source;
const topics = JSON.parse(catalog.match(/<script type="application\/json" id="search-index">([^<]+)<\/script>/)[1]);
const expected = [...manuscript.matchAll(/^=== ([\dAB]+\.\d+) /gm)].map(m => m[1]);
assert.deepEqual(topics.map(topic => topic.id), expected, 'Search must cover the manuscript sections in order');
for (const { id, url } of topics) {
  const [page, section] = url.split('#');
  assert(pages.get(`docs/${page}`)?.ids.has(section), `Broken search result for §${id}`);
  assert(catalog.includes(`href="${page}"`), `Missing chapter link to ${page}`);
}
const sitemap = await readFile(resolve(dist, 'sitemap.xml'), 'utf8');
for (const file of docs) {
  const url = 'https://vesper-lang.org/' + file.replace(/index\.html$/, '');
  assert(sitemap.includes(`<loc>${url}</loc>`), `Missing sitemap URL ${url}`);
}
const { source: html } = pages.get('index.html');
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
console.log(`PASS: ${pages.size} pages, ${topics.length} searchable manuscript sections, local links, assets, anchors, examples, PDF destinations, sitemap, and custom domain.`);
