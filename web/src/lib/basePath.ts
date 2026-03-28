// Runtime base path injected by the Rust gateway into index.html.
// Allows the SPA to work under a reverse-proxy path prefix.
// When running inside Tauri, the frontend is served from disk so basePath is
// empty and API calls target the gateway URL directly.

import { isTauri, tauriGatewayUrl } from './tauri';

declare global {
  interface Window {
    __ZEROCLAW_BASE__?: string;
  }
}

/** Gateway path prefix (e.g. "/zeroclaw"), or empty string when served at root. */
export const basePath: string = isTauri()
  ? ''
  : (window.__ZEROCLAW_BASE__ ?? '').replace(/\/+$/, '');

/** Full origin for API requests. Empty when served by the gateway (same-origin). */
export const apiOrigin: string = isTauri() ? tauriGatewayUrl() : '';
