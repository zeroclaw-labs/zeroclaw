// Which dashboard pages the current caller can use.
//
// A paired browser (or an open gateway) reaches everything, as before. A
// signed-in account reaches what its session's `features` allow: operator
// pages only with `operator_pages`, chat only with `chat`, and the config
// pages always (the gateway checks each config path against the account's
// grants). The ACP console needs a paired browser: the gateway refuses
// dashboard sessions there.

import type { OidcSession } from './auth';

export interface NavAccess {
  operatorPages: boolean;
  chat: boolean;
  /** Pages that only accept a pairing token. */
  pairedBrowserPages: boolean;
}

export const FULL_ACCESS: NavAccess = {
  operatorPages: true,
  chat: true,
  pairedBrowserPages: true,
};

export function navAccessFor(session: OidcSession | null): NavAccess {
  if (!session) return FULL_ACCESS;
  return {
    operatorPages: session.features.operator_pages,
    chat: session.features.chat,
    pairedBrowserPages: false,
  };
}

export type PathAccess = 'allowed' | 'not-available' | 'needs-pairing';

function underPath(pathname: string, prefix: string): boolean {
  return pathname === prefix || pathname.startsWith(`${prefix}/`);
}

const PAIRED_BROWSER_PATHS = ['/acp-console'];
const CONFIG_PATHS = ['/config', '/setup'];

/** Chat: the agent list and an agent's chat, not its workspace files. */
function isChatPath(pathname: string): boolean {
  if (pathname === '/agents' || pathname === '/agent') return true;
  const segments = pathname.split('/').filter(Boolean);
  return segments.length === 2 && segments[0] === 'agent';
}

export function pathAccess(rawPathname: string, access: NavAccess): PathAccess {
  const pathname = rawPathname.toLowerCase().replace(/\/+$/, '') || '/';
  if (PAIRED_BROWSER_PATHS.some((p) => underPath(pathname, p))) {
    return access.pairedBrowserPages ? 'allowed' : 'needs-pairing';
  }
  if (access.operatorPages) return 'allowed';
  if (CONFIG_PATHS.some((p) => underPath(pathname, p))) return 'allowed';
  if (isChatPath(pathname)) return access.chat ? 'allowed' : 'not-available';
  return 'not-available';
}

/** Where `/` leads for an account without the operator dashboard. */
export function homePathFor(access: NavAccess): string {
  if (access.operatorPages) return '/';
  return access.chat ? '/agents' : '/config';
}
