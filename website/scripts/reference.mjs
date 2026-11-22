import { execFile } from 'node:child_process';
import { resolve } from 'node:path';
import { promisify } from 'node:util';
import { page } from './docs.mjs';
import { escape } from './highlight.mjs';

const compile = promisify(execFile);
const decoded = { amp: '&', gt: '>', lt: '<', quot: '"', apos: "'" };
const unescape = source => source.replace(/&(#x[\da-f]+|#\d+|amp|lt|gt|quot|apos);/gi, (_, token) => {
  if (token.startsWith('#x')) return String.fromCodePoint(Number.parseInt(token.slice(2), 16));
  if (token.startsWith('#')) return String.fromCodePoint(Number(token.slice(1)));
  return decoded[token];
});

export async function manuscriptReference(root) {
  const { stdout } = await compile(process.env.TYPST_BIN || 'typst', [
    'compile', '--format', 'html', '--features', 'html', '--ignore-system-fonts', '--root', resolve(root, '..'),
    resolve(root, '../docs/manuscript/web.typ'), '-',
  ], { encoding: 'utf8', maxBuffer: 8 * 1024 * 1024, windowsHide: true });
  const rawBody = stdout.match(/<body>([\s\S]+)<\/body>/)?.[1];
  if (!rawBody) throw new Error('Typst did not produce an HTML reference');
  const mathStyle = stdout.match(/<style>[\s\S]+?<\/style>/)?.[0] ?? '';
  let frame = 0;
  const body = rawBody.replace(/<svg\b[\s\S]*?<\/svg>/g, svg => {
    // Typst exports independent SVG frames. Give their reusable glyphs
    // distinct IDs when all frames appear in one HTML document.
    const prefix = 'frame-' + ++frame + '-';
    return svg.replace(/(id="|href="#|url\(#)([\w-]+)/g, (_, lead, id) => lead + prefix + id);
  }).replace(/<pre([^>]*)><code data-lang="([\w-]+)">([^<]*)<\/code><\/pre>/g,
    (_, attrs, lang, code) => `<pre${attrs}><code data-lang="${lang}">${escape(unescape(code))}</code></pre>`);
  const intro = `<main class="wrap reader-body manuscript-reference" id="main">
    <h1>Vesper language and platform specification</h1>
    <p class="scope-note">This reference contains implemented features, proposed designs, and models of specific fragments. The listings include illustrations. Check the chapter guides and review notes before relying on a claim.
      <a href="index.html">Browse chapter guides</a> · <a href="../vesper-specification.pdf">Read the PDF</a> ·
      <a href="https://github.com/robot-head/vesper/blob/main/docs/manuscript/REVIEW-STATUS.md">Read review notes</a></p>
    ${body}</main>`;
  const html = page('reference.html', 'Language and platform specification',
    'The complete Vesper manuscript with equations, tables, code listings, and formal inference rules.', intro)
    .replace('</head>', () => mathStyle + '\n</head>');
  const overview = body.slice(body.indexOf('<h2 id="overview">'), body.indexOf('<h2 id="part-1">'));
  const architecture = overview.match(/<table>[\s\S]*?<\/table>/)?.[0];
  if (!architecture) throw new Error('Missing architecture table in the manuscript overview');
  const sections = new Map([...body.matchAll(/<h4 id="([^"]+)">([\s\S]*?)(?=<h[234]\b|$)/g)]
    .map(([, id, contents]) => [id, unescape(contents
      .replace(/<\/\w+>/g, ' ').replace(/<[^>]+>/g, '').replace(/\s+/g, ' ')).trim()]));
  return { html, architecture: architecture.replaceAll('href="#', 'href="reference.html#'), sections };
}
