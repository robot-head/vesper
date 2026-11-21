import { mkdir, readdir, readFile, unlink, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { page } from './docs.mjs';
import { escape } from './highlight.mjs';

function renderLesson(body) {
  return body.trim().split(/(^~~~\w+(?: [A-Z0-9_]+)?\n[\s\S]*?^~~~$)/m).filter(part => part.trim()).map(part => {
    if (part.startsWith('~~~')) {
      const [, language, error, code] = part.match(/^~~~(vesper|sh)(?: ([A-Z0-9_]+))?\n([\s\S]*)\n~~~$/);
      return `<pre tabindex="0"><code data-lang="${language}"${error ? ` data-error="${error}"` : ''}>${escape(code)}</code></pre>`;
    }
    return part.trim().split(/\n\n+/).map(p => `<p>${escape(p).replace(/`([^`]+)`/g, '<code>$1</code>')}</p>`).join('\n');
  }).join('\n');
}

export async function buildTour(root, out) {
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
  const filenames = new Set(lessons.map(s => s.file));
  for (const name of await readdir(dir)) {
    if (name.endsWith('.html') && !filenames.has(name)) await unlink(resolve(dir, name));
  }
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
      <nav class="neighbors" aria-label="Tour navigation">
        ${i ? neighbor(lessons[i - 1], 'prev', 'Previous') : '<a href="../index.html#guide"><small>Setup</small>Build the compiler</a>'}
        ${lessons[i + 1] ? neighbor(lessons[i + 1], 'next', 'Next') : '<a href="../language.html#sugar"><small>Finish tour</small>Sugar in the language guide</a>'}
      </nav></main>`;
    const description = step.body.split('\n')[0].replaceAll('`', '');
    await writeFile(resolve(dir, step.file), page(step.file, step.title, description, body, 'tour'));
  }
  return lessons.map(s => 'tour/' + s.file);
}
