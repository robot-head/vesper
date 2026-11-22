import { evaluate } from './tour-wasm.js';

let runtime;
async function loadRuntime() {
  const response = await fetch(new URL('vesper-tour.wasm', import.meta.url));
  if (!response.ok) throw new Error('Could not load the Vesper compiler.');
  const { instance } = await WebAssembly.instantiate(await response.arrayBuffer());
  return instance.exports;
}

self.addEventListener('message', async ({ data }) => {
  try {
    const wasm = await (runtime ??= loadRuntime());
    self.postMessage({ ...evaluate(wasm, data), mode: data.mode, id: data.id, live: data.live });
  } catch (error) {
    const message = error instanceof WebAssembly.RuntimeError
      ? 'The example stopped while checking or running. Try a smaller input.'
      : 'Could not start the Vesper compiler. Try again.';
    self.postMessage({ ok: false, output: message, restart: true, mode: data.mode, id: data.id, live: data.live });
  }
});
