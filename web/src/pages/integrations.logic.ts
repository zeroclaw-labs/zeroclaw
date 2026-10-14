// Pure routing helpers for the Integrations page, split out of the component
// so the config deep-link contract stays unit-testable without a renderer
// (same pattern as runs.logic.ts).

// Config-backed automations in the ToolsAutomation bucket that live under a
// schema [section] (or a dedicated page) rather than on the Tools page. Keyed
// by integration display name (lower-cased); this is exactly the set surfaced
// by Config::integration_descriptors(). Everything else in the bucket is a
// built-in tool, which the Tools page manages.
//
// MAINTENANCE: keys mirror the descriptor `display_name`s and values mirror the
// schema `#[prefix]` section keys (crates/zeroclaw-config/src/schema.rs:
// Browser→`browser`, "Google Workspace"→`google_workspace`; Cron→the /cron
// page). Renaming either in the schema requires updating this table, or the
// deep-link silently falls back to /tools.
const TOOLS_AUTOMATION_ROUTES: Record<string, string> = {
  cron: '/cron',
  browser: '/config/browser',
  'google workspace': '/config/google_workspace',
};

/** Where an integration's "Configure / Set up" CTA should land, routed by
 *  category. Returns null when the integration isn't configurable — Platform
 *  entries (macOS / Linux / Windows) are compile-time OS facts with nothing to
 *  set up — so the card renders as an inert status tile instead of dead-ending
 *  on the bare /config root. Entries backed by a schema config slot carry the
 *  API's canonical `key` (model-provider family key, ChannelsConfig map key)
 *  and deep-link to that exact section; a display-name slug cannot be made
 *  reliable (`Z.AI` → `z-ai` vs slot `zai`), so entries without a key fall
 *  back to the bare section instead. Built-in tools go to the Tools page
 *  (allow/block per risk profile), and Cron (a config-backed automation) to
 *  its own page. */
export function configHref(
  name: string,
  category: string,
  key: string | null | undefined,
): string | null {
  const c = category.toLowerCase();
  // Compile-time OS facts (macOS/Linux/Windows) — nothing to configure.
  if (c === 'platform') return null;

  if (c.includes('model')) {
    return key ? `/config/providers.models/${key}` : '/config/providers.models';
  }
  if (c.includes('chat') || c.includes('channel')) {
    return key ? `/config/channels/${key}` : '/config/channels';
  }
  if (c.includes('tool') || c.includes('automation')) {
    // Config-backed automations (Cron, Browser, Google Workspace) deep-link to
    // their own config; every other entry here is a built-in tool managed on
    // the Tools page.
    return TOOLS_AUTOMATION_ROUTES[name.trim().toLowerCase()] ?? '/tools';
  }
  // Unknown / future category — the config root still beats a broken link.
  return '/config';
}
