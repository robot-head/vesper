import { insertFigures } from './figures.mjs';
import { appendFile, cp, mkdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { escape, highlightSite } from './highlight.mjs';
import { buildTour, movedLessons } from './tour.mjs';
import { buildDocs } from './docs.mjs';
import { siteChrome } from './chrome.mjs';
import { manuscriptReference } from './reference.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const out = resolve(root, 'dist');
await mkdir(out, { recursive: true });
let html = await readFile(resolve(root, 'index.html'), 'utf8');
for (const file of ['transaction.ves', 'projections.ves', 'checked_math.ves']) {
  const source = await readFile(resolve(root, '..', 'examples', file));
  html = html.replace(`<!-- source:${file} -->`, () => escape(source.toString('utf8').replaceAll('\r\n', '\n').trimEnd()));
  await mkdir(resolve(out, 'examples'), { recursive: true });
  await writeFile(resolve(out, 'examples', file), source);
}
if (/<!-- source:/.test(html)) throw new Error('Unresolved source inclusion');
await writeFile(resolve(out, 'index.html'), siteChrome(insertFigures(html), '', 'index.html'));
const unwind = (await readFile(resolve(root, '../examples/rejected/error_unwind.ves'), 'utf8')).replaceAll('\r\n', '\n').trimEnd();
const guide = (await readFile(resolve(root, 'language.html'), 'utf8')).replace('<!-- error-unwind -->', () => escape(unwind));
await writeFile(resolve(out, 'language.html'), siteChrome(insertFigures(guide), '', 'language.html'));
const reference = await manuscriptReference(root);
const { pages: chapters, topics } = await buildDocs(root, out, reference.architecture, reference.sections);
await writeFile(resolve(out, 'docs/reference.html'), reference.html);
chapters.push('docs/reference.html');
const wasm = await readFile(resolve(root, '../target/wasm32-unknown-unknown/port-compiler/vesper_tour_wasm.wasm'));
const { instance } = await WebAssembly.instantiate(wasm);
await writeFile(resolve(out, 'vesper-tour.wasm'), wasm);
const tour = await buildTour(root, out, instance.exports);
const activeLessons = tour.filter(path => !Object.hasOwn(movedLessons, path.slice(5)));
const plain = html => html.replace(/<[^>]+>/g, ' ').replace(/\s+/g, ' ').trim();
for (const [, id, title, body] of guide.matchAll(/<section id="([^"]+)">\s*<h2>(.*?)<\/h2>([\s\S]*?)<\/section>/g)) {
  topics.push({ title: plain(title), text: plain(body), chapter: 'Language guide', url: `../language.html#${id}` });
}
for (const path of activeLessons) {
  const lesson = await readFile(resolve(out, path), 'utf8');
  const title = plain(lesson.match(/<h1>(.*?)<\/h1>/)[1]);
  const text = plain(lesson.match(/class="lesson">([\s\S]*?)<\/div>/)[1]) + ' ' + (lesson.match(/data-repair="([^"]+)"/)?.[1] ?? '');
  topics.push({ title, text, chapter: 'Tour', url: '../' + path });
}
await writeFile(resolve(out, 'docs/search-index.json'), JSON.stringify(topics));
for (const file of ['search.js', 'docs.css']) {
  await cp(resolve(root, 'docs', file), resolve(out, 'docs', file));
}
const sitemap = await readFile(resolve(root, 'sitemap.xml'), 'utf8');
const chapterUrls = [...chapters, ...activeLessons]
  .map(path => `  <url><loc>https://vesper-lang.org/${path.replace(/index\.html$/, '')}</loc></url>`).join('\n');
await writeFile(resolve(out, 'sitemap.xml'), sitemap.replace('</urlset>', `${chapterUrls}\n</urlset>`));
for (const path of ['brand', 'diagrams', 'site.css', 'site.js', 'theme.js', 'tour-editor.js', 'tour-worker.js', 'tour-wasm.js', 'favicon.svg', 'vesper-specification.pdf', 'CNAME', 'robots.txt']) {
  await cp(resolve(root, path), resolve(out, path), { recursive: true });
}
const css = await highlightSite(out, ['index.html', 'language.html', 'docs/index.html', ...chapters, ...tour]);
await appendFile(resolve(out, 'site.css'), '\n' + css);
try { await cp(resolve(root, 'share.jpg'), resolve(out, 'share.jpg')); } catch (e) { if (e.code !== 'ENOENT') throw e; }
await writeFile(resolve(out, '.nojekyll'), '');
console.log('Built Vesper website and manuscript reference in website/dist.');
