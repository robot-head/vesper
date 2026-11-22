export function tourPrograms(lessons) {
  const modes = {
    'pass-an-effect-to-the-caller': 'check',
    'use-the-resumed-result': 'test',
    'isolate-one-failing-row': 'test-case',
    'extract-a-lean-model': 'lean',
    'carry-an-existing-allocation-forward': 'check',
    'group-hot-fields-into-tiles': 'check',
    'check-a-masked-vector-load': 'check',
  };
  return lessons.map(({ file, body }) => {
    const slug = file.slice(0, -5);
    const repair = body.match(/^~~~vesper ([A-Z0-9_]+)\n([\s\S]*?)\n~~~$/m);
    const program = repair?.[2] ?? body.match(/^~~~vesper\n([\s\S]*?)\n~~~$/m)?.[1];
    if (!program) throw new Error(`Tour lesson ${slug} has no editable example`);
    return {
      source: program,
      mode: modes[slug] ?? 'run',
      error: repair?.[1],
      expand: ['define-a-new-expression-form', 'define-your-own-sugar'].includes(slug),
      testCase: slug === 'isolate-one-failing-row' ? 'test_reserve::over' : undefined,
    };
  });
}
