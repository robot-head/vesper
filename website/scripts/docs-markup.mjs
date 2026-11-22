import { escape } from './highlight.mjs';

export function markdownBlocks(source) {
  const blocks = [];
  let paragraph = [];
  function flush() {
    if (paragraph.length) blocks.push(paragraph.join('\n'));
    paragraph = [];
  }
  const lines = source.replaceAll('\r\n', '\n').split('\n');
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const fence = line.match(/^(~{3,}|`{3,})(\w+)(?: +(\[[\w-]+\]|[\w-]+))? *$/);
    if (fence) {
      flush();
      const start = i;
      while (++i < lines.length && lines[i].trimEnd() !== fence[1]) {}
      if (i === lines.length) throw new Error('Unclosed code fence on line ' + (start + 1));
      blocks.push(lines.slice(start, i + 1).join('\n'));
    } else if (/^#{1,3} /.test(line) || /^<!-- (figure|diagram):[\w-]+ -->$/.test(line)) {
      flush();
      blocks.push(line);
    } else if (line.trim()) paragraph.push(line);
    else flush();
  }
  flush();
  return blocks;
}

export function inline(source) {
  return source.split(/(`[^`]+?`)/).map((part, i) => {
    if (i % 2) return `<code>${escape(part.slice(1, -1))}</code>`;
    return escape(part).replace(/\[([^\]]+)\]\(([^)\s]+)\)|§([\dAB]+\.\d+)/g, (match, title, url, id) => {
      if (id) {
        const chapter = id.split('.')[0];
        const file = /^\d+$/.test(chapter) ? 'chapter-' + chapter.padStart(2, '0') : 'appendix-' + chapter.toLowerCase();
        return `<a href="${file}.html#section-${id.toLowerCase().replace('.', '-')}">${match}</a>`;
      }
      return /^(?:https?:\/\/|mailto:|[.#]|[\w-]+(?:\/|\.html))/.test(url)
        ? `<a href="${url}">${title}</a>` : match;
    });
  }).join('');
}

const cells = line => line.trim().replace(/^\||\|$/g, '').split(/(?<!\\)\|/).map(cell => cell.trim().replaceAll('\\|', '|'));
const listingLanguages = new Set(['vesper', 'lean', 'rust', 'toml', 'vsys', 'ebnf', 'sh', 'text', 'svg']);

export function renderBlocks(blocks) {
  return blocks.map(block => {
    const lines = block.split('\n');
    const first = lines[0];
    const fence = first.match(/^(?:~{3,}|`{3,})(\w+)(?: +(\[[\w-]+\]|[\w-]+))?/);
    if (fence) {
      const [, language, tag] = fence;
      const code = lines.slice(1, -1);
      if (language === 'rule') {
        const bar = code.indexOf('---');
        if (bar < 0) throw new Error('Inference rule needs a --- separator');
        return `<figure class="infer-rule"><figcaption>${escape(tag ?? 'Inference rule')}</figcaption><div>${escape(code.slice(0, bar).join('\n'))}</div><div>${escape(code.slice(bar + 1).join('\n'))}</div></figure>`;
      }
      if (!listingLanguages.has(language)) throw new Error('Unknown listing language ' + language);
      return `<pre tabindex="0"><code data-lang="${language}"${language === 'vesper' && tag ? ` data-error="${escape(tag)}"` : ''}>${escape(code.join('\n'))}</code></pre>`;
    }
    const subheading = first.match(/^### (.+?)(?: \{#([\w-]+)\})?$/);
    if (subheading) return `<h3${subheading[2] ? ` id="${subheading[2]}"` : ''}>${inline(subheading[1])}</h3>`;
    if (/^<!-- (figure|diagram):/.test(first)) return first;
    if (first.startsWith('    ')) return `<pre tabindex="0"><code data-lang="text">${escape(block.replace(/^    /gm, ''))}</code></pre>`;
    if (lines.length > 1 && lines[1].includes('|') && cells(lines[1]).every(cell => /^:?-{3,}:?$/.test(cell))) {
      const headings = cells(first).map(cell => `<th scope="col">${inline(cell)}</th>`).join('');
      const rows = lines.slice(2).map(line => '<tr>' + cells(line).map(cell => `<td>${inline(cell)}</td>`).join('') + '</tr>').join('');
      return `<div class="reference-table"><table><thead><tr>${headings}</tr></thead><tbody>${rows}</tbody></table></div>`;
    }
    const marker = /^(?:[-+*] |\d+\. )/;
    if (lines.every(line => marker.test(line))) {
      const tag = /^\d/.test(first) ? 'ol' : 'ul';
      return `<${tag}>${lines.map(line => `<li>${inline(line.replace(marker, ''))}</li>`).join('')}</${tag}>`;
    }
    return `<p>${inline(block)}</p>`;
  }).join('\n');
}
