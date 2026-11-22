const themeButton = document.getElementById('theme-toggle');
function applyTheme(theme) {
  themeButton.checked = theme === 'dawn';
  document.documentElement.dataset.vesperTheme = theme;
  document.querySelector('meta[name="theme-color"]').content = theme === 'dusk' ? '#101111' : '#f7f5f0';
  for (const mark of document.querySelectorAll('[data-brand-mark]')) {
    mark.src = mark.src.replace(/(vesper-(?:wordmark|mark))-(?:dusk|dawn)\.svg$/, `$1-${theme}.svg`);
  }
}
applyTheme(document.documentElement.dataset.vesperTheme);
themeButton.addEventListener('change', () => {
  const theme = themeButton.checked ? 'dawn' : 'dusk';
  applyTheme(theme);
  try { localStorage.setItem('vesper-theme', theme); } catch { /* Storage can be blocked. */ }
});

for (const list of document.querySelectorAll('[role="tablist"]')) {
  const tabs = [...list.querySelectorAll('[role="tab"]')];
  const select = active => {
    for (const tab of tabs) {
      const selected = tab === active;
      tab.setAttribute('aria-selected', String(selected));
      tab.tabIndex = selected ? 0 : -1;
      document.getElementById(tab.getAttribute('aria-controls')).hidden = !selected;
    }
  };
  select(tabs.find(tab => tab.getAttribute('aria-selected') === 'true') ?? tabs[0]);
  for (const [i, tab] of tabs.entries()) {
    tab.addEventListener('click', () => select(tab));
    tab.addEventListener('keydown', event => {
      const directions = { ArrowLeft: -1, ArrowRight: 1 };
      const delta = directions[event.key];
      if (delta === undefined) return;
      event.preventDefault();
      const next = tabs[(i + delta + tabs.length) % tabs.length];
      select(next);
      next.focus();
    });
  }
}

for (const button of document.querySelectorAll('[data-copy]')) {
  const originalLabel = button.textContent;
  button.addEventListener('click', async () => {
    const target = document.getElementById(button.dataset.copy);
    const source = target.querySelector('code')?.textContent ?? target.textContent;
    try {
      await navigator.clipboard.writeText(source);
      button.textContent = 'Copied';
      document.getElementById('copy-status').textContent = 'Copied to clipboard.';
    } catch {
      button.textContent = 'Text selected';
      const range = document.createRange();
      range.selectNodeContents(target);
      const selection = window.getSelection();
      selection.removeAllRanges();
      selection.addRange(range);
      document.getElementById('copy-status').textContent = 'Clipboard unavailable. The text is selected. Press Ctrl+C or Command+C to copy it.';
    }
    setTimeout(() => { button.textContent = originalLabel; }, 2200);
  });
}

for (const figure of document.querySelectorAll('.broker-figure')) {
  const control = figure.querySelector('.broker-control');
  const select = control.querySelector('select');
  const update = () => {
    const config = Number(select.value);
    for (const cell of figure.querySelectorAll('[data-config]')) cell.textContent = String(config);
    figure.querySelector('[data-index]').textContent = String(1 + config);
  };
  select.addEventListener('change', () => {
    update();
    figure.querySelector('[role="status"]').textContent =
      `After append, index is ${1 + Number(select.value)}, metrics is 1, and config is ${select.value}.`;
  });
  update();
  control.hidden = false;
  const comparison = figure.querySelector('[data-overlap-toggle]');
  const overlap = figure.querySelector('[data-overlap]');
  if (comparison && overlap) {
    overlap.hidden = true;
    comparison.hidden = false;
    comparison.querySelector('input').addEventListener('change', event => {
      overlap.hidden = !event.target.checked;
    });
  }
}
