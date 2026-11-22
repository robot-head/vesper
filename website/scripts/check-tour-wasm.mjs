import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { evaluate } from '../tour-wasm.js';

export async function checkTourWasm(dist, tour, pages) {
  const wasm = await WebAssembly.compile(await readFile(resolve(dist, 'vesper-tour.wasm')));
  assert.deepEqual(WebAssembly.Module.imports(wasm), [], 'Tour Wasm must load without host bindings');
  const instance = await WebAssembly.instantiate(wasm);
  const invoke = (source, mode = 'run') => evaluate(instance.exports, { source, mode });
  const expectedErrors = new Map([
    ['tour/isolate-one-failing-row.html', 'FAIL test_reserve::over'],
  ]);
  const entities = { lt: '<', gt: '>', quot: '"', amp: '&' };
  for (const file of tour) {
    const example = pages.get(file).source.match(/data-mode="([^"]+)".*?<textarea[^>]*>(.*?)<\/textarea>/s);
    if (!example) continue;
    const [, mode, encoded] = example;
    const source = encoded.replace(/&(lt|gt|quot|amp);/g, (_, key) => entities[key]);
    const repair = pages.get(file).source.match(/data-repair="([A-Z0-9_]+)"/)?.[1];
    if (repair !== 'FORGE_PARSE') assert.deepEqual(invoke(source, 'format'), { ok: true, output: source }, file + ' · starter formatting');
    const result = invoke(source, mode);
    const expected = repair ?? expectedErrors.get(file);
    if (repair) {
      assert(!pages.get(file).source.includes('data-error='), file + ' · duplicate diagnostic listing');
      assert(result.diagnostics?.some(d => d.code === repair && d.end >= d.start), file + ' · inline diagnostic');
    }
    assert.equal(result.ok, !expected, file + ' · ' + result.output);
    if (expected) assert(result.output.includes(expected), file + ' · ' + result.output);
    if (file === 'tour/isolate-one-failing-row.html') {
      assert(invoke(source.replace('over(3, 2, 3)', 'over(3, 2, 2)'), mode).ok);
      assert(invoke(source, 'check-tests').ok, 'Live checking should leave test execution to Run');
      const badRow = source.replace('over(3, 2, 3)', 'over(true, 2, 3)');
      assert(invoke(badRow, 'check-tests').diagnostics.some(d =>
        badRow.slice(d.start, d.end).includes('@case over(true, 2, 3)')));
    }
    if (file === 'tour/use-the-resumed-result.html') {
      assert.equal(invoke(source.replace('tick = 99', 'tick = 98'), mode).ok, false);
    }
  }
  const broken = '// 😀\nfn main() -> i64 { true }';
  const live = invoke(broken, 'check');
  assert(live.diagnostics.some(d => broken.slice(d.start, d.end) === 'true'));
  assert(invoke(broken.replace('true', '42'), 'check').ok);
  assert.deepEqual(invoke('fn main() -> i64 { 42 }'), { ok: true, output: '42' });
  assert.match(invoke('fn main() -> i64 { true }').output, /E\d{4}/);
  const partial = invoke('fn main() {\n    // π <img> &\n', 'highlight');
  assert(partial.ok);
  assert.match(partial.output, /class="vc-k">fn</);
  assert.match(partial.output, /π &lt;img&gt; &amp;/);
  assert(!partial.output.includes('<img>'));
  const formatted = invoke('fn main()->i64{42}', 'format');
  assert(formatted.ok && formatted.output !== 'fn main()->i64{42}');
  assert.equal(invoke(formatted.output).output, '42');
  assert.equal(invoke('fn main(', 'format').ok, false);
  assert.match(invoke('fn main() { loop {} }').output, /fuel/i);
}
