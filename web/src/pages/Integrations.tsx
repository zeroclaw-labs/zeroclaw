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

/** Config destination for an integration, routed by its category: AI-model
 *  providers land under the model-providers section, chat platforms under
 *  channels, everything else on the config root (the navigator). */
function configHref(name: string, category: string): string {
  const slug = channelSlug(name);
  const c = category.toLowerCase();
  if (c.includes('model')) {
    return slug ? `/config/providers.models/${slug}` : '/config/providers.models';
  }
  if (c.includes('chat') || c.includes('channel')) {
    return slug ? `/config/channels/${slug}` : '/config/channels';
  }
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
                'px-3 h-7 inline-flex items-center rounded-[var(--radius-md)] text-[13px] font-medium capitalize transition-colors cursor-pointer border',
                active
                  ? 'bg-pc-accent border-transparent text-[#0b1220]'
                  : 'bg-transparent border-pc-border text-pc-text-secondary hover:bg-[var(--pc-hover)] hover:text-pc-text hover:border-pc-border-strong',
              ].join(' ')}
            >
              {cat}
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
            <h3 className="text-[11px] font-medium uppercase tracking-wider mb-3 capitalize text-pc-text-faint">
              {category}
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
                return (
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
                    <div className="flex items-center gap-1 text-[13px] font-medium text-pc-accent">
                      {ctaLabel}
                      <ArrowRight className="h-3.5 w-3.5 transition-transform group-hover:translate-x-0.5" />
                    </div>
                  </button>
                );
              })}
            </div>
          </div>
        ))
      )}
    </div>
  );
}
