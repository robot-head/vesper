const editor = document.querySelector('[data-tour-editor]');
const form = editor.querySelector('form');
const source = form.elements.source;
const mirror = form.querySelector('.editor-highlight');
const run = form.querySelector('button[type="submit"]');
const format = form.querySelector('[data-format]');
const output = form.querySelector('.editor-output');
const status = form.querySelector('.editor-status');
const checkStatus = form.querySelector('.editor-check-status');
const diagnostics = form.querySelector('.editor-diagnostics');
const diagnosticSummary = diagnostics.querySelector('summary');
const diagnosticItems = diagnostics.querySelector('ul');
const tooltip = form.querySelector('.editor-tooltip');
const tooltipCode = tooltip.querySelector('strong');
const tooltipMessage = tooltip.querySelector('div');

if ('Worker' in globalThis && 'WebAssembly' in globalThis) {
  editor.hidden = false;
  let worker, timer, highlightTimer, checkTimer, checkTimeout, checking, pending;
  let diagnosticList = [];
  let diagnosticRanges = [];
  let hoveredRange;
  let leaveOnTab = false;
  let revision = 0;
  const template = document.createElement('template');
  const painted = document.createElement('span');
  mirror.append(painted);
  let paintedSource = '';

  function boundary(offset) {
    const walker = document.createTreeWalker(painted, NodeFilter.SHOW_TEXT);
    for (let node = walker.nextNode(); node; node = walker.nextNode()) {
      if (offset <= node.length) return [node, offset];
      offset -= node.length;
    }
    return [painted, painted.childNodes.length];
  }

  // Keep the unchanged tokens painted while the worker reclassifies the edit.
  function updatePaint(sourceText) {
    if (mirror.hidden || sourceText === paintedSource) return;
    let start = 0;
    while (start < paintedSource.length && start < sourceText.length
      && paintedSource[start] === sourceText[start]) start++;
    let oldEnd = paintedSource.length;
    let newEnd = sourceText.length;
    while (oldEnd > start && newEnd > start && paintedSource[oldEnd - 1] === sourceText[newEnd - 1]) {
      oldEnd--;
      newEnd--;
    }
    const range = document.createRange();
    range.setStart(...boundary(start));
    range.setEnd(...boundary(oldEnd));
    range.deleteContents();
    if (start < newEnd) range.insertNode(document.createTextNode(sourceText.slice(start, newEnd)));
    // Merge text nodes from successive keystrokes before the worker replies.
    painted.normalize();
    paintedSource = sourceText;
    mirror.classList.toggle('ends-with-newline', sourceText.endsWith('\n'));
    syncScroll();
  }

  function diagnosticRange({ start, end }) {
    if (start === end) {
      const next = paintedSource.slice(start).match(/^[^\n]/u);
      const previous = paintedSource.slice(0, start).match(/[^\n](?![\s\S])/u);
      if (next) end += next[0].length;
      else if (previous) start -= previous[0].length;
      else return null;
    }
    const range = document.createRange();
    range.setStart(...boundary(start));
    range.setEnd(...boundary(end));
    return range;
  }

  function drawLens() {
    hideTooltip();
    diagnosticRanges = [];
    globalThis.CSS?.highlights?.delete('vesper-error');
    if (mirror.hidden || !diagnosticList.length || !globalThis.CSS?.highlights
      || !('Highlight' in globalThis)) return;
    for (const diagnostic of diagnosticList) {
      const range = diagnosticRange(diagnostic);
      if (range) diagnosticRanges.push({ diagnostic, range });
    }
    CSS.highlights.set('vesper-error', new Highlight(...diagnosticRanges.map(entry => entry.range)));
  }

  function hideTooltip() {
    tooltip.hidden = true;
    hoveredRange = undefined;
  }

  function showTooltip({ clientX: x, clientY: y }) {
    for (const { diagnostic, range } of diagnosticRanges) {
      const rect = Array.from(range.getClientRects()).find(rect => rect.width > 0
        && x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom + 3);
      if (!rect) continue;
      if (hoveredRange === range) return;
      hoveredRange = range;
      tooltipCode.textContent = diagnostic.code;
      tooltipMessage.textContent = diagnostic.message;
      tooltip.hidden = false;
      const { offsetWidth: width, offsetHeight: height } = tooltip;
      const { clientWidth, clientHeight } = document.documentElement;
      const below = rect.bottom + 6;
      tooltip.style.left = Math.max(8, Math.min(x, clientWidth - width - 8)) + 'px';
      tooltip.style.top = (below + height + 8 <= clientHeight
        ? below : Math.max(8, rect.top - height - 6)) + 'px';
      return;
    }
    hideTooltip();
  }

  function showDiagnostics(list = [], message = '') {
    diagnosticList = list;
    const summary = list.length === 1 ? '1 compiler error' : list.length + ' compiler errors';
    diagnostics.hidden = !list.length;
    if (diagnostics.hidden) diagnostics.open = false;
    checkStatus.classList.toggle('sr-only', Boolean(list.length));
    const announcement = list.length ? summary : message;
    if (checkStatus.textContent !== announcement) checkStatus.textContent = announcement;
    diagnosticSummary.textContent = summary;
    diagnosticItems.replaceChildren(...list.map(({ start, code, message }) => {
      const line = source.value.slice(0, start).split('\n').length;
      const item = document.createElement('li');
      item.textContent = 'Line ' + line + ' · ' + code + ' · ' + message;
      return item;
    }));
    drawLens();
  }

  function syncScroll() {
    mirror.scrollTop = source.scrollTop;
    mirror.scrollLeft = source.scrollLeft;
    hideTooltip();
  }

  function stop() {
    clearTimeout(checkTimeout);
    checking = undefined;
    worker?.terminate();
    worker = undefined;
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
    painted.replaceChildren(...code.childNodes);
    paintedSource = source.value;
    mirror.classList.toggle('ends-with-newline', paintedSource.endsWith('\n'));
    mirror.hidden = false;
    source.classList.add('has-highlight');
    syncScroll();
    drawLens();
  }

  function startWorker() {
    const next = new Worker(new URL('./tour-worker.js', import.meta.url), { type: 'module' });
    next.onmessage = ({ data }) => {
      if (worker !== next) return;
      if (data.mode === 'highlight') receiveHighlight(data);
      else if (data.live) {
        if (checking !== data.id) return;
        clearTimeout(checkTimeout);
        checking = undefined;
        if (data.id === revision) {
          const message = data.ok ? (form.dataset.repair ? 'Checks passed.' : '') : data.output;
          showDiagnostics(data.diagnostics, message);
        }
      } else if (pending) {
        if (pending.revision === revision && data.diagnostics) showDiagnostics(data.diagnostics);
        finish(data);
      }
    };
    next.onerror = () => {
      if (worker !== next) return;
      if (pending) finish({ ok: false, restart: true, output: 'Could not start the Vesper compiler.' });
      else stop();
    };
    return next;
  }

  function refreshHighlight({ immediate = false } = {}) {
    revision++;
    showDiagnostics();
    updatePaint(source.value);
    clearTimeout(checkTimer);
    if (immediate) check();
    else checkTimer = setTimeout(check, 300);
    clearTimeout(highlightTimer);
    highlightTimer = setTimeout(() => {
      try {
        worker ??= startWorker();
        worker.postMessage({ source: source.value, mode: 'highlight', id: revision });
      } catch {
        // The textarea remains readable if a worker could not start.
      }
    }, 50);
  }

  function check() {
    if (pending || checking !== undefined) {
      checkTimer = setTimeout(check, 300);
      return;
    }
    try {
      worker ??= startWorker();
      const mode = ['test', 'test-case'].includes(form.dataset.mode) ? 'check-tests'
        : ['macro', 'expand', 'format'].includes(form.dataset.mode) ? 'expand' : 'check';
      checking = revision;
      checkTimeout = setTimeout(() => { if (!pending) stop(); }, 10000);
      worker.postMessage({ source: source.value, mode, id: revision, live: true });
    } catch {
      stop();
    }
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
    queueMicrotask(refreshHighlight);
  });
  source.addEventListener('input', refreshHighlight);
  source.addEventListener('scroll', syncScroll);
  source.addEventListener('pointermove', showTooltip);
  source.addEventListener('pointerleave', hideTooltip);
  window.addEventListener('scroll', hideTooltip);
  document.addEventListener('keydown', event => {
    if (event.key === 'Escape') hideTooltip();
  });
  if ('ResizeObserver' in globalThis) new ResizeObserver(syncScroll).observe(source);
  source.addEventListener('blur', () => { leaveOnTab = false; });
  source.addEventListener('keydown', event => {
    if (event.isComposing) return;
    const moveFocus = leaveOnTab;
    leaveOnTab = event.key === 'Escape';
    if (event.key === 'Tab' && !moveFocus
      && !event.shiftKey && !event.ctrlKey && !event.metaKey && !event.altKey) {
      event.preventDefault();
      const indent = '    ';
      const { selectionStart, selectionEnd, value, maxLength } = source;
      if (value.length - (selectionEnd - selectionStart) + indent.length > maxLength) return;
      // Insert through the browser so undo and the standard input event work.
      if (!document.execCommand('insertText', false, indent)) {
        source.setRangeText(indent, selectionStart, selectionEnd, 'end');
        source.dispatchEvent(new Event('input', { bubbles: true }));
      }
    } else if (event.key === 'Enter' && (event.ctrlKey || event.metaKey)) {
      event.preventDefault();
      form.requestSubmit();
    }
  });

  refreshHighlight({ immediate: true });
}
