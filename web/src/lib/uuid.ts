/**
 * Generates a UUID v4 string.
 *
 * Uses `crypto.randomUUID()` when available (secure contexts: HTTPS / localhost).
 * Falls back to `crypto.getRandomValues()` for plain-HTTP non-localhost access,
 * where `randomUUID` is restricted by browsers but `getRandomValues` still works.
 */
export function randomUUID(): string {
  if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
    return crypto.randomUUID();
  }

  // RFC 4122 §4.4 compliant UUID v4 via getRandomValues
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  // version: 4
  bytes[6] = ((bytes[6] ?? 0) & 0x0f) | 0x40;
  // variant: 10xx
  bytes[8] = ((bytes[8] ?? 0) & 0x3f) | 0x80;

  const hex = Array.from(bytes, (b) => b.toString(16).padStart(2, '0'));
  return (
    hex.slice(0, 4).join('') +
    '-' +
    hex.slice(4, 6).join('') +
    '-' +
    hex.slice(6, 8).join('') +
    '-' +
    hex.slice(8, 10).join('') +
    '-' +
    hex.slice(10, 16).join('')
  );
}
