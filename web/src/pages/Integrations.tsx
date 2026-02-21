import { useState, useEffect } from 'react';
import { Puzzle, Check, Zap, Clock } from 'lucide-react';
import type { Integration } from '@/types/api';
import { getIntegrations } from '@/lib/api';

function statusBadge(status: Integration['status']) {
  switch (status) {
    case 'Active':
      return {
        icon: Check,
        label: 'Active',
        classes: 'bg-green-900/40 text-green-400 border-green-700/50',
      };
    case 'Available':
      return {
        icon: Zap,
        label: 'Available',
        classes: 'bg-blue-900/40 text-blue-400 border-blue-700/50',
      };
    case 'ComingSoon':
      return {
        icon: Clock,
        label: 'Coming Soon',
        classes: 'bg-gray-800 text-gray-400 border-gray-700',
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
      <div className="p-6">
        <div className="rounded-lg bg-red-900/30 border border-red-700 p-4 text-red-300">
          Failed to load integrations: {error}
        </div>
      </div>
    );
  }

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="animate-spin rounded-full h-8 w-8 border-2 border-blue-500 border-t-transparent" />
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6">
      {/* Header */}
      <div className="flex items-center gap-2">
        <Puzzle className="h-5 w-5 text-blue-400" />
        <h2 className="text-base font-semibold text-white">
          Integrations ({integrations.length})
        </h2>
      </div>

      {/* Category Filter Tabs */}
      <div className="flex flex-wrap gap-2">
        {categories.map((cat) => (
          <button
            key={cat}
            onClick={() => setActiveCategory(cat)}
            className={`px-3 py-1.5 rounded-lg text-sm font-medium transition-colors capitalize ${
              activeCategory === cat
                ? 'bg-blue-600 text-white'
                : 'bg-gray-900 text-gray-400 border border-gray-700 hover:bg-gray-800 hover:text-white'
            }`}
          >
            {cat}
          </button>
        ))}
      </div>

      {/* Grouped Integration Cards */}
      {Object.keys(grouped).length === 0 ? (
        <div className="bg-gray-900 rounded-xl border border-gray-800 p-8 text-center">
          <Puzzle className="h-10 w-10 text-gray-600 mx-auto mb-3" />
          <p className="text-gray-400">No integrations found.</p>
        </div>
      ) : (
        Object.entries(grouped)
          .sort(([a], [b]) => a.localeCompare(b))
          .map(([category, items]) => (
            <div key={category}>
              <h3 className="text-sm font-semibold text-gray-400 uppercase tracking-wider mb-3 capitalize">
                {category}
              </h3>
              <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-4">
                {items.map((integration) => {
                  const badge = statusBadge(integration.status);
                  const BadgeIcon = badge.icon;
                  return (
                    <div
                      key={integration.name}
                      className="bg-gray-900 rounded-xl border border-gray-800 p-5 hover:border-gray-700 transition-colors"
                    >
                      <div className="flex items-start justify-between gap-3">
                        <div className="min-w-0">
                          <h4 className="text-sm font-semibold text-white truncate">
                            {integration.name}
                          </h4>
                          <p className="text-sm text-gray-400 mt-1 line-clamp-2">
                            {integration.description}
                          </p>
                        </div>
                        <span
                          className={`flex-shrink-0 inline-flex items-center gap-1 px-2 py-1 rounded-full text-xs font-medium border ${badge.classes}`}
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
