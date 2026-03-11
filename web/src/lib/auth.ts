export const TOKEN_STORAGE_KEY = 'zeroclaw_token';
let inMemoryToken: string | null = null;

function readPersistentStorage(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function writePersistentStorage(key: string, value: string): boolean {
  try {
    localStorage.setItem(key, value);
    return true;
  } catch {
    return false;
  }
}

function removePersistentStorage(key: string): void {
  try {
    localStorage.removeItem(key);
  } catch {
    // Ignore
  }
}

function readSessionFallback(key: string): string | null {
  try {
    return sessionStorage.getItem(key);
  } catch {
    return null;
  }
}

function writeSessionFallback(key: string, value: string): void {
  try {
    sessionStorage.setItem(key, value);
  } catch {
    // sessionStorage may be unavailable in some browser privacy modes
  }
}

function removeSessionFallback(key: string): void {
  try {
    sessionStorage.removeItem(key);
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

  const persistedToken = readPersistentStorage(TOKEN_STORAGE_KEY);
  if (persistedToken && persistedToken.length > 0) {
    inMemoryToken = persistedToken;
    return persistedToken;
  }

  // Migrate session-only pairings created by previous dashboard builds into
  // persistent storage so pairing survives a browser restart.
  const sessionToken = readSessionFallback(TOKEN_STORAGE_KEY);
  if (sessionToken && sessionToken.length > 0) {
    inMemoryToken = sessionToken;
    if (writePersistentStorage(TOKEN_STORAGE_KEY, sessionToken)) {
      removeSessionFallback(TOKEN_STORAGE_KEY);
    }
    return sessionToken;
  }

  return null;
}

/**
 * Store an authentication token.
 */
export function setToken(token: string): void {
  inMemoryToken = token;
  if (!writePersistentStorage(TOKEN_STORAGE_KEY, token)) {
    writeSessionFallback(TOKEN_STORAGE_KEY, token);
    return;
  }
  removeSessionFallback(TOKEN_STORAGE_KEY);
}

/**
 * Remove the stored authentication token.
 */
export function clearToken(): void {
  inMemoryToken = null;
  removePersistentStorage(TOKEN_STORAGE_KEY);
  removeSessionFallback(TOKEN_STORAGE_KEY);
}

/**
 * Returns true if a token is currently stored.
 */
export function isAuthenticated(): boolean {
  const token = getToken();
  return token !== null && token.length > 0;
}
