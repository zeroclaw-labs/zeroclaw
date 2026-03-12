// Safe randomUUID for environments without crypto.randomUUID.
// Order: ① crypto.randomUUID (standard) ② RFC4122 v4 via getRandomValues (crypto, then msCrypto).
// No fallback: if neither is available, throws.

function hasNativeRandomUUID(): boolean {
  try {
    return (
      typeof globalThis.crypto !== 'undefined' &&
      typeof (globalThis.crypto as Crypto).randomUUID === 'function'
    );
  } catch {
    return false;
  }
}

/** Resolve Crypto for getRandomValues: standard crypto first, then msCrypto. */
function getCryptoForGetRandomValues(): Crypto {
  if (
    typeof globalThis.crypto !== 'undefined' &&
    typeof (globalThis.crypto as Crypto).getRandomValues === 'function'
  ) {
    return globalThis.crypto as Crypto;
  }
  const msCrypto = (globalThis as unknown as { msCrypto?: Crypto }).msCrypto;
  if (
    typeof msCrypto !== 'undefined' &&
    typeof msCrypto.getRandomValues === 'function'
  ) {
    return msCrypto;
  }
  throw new Error(
    'zeroclaw: crypto.randomUUID and crypto.getRandomValues are not available in this environment.'
  );
}

function uuidV4FromGetRandomValues(): string {
  const crypto = getCryptoForGetRandomValues();
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  // RFC4122 v4: version nibble 4, variant 10
  bytes[6] = ((bytes[6] ?? 0) & 0x0f) | 0x40;
  bytes[8] = ((bytes[8] ?? 0) & 0x3f) | 0x80;
  const toHex = (n: number): string => n.toString(16).padStart(2, '0');
  const hex = Array.from(bytes, toHex).join('');
  return [
    hex.slice(0, 8),
    hex.slice(8, 12),
    hex.slice(12, 16),
    hex.slice(16, 20),
    hex.slice(20),
  ].join('-');
}

/**
 * UUID v4 generator compatible with older browsers.
 * Uses crypto.randomUUID() when available, otherwise RFC4122 v4 via getRandomValues (crypto or msCrypto).
 */
export function randomUUID(): string {
  if (hasNativeRandomUUID()) {
    return (globalThis.crypto as Crypto).randomUUID();
  }
  return uuidV4FromGetRandomValues();
}
