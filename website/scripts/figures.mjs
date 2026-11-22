const broker = `<figure class="worked-figure broker-figure">
  <figcaption>What can one call change?</figcaption>
  <pre tabindex="0"><code data-lang="vesper">append(inout broker, in broker.config);</code></pre>
  <table aria-label="Broker fields before and after append">
    <thead><tr><th scope="col">Field</th><th scope="col">Loan</th><th scope="col">Before</th><th scope="col">After</th></tr></thead>
    <tbody>
      <tr><th scope="row"><code>index</code></th><td><code>inout</code><small>Read + write</small></td><td>1</td><td><strong data-index>42</strong></td></tr>
      <tr><th scope="row"><code>metrics</code></th><td><code>inout</code><small>Read + write</small></td><td>0</td><td><strong>1</strong></td></tr>
      <tr><th scope="row"><code>config</code></th><td><code>in</code><small>Read only</small></td><td data-config>41</td><td data-config>41</td></tr>
    </tbody>
  </table>
  <p>Each call adds <code>config</code> to <code>index</code> and counts one append in <code>metrics</code>.</p>
  <label class="broker-control" hidden>Try a starting <code>config</code>
    <select><option>0</option><option>8</option><option selected>41</option></select>
  </label>
  <span class="sr-only" role="status"></span>
  <details class="loan-map"><summary>Trace the two field loans</summary><!-- diagram:field-loans --></details>
</figure>`;

function flow(caption, stages) {
  return `<figure class="worked-figure"><figcaption>${caption}</figcaption>
    <ol class="value-flow">${stages.map(([name, value]) =>
      `<li><span>${name}</span><strong>${value}</strong></li>`).join('')}</ol></figure>`;
}

function expansion(macro) {
  return `<figure class="worked-figure"><figcaption>From macro call to result</figcaption>
    <ol class="value-flow expansion">
      <li><span>Written</span><pre tabindex="0"><code data-lang="vesper">${macro}!(41)</code></pre></li>
      <li><span>Expanded</span><pre tabindex="0"><code data-lang="vesper">wrapping_add(41, 1)</code></pre></li>
      <li><span>Evaluated</span><strong>42</strong></li>
    </ol></figure>`;
}

export const figures = {
  broker,
  'ownership-modes': `<figure class="worked-figure mode-figure">
    <figcaption>What can an owner do?</figcaption>
    <table aria-label="Ownership rules by mode">
      <thead><tr><th scope="col">Mode</th><th scope="col">Copy</th><th scope="col">Move</th><th scope="col">Drop unused</th></tr></thead>
      <tbody>
        <tr><th scope="row"><code>copy</code></th><td>Yes</td><td>Yes</td><td>Yes</td></tr>
        <tr><th scope="row"><code>affine</code></th><td>No</td><td>Yes</td><td>Yes</td></tr>
        <tr><th scope="row"><code>linear</code></th><td>No</td><td>Yes</td><td>No</td></tr>
      </tbody>
    </table>
    <p>Moving a linear value hands its obligation to a new owner. That owner must eventually consume it or move it again.</p>
  </figure>`,
  ownership: '<!-- diagram:linear-transaction -->',
  exit: flow('One return value, seen at two boundaries', [
    ['Vesper integer', '−1'], ['Low eight bits', '1111 1111'], ['Linux shell status', '255'],
  ]),
  'pulse-guide': '<p><a href="../docs/chapter-15.html#section-15-4">Read about fibers, worker threads, and nurseries</a>.</p>',
  resume: '<!-- diagram:effect-resumption -->',
  'clock-test': flow('What the clock test supplies', [
    ['Reservation asks', 'now()'], ['Test resumes with', '100'], ['At the deadline', 'true'],
  ]),
  'guide-expansion': expansion('addone'),
  'tour-expansion': expansion('next_seat'),
};

const diagrams = {
  'field-loans': ['Two loans into one record', 'The inout loan reaches index and metrics. A separate in loan reads config.', 260],
  'linear-transaction': ['Three ways to leave a transaction', 'Commit and abort both consume the transaction. Returning while it is still live produces E0302.', 296],
  'effect-resumption': ['Follow the request and its reply', 'The computation asks for a value. The handler resumes with 20, receives 21, then doubles it to 42.', 340],
  'macro-substitution': ['Where the two operands come from', 'The call supplies 41 through the expression slot. The template supplies the constant 1. The resulting addition gives 42.', 312],
};

export function insertFigures(html, prefix = '') {
  return html.replace(/<!-- figure:([\w-]+) -->/g, (_, name) => {
    if (!Object.hasOwn(figures, name)) throw new Error(`Unknown figure: ${name}`);
    return figures[name];
  }).replace(/<!-- diagram:([\w-]+) -->/g, (_, name) => {
    const [title, description, height] = diagrams[name];
    const url = `${prefix}diagrams/${name}.svg`;
    return `<figure class="teaching-diagram"><figcaption>${title}</figcaption>
      <img src="${url}" width="560" height="${height}" loading="lazy" alt="${description}">
      <a href="${url}">Open full-size diagram<span class="sr-only"> · ${title}</span></a></figure>`;
  });
}
