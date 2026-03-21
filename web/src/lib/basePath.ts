// Runtime base path injected by the Rust gateway into index.html.
// Allows the SPA to work under a reverse-proxy path prefix.

declare global {
  interface Window {
    __ZEROCLAW_BASE__?: string;
  }
}

/** Gateway path prefix (e.g. "/zeroclaw"), or empty string when served at root. */
export const basePath: string = (window.__ZEROCLAW_BASE__ ?? '').replace(/\/+$/, '');
