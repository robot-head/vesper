import { execFileSync, spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const escape = text => text.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;').replaceAll('"', '&quot;');
const entities = { '&lt;': '<', '&gt;': '>', '&amp;': '&', '&quot;': '"', '&#13;': '\r' };
const decode = html => html.replace(/&(?:lt|gt|amp|quot|#13);/g, key => entities[key]);
const bin = process.env.VESPER_BIN || fileURLToPath(new URL(
  '../../target/debug/' + (process.platform === 'win32' ? 'vesper.exe' : 'vesper'), import.meta.url));
const options = { encoding: 'utf8', windowsHide: true };

function rust(source) {
  return escape(source).replace(/\/\/[^\n]*|\b(?:struct|let|fn|mut|[ui]\d+|[A-Z]\w*)\b/g, word => {
    const kind = word.startsWith('//') ? 'comment' : /^[A-Z]|^[ui]\d/.test(word) ? 'type' : 'keyword';
    return `<span class="tok-${kind}">${word}</span>`;
  });
}

function diagnostic(file, expected) {
  const forge = expected.startsWith('FORGE');
  const args = forge ? ['forge', 'check'] : ['check'];
  const result = spawnSync(bin, [...args, '--json', file], options);
  if (result.error) throw result.error;
  if (result.status !== 1) throw new Error(`Expected checker rejection ${expected}, got status ${result.status}`);
  const report = JSON.parse(result.stdout);
  const primary = (forge ? report.diagnostics : report).find(d => d.code === expected);
  if (!primary?.span) throw new Error(`Missing expected diagnostic ${expected}: ${result.stdout}`);
  return primary;
}

// Compiler spans count UTF-8 bytes. Walk Chroma's text nodes without touching
// its tags or hover text, and underline the exact range named by the checker.
function underline(html, source, { start, end }) {
  const bytes = Buffer.from(source);
  const from = bytes.subarray(0, start).toString().length;
  const to = bytes.subarray(0, end).toString().length;
  let offset = 0;
  return html.split(/(<[^>]+>)/).map(part => {
    if (part.startsWith('<')) return part;
    const text = decode(part);
    const left = Math.max(0, from - offset), right = Math.min(text.length, to - offset);
    offset += text.length;
    if (left >= right) return part;
    return escape(text.slice(0, left)) + '<span class="lens-range">' + escape(text.slice(left, right)) + '</span>' + escape(text.slice(right));
  }).join('');
}

function renderVesper(file, source, attrs, expected) {
  writeFileSync(file, source);
  const rendered = execFileSync(bin, ['highlight', '--html', '--no-line-numbers', file], { ...options, stdio: ['ignore', 'pipe', 'pipe'] });
  const [, tier, code] = rendered.trimEnd().match(/^<pre class="vc" data-vesper-theme="dusk" data-vc-tier="([01])"><code>([\s\S]*)<\/code><\/pre>$/);
  let body = code, caption = '';
  if (expected) {
    const error = diagnostic(file, expected);
    const before = Buffer.from(source).subarray(0, error.span.start).toString();
    const line = before.split('\n').length;
    const column = Array.from(before.slice(before.lastIndexOf('\n') + 1)).length + 1;
    const lines = underline(code, source, error.span).split('\n');
    lines[line - 1] += `<span aria-hidden="true" class="error-lens" data-code="× ${escape(error.code)}" data-message="${escape(error.message)}"></span>`;
    body = lines.join('\n');
    caption = `<figcaption><strong>Error ${escape(error.code)}</strong> · Line ${line}, column ${column}<br>${escape(error.message)}</figcaption>`;
  }
  if (decode(body.replace(/<[^>]+>/g, '')) !== source) {
    throw new Error('Highlighting changed the copyable Vesper source');
  }
  const pre = `<pre${attrs} class="vc" data-vc-tier="${tier}"><code data-lang="vesper">${body}</code></pre>`;
  return caption ? `<figure class="diagnostic-example">${pre}${caption}</figure>` : pre;
}

export async function highlightSite(out, pages) {
  const dir = mkdtempSync(join(tmpdir(), 'vesper-site-'));
  try {
    const css = execFileSync(bin, ['highlight', '--css'], options);
    for (const page of pages) {
      const path = resolve(out, page);
      const html = await readFile(path, 'utf8');
      const result = html.replace(/<pre([^>]*)><code data-lang="(vesper|rust)"(?: data-error="([\w-]+)")?>([^<]*)<\/code><\/pre>/g,
        (_, attrs, lang, expected, raw) => {
          const source = decode(raw).replaceAll('\r\n', '\n');
          return lang === 'rust' ? `<pre${attrs}><code data-lang="rust">${rust(source)}</code></pre>`
            : renderVesper(join(dir, 'example.ves'), source, attrs, expected);
        });
      if (result !== html) await writeFile(path, result);
    }
    return css;
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}
