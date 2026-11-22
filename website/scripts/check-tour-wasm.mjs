import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { evaluate } from '../tour-wasm.js';

export async function checkTourWasm(dist, tour, pages) {
  const wasm = await WebAssembly.compile(await readFile(resolve(dist, 'vesper-tour.wasm')));
  assert.deepEqual(WebAssembly.Module.imports(wasm), [], 'Tour Wasm must load without host bindings');
  const instance = await WebAssembly.instantiate(wasm);
  const invoke = (source, mode = 'run', test_case) => evaluate(instance.exports, { source, mode, test_case });
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
    assert.deepEqual(invoke(source, 'format'), { ok: true, output: source }, file + ' · starter formatting');
    const testCase = pages.get(file).source.match(/name="test_case" value="([^"]+)"/)?.[1];
    const result = invoke(source, mode, testCase);
    const expected = repair ?? expectedErrors.get(file);
    assert.match(pages.get(file).source, /<section data-tour-fallback><h2>Starting code<\/h2>/, file + ' · missing visible static fallback');
    if (repair) {
      assert(result.diagnostics?.some(d => d.code === repair && d.end >= d.start), file + ' · inline diagnostic');
      const solution = pages.get(file).source.match(/class="tour-solution">[\s\S]*?<code data-lang="vesper">([\s\S]*?)<\/code>/)?.[1];
      assert(solution, file + ' · missing solution');
      const fixed = solution.replace(/<[^>]*>/g, '').replace(/&(lt|gt|quot|amp);/g, (_, key) => entities[key]);
      assert(invoke(fixed, mode).ok, file + ' · invalid solution');
    }
    assert.equal(result.ok, !expected, file + ' · ' + result.output);
    if (expected) assert(result.output.includes(expected), file + ' · ' + result.output);
    if (file === 'tour/isolate-one-failing-row.html') {
      assert(invoke(source.replace('over(3, 2, 3)', 'over(3, 2, 2)'), mode, testCase).ok);
      assert.match(invoke(source.replaceAll('test_reserve', 'test_stock'), mode, 'test_stock::over').output, /FAIL test_stock::over/);
      assert(invoke(source, 'check-tests').ok, 'Live checking should leave test execution to Run');
      const badRow = source.replace('over(3, 2, 3)', 'over(true, 2, 3)');
      assert(invoke(badRow, 'check-tests').diagnostics.some(d =>
        badRow.slice(d.start, d.end).includes('@case over(true, 2, 3)')));
    }
    if (file === 'tour/use-the-resumed-result.html') {
      assert.equal(invoke(source.replace('tick = 99', 'tick = 98'), mode).ok, false);
    }
    if (file === 'tour/define-a-new-expression-form.html') {
      assert.equal(result.output, '42');
      assert.match(invoke(source, 'expand').output, /wrapping_add/);
    }
    if (file === 'tour/extract-a-lean-model.html') assert.match(result.output, /def .*main/);
    if (file === 'tour/find-a-conflicting-loan.html') {
      assert(result.diagnostics.some(d => d.related?.some(label =>
        source.slice(label.start, label.end) === 'room' && /earlier overlapping/.test(label.message))));
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
  assert.match(partial.output, /data-vc-tier="0"/);
  assert.match(partial.output, /π &lt;img&gt; &amp;/);
  assert(!partial.output.includes('<img>'));
  assert.match(invoke('fn main() -> i64 { 42 }', 'highlight').output, /data-vc-tier="1"/);
  const formatted = invoke('fn main()->i64{42}', 'format');
  assert(formatted.ok && formatted.output !== 'fn main()->i64{42}');
  assert.equal(invoke(formatted.output).output, '42');
  assert.equal(invoke('fn main(', 'format').ok, false);
  assert.match(invoke('fn main() { loop {} }').output, /fuel/i);
}
