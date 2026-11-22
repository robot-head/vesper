import { evaluate } from '../tour-wasm.js';
import { tourPrograms } from './tour-programs.mjs';
import { mkdir, readdir, readFile, unlink, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { page } from './docs.mjs';
import { escape } from './highlight.mjs';

export const movedLessons = {
  'require-a-decision-at-scope-exit.html': 'index.html',
  'name-a-value.html': '../language.html#functions',
  'choose-an-integer-type.html': '../language.html#numeric-widths',
  'change-a-local.html': '../language.html#functions',
  'pass-an-argument.html': '../language.html#functions',
  'choose-a-branch.html': '../language.html#functions',
  'name-a-condition.html': '../language.html#functions',
  'repeat-an-update.html': '../language.html#loops',
  'include-the-endpoint.html': '../language.html#loops',
  'read-an-exit-status.html': '../language.html#functions',
  'group-two-fields.html': '../language.html#ownership',
  'copy-a-record.html': '../language.html#ownership',
  'read-through-a-loan.html': '../language.html#places',
  'lend-a-field-for-writing.html': 'limit-a-helper-to-one-field.html',
  'name-two-alternatives.html': '../language.html#patterns',
  'carry-data-in-a-variant.html': '../language.html#patterns',
  'cover-every-case.html': '../language.html#patterns',
  'represent-an-absent-value.html': '../language.html#patterns',
  'give-a-ticket-one-owner.html': '../language.html#ownership',
  'detect-a-second-use.html': '../language.html#ownership',
  'consume-a-ticket.html': 'index.html',
  'give-consumption-a-name.html': 'finish-on-both-paths.html',
  'transfer-the-obligation.html': 'finish-on-both-paths.html',
  'inspect-before-consuming.html': '../language.html#ownership',
  'capture-a-value.html': '../language.html#closures',
  'write-one-test.html': '../language.html#tests',
  'turn-inputs-into-rows.html': 'isolate-one-failing-row.html',
  'declare-an-operation.html': 'use-the-resumed-result.html',
  'request-a-value.html': 'use-the-resumed-result.html',
  'resume-the-computation.html': 'use-the-resumed-result.html',
  'declare-checked-addition.html': 'pass-an-effect-to-the-caller.html',
  'handle-an-overflow.html': 'exercise-the-failure-path.html',
  'choose-wrapping-arithmetic.html': 'exercise-the-failure-path.html',
  'recover-from-division-by-zero.html': 'prove-a-divisor-is-nonzero.html',
  'inspect-the-error.html': 'exercise-the-failure-path.html',
  'return-a-value-from-a-loop.html': '../language.html#loops',
  'let-a-bound-prove-an-increment.html': 'prove-a-divisor-is-nonzero.html',
  'round-a-floating-point-calculation.html': '../language.html#floating-point',
  'name-a-module-constant.html': '../language.html#fixed-arrays',
  'index-a-fixed-array.html': '../language.html#fixed-arrays',
  'give-variants-numeric-values.html': '../language.html#fixed-arrays',
  'grow-a-sequence.html': '../language.html#sequences',
  'borrow-disjoint-sub-slices.html': '../language.html#sequences',
  'read-a-utf-8-string.html': '../language.html#strings',
  'lend-a-string-view.html': '../language.html#strings',
  'select-behavior-through-a-trait.html': '../language.html#trait-objects',
  'read-an-atomic-counter.html': '../docs/chapter-15.html#ch-14-4-atomics',
  'join-scoped-workers-before-reading-a-total.html': '../docs/chapter-15.html#ch-14-4-workers',
  'check-a-tensor-shape.html': '../docs/chapter-42.html',
  'bind-a-piece-of-syntax.html': '../language.html#sugar',
  'read-a-macro-call.html': '../language.html#sugar',
  'inspect-the-expansion.html': 'define-a-new-expression-form.html',
  'check-a-quoted-expression.html': '../language.html#sugar',
  'format-a-user-defined-spelling.html': 'define-a-new-expression-form.html',
  'read-a-proposed-sugar-declaration.html': 'define-a-new-expression-form.html',
};

function consoleListing(source) {
  return source.split('\n').map(line => {
    const kind = line.startsWith('$ ') ? 'command' : /\bFAIL\b|\b[1-9]\d* failed\b/.test(line) ? 'failure' : '';
    return kind ? `<span class="terminal-${kind}">${escape(line)}</span>` : escape(line);
  }).join('\n');
}

function renderLesson(body, program) {
  return body.trim().split(/(^~~~\w+(?: [A-Z0-9_]+)?\n[\s\S]*?^~~~$)/m).filter(part => part.trim()).map(part => {
    if (part.startsWith('~~~')) {
      const [, language, error, code] = part.match(/^~~~(vesper|sh)(?: ([A-Z0-9_]+))?\n([\s\S]*)\n~~~$/);
      if (language === 'vesper' && (program.error || code === program.originalSource)) return '';
      return `<pre tabindex="0"><code data-lang="${language}"${error ? ` data-error="${error}"` : ''}>${language === 'sh' && code.startsWith('$ ') ? consoleListing(code) : escape(code)}</code></pre>`;
    }
    return part.trim().split(/\n\n+/).map(p => /^<!-- (?:figure|diagram):[\w-]+ -->$/.test(p) ? p : `<p>${escape(p).replace(/`([^`]+)`/g, '<code>$1</code>')}</p>`).join('\n');
  }).join('\n');
}

const labels = { check: 'Check', test: 'Run tests', 'test-case': 'Run one case', expand: 'Expand', lean: 'Extract Lean', format: 'Format' };

function editor(program) {
  return `<section class="tour-editor" data-tour-editor hidden aria-labelledby="editor-title">
    <h2 id="editor-title">Try it</h2>
    <form data-mode="${program.mode}"${program.error ? ` data-repair="${program.error}"` : ''}>
      <label for="tour-source">Vesper source</label>
      <div class="editor-source">
        <pre class="editor-highlight vc" aria-hidden="true" hidden></pre>
        <textarea wrap="off" id="tour-source" name="source" rows="12" maxlength="16384" spellcheck="false" autocapitalize="off" autocomplete="off" aria-describedby="tour-editor-keys">${escape(program.source)}</textarea>
        <div class="editor-tooltip" role="tooltip" hidden>
          <strong></strong>
          <div></div>
        </div>
      </div>
      <div class="actions">
        <button class="button primary" type="submit">${labels[program.mode] ?? 'Run'}</button>
        ${program.mode !== 'format' ? '<button class="button" type="button" data-format>Format</button>' : ''}
        ${program.expand ? '<button class="button" type="button" data-expand>Expand</button>' : ''}
        <button class="button" type="reset">Reset</button>
      </div>
      ${program.testCase ? `<label>Test or row name <input name="test_case" value="${escape(program.testCase)}"></label>` : ''}
      <p>Vesper checks your code after you pause typing.</p>
      <p id="tour-editor-keys">Tab inserts four spaces. Press Esc then Tab to move focus out. Ctrl+Enter or Command+Enter runs the selected action.</p>
      <div class="editor-check-status" role="status"></div>
      <details class="editor-diagnostics" hidden>
        <summary></summary>
        <ul></ul>
      </details>
      <div class="editor-status" role="status"></div>
      <pre class="editor-output" tabindex="0" aria-label="Vesper output" hidden></pre>
    </form>
    <script type="module" src="../tour-editor.js"></script>
  </section>`;
}

function fallbackListings(body, program) {
  const { error: repair, source } = program;
  const fallback = listing => `<section data-tour-fallback><h2>Starting code</h2>${listing}</section>`;
  if (!repair) return fallback(`<pre tabindex="0"><code data-lang="vesper">${escape(source.trimEnd())}</code></pre>`);
  const fences = [...body.matchAll(/^~~~vesper(?: ([A-Z0-9_]+))?\n([\s\S]*?)^~~~$/gm)];
  const listing = ([, error, code]) =>
    `<pre tabindex="0"><code data-lang="vesper"${error ? ` data-error="${error}"` : ''}>${escape(code.trimEnd())}</code></pre>`;
  const starter = fences.find(([, error]) => error === repair);
  const solution = fences.find(([, error]) => !error);
  return `${fallback(listing(starter))}${
    solution ? `<details class="tour-solution"><summary>Show solution</summary>${listing(solution)}</details>` : ''
  }`;
}

export async function buildTour(root, out, wasm) {
  const source = (await readFile(resolve(root, 'tour.md'), 'utf8')).replaceAll('\r\n', '\n');
  const lessons = [];
  for (const group of source.split(/^# /m).slice(1)) {
    const [subject, ...entries] = group.split(/^## /m);
    for (const entry of entries) {
      const [title, ...lines] = entry.split('\n');
      const file = lessons.length ? title.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/-$/, '') + '.html' : 'index.html';
      lessons.push({ subject: subject.trim(), title, file, body: lines.join('\n').trim() });
    }
  }
  if (new Set(lessons.map(step => step.file)).size !== lessons.length) throw new Error('Duplicate tour URL');
  if (lessons.some(step => Object.hasOwn(movedLessons, step.file))) throw new Error('Active lesson marked as moved');
  const dir = resolve(out, 'tour');
  await mkdir(dir, { recursive: true });
  const filenames = new Set([...lessons.map(s => s.file), ...Object.keys(movedLessons)]);
  for (const name of await readdir(dir)) {
    if (name.endsWith('.html') && !filenames.has(name)) await unlink(resolve(dir, name));
  }
  const programs = tourPrograms(lessons).map((program, i) => {
    const formatted = evaluate(wasm, { source: program.source, mode: 'format' });
    if (!formatted.ok) throw new Error(lessons[i].file + ' · starter formatting failed: ' + formatted.output);
    return { ...program, originalSource: program.source, source: formatted.output };
  });
  for (const [i, step] of lessons.entries()) {
    const subjects = [...new Set(lessons.map(s => s.subject))];
    const menu = subjects.map(subject => `<li>${escape(subject)}<ol start="${1 + lessons.findIndex(s => s.subject === subject)}">${lessons.filter(s => s.subject === subject).map(s =>
      `<li><a href="${s.file}"${s === step ? ' aria-current="step"' : ''}>${escape(s.title)}</a></li>`).join('')}</ol></li>`).join('');
    const neighbor = (lesson, rel, label) => `<a rel="${rel}" href="${lesson.file}"><small>${label}</small>${escape(lesson.title)}</a>`;
    const body = `<main id="main" class="wrap tour reader-body">
      <details class="tour-contents"><summary>Tour contents</summary><nav aria-label="Tour contents"><ul>${menu}</ul></nav></details>
      <p class="chapter-label">Language tour · ${i + 1} of ${lessons.length} · ${escape(step.subject)}</p>
      <h1>${escape(step.title)}</h1>
      <div class="lesson">${renderLesson(step.body, programs[i])}</div>
      ${editor(programs[i])}
      ${fallbackListings(step.body, programs[i])}
      <nav class="neighbors" aria-label="Tour navigation">
        ${i ? neighbor(lessons[i - 1], 'prev', 'Previous') : '<a href="../language.html"><small>Reference</small>Language guide</a>'}
        ${lessons[i + 1] ? neighbor(lessons[i + 1], 'next', 'Next') : '<a href="../language.html#sugar"><small>Finish tour</small>Sugar in the language guide</a>'}
      </nav></main>`;
    const description = step.body.split('\n')[0].replaceAll('`', '');
    await writeFile(resolve(dir, step.file), page(step.file, step.title, description, body, 'tour'));
  }
  for (const [file, target] of Object.entries(movedLessons)) {
    await writeFile(resolve(dir, file), page(file, 'Lesson moved', 'Continue to the revised lesson.',
      `<main class="wrap tour" id="main"><h1>Lesson moved</h1>
      <p><a class="inline-link" href="${target}">Open the revised lesson</a></p></main>`, 'tour', target.split('#')[0]));
  }
  return [...filenames].map(file => 'tour/' + file);
}
