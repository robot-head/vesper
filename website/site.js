const themeButton = document.getElementById('theme-toggle');
themeButton.addEventListener('change', () => {
  const theme = themeButton.checked ? 'dawn' : 'dusk';
  document.documentElement.dataset.vesperTheme = theme;
  document.querySelector('meta[name="theme-color"]').content = theme === 'dusk' ? '#101111' : '#f7f5f0';
  for (const mark of document.querySelectorAll('[data-brand-mark]')) {
    mark.src = `brand/vesper-mark-${theme}.svg`;
  }
});

for (const button of document.querySelectorAll('[data-copy]')) {
  button.addEventListener('click', async () => {
    const target = document.getElementById(button.dataset.copy);
    const source = target.querySelector('code')?.textContent ?? target.textContent;
    try {
      await navigator.clipboard.writeText(source);
      button.textContent = 'Copied';
      document.getElementById('copy-status').textContent = 'Source copied to clipboard.';
    } catch {
      button.textContent = 'Select source';
      const range = document.createRange();
      range.selectNodeContents(target);
      const selection = window.getSelection();
      selection.removeAllRanges();
      selection.addRange(range);
      document.getElementById('copy-status').textContent = 'Source selected. Use your keyboard to copy it.';
    }
    setTimeout(() => { button.textContent = 'Copy source'; }, 2200);
  });
}
