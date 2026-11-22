import { checkTourWasm } from './check-tour-wasm.mjs';
import { movedLessons } from './tour.mjs';
import assert from 'node:assert/strict';
import { access, readFile, readdir } from 'node:fs/promises';
import { dirname, posix, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const dist = resolve(root, 'dist');
const pages = new Map();
const docs = (await readdir(resolve(dist, 'docs'))).filter(name => name.endsWith('.html')).map(name => `docs/${name}`);
const tour = (await readdir(resolve(dist, 'tour'))).filter(name => name.endsWith('.html')).map(name => `tour/${name}`);
const manuscript = await readFile(resolve(root, '../docs/manuscript/content.typ'), 'utf8');
const destinations = new Set([...manuscript.matchAll(/^<([\w-]+)>\s*$/gm)].map(m => m[1]));
for (const file of ['index.html', 'language.html', ...docs, ...tour]) {
  const source = await readFile(resolve(dist, file), 'utf8');
  const ids = [...source.matchAll(/\bid="([^"]+)"/g)].map(m => m[1]);
  assert.equal(ids.length, new Set(ids).size, `Duplicate HTML IDs in ${file}`);
  pages.set(file, { source, ids: new Set(ids) });
}
let chromaListings = 0, annotatedListings = 0;
for (const [file, { source, ids }] of pages) {
  for (const [, attrs, codeAttrs, code] of source.matchAll(/<pre\b([^>]*)><code([^>]*)>([\s\S]*?)<\/code><\/pre>/g)) {
    assert.match(codeAttrs, /data-lang="(?:vesper|rust|text|sh)"/, `Unclassified listing in ${file}`);
    if (!codeAttrs.includes('data-lang="vesper"')) continue;
    assert.match(attrs, /data-vc-tier="[01]"/, `Missing Chroma output in ${file}`);
    assert.match(code, /class="vc-/, `Missing Chroma token in ${file}`);
    chromaListings++;
    if (code.includes('class="error-lens"')) {
      assert(code.includes('class="lens-range"') && source.includes('<figcaption><strong>Error '), `Incomplete diagnostic in ${file}`);
      annotatedListings++;
    }
  }
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
for (const file of [...docs, ...tour]) {
  const url = 'https://vesper-lang.org/' + file.replace(/index\.html$/, '');
  assert(sitemap.includes(`<loc>${url}</loc>`), `Missing sitemap URL ${url}`);
}
const tourEditors = tour.filter(file => pages.get(file).source.includes('data-tour-editor'));
const lessonCount = tour.length - Object.keys(movedLessons).length;
assert.equal(tourEditors.length, lessonCount - 1, 'Incorrect number of tour editors');
assert(!tourEditors.includes('tour/read-a-proposed-sugar-declaration.html'), 'Proposed syntax is a reference listing');
const { source: html } = pages.get('index.html');
const visited = new Set();
let current = 'tour/index.html', previous;
while (current) {
  const { source } = pages.get(current);
  assert(!visited.has(current), 'Tour navigation contains a cycle');
  visited.add(current);
  assert(source.includes(`Language tour · ${visited.size} of ${lessonCount}`), 'Incorrect tour progress');
  assert.equal(source.match(/rel="prev" href="([^"]+)"/)?.[1], previous);
  previous = posix.basename(current);
  const next = source.match(/rel="next" href="([^"]+)"/)?.[1];
  current = next ? `tour/${next}` : null;
}
assert.equal(visited.size, lessonCount, 'Tour should reach every lesson');
await checkTourWasm(dist, tour, pages);
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
console.log(`PASS: ${pages.size} pages, ${chromaListings} Chroma listings, ${annotatedListings} annotated failures, ${topics.length} searchable manuscript sections, local links, assets, anchors, examples, PDF destinations, sitemap, and custom domain.`);
