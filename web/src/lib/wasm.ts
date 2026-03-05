// Canonical tiny WASM module header for capability checks.
const wasmProbeBytes = Uint8Array.of(
  0x00,
  0x61,
  0x73,
  0x6d,
  0x01,
  0x00,
  0x00,
  0x00,
);

export function isWasmSupported(): boolean {
  try {
    if (typeof WebAssembly !== 'object') {
      return false;
    }
    if (typeof WebAssembly.Module !== 'function') {
      return false;
    }
    if (typeof WebAssembly.Instance !== 'function') {
      return false;
    }

    const module = new WebAssembly.Module(wasmProbeBytes);
    const instance = new WebAssembly.Instance(module);
    return instance instanceof WebAssembly.Instance;
  } catch {
    return false;
  }
}
