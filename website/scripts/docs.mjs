import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';

const parts = [
  'Language foundations', 'Compiler, proofs, and packages', 'Memory and runtime',
  'Developer tools', 'Operating systems and security', 'Data and distributed systems',
  'Interfaces and compute', 'Proof models and visual identity',
];
const escape = text => text.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;').replaceAll('"', '&quot;');
const inline = text => escape(text).replace(/`([^`]+)`/g, '<code>$1</code>');
function renderBlocks(blocks) {
  return blocks.map(block => block.startsWith('    ')
    ? `<pre tabindex="0"><code>${escape(block.replace(/^    /gm, ''))}</code></pre>`
    : `<p>${inline(block)}</p>`).join('\n');
}
const filename = id => /^\d+$/.test(id) ? `chapter-${id.padStart(2, '0')}.html` : `appendix-${id.toLowerCase()}.html`;
const label = id => /^\d+$/.test(id) ? `Chapter ${id}` : `Appendix ${id}`;
const pdf = anchor => `../vesper-specification.pdf#nameddest=${anchor}`;
const sectionId = id => 'section-' + id.toLowerCase().replace('.', '-');

// Keep the printed section number and the Typst label separate. In particular,
// the insertion of chapter 6 preserved older PDF destinations.
function manuscriptOutline(source) {
  const headings = new Map();
  let part = 0;
  for (const match of source.matchAll(/^(={1,3}) (.+)\r?\n<([^>\r\n]+)>/gm)) {
    const [, level, title, anchor] = match;
    if (level === '=') {
      if (title.startsWith('Part ')) part++;
      continue;
    }
    const id = title.match(/^(?:Appendix )?(\d+(?:\.\d+)?|[AB](?:\.\d+)?)[. ]/)?.[1];
    if (!id || headings.has(id)) throw new Error(`Unexpected manuscript heading: ${title}`);
    headings.set(id, { title: title.trim(), anchor, part });
  }
  return headings;
}

