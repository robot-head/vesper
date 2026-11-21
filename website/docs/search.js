const topics = JSON.parse(document.getElementById('search-index').textContent);
const input = document.getElementById('topic-query');
const results = document.getElementById('search-results');
const status = document.getElementById('search-status');
document.querySelector('.docs-search').hidden = false;

function findTopics() {
  const terms = input.value.trim().toLowerCase().split(/\s+/).filter(Boolean);
  results.replaceChildren();
  results.hidden = !terms.length;
  status.textContent = '';
  if (!terms.length) return;

  const matches = [];
  for (const topic of topics) {
    const title = topic.title.toLowerCase();
    const words = [topic.id, topic.part, topic.chapter, topic.text, title].join(' ').toLowerCase();
    if (terms.every(term => words.includes(term))) {
      const score = terms.filter(term => title.includes(term)).length;
      matches.push({ topic, score });
    }
  }
  matches.sort((a, b) => b.score - a.score);
  status.textContent = matches.length
    ? `${matches.length} matching section${matches.length === 1 ? '' : 's'}`
    : `No sections match “${input.value.trim()}”. Search for one term, such as “ownership”, or browse by subject.`;
  for (const { topic } of matches) {
    const item = document.createElement('li');
    const link = document.createElement('a');
    link.href = topic.url;
    link.textContent = topic.title;
    const context = document.createElement('small');
    context.textContent = `§${topic.id} · ${topic.chapter}`;
    item.append(link, context);
    results.append(item);
  }
}
input.addEventListener('input', findTopics);
findTopics();
