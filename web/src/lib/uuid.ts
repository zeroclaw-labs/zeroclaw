/**
 * Generate a RFC 4122 UUID v4. Uses crypto.randomUUID() when available,
 * otherwise falls back to crypto.getRandomValues() for older browsers
 * (e.g. Safari < 15.4, some Electron environments).
 */
export function randomUUID(): string {
  if (
    typeof crypto !== 'undefined' &&
    typeof (crypto as Crypto & { randomUUID?: () => string }).randomUUID ===
      'function'
  ) {
    return (crypto as Crypto & { randomUUID: () => string }).randomUUID();
  }
  return fallbackUUID();
}

function fallbackUUID(): string {
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  bytes[6] = (bytes[6]! & 0x0f) | 0x40;
  bytes[8] = (bytes[8]! & 0x3f) | 0x80;
  const hex = [...bytes]
    .map((b) => b.toString(16).padStart(2, '0'))
    .join('');
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}