function readLessons(source, part, outline, covered) {
  const chapters = [];
  let chapter, entry;
  for (const block of source.trimEnd().split(/\r?\n(?:[ \t]*\r?\n)+/)) {
    const line = block.trim().replace(/\s*\r?\n\s*/g, ' ');
    const heading = line.match(/^(#{1,2}) ([\dAB]+(?:\.\d+)?) (.+)$/);
    if (!heading) {
      if (!entry || line.startsWith('#')) throw new Error(`Unexpected lesson text: ${line}`);
      entry.blocks.push(block.startsWith('    ') ? block : line);
      continue;
    }
    const [, level, id, title] = heading;
    const original = outline.get(id);
    if (original?.part !== part || covered.has(id)) throw new Error(`Misplaced or duplicate lesson ${id}`);
    covered.add(id);
    entry = { id, title, original, blocks: [] };
    if (level === '#') {
      chapter = { ...entry, part, sections: [] };
      chapters.push(chapter);
    } else {
      if (chapter?.id !== id.split('.')[0]) throw new Error(`Misplaced section ${id}`);
      chapter.sections.push(entry);
    }
  }
  return chapters;
}

export function page(file, title, description, body, directory = 'docs') {
  const script = directory === 'docs' && file === 'index.html' ? '<script defer src="search.js"></script>' : '';
  return `<!doctype html>
<html lang="en" data-vesper-theme="dusk">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>${escape(title)} · Vesper documentation</title>
  <meta name="description" content="${escape(description)}">
  <meta name="theme-color" content="#101111">
  <link rel="canonical" href="https://vesper-lang.org/${directory}/${file === 'index.html' ? '' : file}">
  <link rel="icon" href="../favicon.svg" type="image/svg+xml">
  <link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Space+Grotesk:wght@400;500;600;700&amp;family=Inter:wght@400;500;600&amp;family=JetBrains+Mono:wght@400;500&amp;display=swap">
  <link rel="stylesheet" href="../site.css">
  <link rel="stylesheet" href="../docs/docs.css">
  <noscript><style>.theme { display: none; }</style></noscript>
  ${script}
</head>
<body>
  <a class="skip" href="#main">Skip to content</a>
  <header class="nav"><div class="wrap reader-bar docs-bar">
    <a class="brand" href="../index.html" aria-label="Vesper home"><img data-brand-mark src="../brand/vesper-mark-dusk.svg" width="34" height="34" alt="">vesper</a>
    <a class="docs-home" href="../docs/index.html">Docs</a>
    <a href="../language.html">Language guide</a>
    <label class="theme" title="Switch between dusk and dawn"><input type="checkbox" id="theme-toggle" role="switch" aria-label="Dawn theme"><span aria-hidden="true">◐</span></label>
  </div></header>
  ${body}
  <footer><div class="wrap footer-bar"><a href="../docs/index.html">Documentation home</a><a href="#main">Back to top</a></div></footer>
  <script type="module" src="../site.js"></script>
</body></html>\n`;
}

const practice = {
  1: ['ownership', 'Run the transaction example'],
  2: ['functions', 'Run your first Vesper function'],
  3: ['ownership', 'Try consuming a linear value'],
  4: ['places', 'Try borrowing two fields'],
  5: ['effects', 'Handle division by zero'],
  6: ['resumptions', 'Run a one-shot handler'],
  7: ['models', 'Select a named model'],
  18: ['places', 'Reproduce a field projection diagnostic'],
  20: ['tests', 'Run a table-driven test'],
};

const figures = {
  '4.3': `<figure class="concept-figure"><img src="../brand/broker-fields.webp" width="1360" height="840" loading="lazy" alt="The diagram separates the broker’s three fields into layers."><figcaption>The append operation borrows <code>index</code> and <code>metrics</code> for updates. A separate loan lets it read <code>config</code> during the same call.</figcaption></figure>`,
  'B.1': `<figure class="concept-figure prism"><img data-brand-mark src="../brand/vesper-mark-dusk.svg" width="256" height="256" loading="lazy" alt="A V-shaped prism with an upper chevron and open channels between three blades."><figcaption>The blades mark consumption, exclusive access, and shared access or an effect. The gaps keep the three visible in monochrome.</figcaption></figure>`,
};

function article(chapter, previous, next) {
  const { id, title, part, sections } = chapter;
  const neighbor = (other, direction) => other ? `<a href="${filename(other.id)}"><small>${direction} · ${label(other.id)}</small>${escape(other.title)}</a>` : '';
  const body = `<main class="wrap reader docs-reader" id="main">
    <div class="reader-intro">
      <a class="part-link" href="index.html#part-${part}">${parts[part - 1]}</a>
      <p class="chapter-label">${label(id)} · Manuscript guide</p>
      <h1>${escape(title)}</h1>${renderBlocks(chapter.blocks)}
    </div>
    <nav class="contents" aria-label="Chapter contents"><h2>In this chapter</h2>
      ${sections.map(s => `<a href="#${sectionId(s.id)}">${escape(s.title)}</a>`).join('\n')}
    </nav>
    <div class="reader-body">
      ${sections.map(s => `<section id="${sectionId(s.id)}"><h2>${escape(s.title)}</h2>${renderBlocks(s.blocks)}${figures[s.id] ?? ''}
        <p class="source-ref"><a href="${pdf(s.original.anchor)}" aria-label="Manuscript section ${s.id}, ${escape(s.title)} (PDF)">Manuscript §${s.id} (PDF)</a></p></section>`).join('\n')}
      <div class="chapter-sources">
        <a href="${pdf(chapter.original.anchor)}">Read ${label(id).toLowerCase()} in the specification (PDF)</a>
        ${practice[id] ? `<a href="../language.html#${practice[id][0]}">${practice[id][1]}</a>` : ''}
      </div>
      <nav class="neighbors" aria-label="Adjacent chapters">${neighbor(previous, 'Previous')}${neighbor(next, 'Next')}</nav>
    </div></main>`;
  return page(filename(id), title, chapter.blocks[0], body);
}

function homepage(chapters) {
  const topics = chapters.flatMap(ch => ch.sections.map(s => ({
    chapter: ch.title, part: parts[ch.part - 1], title: s.title, id: s.id,
    url: `${filename(ch.id)}#${sectionId(s.id)}`,
    text: [ch.original.title, s.original.title, ...s.blocks].join(' ').replaceAll('`', ''),
  })));
  const body = `<main class="wrap docs-index" id="main">
    <header class="docs-intro"><h1>Vesper documentation</h1>
      <p class="lead">Run a program in the language guide, or look up a concept from the specification. The chapter guides cover the compiler, runtime, and proposed platform designs.</p>
      <div class="actions"><a class="button primary" href="../language.html">Start the language guide</a><a class="button" href="../tour/index.html">Take the guided tour</a><a class="button" href="../vesper-specification.pdf">Read the specification (PDF)</a></div>
    </header>
    <div class="docs-search" hidden>
      <label for="topic-query">Find a concept</label>
      <input type="search" id="topic-query" placeholder="Try “field loans”, “escrow”, or “KV cache”" autocomplete="off" aria-controls="search-results">
      <div role="status" id="search-status"></div>
      <ul id="search-results" hidden></ul>
    </div>
    <div class="catalog"><nav class="contents" aria-label="Browse by subject"><h2>Browse by subject</h2>
      <a href="#overview">Design overview</a>
      ${parts.map((name, i) => `<a href="#part-${i + 1}">${name}</a>`).join('')}
    </nav><div class="catalog-body">
      <section class="overview" id="overview"><h2>Design overview</h2>
        <p>When a function appends to a log, who finishes the transaction if the write fails? <a href="chapter-03.html">Ownership modes</a> identify who holds that obligation. <a href="chapter-04.html">Field projections</a> limit which fields the writer can change. An <a href="chapter-05.html">effect row</a> declares the operations the caller must handle or pass on, including failures.</p>
        <p>The manuscript applies these contracts to package permissions, device buffers, and network protocols. <a href="chapter-09.html">Proof extraction</a> translates supported operations into Lean definitions. Each theorem depends on the operations and assumptions of a particular model. <a href="appendix-a.html">Read about the proof models.</a></p>
        <p class="scope-note">The chapter guides include proposed designs. The language guide uses programs checked against the compiler. <a href="${pdf('overview')}">Specification overview (PDF)</a> · <a href="https://github.com/robot-head/vesper/blob/main/docs/manuscript/REVIEW-STATUS.md">Manuscript review notes</a></p>
      </section>
      ${parts.map((name, i) => `<section id="part-${i + 1}"><h2>${name}</h2><ul>${chapters.filter(c => c.part === i + 1).map(c => `<li><a href="${filename(c.id)}"><small>${label(c.id)}</small><strong>${escape(c.title)}</strong></a><p>${inline(c.blocks[0])}</p></li>`).join('')}</ul></section>`).join('')}
    </div></div>
    <script type="application/json" id="search-index">${JSON.stringify(topics).replaceAll('<', '\\u003c')}</script>
    </main>`;
  return page('index.html', 'Explore the manuscript', 'A practical guide to all eight parts of the Vesper language and platform manuscript.', body);
}

export async function buildDocs(root, out) {
  const source = await readFile(resolve(root, '../docs/manuscript/content.typ'), 'utf8');
  const outline = manuscriptOutline(source);
  const covered = new Set();
  const chapters = [];
  for (let part = 1; part <= parts.length; part++) {
    const lessons = await readFile(resolve(root, 'docs', `part-${part}.md`), 'utf8');
    chapters.push(...readLessons(lessons, part, outline, covered));
  }
  for (const id of outline.keys()) {
    if (!covered.has(id)) throw new Error(`Missing documentation for manuscript ${id}`);
  }
  for (const chapter of chapters) {
    for (const entry of [chapter, ...chapter.sections]) {
      if (!entry.blocks.length) throw new Error(`Empty lesson ${entry.id}`);
    }
  }
  const dir = resolve(out, 'docs');
  await mkdir(dir, { recursive: true });
  await writeFile(resolve(dir, 'index.html'), homepage(chapters));
  for (const [i, chapter] of chapters.entries()) {
    await writeFile(resolve(dir, filename(chapter.id)), article(chapter, chapters[i - 1], chapters[i + 1]));
  }
  return chapters.map(chapter => `docs/${filename(chapter.id)}`);
}
