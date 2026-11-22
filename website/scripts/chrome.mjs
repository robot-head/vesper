const links = [
  ['index.html', 'Home'],
  ['tour/index.html', 'Tour'],
  ['language.html', 'Language guide'],
  ['docs/index.html', 'Docs'],
  ['vesper-specification.pdf', 'Specification PDF'],
  ['https://github.com/robot-head/vesper', 'GitHub'],
];

const href = (path, prefix) => path.startsWith('https://') ? path : prefix + path;

export function siteChrome(html, prefix = '', current = '') {
  const navigation = links.map(([path, title]) =>
    `<a href="${href(path, prefix)}"${path === current ? ' aria-current="page"' : ''}>${title}</a>`).join('\n');
  const header = `<header class="nav"><div class="wrap site-nav">
    <a class="brand" href="${prefix}index.html" aria-label="Vesper home"><img data-brand-mark src="${prefix}brand/vesper-mark-dusk.svg" width="34" height="34" alt="">vesper</a>
    <nav aria-label="Main navigation">${navigation}</nav>
    <label class="theme" title="Switch between dusk and dawn"><input type="checkbox" id="theme-toggle" role="switch" aria-label="Dawn theme"><span aria-hidden="true">◐</span></label>
  </div></header>`;
  const footer = `<footer><div class="wrap site-footer">
    <img data-brand-mark src="${prefix}brand/vesper-wordmark-dusk.svg" width="130" height="35" alt="Vesper">
    <nav aria-label="Footer navigation">${navigation}</nav>
  </div></footer>`;
  return html.replace('</head>', `<script src="${prefix}theme.js"></script>\n</head>`)
    .replace('<!-- site-header -->', header).replace('<!-- site-footer -->', footer);
}
