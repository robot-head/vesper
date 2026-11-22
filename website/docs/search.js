
const input = document.getElementById('topic-query');
const results = document.getElementById('search-results');
const status = document.getElementById('search-status');
document.querySelector('.docs-search').hidden = false;
let index;
const words = text => text.toLowerCase().match(/[\p{L}\p{N}_]+(?:\.[\p{N}]+)?/gu) ?? [];

function searchIndex() {
  return index ??= fetch(new URL('search-index.json', import.meta.url)).then(response => {
    if (!response.ok) throw new Error('Search index unavailable');
    return response.json();
  }).then(topics => topics.map(topic => ({
    topic,
    title: new Set(words(topic.title)),
    body: new Set(words([topic.id, topic.chapter, topic.part, topic.text].join(' '))),
  }))).catch(error => {
    index = undefined;
    throw error;
  });
}

async function findTopics() {
  const query = input.value.trim().toLowerCase().replace(/§\s*/g, '');
  const terms = words(query);
  results.replaceChildren();
  results.hidden = true;
  status.textContent = '';
  if (!terms.length) return;

  let topics;
  try {
    topics = await searchIndex();
  } catch {
    status.textContent = 'Search is unavailable. Try again or browse the chapter links.';
    return;
  }
  if (query !== input.value.trim().toLowerCase().replace(/§\s*/g, '')) return;
  const matches = topics.flatMap(({ topic, title, body }) => {
    const scores = terms.map(term => title.has(term) ? 4 : body.has(term) ? 2
      : term.length >= 3 && [...title, ...body].some(word => word.startsWith(term)) ? 1 : 0);
    return scores.every(Boolean) ? [{ topic, score: scores.reduce((sum, score) => sum + score, 0) }] : [];
  }).sort((a, b) => b.score - a.score);
  results.hidden = !matches.length;
  status.textContent = matches.length
    ? matches.length + ' matching results' : 'No results. Try another term or a section number.';
  for (const { topic } of matches.slice(0, 40)) {
    const item = document.createElement('li');
    const link = document.createElement('a');
    link.href = topic.url;
    link.textContent = topic.title;
    const context = document.createElement('small');
    context.textContent = topic.id ? '§' + topic.id + ' · ' + topic.chapter : topic.chapter;
    item.append(link, context);
    results.append(item);
  }
}
input.addEventListener('input', findTopics);
document.addEventListener('keydown', event => {
  if (event.key === '/' && !event.ctrlKey && !event.metaKey && !event.altKey
    && !event.target.closest('input, textarea, [contenteditable]')) {
    event.preventDefault();
    input.focus();
  }
});
