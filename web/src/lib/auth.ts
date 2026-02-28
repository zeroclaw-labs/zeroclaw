export const TOKEN_STORAGE_KEY = 'zeroclaw_token';
let inMemoryToken: string | null = null;

function readStorage(key: string): string | null {
  try {
    return sessionStorage.getItem(key);
  } catch {
    return null;
  }
}

function writeStorage(key: string, value: string): void {
  try {
    sessionStorage.setItem(key, value);
  } catch {
    // sessionStorage may be unavailable in some browser privacy modes
  }
}

function removeStorage(key: string): void {
  try {
    sessionStorage.removeItem(key);
  } catch {
    // Ignore
  }
}

function clearLegacyLocalStorageToken(key: string): void {
  try {
    localStorage.removeItem(key);
  } catch {
    // Ignore
  }
}

/**
 * Retrieve the stored authentication token.
 */
export function getToken(): string | null {
  if (inMemoryToken && inMemoryToken.length > 0) {
    return inMemoryToken;
  }

  const sessionToken = readStorage(TOKEN_STORAGE_KEY);
  if (sessionToken && sessionToken.length > 0) {
    inMemoryToken = sessionToken;
    return sessionToken;
  }

  // One-time migration from older localStorage-backed sessions.
  try {
    const legacy = localStorage.getItem(TOKEN_STORAGE_KEY);
    if (legacy && legacy.length > 0) {
      inMemoryToken = legacy;
      writeStorage(TOKEN_STORAGE_KEY, legacy);
      localStorage.removeItem(TOKEN_STORAGE_KEY);
      return legacy;
    }
  } catch {
    // Ignore
  }

  return null;
}

/**
 * Store an authentication token.
 */
export function setToken(token: string): void {
  inMemoryToken = token;
  writeStorage(TOKEN_STORAGE_KEY, token);
  clearLegacyLocalStorageToken(TOKEN_STORAGE_KEY);
}

/**
 * Remove the stored authentication token.
 */
export function clearToken(): void {
  inMemoryToken = null;
  removeStorage(TOKEN_STORAGE_KEY);
  clearLegacyLocalStorageToken(TOKEN_STORAGE_KEY);
}

/**
 * Returns true if a token is currently stored.
 */
export function isAuthenticated(): boolean {
  const token = getToken();
  return token !== null && token.length > 0;
}
