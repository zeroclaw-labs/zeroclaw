import { useState, useEffect } from 'react';
import { Puzzle, Check, Zap, Clock } from 'lucide-react';
import type { Integration } from '@/types/api';
import { getIntegrations } from '@/lib/api';
import { t } from '@/lib/i18n';

function statusBadge(status: Integration['status']) {
  switch (status) {
    case 'Active':
      return {
        icon: Check,
        label: t('integrations.status_active'),
        color: 'var(--color-status-success)',
        border: 'rgba(0, 230, 138, 0.2)',
        bg: 'rgba(0, 230, 138, 0.06)'
      };
    case 'Available':
      return {
        icon: Zap,
        label: t('integrations.status_available'),
        color: 'var(--pc-accent)',
        border: 'var(--pc-accent-dim)',
        bg: 'var(--pc-accent-glow)'
      };
    case 'ComingSoon':
      return {
        icon: Clock,
        label: t('integrations.status_coming_soon'),
        color: 'var(--pc-text-muted)',
        border: 'var(--pc-border)',
        bg: 'transparent'
      };
  }
}

export default function Integrations() {
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
    <div className="p-6 animate-fade-in">
      <div className="rounded-2xl border p-4" style={{ background: 'rgba(239, 68, 68, 0.08)', borderColor: 'rgba(239, 68, 68, 0.2)', color: '#f87171' }}>
        {t('integrations.load_error')}: {error}
      </div>
    </div>
  );
  }

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="h-8 w-8 border-2 rounded-full animate-spin" style={{ borderColor: 'var(--pc-border)', borderTopColor: 'var(--pc-accent)' }} />
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6 animate-fade-in">
      <div className="flex items-center gap-2">
        <Puzzle className="h-5 w-5" style={{ color: 'var(--pc-accent)' }} />
        <h2 className="text-sm font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-primary)' }}>
          {t('integrations.title')} ({integrations.length})
        </h2>
      </div>

      {/* Category Filter Tabs */}
      <div className="flex flex-wrap gap-2">
        {categories.map((cat) => (
          <button
            key={cat}
            onClick={() => setActiveCategory(cat)}
            className="px-3.5 py-1.5 rounded-xl text-xs font-semibold transition-all capitalize"
            style={activeCategory === cat
              ? { background: 'var(--pc-accent)', color: 'white' }
              : { color: 'var(--pc-text-muted)', border: '1px solid var(--pc-border)', background: 'transparent' }
            }
          >
            {cat}
          </button>
        ))}
      </div>

      {/* Grouped Integration Cards */}
      {Object.keys(grouped).length === 0 ? (
        <div className="card p-8 text-center">
          <Puzzle className="h-10 w-10 mx-auto mb-3" style={{ color: 'var(--pc-text-faint)' }} />
          <p style={{ color: 'var(--pc-text-muted)' }}>{t('integrations.empty')}</p>
        </div>
      ) : (
        Object.entries(grouped).sort(([a], [b]) => a.localeCompare(b)).map(([category, items]) => (
          <div key={category}>
            <h3 className="text-[10px] font-semibold uppercase tracking-wider mb-3 capitalize" style={{ color: 'var(--pc-text-faint)' }}>
              {category}
            </h3>
            <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-4 stagger-children">
              {items.map((integration) => {
                const badge = statusBadge(integration.status);
                const BadgeIcon = badge.icon;
                return (
                  <div
                    key={integration.name}
                    className="card p-5 animate-slide-in-up"
                  >
                    <div className="flex items-start justify-between gap-3">
                      <div className="min-w-0">
                        <h4 className="text-sm font-semibold truncate" style={{ color: 'var(--pc-text-primary)' }}>
                          {integration.name}
                        </h4>
                        <p className="text-sm mt-1 line-clamp-2" style={{ color: 'var(--pc-text-muted)' }}>
                          {integration.description}
                        </p>
                      </div>
                      <span
                        className="flex-shrink-0 inline-flex items-center gap-1 px-2.5 py-1 rounded-full text-[10px] font-semibold border"
                        style={badge}
                      >
                        <BadgeIcon className="h-3 w-3" />
                        {badge.label}
                      </span>
                    </div>
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
