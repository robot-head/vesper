const themeButton = document.getElementById('theme-toggle');
themeButton.addEventListener('change', () => {
  const theme = themeButton.checked ? 'dawn' : 'dusk';
  document.documentElement.dataset.vesperTheme = theme;
  document.querySelector('meta[name="theme-color"]').content = theme === 'dusk' ? '#101111' : '#f7f5f0';
  for (const mark of document.querySelectorAll('[data-brand-mark]')) {
    mark.src = mark.src.replace(/vesper-mark-(?:dusk|dawn)\.svg$/, `vesper-mark-${theme}.svg`);
  }
});

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
}
