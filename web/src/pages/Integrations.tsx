import { useState, useEffect } from 'react';
import { useNavigate } from 'react-router-dom';
import { Puzzle, Check, Zap, ArrowRight } from 'lucide-react';
import type { Integration } from '@/types/api';
import { getIntegrations } from '@/lib/api';
import { t } from '@/lib/i18n';
import { Badge, Card, PageHeader } from '@/components/ui';
import type { BadgeTone } from '@/components/ui';

/**
 * Derive a channel-type slug from an integration's display name so a card can
 * link into the schema-driven Channels config. Lower-cased, trimmed, with runs
 * of non-alphanumerics collapsed to a single hyphen and edges trimmed.
 * Returns `null` when nothing slug-worthy remains, signalling the caller to
 * fall back to the bare Channels section.
 */
function channelSlug(name: string): string | null {
  const slug = name
    .toLowerCase()
    .trim()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '');
  return slug.length > 0 ? slug : null;
}

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
 *  on the bare /config root. AI-model providers go to the model-providers
 *  section, chat platforms to channels, built-in tools to the Tools page
 *  (allow/block per risk profile), and Cron (a config-backed automation) to its
 *  own page. */
function configHref(name: string, category: string): string | null {
  const c = category.toLowerCase();
  // Compile-time OS facts (macOS/Linux/Windows) — nothing to configure.
  if (c === 'platform') return null;

  const slug = channelSlug(name);
  if (c.includes('model')) {
    return slug ? `/config/providers.models/${slug}` : '/config/providers.models';
  }
  if (c.includes('chat') || c.includes('channel')) {
    return slug ? `/config/channels/${slug}` : '/config/channels';
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

function statusBadge(status: Integration['status']) {
  switch (status) {
    case 'Active':
      return {
        icon: Check,
        label: t('integrations.status_active'),
        tone: 'ok' as BadgeTone,
      };
    case 'Available':
      return {
        icon: Zap,
        label: t('integrations.status_available'),
        tone: 'neutral' as BadgeTone,
      };
    // `status` is the declared union, but the value comes from unvalidated
    // backend JSON — any drift (a new state, lowercase 'active', whitespace)
    // would otherwise return undefined and crash the caller's badge.icon /
    // badge.tone deref inside .map. Fall back to a neutral badge that echoes
    // the raw status so an unknown state still renders something meaningful.
    default:
      return {
        icon: Puzzle,
        label: String(status),
        tone: 'neutral' as BadgeTone,
      };
  }
}

// Display labels for the integration `category` enum, keyed by the stable
// enum-variant value the API emits (Chat / AiModel / ToolsAutomation /
// Platform). Routed through t() at the call site so the label localizes;
// unknown/future variants fall back to the API's derived display label, then
// the raw key. Values mirror the backend IntegrationCategory::label().
const CATEGORY_LABEL_KEYS: Record<string, string> = {
  Chat: 'integrations.cat_chat',
  AiModel: 'integrations.cat_ai_model',
  ToolsAutomation: 'integrations.cat_tools_automation',
  Platform: 'integrations.cat_platform',
};

export default function Integrations() {
  const navigate = useNavigate();
  const [integrations, setIntegrations] = useState<Integration[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [activeCategory, setActiveCategory] = useState<string>('all');

  useEffect(() => {
    getIntegrations().then(setIntegrations).catch((err) => setError(err.message)).finally(() => setLoading(false));
  }, []);

  const categories = ['all',
    ...Array.from(new Set(integrations.map((i) => i.category))).sort()
  ];
  const filtered =
    activeCategory === 'all'
      ? integrations
      : integrations.filter((i) => i.category === activeCategory);

  // Group by category for display
  const grouped = filtered.reduce<Record<string, Integration[]>>((acc, item) => {
    const key = item.category;
    if (!acc[key]) acc[key] = [];
    acc[key].push(item);
    return acc;
  }, {});

  // Resolve a category enum key to its human label. 'all' is the synthetic
  // filter pseudo-category; known keys localize in the web bundle, and future
  // API keys can still render their backend-derived label.
  const labelFor = (cat: string): string => {
    if (cat === 'all') return t('integrations.cat_all');
    const key = CATEGORY_LABEL_KEYS[cat];
    if (key) return t(key);
    return integrations.find((integration) => integration.category === cat)?.category_label ?? cat;
  };

  if (error) {
    return (
      <div className="p-6">
        <div className="rounded-[var(--radius-md)] border border-status-error/25 bg-status-error/10 p-4 text-sm text-status-error">
          {t('integrations.load_error')}: {error}
        </div>
      </div>
    );
  }

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="h-8 w-8 border-2 rounded-full animate-spin border-pc-border" style={{ borderTopColor: 'var(--pc-accent)' }} />
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6">
      <PageHeader
        title={t('integrations.title')}
        description={t('integrations.subtitle')}
        actions={<Badge tone="neutral">{integrations.length}</Badge>}
      />

      {/* Category Filter Tabs */}
      <div className="flex flex-wrap gap-2">
        {categories.map((cat) => {
          const active = activeCategory === cat;
          return (
            <button
              key={cat}
              type="button"
              onClick={() => setActiveCategory(cat)}
              className={[
                'px-3 h-7 inline-flex items-center rounded-[var(--radius-md)] text-[13px] font-medium transition-colors cursor-pointer border',
                active
                  ? 'bg-pc-accent border-transparent text-[#0b1220]'
                  : 'bg-transparent border-pc-border text-pc-text-secondary hover:bg-[var(--pc-hover)] hover:text-pc-text hover:border-pc-border-strong',
              ].join(' ')}
            >
              {labelFor(cat)}
            </button>
          );
        })}
      </div>

      {/* Grouped Integration Cards */}
      {Object.keys(grouped).length === 0 ? (
        <Card className="p-10 text-center">
          <Puzzle className="h-10 w-10 mx-auto mb-3 text-pc-text-faint" />
          <p className="text-sm text-pc-text-muted">{t('integrations.empty')}</p>
        </Card>
      ) : (
        Object.entries(grouped).sort(([a], [b]) => a.localeCompare(b)).map(([category, items]) => (
          <div key={category}>
            <h3 className="text-[11px] font-medium uppercase tracking-wider mb-3 text-pc-text-faint">
              {labelFor(category)}
            </h3>
            <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-3">
              {items.map((integration) => {
                const badge = statusBadge(integration.status);
                const BadgeIcon = badge.icon;
                const href = configHref(integration.name, integration.category);
                const ctaLabel =
                  integration.status === 'Active'
                    ? t('integrations.configure')
                    : t('integrations.set_up');
                const body = (
                  <>
                    <div className="flex items-start justify-between gap-3">
                      <div className="min-w-0">
                        <h4 className="text-sm font-medium truncate text-pc-text">
                          {integration.name}
                        </h4>
                        <p className="text-sm mt-1 line-clamp-2 text-pc-text-muted">
                          {integration.description}
                        </p>
                      </div>
                      <Badge tone={badge.tone} className="flex-shrink-0">
                        <BadgeIcon className="h-3 w-3" />
                        {badge.label}
                      </Badge>
                    </div>
                    {href && (
                      <div className="flex items-center gap-1 text-[13px] font-medium text-pc-accent">
                        {ctaLabel}
                        <ArrowRight className="h-3.5 w-3.5 transition-transform group-hover:translate-x-0.5" />
                      </div>
                    )}
                  </>
                );
                // Configurable integrations are launcher buttons; the rest
                // (Platform/OS facts) render as inert status tiles.
                return href ? (
                  <button
                    key={integration.name}
                    type="button"
                    onClick={() => navigate(href)}
                    aria-label={`${ctaLabel}: ${integration.name}`}
                    className={[
                      'group p-5 w-full text-left flex flex-col gap-3 cursor-pointer',
                      'bg-pc-surface border border-pc-border rounded-[var(--radius-lg)]',
                      'transition-colors hover:bg-[var(--pc-hover)] hover:border-pc-border-strong',
                      'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)]',
                      'focus-visible:ring-offset-2 focus-visible:ring-offset-pc-base',
                    ].join(' ')}
                  >
                    {body}
                  </button>
                ) : (
                  <div
                    key={integration.name}
                    className="p-5 w-full text-left flex flex-col gap-3 bg-pc-surface border border-pc-border rounded-[var(--radius-lg)]"
                  >
                    {body}
                  </div>
                );
              })}
            </div>
          </div>
        ))
      )}
    </div>
  );
}
