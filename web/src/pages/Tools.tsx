import { useState, useEffect } from 'react';
import {
  Wrench,
  Search,
  ChevronDown,
  ChevronRight,
  Terminal,
  Package,
} from 'lucide-react';
import type { ToolSpec, CliTool } from '@/types/api';
import { getTools, getCliTools } from '@/lib/api';
import { t } from '@/lib/i18n';

export default function Tools() {
  const [tools, setTools] = useState<ToolSpec[]>([]);
  const [cliTools, setCliTools] = useState<CliTool[]>([]);
  const [search, setSearch] = useState('');
  const [expandedTool, setExpandedTool] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    Promise.all([getTools(), getCliTools()])
      .then(([t, c]) => { setTools(t); setCliTools(c); })
      .catch((err) => setError(err.message))
      .finally(() => setLoading(false));
  }, []);

  const filtered = tools.filter((t) =>
    t.name.toLowerCase().includes(search.toLowerCase()) ||
    t.description.toLowerCase().includes(search.toLowerCase()),
  );

  const filteredCli = cliTools.filter((t) =>
    t.name.toLowerCase().includes(search.toLowerCase()) ||
    t.category.toLowerCase().includes(search.toLowerCase()),
  );

  if (error) {
    return (
      <div className="p-6 animate-fade-in">
        <div className="rounded-2xl border p-4" style={{ background: 'rgba(239, 68, 68, 0.08)', borderColor: 'rgba(239, 68, 68, 0.2)', color: '#f87171' }}>
          {t('tools.load_error')}: {error}
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
      {/* Search */}
      <div className="relative max-w-md">
        <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4" style={{ color: 'var(--pc-text-faint)' }} />
        <input
          type="text"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder={t('tools.search')}
          className="input-electric w-full pl-10 pr-4 py-2.5 text-sm"
        />
      </div>

      {/* Agent Tools Grid */}
      <div>
        <div className="flex items-center gap-2 mb-4">
          <Wrench className="h-5 w-5" style={{ color: 'var(--pc-accent)' }} />
          <h2 className="text-sm font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-primary)' }}>
            {t('tools.agent_tools')} ({filtered.length})
          </h2>
        </div>

        {filtered.length === 0 ? (
          <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>{t('tools.empty')}</p>
        ) : (
          <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-4 stagger-children">
            {filtered.map((tool) => {
              const isExpanded = expandedTool === tool.name;
              return (
                <div
                  key={tool.name}
                  className="card overflow-hidden animate-slide-in-up"
                >
                  <button
                    onClick={() => setExpandedTool(isExpanded ? null : tool.name)}
                    className="w-full text-left p-4 transition-all"
                    style={{ background: 'transparent' }}
                    onMouseEnter={(e) => { e.currentTarget.style.background = 'var(--pc-hover)'; }}
                    onMouseLeave={(e) => { e.currentTarget.style.background = 'transparent'; }}
                  >
                    <div className="flex items-start justify-between gap-2">
                      <div className="flex items-center gap-2 min-w-0">
                        <Package className="h-4 w-4 flex-shrink-0" style={{ color: 'var(--pc-accent)' }} />
                        <h3 className="text-sm font-semibold truncate" style={{ color: 'var(--pc-text-primary)' }}>{tool.name}</h3>
                      </div>
                      {isExpanded
                        ? <ChevronDown className="h-4 w-4 flex-shrink-0" style={{ color: 'var(--pc-accent)' }} />
                        : <ChevronRight className="h-4 w-4 flex-shrink-0" style={{ color: 'var(--pc-text-faint)' }} />
                      }
                    </div>
                    <p className="text-sm mt-2 line-clamp-2" style={{ color: 'var(--pc-text-muted)' }}>
                      {tool.description}
                    </p>
                  </button>

                  {isExpanded && tool.parameters && (
                    <div className="border-t p-4 animate-fade-in" style={{ borderColor: 'var(--pc-border)' }}>
                      <p className="text-[10px] font-semibold uppercase tracking-wider mb-2" style={{ color: 'var(--pc-text-muted)' }}>
                        {t('tools.parameter_schema')}
                      </p>
                      <pre className="text-xs rounded-xl p-3 overflow-x-auto max-h-64 overflow-y-auto font-mono" style={{ background: 'var(--pc-bg-base)', color: 'var(--pc-text-secondary)' }}>
                        {JSON.stringify(tool.parameters, null, 2)}
                      </pre>
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </div>

      {/* CLI Tools Section */}
      {filteredCli.length > 0 && (
        <div className="animate-slide-in-up" style={{ animationDelay: '200ms' }}>
          <div className="flex items-center gap-2 mb-4">
            <Terminal className="h-5 w-5" style={{ color: 'var(--color-status-success)' }} />
            <h2 className="text-sm font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-primary)' }}>
              {t('tools.cli_tools')} ({filteredCli.length})
            </h2>
          </div>

          <div className="card overflow-hidden rounded-2xl">
            <table className="table-electric">
              <thead>
                <tr>
                  <th>{t('tools.name')}</th>
                  <th>{t('tools.path')}</th>
                  <th>{t('tools.version')}</th>
                  <th>{t('tools.category')}</th>
                </tr>
              </thead>
              <tbody>
                {filteredCli.map((tool) => (
                  <tr key={tool.name}>
                    <td className="font-medium text-sm" style={{ color: 'var(--pc-text-primary)' }}>
                      {tool.name}
                    </td>
                    <td className="font-mono text-xs truncate max-w-[200px]" style={{ color: 'var(--pc-text-muted)' }}>
                      {tool.path}
                    </td>
                    <td style={{ color: 'var(--pc-text-muted)' }}>
                      {tool.version ?? '-'}
                    </td>
                    <td>
                      <span className="inline-flex items-center px-2.5 py-0.5 rounded-full text-[10px] font-semibold capitalize border" style={{ borderColor: 'var(--pc-border)', color: 'var(--pc-text-secondary)', background: 'var(--pc-accent-glow)' }}>
                        {tool.category}
                      </span>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}
    </div>
  );
}
