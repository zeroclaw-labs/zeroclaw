import { describe, expect, it } from 'vitest';
import { isWasmSupported } from './wasm';

describe('isWasmSupported', () => {
  it('returns a boolean without throwing', () => {
    expect(() => isWasmSupported()).not.toThrow();
    expect(typeof isWasmSupported()).toBe('boolean');
  });
});
