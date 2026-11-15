import { cp, mkdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const out = resolve(root, 'dist');
const escape = s => s.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;').replaceAll('"', '&quot;');
const token = /\/\/[^\n]*|\b(?:linear|copy|affine|struct|impl|fn|let|sink|inout|in|eff|handle|with|return|match|if|else)\b|\b(?:u64|i64|Fail|RuntimeError|WriteTxn|Broker|ArithOverflow)\b|\b\d+\b/g;
function highlight(source) {
  let html = '', end = 0;
  for (const m of source.matchAll(token)) {
    html += escape(source.slice(end, m.index));
    const kind = m[0].startsWith('//') ? 'comment' : /^\d/.test(m[0]) ? 'number' : /^(linear|sink)$/.test(m[0]) ? 'linear' : m[0] === 'inout' ? 'place' : /^(in|eff|handle|with)$/.test(m[0]) ? 'effect' : /^[A-Z]|^[ui]\d/.test(m[0]) ? 'type' : 'keyword';
    html += `<span class="tok-${kind}">${escape(m[0])}</span>`;
    end = m.index + m[0].length;
  }
  return html + escape(source.slice(end));
}

await mkdir(out, { recursive: true });
let html = await readFile(resolve(root, 'index.html'), 'utf8');
for (const file of ['transaction.ves', 'projections.ves', 'checked_math.ves']) {
  const original = (await readFile(resolve(root, '..', 'examples', file), 'utf8')).replaceAll('\r\n', '\n').trimEnd();
  // Compact two single-field records for the source tour; tokens remain unchanged.
  const source = original
    .replace(/WriteTxn \{\s+wal_slot\s+\}\s+= self;/g, 'WriteTxn { wal_slot } = self;')
    .replace(/WriteTxn \{\s+wal_slot: 42\s+\};/g, 'WriteTxn { wal_slot: 42 };')
    .replace(/\n{2,}/g, '\n');
  html = html.replace(`<!-- source:${file} -->`, highlight(source));
  await mkdir(resolve(out, 'examples'), { recursive: true });
  await writeFile(resolve(out, 'examples', file), source + '\n');
}
if (/<!-- source:/.test(html)) throw new Error('Unresolved source inclusion');
await writeFile(resolve(out, 'index.html'), html);
for (const path of ['assets', 'brand', 'site.css', 'site.js', 'favicon.svg', 'vesper-specification.pdf', 'CNAME', 'robots.txt', 'sitemap.xml']) {
  await cp(resolve(root, path), resolve(out, path), { recursive: true });
}
try { await cp(resolve(root, 'share.jpg'), resolve(out, 'share.jpg')); } catch (e) { if (e.code !== 'ENOENT') throw e; }
await writeFile(resolve(out, '.nojekyll'), '');
console.log('Built Vesper website in website/dist (no third-party build dependencies).');
