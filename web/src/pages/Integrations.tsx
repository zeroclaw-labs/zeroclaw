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
        classes: 'text-[#00e68a] border-[#00e68a30]',
        bg: 'rgba(0,230,138,0.06)',
      };
    case 'Available':
      return {
        icon: Zap,
        label: t('integrations.status_available'),
        classes: 'text-[#0080ff] border-[#0080ff30]',
        bg: 'rgba(0,128,255,0.06)',
      };
    case 'ComingSoon':
      return {
        icon: Clock,
        label: t('integrations.status_coming_soon'),
        classes: 'text-[#556080] border-[#1a1a3e]',
        bg: 'rgba(26,26,62,0.3)',
      };
  }
}

export default function Integrations() {
  const [integrations, setIntegrations] = useState<Integration[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [activeCategory, setActiveCategory] = useState<string>('all');

  useEffect(() => {
    getIntegrations()
      .then(setIntegrations)
      .catch((err) => setError(err.message))
      .finally(() => setLoading(false));
  }, []);

  const categories = [
    'all',
    ...Array.from(new Set(integrations.map((i) => i.category))).sort(),
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
        <div className="rounded-xl bg-[#ff446615] border border-[#ff446630] p-4 text-[#ff6680]">
          {t('integrations.load_error')}: {error}
        </div>
      </div>
    );
  }

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="h-8 w-8 border-2 border-[#0080ff30] border-t-[#0080ff] rounded-full animate-spin" />
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6 animate-fade-in">
      {/* Header */}
      <div className="flex items-center gap-2">
        <Puzzle className="h-5 w-5 text-[#0080ff]" />
        <h2 className="text-sm font-semibold text-white uppercase tracking-wider">
          {t('integrations.title')} ({integrations.length})
        </h2>
      </div>

      {/* Category Filter Tabs */}
      <div className="flex flex-wrap gap-2">
        {categories.map((cat) => (
          <button
            key={cat}
            onClick={() => setActiveCategory(cat)}
            className={`px-3.5 py-1.5 rounded-xl text-xs font-semibold transition-all duration-300 capitalize ${activeCategory === cat
                ? 'text-white shadow-[0_0_15px_rgba(0,128,255,0.2)]'
                : 'text-[#556080] border border-[#1a1a3e] hover:text-white hover:border-[#0080ff40]'
              }`}
            style={activeCategory === cat ? { background: 'linear-gradient(135deg, #0080ff, #0066cc)' } : {}}
          >
            {cat}
          </button>
        ))}
      </div>

      {/* Grouped Integration Cards */}
      {Object.keys(grouped).length === 0 ? (
        <div className="glass-card p-8 text-center">
          <Puzzle className="h-10 w-10 text-[#1a1a3e] mx-auto mb-3" />
          <p className="text-[#556080]">{t('integrations.empty')}</p>
        </div>
      ) : (
        Object.entries(grouped)
          .sort(([a], [b]) => a.localeCompare(b))
          .map(([category, items]) => (
            <div key={category}>
              <h3 className="text-[10px] font-semibold text-[#334060] uppercase tracking-wider mb-3 capitalize">
                {category}
              </h3>
              <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-4 stagger-children">
                {items.map((integration) => {
                  const badge = statusBadge(integration.status);
                  const BadgeIcon = badge.icon;
                  return (
                    <div
                      key={integration.name}
                      className="glass-card p-5 animate-slide-in-up"
                    >
                      <div className="flex items-start justify-between gap-3">
                        <div className="min-w-0">
                          <h4 className="text-sm font-semibold text-white truncate">
                            {integration.name}
                          </h4>
                          <p className="text-sm text-[#556080] mt-1 line-clamp-2">
                            {integration.description}
                          </p>
                        </div>
                        <span
                          className={`flex-shrink-0 inline-flex items-center gap-1 px-2.5 py-1 rounded-full text-[10px] font-semibold border ${badge.classes}`}
                          style={{ background: badge.bg }}
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
