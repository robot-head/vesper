const broker = prefix => `<figure class="worked-figure broker-figure">
  <figcaption>What can one call change?</figcaption>
  <pre tabindex="0"><code data-lang="vesper">// Callee
fn append(inout broker.{index, metrics}: Broker, in config: u64)</code></pre>
  <pre tabindex="0"><code data-lang="vesper">// Caller
append(inout broker, in broker.config);</code></pre>
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
  <label data-overlap-toggle hidden><input type="checkbox"> Show an overlapping loan</label>
  <div data-overlap>
    <pre tabindex="0"><code data-lang="vesper">// Two exclusive parameters
fn transfer(inout broker.{index}: Broker, inout other: u64)
transfer(inout broker, inout broker.index);</code></pre>
    <p><strong>E0301</strong> · Both arguments ask for exclusive access to <code>index</code>.
    <a href="${prefix}tour/find-a-conflicting-loan.html">Try a repair in the tour</a>.</p>
  </div>
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
  'continuation-fission': ["Proposed continuation fission", "The normal continuation keeps the ownership obligation. The proposed failure continuation runs cleanup or transfers custody to a quarantine cell.", 324],
  'compiler-pipeline': ["Five stages in the compiler design", "Source passes through syntax analysis, resolution, place checking, and optimization before native emission or Lean extraction.", 372],
  'lean-extraction': ["Translate supported source to Lean", "An inout projection becomes a returned state update. The model represents selected effects explicitly. The proof audit checks the resulting artifact under its declared axiom policy.", 336],
  'aosoa-tile-layout': ["Compare array and tile layouts", "Three layouts show interleaved records, separate field arrays, and contiguous field lanes inside an AoSoA tile.", 356],
  'pulse-nursery-dma': ["Pulse nurseries and device ownership", "Child work joins before the scope exits. A submitted device buffer must remain owned until terminal completion. The escrow alternative is a design proposal.", 356],
  'wire-struct-layout': ["Explicit wire layout and foreign-call boundary", "The example labels each byte of a 12-byte wire record. The separate MPK transition illustrates one host protection mechanism.", 344],
  'lens-early-cutoff': ["Lens incremental checking design", "A body edit invalidates its check. If the public signature is stable, downstream queries can reuse prior results.", 336],
  'chrono-flight-recorder': ["Chrono effect recording and transcript replay", "The design records effect completions between keyframes. Transcript replay does not reconstruct nondeterminism outside the captured events.", 340],
  'silicon-lifo-mmio': ["Silicon ordered guards and DMA ownership", "The diagram illustrates last-in, first-out interrupt restoration and an owned buffer moving between CPU and device.", 356],
  'aegis-bounce-grant': ["Aegis single-fetch design", "An illustrated copy from shared host memory into private guest memory. The copy limits double-fetch races during validation.", 336],
  'warp-umem-rings': ["Warp AF_XDP buffer ownership", "An illustrated frame moves through the fill, receive, transmit, and completion rings. A live linear frame must be returned or consumed.", 348],
  'flux-dbsp-circuit': ["Flux incremental join and frontier", "Input changes join with retained state. The circuit consolidates weights and emits output deltas under a frontier ticket.", 336],
  'ignis-render-graph': ["Ignis render-graph design", "Passes describe which attachments they read, initialize, or change. The design uses those edges to derive barriers for the selected backend.", 340],
  'tensor-kv-paged': ["Tensor grouped-query attention and page ownership", "Query heads map to key and value head groups. The illustrated page table maps logical blocks to physical pages held by linear leases.", 352],
  'metatheory-map': ["Lean models and their boundaries", "Separate models cover closures, polymorphism, places, and finite executable correspondence. Their results apply to the fragments and assumptions stated in each model.", 368],
  'prism-cad-grid': ["Geometry of the Vesper prism", "Construction grid showing the outer rays, the three blades, and the channels that separate them.", 356],
  'field-loans': ['Two loans into one record', 'The inout loan reaches index and metrics. A separate in loan reads config.', 260],
  'linear-transaction': ['Three ways to leave a transaction', 'Commit and abort both consume the transaction. Returning while it is still live produces E0302.', 296],
  'effect-resumption': ['Follow the request and its reply', 'The computation asks for a value. The handler resumes with 20, receives 21, then doubles it to 42.', 340],
  'macro-substitution': ['Where the two operands come from', 'The call supplies 41 through the expression slot. The template supplies the constant 1. The resulting addition gives 42.', 312],
};

export function insertFigures(html, prefix = '') {
  return html.replace(/<!-- figure:([\w-]+) -->/g, (_, name) => {
    if (!Object.hasOwn(figures, name)) throw new Error(`Unknown figure: ${name}`);
    const figure = figures[name];
    return typeof figure === 'function' ? figure(prefix) : figure;
  }).replace(/<!-- diagram:([\w-]+) -->/g, (_, name) => {
    const [title, description, height] = diagrams[name];
    const url = `${prefix}diagrams/${name}.svg`;
    return `<figure class="teaching-diagram"><figcaption>${title}</figcaption>
      <img src="${url}" width="560" height="${height}" loading="lazy" alt="${description}">
      <a href="${url}">Open full-size diagram<span class="sr-only"> · ${title}</span></a></figure>`;
  });
}
