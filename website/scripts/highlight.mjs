export const escape = text => text.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;').replaceAll('"', '&quot;');

const keywords = new Set('copy affine struct enum impl fn let mut match if else for return where mod test true false quote macro expr sugar rule pub drop break loop'.split(' '));
const effects = new Set(['in', 'eff', 'handle', 'with', 'effect', 'resume']);
const tokens = /\/\/[^\n]*|"(?:\\.|[^"\\])*"|\$?[A-Za-z_][A-Za-z0-9_]*|\b\d+\b/g;

export function highlight(source) {
  let html = '', end = 0;
  for (const { 0: word, index } of source.matchAll(tokens)) {
    let kind;
    if (word.startsWith('//')) kind = 'comment';
    else if (keywords.has(word)) kind = 'keyword';
    else if (effects.has(word)) kind = 'effect';
    else if (word === 'linear' || word === 'sink') kind = 'linear';
    else if (word === 'inout' || word.startsWith('$')) kind = 'place';
    else if (/^[A-Z"]|^(?:[ui]\d+|bool)$/.test(word)) kind = 'type';
    else if (/^\d/.test(word)) kind = 'number';
    if (!kind) continue;
    html += escape(source.slice(end, index)) + `<span class="tok-${kind}">${escape(word)}</span>`;
    end = index + word.length;
  }
  return html + escape(source.slice(end));
}

// The authored HTML already escapes source. Decode it once before tokenizing.
export function highlightListings(html) {
  const entities = { '&lt;': '<', '&gt;': '>', '&amp;': '&', '&quot;': '"' };
  return html.replace(/(<code data-lang="(?:rust|vesper)">)([^<]*)(<\/code>)/g,
    (_, open, source, close) => open + highlight(source.replace(/&(?:lt|gt|amp|quot);/g, key => entities[key])) + close);
}
