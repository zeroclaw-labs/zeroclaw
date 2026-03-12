function formatUuidFromBytes(bytes: Uint8Array): string {
  const hex = Array.from(bytes, (value) => value.toString(16).padStart(2, '0')).join('');
  return [
    hex.slice(0, 8),
    hex.slice(8, 12),
    hex.slice(12, 16),
    hex.slice(16, 20),
    hex.slice(20, 32),
  ].join('-');
}

export function createRandomId(): string {
  const webCrypto = globalThis.crypto;
  if (webCrypto && typeof webCrypto.randomUUID === 'function') {
    return webCrypto.randomUUID();
  }

  if (webCrypto && typeof webCrypto.getRandomValues === 'function') {
    const bytes = new Uint8Array(16);
    webCrypto.getRandomValues(bytes);
    const versionByte = bytes[6] ?? 0;
    const variantByte = bytes[8] ?? 0;
    bytes[6] = (versionByte & 0x0f) | 0x40;
    bytes[8] = (variantByte & 0x3f) | 0x80;
    return formatUuidFromBytes(bytes);
  }

  return `zeroclaw-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}
