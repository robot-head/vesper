const editor = document.querySelector('[data-tour-editor]');
const form = editor.querySelector('form');
const source = form.elements.source;
const mirror = form.querySelector('.editor-highlight');
const run = form.querySelector('button[type="submit"]');
const format = form.querySelector('[data-format]');
const output = form.querySelector('.editor-output');
const status = form.querySelector('[role="status"]');

if ('Worker' in globalThis && 'WebAssembly' in globalThis) {
  editor.hidden = false;
  let worker, timer, highlightTimer, pending;
  let revision = 0;
  const template = document.createElement('template');

  function syncScroll() {
    mirror.scrollTop = source.scrollTop;
    mirror.scrollLeft = source.scrollLeft;
  }

  function clearHighlight() {
    mirror.hidden = true;
    source.classList.remove('has-highlight');
  }

  function stop() {
    worker?.terminate();
    worker = undefined;
    clearHighlight();
  }

  function finish(result) {
    clearTimeout(timer);
    const action = pending;
    pending = undefined;
    run.disabled = false;
    if (format) format.disabled = false;
    if (result.restart) stop();

    if (action.mode === 'format' && result.ok) {
      output.hidden = true;
      if (action.revision !== revision) {
        status.textContent = 'Source changed during formatting. Select Format to try again.';
        return;
      }
      const { selectionStart, selectionEnd, selectionDirection, scrollTop, scrollLeft } = source;
      source.value = result.output;
      source.setSelectionRange(selectionStart, selectionEnd, selectionDirection);
      source.scrollTop = scrollTop;
      source.scrollLeft = scrollLeft;
      status.textContent = 'Source formatted.';
      refreshHighlight();
      return;
    }

    status.textContent = result.ok ? 'Result' : 'Diagnostic';
    output.textContent = result.output || 'No output.';
    output.hidden = false;
  }

  function receiveHighlight(result) {
    if (result.id !== revision || !result.ok) return;
    // Chroma escapes source text. Only use an exact rendering of the
    // textarea so the display layer always lines up with the actual input.
    template.innerHTML = result.output;
    const code = template.content.querySelector('pre > code');
    if (!code || code.textContent !== source.value) return;
    mirror.replaceChildren(...code.childNodes);
    if (source.value.endsWith('\n')) mirror.append('\u200b');
    mirror.hidden = false;
    source.classList.add('has-highlight');
    syncScroll();
  }

  function startWorker() {
    const next = new Worker(new URL('./tour-worker.js', import.meta.url), { type: 'module' });
    next.onmessage = ({ data }) => {
      if (worker !== next) return;
      if (data.mode === 'highlight') receiveHighlight(data);
      else if (pending) finish(data);
    };
    next.onerror = () => {
      if (worker !== next) return;
      if (pending) finish({ ok: false, restart: true, output: 'Could not start the Vesper compiler.' });
      else stop();
    };
    return next;
  }

  function refreshHighlight() {
    revision++;
    clearHighlight();
    clearTimeout(highlightTimer);
    highlightTimer = setTimeout(() => {
      try {
        worker ??= startWorker();
        worker.postMessage({ source: source.value, mode: 'highlight', id: revision });
      } catch {
        clearHighlight();
      }
    }, 50);
  }

  function start(mode) {
    if (pending) return;
    pending = { mode, revision };
    status.textContent = mode === 'format' ? 'Formatting…' : 'Running…';
    output.hidden = true;
    run.disabled = true;
    if (format) format.disabled = true;
    try {
      worker ??= startWorker();
      timer = setTimeout(() => finish({
        ok: false, restart: true, output: 'The example exceeded the time limit.',
      }), 10000);
      worker.postMessage({ source: source.value, mode });
    } catch {
      finish({ ok: false, restart: true, output: 'Could not start the Vesper compiler.' });
    }
  }

  form.addEventListener('submit', event => {
    event.preventDefault();
    start(form.dataset.mode);
  });
  format?.addEventListener('click', () => start('format'));

  form.addEventListener('reset', () => {
    clearTimeout(timer);
    if (pending) stop();
    pending = undefined;
    run.disabled = false;
    if (format) format.disabled = false;
    output.hidden = true;
    status.textContent = 'Example restored.';
    refreshHighlight();
  });
  source.addEventListener('input', refreshHighlight);
  source.addEventListener('scroll', syncScroll);
  source.addEventListener('keydown', event => {
    if (event.key === 'Enter' && (event.ctrlKey || event.metaKey) && !event.isComposing) {
      event.preventDefault();
      form.requestSubmit();
    }
  });

  if ('IntersectionObserver' in globalThis) {
    const observer = new IntersectionObserver(entries => {
      if (entries.some(entry => entry.isIntersecting)) {
        observer.disconnect();
        refreshHighlight();
      }
    });
    observer.observe(source);
  } else {
    refreshHighlight();
  }
}
