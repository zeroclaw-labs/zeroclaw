import { useEffect, useState } from 'react'
import { Check, Clock, Puzzle, Zap } from 'lucide-react'
import type { Integration } from '@/types/api'
import { getIntegrations } from '@/lib/api'

function statusBadge(status: Integration['status']) {
  switch (status) {
    case 'Active':
      return {
        icon: Check,
        label: 'Active',
        color: 'var(--color-status-success)',
        borderColor: 'var(--color-status-success)',
        bg: 'var(--color-bg-success-subtle)',
      };
    case 'Available':
      return {
        icon: Zap,
        label: 'Available',
        color: 'var(--color-accent-blue)',
        borderColor: 'var(--color-accent-blue)',
        bg: 'var(--color-bg-blue-subtle)',
      };
    case 'ComingSoon':
      return {
        icon: Clock,
        label: 'Coming Soon',
        color: 'var(--color-text-muted)',
        borderColor: 'var(--color-border-default)',
        bg: 'var(--color-bg-muted-subtle)',
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

  const grouped = filtered.reduce<Record<string, Integration[]>>((acc, item) => {
    const key = item.category;
    if (!acc[key]) acc[key] = [];
    acc[key].push(item);
    return acc;
  }, {});

  if (error) {
    return (
      <div className="p-6 animate-fade-in">
        <div className="rounded-xl p-4" style={{ backgroundColor: 'var(--color-bg-error-subtle)', border: '1px solid var(--color-status-error)', color: 'var(--color-status-error)' }}>
          Failed to load integrations: {error}
        </div>
      </div>
    );
  }

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="h-8 w-8 border-2 rounded-full animate-spin" style={{ borderColor: 'var(--color-glow-blue)', borderTopColor: 'var(--color-accent-blue)' }} />
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6 animate-fade-in">
      <div className="flex items-center gap-2">
        <Puzzle className="h-5 w-5" style={{ color: 'var(--color-accent-blue)' }} />
        <h2 className="text-sm font-semibold uppercase tracking-wider" style={{ color: 'var(--color-text-primary)' }}>
          Integrations ({integrations.length})
        </h2>
      </div>

      <div className="flex flex-wrap gap-2">
        {categories.map((cat) => (
          <button
            key={cat}
            onClick={() => setActiveCategory(cat)}
            className="px-3.5 py-1.5 rounded-xl text-xs font-semibold transition-all duration-300 capitalize"
            style={activeCategory === cat ? 
              { background: 'linear-gradient(135deg, var(--color-accent-blue), var(--color-accent-blue-hover))', color: 'white', boxShadow: '0 0 15px var(--color-glow-blue)' } : 
              { color: 'var(--color-text-muted)', border: '1px solid var(--color-border-default)' }
            }
          >
            {cat}
          </button>
        ))}
      </div>

      {Object.keys(grouped).length === 0 ? (
        <div className="glass-card p-8 text-center">
          <Puzzle className="h-10 w-10 mx-auto mb-3" style={{ color: 'var(--color-border-default)' }} />
          <p style={{ color: 'var(--color-text-muted)' }}>No integrations found.</p>
        </div>
      ) : (
        Object.entries(grouped)
          .sort(([a], [b]) => a.localeCompare(b))
          .map(([category, items]) => (
            <div key={category}>
              <h3 className="text-xs font-semibold uppercase tracking-wider mb-3 capitalize" style={{ color: 'var(--color-text-muted)' }}>
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
                          <h4 className="text-sm font-semibold truncate" style={{ color: 'var(--color-text-primary)' }}>
                            {integration.name}
                          </h4>
                          <p className="text-sm mt-1 line-clamp-2" style={{ color: 'var(--color-text-muted)' }}>
                            {integration.description}
                          </p>
                        </div>
                        <span
                          className="flex-shrink-0 inline-flex items-center gap-1 px-2.5 py-1 rounded-full text-xs font-semibold border"
                          style={{ 
                            color: badge.color, 
                            borderColor: badge.borderColor, 
                            backgroundColor: badge.bg,
                          }}
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
