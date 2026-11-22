(() => {
  let stored;
  try { stored = localStorage.getItem('vesper-theme'); } catch { /* Storage can be blocked. */ }
  const theme = stored === 'dusk' || stored === 'dawn'
    ? stored : matchMedia('(prefers-color-scheme: light)').matches ? 'dawn' : 'dusk';
  document.documentElement.dataset.vesperTheme = theme;
})();
