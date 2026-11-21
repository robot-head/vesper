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
  ownership: `<figure class="worked-figure"><figcaption>Who owns the transaction at return?</figcaption>
    <div class="branch-root">Caller owns <code>tx</code></div>
    <ul class="ownership-branches">
      <li><strong>Commit</strong><span>The method consumes <code>tx</code> and returns its slot.</span></li>
      <li><strong>Abort</strong><span>The method consumes <code>tx</code> and returns <code>Unit</code>.</span></li>
      <li><strong>Skip both</strong><span>The caller still owns <code>tx</code>. The checker reports <code>E0302</code>.</span></li>
    </ul></figure>`,
  exit: flow('One return value, seen at two boundaries', [
    ['Vesper integer', '−1'], ['Low eight bits', '1111 1111'], ['Linux shell status', '255'],
  ]),
  resume: flow('Where the handler gets 42', [
    ['Handler resumes', '20'], ['Computation adds 1', '21'], ['Handler doubles', '42'],
  ]),
  'guide-expansion': expansion('addone'),
  'tour-expansion': expansion('next_seat'),
};

export function insertFigures(html) {
  return html.replace(/<!-- figure:([\w-]+) -->/g, (_, name) => {
    if (!Object.hasOwn(figures, name)) throw new Error(`Unknown figure: ${name}`);
    return figures[name];
  });
}
