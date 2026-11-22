import { inline, markdownBlocks, renderBlocks } from './docs-markup.mjs';
import { escape } from './highlight.mjs';
import { siteChrome } from './chrome.mjs';
import { insertFigures } from './figures.mjs';
import { mkdir, readdir, readFile, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';

const parts = [
  'Language foundations', 'Compiler, proofs, and packages', 'Memory and runtime',
  'Developer tools', 'Operating systems and security', 'Data and distributed systems',
  'Interfaces and compute', 'Proof models and visual identity',
];
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
  for (const block of markdownBlocks(source)) {
    const heading = block.match(/^(#{1,2}) ([\dAB]+(?:\.\d+)?) (.+)$/);
    if (!heading) {
      if (!entry) throw new Error(`Unexpected lesson text: ${block}`);
      entry.blocks.push(block);
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

export function page(file, title, description, body, directory = 'docs', canonical = file) {
  const script = directory === 'docs' ? '<script type="module" src="search.js"></script>' : '';
  const canonicalUrl = new URL(canonical === 'index.html' ? '' : canonical, `https://vesper-lang.org/${directory}/`);
  return siteChrome(`<!doctype html>
<html lang="en" data-vesper-theme="dusk">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>${escape(title)} · Vesper documentation</title>
  <meta name="description" content="${escape(description)}">
  <meta name="theme-color" content="#101111">
  <link rel="canonical" href="${canonicalUrl}">
  <link rel="icon" href="../favicon.svg" type="image/svg+xml">
  <link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Space+Grotesk:wght@400;500;600;700&amp;family=Inter:wght@400;500;600&amp;family=JetBrains+Mono:wght@400;500&amp;display=swap">
  <link rel="stylesheet" href="../site.css">
  <link rel="stylesheet" href="../docs/docs.css">
  <noscript><style>.theme { display: none; }</style></noscript>
  ${script}
</head>
<body>
  <a class="skip" href="#main">Skip to content</a>
  <!-- site-header -->
  ${insertFigures(directory === 'docs' ? body.replace('</h1>', '</h1>' + search) : body, '../')}
  <!-- site-footer -->
  <script type="module" src="../site.js"></script>
</body></html>\n`, '../', directory + '/' + file);
}

const practice = {
  1: ['index.html?example=transaction.ves', 'Run the transaction example'],
  2: ['prove-a-divisor-is-nonzero.html', 'Check arithmetic with a range fact'],
  3: ['index.html', 'Try consuming a linear value'],
  4: ['find-a-conflicting-loan.html', 'Repair a conflicting loan'],
  5: ['use-the-resumed-result.html', 'Inject a clock into a test'],
  7: ['define-a-new-expression-form.html', 'Run and expand a sugar declaration'],
  9: ['extract-a-lean-model.html', 'Extract a Lean model'],
  13: ['allocate-in-a-lexical-region.html', 'Allocate in a region'],
  14: ['check-a-masked-vector-load.html', 'Check a vector load'],
  18: ['find-a-conflicting-loan.html', 'Inspect a field projection diagnostic'],
  19: ['define-a-new-expression-form.html', 'Format a custom syntax form'],
  20: ['isolate-one-failing-row.html', 'Run a table-driven test'],
};

const figures = {
  '6.1': '<!-- diagram:continuation-fission -->',
  '8.1': '<!-- diagram:compiler-pipeline -->',
  '9.2': '<!-- diagram:lean-extraction -->',
  '13.4': '<!-- diagram:aosoa-tile-layout -->',
  '15.4': '<!-- diagram:pulse-nursery-dma -->',
  '16.2': '<!-- diagram:wire-struct-layout -->',
  '18.2': '<!-- diagram:lens-early-cutoff -->',
  '23.2': '<!-- diagram:chrono-flight-recorder -->',
  '25.3': '<!-- diagram:silicon-lifo-mmio -->',
  '29.3': '<!-- diagram:aegis-bounce-grant -->',
  '33.2': '<!-- diagram:warp-umem-rings -->',
  '37.2': '<!-- diagram:flux-dbsp-circuit -->',
  '39.2': '<!-- diagram:ignis-render-graph -->',
  '42.4': '<!-- diagram:tensor-kv-paged -->',
  'A.1': '<!-- diagram:metatheory-map -->',
  'B.2': '<!-- diagram:prism-cad-grid -->',
  '1.4': '<!-- diagram:linear-transaction -->',
  '5.1': '<!-- diagram:effect-resumption -->',
  '7.4': '<!-- diagram:macro-substitution -->',
  '4.3': '<!-- figure:broker -->',
  'B.1': `<figure class="concept-figure prism"><img data-brand-mark src="../brand/vesper-mark-dusk.svg" width="256" height="256" loading="lazy" alt="A V-shaped prism with an upper chevron and open channels between three blades."><figcaption>The blades mark consumption, exclusive access, and shared access or an effect. The gaps keep the three visible in monochrome.</figcaption></figure>`,
};

function renderEntry(blocks, figure = '') {
  const detail = blocks.indexOf('<!-- technical-detail -->');
  if (detail < 0) return renderBlocks(blocks) + figure;
  return renderBlocks(blocks.slice(0, detail)) + figure + `<details class="technical-detail">
    <summary>Technical details</summary>
    <p class="scope-note">These notes describe the manuscript design. Code listings are illustrative unless linked to a checked example.</p>
    ${renderBlocks(blocks.slice(detail + 1))}
  </details>`;
}

const search = `<div class="docs-search" hidden>
  <label for="topic-query">Search the docs, guide, and tour</label>
  <input type="search" id="topic-query" placeholder="Try “field loans”, “§4.3”, or “E0301”" autocomplete="off" aria-controls="search-results">
  <div role="status" id="search-status"></div>
  <ul id="search-results" hidden></ul>
</div>`;

const review = 'https://github.com/robot-head/vesper/blob/main/docs/manuscript/REVIEW-STATUS.md';
const chapterStatus = {
  2: ['Compiler and proposed grammar', 'The source parser supports the guide’s checked examples. The short-lookahead grammar in the manuscript remains a design sketch.'],
  3: ['Compiler and mechanized fragments', 'The checker implements source modes and field floors. Ghost source elaboration remains open.'],
  4: ['Compiler and mechanized fragments', 'Field and slice loans are implemented. General dependent heap typing and unrestricted refinement solving remain open.'],
  5: ['Compiler and mechanized fragments', 'The checker supports lexical handlers. The graded, guarded-clock, and quotient models have separate premises.'],
  6: ['Design proposal', 'Continuation fission is unimplemented. The illustrative/ContinuationFission.lean path in the manuscript is an exposition sketch.'],
  9: ['Compiler and proof tools', 'Extraction and proof audit are implemented for supported inputs. The proofs cover their modeled fragments.'],
  10: ['Compiler and design proposal', 'The restricted verified backend is implemented. Verified self-hosting and the whole executable correspondence argument remain open.'],
  12: ['Source compatibility', 'The variance checks cover source contracts. Binary layout and proof transport need additional checks.'],
  13: ['Compiler and host runtime', 'The region runtime checks stale allocation handles dynamically. The model does not establish static liveness across a rewind.'],
  15: ['Compiler and host runtime', 'Scoped workers and atomics have source checks. Device DMA ownership and cancellation have a separate hardware boundary.'],
  16: ['Compiler and host runtime', 'Check the selected ABI and host implementation. Hardware protection keys alone do not isolate adversarial native code.'],
  17: ['Host prototype and mechanized fragment', 'Handle models and tenant checks cover specific operations. They are not an end-to-end plugin-isolation proof.'],
  22: ['Host tooling and mechanized fragments', 'The concolic path covers scalar operations. The shrink model does not establish alias safety for lifted programs.'],
  23: ['Host tooling', 'Replay consumes a recorded transcript. Full treatment of external nondeterminism remains open.'],
  27: ['Host runtime and design notes', 'Close active network sessions before cloning a VM. Memory snapshots do not independently transfer peer protocol state.'],
  28: ['Host tooling and design notes', 'The compiler checks selected constant-time properties. Hardware setup, load taint, and spills have separate limits.'],
  29: ['Host runtime and design notes', 'The binding policy checks full digests, with separate hardware and deployment assumptions.'],
  35: ['Host prototype and mechanized fragments', 'The quorum model does not establish liveness during a network partition or completion of physical RDMA revocation.'],
  36: ['Host prototype and mechanized fragments', 'Storage proofs cover modeled operations. Page latching and suspension boundaries depend on runtime enforcement.'],
  37: ['Host prototype and mechanized fragments', 'The stream model and in-memory operators do not extend their bounds to arbitrary spill effects.'],
  38: ['Host prototype and design notes', 'UI backend support and foreign view lifetimes vary across targets.'],
  39: ['Host prototype and mechanized fragments', 'Backend equivalence results cover stated fragments. Work-graph support and resource barriers differ by target.'],
  40: ['Host prototype and mechanized fragments', 'Deterministic numerical contracts cover selected operations. Cross-ISA simulation needs separate validation.'],
  41: ['Host prototype and mechanized fragments', 'Rational DSP stability lemmas do not establish floating-point stability for every input.'],
  A: ['Mechanized fragments', 'Each Lean model states its own assumptions and theorem scope. A proof for the combined language remains open.'],
};

function article(chapter, previous, next, models) {
  const { id, title, part, sections } = chapter;
  const status = chapterStatus[id] ?? ['Implementation and design notes', 'See the manuscript and review notes for the scope of each implementation, illustration, and proof.'];
  const neighbor = (other, direction) => other ? `<a href="${filename(other.id)}"><small>${direction} · ${label(other.id)}</small>${escape(other.title)}</a>` : '';
  const body = `<main class="wrap reader docs-reader" id="main">
    <div class="reader-intro">
      <a class="part-link" href="index.html#part-${part}">${parts[part - 1]}</a>
      <p class="chapter-label">${label(id)} · Manuscript guide</p>
      <h1>${escape(title)}</h1>
      <p class="scope-note"><strong>${status[0]}</strong> · ${status[1]} <a href="${review}">Review status</a></p>
      ${renderEntry(chapter.blocks)}
    </div>
    <nav class="contents" aria-label="Chapter contents"><h2>In this chapter</h2>
      ${sections.map(s => `<a href="#${sectionId(s.id)}">${escape(s.title)}</a>`).join('\n')}
    </nav>
    <div class="reader-body">
      ${sections.map(s => `<section id="${sectionId(s.id)}"><h2>${escape(s.title)}</h2>${renderEntry(s.blocks, figures[s.id])}
        <p class="source-ref"><a href="reference.html#${s.original.anchor}">Manuscript §${s.id} (HTML)</a> · <a href="${pdf(s.original.anchor)}" aria-label="Manuscript section ${s.id}, ${escape(s.title)} (PDF)">PDF</a></p></section>`).join('\n')}
      <div class="chapter-sources">
        <a href="${pdf(chapter.original.anchor)}">Read ${label(id).toLowerCase()} in the specification (PDF)</a>
        ${practice[id] ? `<a href="../tour/${practice[id][0]}">${practice[id][1]}</a>` : ''}
      </div>
      ${id === 'A' ? models : ''}
      <nav class="neighbors" aria-label="Adjacent chapters">${neighbor(previous, 'Previous')}${neighbor(next, 'Next')}</nav>
    </div></main>`;
  return page(filename(id), title, chapter.blocks[0], body);
}

async function modelIndex(root) {
  const dir = resolve(root, '../lean');
  const files = (await readdir(dir, { recursive: true, withFileTypes: true }))
    .filter(entry => entry.isFile() && entry.name.endsWith('.lean'))
    .map(entry => resolve(entry.parentPath, entry.name)).sort();
  const list = await Promise.all(files.map(async file => {
    const path = file.slice(dir.length + 1).replaceAll('\\', '/');
    const source = await readFile(file, 'utf8');
    const names = [...source.matchAll(/^(?:(?:private|protected) )?(?:theorem|lemma) ([^\s(:{\[]+)/gm)].map(match => match[1]);
    return `<li><a href="https://github.com/robot-head/vesper/blob/main/lean/${encodeURI(path)}">${escape(path)}</a>${
      names.length ? `<div><code>${escape(names.join(', '))}</code></div>` : ''}</li>`;
  }));
  return `<details class="model-index"><summary>Browse Lean modules and declarations</summary>
    <p>This inventory lists repository files and their named declarations. Read each statement and its assumptions to determine which property it proves.</p><ul>${list.join('')}</ul></details>`;
}

function chapterTopics(chapters, referenceSections) {
  return chapters.flatMap(ch => ch.sections.map(s => ({
    chapter: ch.title, part: parts[ch.part - 1], title: s.title, id: s.id,
    url: `${filename(ch.id)}#${sectionId(s.id)}`,
    text: [ch.original.title, s.original.title, ...s.blocks, referenceSections.get(s.original.anchor)]
      .join(' ').replaceAll('`', ''),
  })));
}

function homepage(chapters, architecture) {
  const body = `<main class="wrap docs-index" id="main">
    <header class="docs-intro"><h1>Vesper documentation</h1>
      <p class="lead">Start with the tour and language guide to write Vesper. For the type theory and proof scope, read <a href="chapter-01.html">Chapters 1–5</a>, then <a href="chapter-09.html">Chapters 9–10</a> and <a href="appendix-a.html">Appendix A</a>. <a href="chapter-06.html">Chapter 6</a> proposes continuation fission, which the compiler does not implement.</p>
      <div class="actions"><a class="button primary" href="../language.html">Start the language guide</a><a class="button" href="../tour/index.html">Take the guided tour</a><a class="button" href="reference.html">Read the specification online</a><a class="button" href="../vesper-specification.pdf">Download PDF</a></div>
    </header>
    <div class="catalog"><nav class="contents" aria-label="Browse by subject"><h2>Browse by subject</h2>
      <a href="#overview">Design overview</a>
      ${parts.map((name, i) => `<a href="#part-${i + 1}">${name}</a>`).join('')}
    </nav><div class="catalog-body">
      <section class="overview" id="overview"><h2>Design overview</h2>
        <p>When a function appends to a log, who finishes the transaction if the write fails? <a href="chapter-03.html">Ownership modes</a> identify who holds that obligation. <a href="chapter-04.html">Field projections</a> limit which fields the writer can change. An <a href="chapter-05.html">effect row</a> declares the operations the caller must handle or pass on, including failures.</p>
        <p>The manuscript applies these contracts to package permissions, device buffers, and network protocols. <a href="chapter-09.html">Proof extraction</a> translates supported operations into Lean definitions. Each theorem depends on the operations and assumptions of a particular model. <a href="appendix-a.html">Read about the proof models.</a></p>
        <p class="scope-note">The chapter guides include proposed designs. Paths under <code>illustrative/</code> identify exposition sketches. <a href="reference.html#overview">Eight-part architecture map</a> · <a href="${pdf('overview')}">Specification overview (PDF)</a> · <a href="${review}">Manuscript review notes</a></p>
        <details><summary>Browse the eight-part architecture map</summary>
          <div class="reference-table">${architecture}</div>
        </details>
      </section>
      ${parts.map((name, i) => `<section id="part-${i + 1}"><h2>${name}</h2><ul>${chapters.filter(c => c.part === i + 1).map(c => `<li><a href="${filename(c.id)}"><small>${label(c.id)}</small><strong>${escape(c.title)}</strong></a><p>${inline(c.blocks[0])}</p></li>`).join('')}</ul></section>`).join('')}
    </div></div>
    </main>`;
  return page('index.html', 'Explore the manuscript', 'A practical guide to all eight parts of the Vesper language and platform manuscript.', body);
}

export async function buildDocs(root, out, architecture, referenceSections) {
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
  const models = await modelIndex(root);
  await mkdir(dir, { recursive: true });
  await writeFile(resolve(dir, 'index.html'), homepage(chapters, architecture));
  for (const [i, chapter] of chapters.entries()) {
    await writeFile(resolve(dir, filename(chapter.id)), article(chapter, chapters[i - 1], chapters[i + 1], models));
  }
  return { pages: chapters.map(chapter => `docs/${filename(chapter.id)}`), topics: chapterTopics(chapters, referenceSections) };
}
