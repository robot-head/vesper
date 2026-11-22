// Lessons can show only the code under discussion. Give the editor the rest
// of the program so readers can start a step without carrying edits forward.
export function tourPrograms(lessons) {
  const source = Object.fromEntries(lessons.map(({ file, body }) => [
    file, [...body.matchAll(/^~~~vesper\n([\s\S]*?)\n~~~$/gm)].at(-1)?.[1] ??
      body.match(/^~~~vesper [A-Z0-9_]+\n([\s\S]*?)\n~~~$/m)?.[1],
  ]));
  const code = slug => source[slug + '.html'];
  const join = (...parts) => parts.join('\n\n');
  const ticket = code('give-consumption-a-name').split('fn main()')[0].trim();
  const checked = code('declare-checked-addition');
  const reserve = code('write-one-test').split('test {')[0].trim();
  const macro = code('bind-a-piece-of-syntax');
  const withSource = {
    'include-the-endpoint': code('repeat-an-update').replace(/for n[^\n]+/, code('include-the-endpoint')),
    'transfer-the-obligation': join(ticket, code('transfer-the-obligation')),
    'inspect-before-consuming': join(ticket, code('inspect-before-consuming')),
    'handle-an-overflow': join(checked, code('handle-an-overflow')),
    'turn-inputs-into-rows': join(reserve, code('turn-inputs-into-rows')),
    'isolate-one-failing-row': join(reserve, code('turn-inputs-into-rows').replace('over(3, 2, 2)', 'over(3, 2, 3)')),
    'format-a-user-defined-spelling': macro,
  };
  const modes = {
    'declare-checked-addition': 'check',
    'pass-an-effect-to-the-caller': 'check',
    'resume-the-computation': 'test',
    'use-the-resumed-result': 'test',
    'write-one-test': 'test',
    'turn-inputs-into-rows': 'test',
    'isolate-one-failing-row': 'test-case',
    'bind-a-piece-of-syntax': 'macro',
    'define-your-own-sugar': 'macro',
    'format-a-user-defined-spelling': 'format',
  };
  return lessons.map(({ file }) => {
    const slug = file.slice(0, -5);
    if (slug === 'read-a-proposed-sugar-declaration') return null;
    const program = withSource[slug] ?? source[file];
    if (!program) throw new Error(`Tour lesson ${slug} has no editable example`);
    return { source: program, mode: modes[slug] ?? 'run' };
  });
}
