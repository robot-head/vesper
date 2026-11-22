import { evaluate } from '../tour-wasm.js';
import { tourPrograms } from './tour-programs.mjs';
import { mkdir, readdir, readFile, unlink, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { page } from './docs.mjs';
import { escape } from './highlight.mjs';

export const movedLessons = {
  'detect-a-second-use.html': 'give-a-ticket-one-owner.html#use-after-move',
  'declare-an-operation.html': 'resume-the-computation.html',
  'request-a-value.html': 'resume-the-computation.html',
  'choose-wrapping-arithmetic.html': 'exercise-the-failure-path.html',
  'inspect-the-error.html': 'recover-from-division-by-zero.html',
  'read-a-macro-call.html': 'bind-a-piece-of-syntax.html',
  'inspect-the-expansion.html': 'bind-a-piece-of-syntax.html',
  'check-a-quoted-expression.html': 'bind-a-piece-of-syntax.html',
};

function consoleListing(source) {
  return source.split('\n').map(line => {
    const kind = line.startsWith('$ ') ? 'command' : /\bFAIL\b|\b[1-9]\d* failed\b/.test(line) ? 'failure' : '';
    return kind ? `<span class="terminal-${kind}">${escape(line)}</span>` : escape(line);
  }).join('\n');
}

function renderLesson(body) {
  return body.trim().split(/(^~~~\w+(?: [A-Z0-9_]+)?\n[\s\S]*?^~~~$)/m).filter(part => part.trim()).map(part => {
    if (part.startsWith('~~~')) {
      const [, language, error, code] = part.match(/^~~~(vesper|sh)(?: ([A-Z0-9_]+))?\n([\s\S]*)\n~~~$/);
      return `<pre tabindex="0"><code data-lang="${language}"${error ? ` data-error="${error}"` : ''}>${language === 'sh' && code.startsWith('$ ') ? consoleListing(code) : escape(code)}</code></pre>`;
    }
    return part.trim().split(/\n\n+/).map(p => /^<!-- (?:figure|diagram|anchor):[\w-]+ -->$/.test(p) ? p : `<p>${escape(p).replace(/`([^`]+)`/g, '<code>$1</code>')}</p>`).join('\n');
  }).join('\n').replace('<!-- anchor:use-after-move -->', '<h2 id="use-after-move">Transfer the same ticket twice</h2>');
}

const labels = { check: 'Check', test: 'Run tests', 'test-case': 'Run one case', expand: 'Expand', format: 'Format' };

function editor(program) {
  if (!program) return '';
  return `<section class="tour-editor" data-tour-editor hidden aria-labelledby="editor-title">
    <h2 id="editor-title">Try it</h2>
    <form data-mode="${program.mode}">
      <label for="tour-source">Vesper source</label>
      <div class="editor-source">
        <pre class="editor-highlight vc" aria-hidden="true" hidden></pre>
        <textarea wrap="off" id="tour-source" name="source" rows="12" maxlength="16384" spellcheck="false" autocapitalize="off" autocomplete="off">${escape(program.source)}</textarea>
      </div>
      <div class="actions">
        <button class="button primary" type="submit">${labels[program.mode] ?? 'Run'}</button>
        ${program.mode !== 'format' ? '<button class="button" type="button" data-format>Format</button>' : ''}
        <button class="button" type="reset">Reset</button>
      </div>
      <p>Ctrl+Enter or Command+Enter runs the selected action. Vesper runs your code in this browser.</p>
      <div role="status"></div>
      <pre class="editor-output" tabindex="0" aria-label="Vesper output" hidden></pre>
    </form>
    <script type="module" src="../tour-editor.js"></script>
  </section>`;
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
  const dir = resolve(out, 'tour');
  await mkdir(dir, { recursive: true });
  const filenames = new Set([...lessons.map(s => s.file), ...Object.keys(movedLessons)]);
  for (const name of await readdir(dir)) {
    if (name.endsWith('.html') && !filenames.has(name)) await unlink(resolve(dir, name));
  }
  const programs = tourPrograms(lessons).map((program, i) => {
    if (!program) return null;
    const formatted = evaluate(wasm, { source: program.source, mode: 'format' });
    if (!formatted.ok) throw new Error(lessons[i].file + ' · starter formatting failed: ' + formatted.output);
    return { ...program, source: formatted.output };
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
      <div class="lesson">${renderLesson(step.body)}</div>
      ${editor(programs[i])}
      <nav class="neighbors" aria-label="Tour navigation">
        ${i ? neighbor(lessons[i - 1], 'prev', 'Previous') : '<a href="../index.html#guide"><small>Setup</small>Build the compiler</a>'}
        ${lessons[i + 1] ? neighbor(lessons[i + 1], 'next', 'Next') : '<a href="../language.html#sugar"><small>Finish tour</small>Sugar in the language guide</a>'}
      </nav></main>`;
    const description = step.body.split('\n')[0].replaceAll('`', '');
    await writeFile(resolve(dir, step.file), page(step.file, step.title, description, body, 'tour'));
  }
  for (const [file, target] of Object.entries(movedLessons)) {
    await writeFile(resolve(dir, file), page(file, 'Lesson moved', 'Continue to the revised lesson.',
      `<main class="wrap tour" id="main"><h1>Lesson moved</h1>
      <p><a class="inline-link" href="${target}">Open the revised lesson</a></p></main>`, 'tour'));
  }
  return [...filenames].map(file => 'tour/' + file);
}
