const encoder = new TextEncoder();
const decoder = new TextDecoder();

export function evaluate(wasm, request) {
  const input = encoder.encode(JSON.stringify(request));
  const offset = wasm.vesper_tour_input(input.length);
  if (!offset) return { ok: false, output: 'This example exceeds the 16 KiB source limit.' };
  new Uint8Array(wasm.memory.buffer, offset, input.length).set(input);
  const output = wasm.vesper_tour_execute();
  const bytes = new Uint8Array(wasm.memory.buffer, output, wasm.vesper_tour_output_len());
  return JSON.parse(decoder.decode(bytes));
}
