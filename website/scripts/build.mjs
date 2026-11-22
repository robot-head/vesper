import { insertFigures } from './figures.mjs';
import { appendFile, cp, mkdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { escape, highlightSite } from './highlight.mjs';
import { buildTour } from './tour.mjs';
import { buildDocs } from './docs.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const out = resolve(root, 'dist');
await mkdir(out, { recursive: true });
let html = await readFile(resolve(root, 'index.html'), 'utf8');
for (const file of ['transaction.ves', 'projections.ves', 'checked_math.ves']) {
  const original = (await readFile(resolve(root, '..', 'examples', file), 'utf8')).replaceAll('\r\n', '\n').trimEnd();
  // Compact two single-field records for the source tour; tokens remain unchanged.
  const source = original
    .replace(/WriteTxn \{\s+wal_slot\s+\}\s+= self;/g, 'WriteTxn { wal_slot } = self;')
    .replace(/WriteTxn \{\s+wal_slot: 42\s+\};/g, 'WriteTxn { wal_slot: 42 };')
    .replace(/\n{2,}/g, '\n');
  html = html.replace(`<!-- source:${file} -->`, escape(source));
  await mkdir(resolve(out, 'examples'), { recursive: true });
  await writeFile(resolve(out, 'examples', file), source + '\n');
}
if (/<!-- source:/.test(html)) throw new Error('Unresolved source inclusion');
await writeFile(resolve(out, 'index.html'), insertFigures(html));
const guide = await readFile(resolve(root, 'language.html'), 'utf8');
await writeFile(resolve(out, 'language.html'), insertFigures(guide));
const chapters = await buildDocs(root, out);
const wasm = await readFile(resolve(root, '../target/wasm32-unknown-unknown/port-compiler/vesper_tour_wasm.wasm'));
const { instance } = await WebAssembly.instantiate(wasm);
await writeFile(resolve(out, 'vesper-tour.wasm'), wasm);
const tour = await buildTour(root, out, instance.exports);
for (const file of ['search.js', 'docs.css']) {
  await cp(resolve(root, 'docs', file), resolve(out, 'docs', file));
}
const sitemap = await readFile(resolve(root, 'sitemap.xml'), 'utf8');
const chapterUrls = [...chapters, ...tour].map(path => `  <url><loc>https://vesper-lang.org/${path.replace(/index\.html$/, '')}</loc></url>`).join('\n');
await writeFile(resolve(out, 'sitemap.xml'), sitemap.replace('</urlset>', `${chapterUrls}\n</urlset>`));
for (const path of ['assets', 'brand', 'diagrams', 'site.css', 'site.js', 'tour-editor.js', 'tour-worker.js', 'tour-wasm.js', 'favicon.svg', 'vesper-specification.pdf', 'CNAME', 'robots.txt']) {
  await cp(resolve(root, path), resolve(out, path), { recursive: true });
}
const css = await highlightSite(out, ['index.html', 'language.html', 'docs/index.html', ...chapters, ...tour]);
await appendFile(resolve(out, 'site.css'), '\n' + css);
try { await cp(resolve(root, 'share.jpg'), resolve(out, 'share.jpg')); } catch (e) { if (e.code !== 'ENOENT') throw e; }
await writeFile(resolve(out, '.nojekyll'), '');
console.log('Built Vesper website in website/dist (no third-party build dependencies).');
