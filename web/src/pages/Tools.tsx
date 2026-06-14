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
import { Badge, Card, PageHeader } from '@/components/ui';

export default function Tools() {
  const [tools, setTools] = useState<ToolSpec[]>([]);
  const [cliTools, setCliTools] = useState<CliTool[]>([]);
  const [search, setSearch] = useState('');
  const [expandedTool, setExpandedTool] = useState<string | null>(null);
  const [agentSectionOpen, setAgentSectionOpen] = useState(true);
  const [cliSectionOpen, setCliSectionOpen] = useState(true);
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
      <div className="p-6">
        <div className="rounded-[var(--radius-md)] border border-status-error/25 bg-status-error/10 p-4 text-sm text-status-error">
          {t('tools.load_error')}: {error}
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
        title={t('tools.title')}
        actions={
          <div className="relative w-64 max-w-full">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-pc-text-faint pointer-events-none" />
            <input
              type="text"
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              placeholder={t('tools.search')}
              className="w-full h-9 pl-9 pr-3 text-sm rounded-[var(--radius-md)] border border-pc-border bg-pc-input text-pc-text placeholder:text-pc-text-faint transition-colors focus:outline-none focus:border-pc-border-strong focus:ring-2 focus:ring-[var(--pc-focus)]/30"
            />
          </div>
        }
      />

      {/* Agent Tools Grid */}
      <section>
        <button
          onClick={() => setAgentSectionOpen((v) => !v)}
          type="button"
          className="flex items-center gap-2 mb-4 w-full text-left group cursor-pointer"
          aria-expanded={agentSectionOpen}
          aria-controls="agent-tools-section"
        >
          <Wrench className="h-4 w-4 text-pc-accent" />
          <span className="text-xs font-semibold uppercase tracking-wider flex-1 text-pc-text-secondary" role="heading" aria-level={2}>
            {t('tools.agent_tools')}
          </span>
          <Badge tone="neutral">{filtered.length}</Badge>
          <ChevronDown
            className="h-4 w-4 text-pc-text-muted transition-transform"
            style={{ transform: agentSectionOpen ? 'rotate(0deg)' : 'rotate(-90deg)' }}
          />
        </button>

        <div id="agent-tools-section">
          {agentSectionOpen && (filtered.length === 0 ? (
            <p className="text-sm text-pc-text-muted">{t('tools.empty')}</p>
          ) : (
            <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-3">
              {filtered.map((tool) => {
                const isExpanded = expandedTool === tool.name;
                return (
                  <Card key={tool.name} padded={false} className="overflow-hidden">
                    <button
                      onClick={() => setExpandedTool(isExpanded ? null : tool.name)}
                      type="button"
                      className="w-full text-left p-4 h-full transition-colors hover:bg-pc-elevated/50 cursor-pointer"
                    >
                      <div className="flex items-start justify-between gap-2">
                        <div className="flex items-center gap-2 min-w-0">
                          <Package className="h-4 w-4 flex-shrink-0 text-pc-text-muted" />
                          <h3 className="text-sm font-medium truncate text-pc-text">{tool.name}</h3>
                        </div>
                        {isExpanded
                          ? <ChevronDown className="h-4 w-4 flex-shrink-0 text-pc-text-muted" />
                          : <ChevronRight className="h-4 w-4 flex-shrink-0 text-pc-text-faint" />
                        }
                      </div>
                      <p className="text-sm mt-2 line-clamp-2 text-pc-text-muted">
                        {tool.description}
                      </p>
                    </button>

                    {isExpanded && tool.parameters && (
                      <div className="border-t border-pc-border p-4">
                        <p className="text-[10px] font-semibold uppercase tracking-wider mb-2 text-pc-text-faint">
                          {t('tools.parameter_schema')}
                        </p>
                        <pre className="text-xs rounded-[var(--radius-md)] p-3 overflow-x-auto max-h-64 overflow-y-auto font-mono bg-pc-code text-pc-text-secondary">
                          {JSON.stringify(tool.parameters, null, 2)}
                        </pre>
                      </div>
                    )}
                  </Card>
                );
              })}
            </div>
          ))}
        </div>
      </section>

      {/* CLI Tools Section */}
      {filteredCli.length > 0 && (
        <section>
          <button
            onClick={() => setCliSectionOpen((v) => !v)}
            type="button"
            className="flex items-center gap-2 mb-4 w-full text-left group cursor-pointer"
            aria-expanded={cliSectionOpen}
            aria-controls="cli-tools-section"
          >
            <Terminal className="h-4 w-4 text-pc-text-muted" />
            <span className="text-xs font-semibold uppercase tracking-wider flex-1 text-pc-text-secondary" role="heading" aria-level={2}>
              {t('tools.cli_tools')}
            </span>
            <Badge tone="neutral">{filteredCli.length}</Badge>
            <ChevronDown
              className="h-4 w-4 text-pc-text-muted transition-transform"
              style={{ transform: cliSectionOpen ? 'rotate(0deg)' : 'rotate(-90deg)' }}
            />
          </button>

          <div id="cli-tools-section">
            {cliSectionOpen && (
              <Card padded={false} className="overflow-hidden">
                <div className="overflow-x-auto">
                  <table className="w-full text-sm border-collapse">
                    <thead>
                      <tr className="border-b border-pc-border text-left text-[11px] font-medium uppercase tracking-wider text-pc-text-faint">
                        <th className="px-4 py-2.5 font-medium">{t('tools.name')}</th>
                        <th className="px-4 py-2.5 font-medium">{t('tools.path')}</th>
                        <th className="px-4 py-2.5 font-medium">{t('tools.version')}</th>
                        <th className="px-4 py-2.5 font-medium">{t('tools.category')}</th>
                      </tr>
                    </thead>
                    <tbody>
                      {filteredCli.map((tool) => (
                        <tr key={tool.name} className="border-b border-pc-border/60 last:border-0">
                          <td className="px-4 py-2.5 font-medium text-pc-text">
                            {tool.name}
                          </td>
                          <td className="px-4 py-2.5 font-mono text-xs truncate max-w-[200px] text-pc-text-muted">
                            {tool.path}
                          </td>
                          <td className="px-4 py-2.5 text-pc-text-muted">
                            {tool.version ?? '-'}
                          </td>
                          <td className="px-4 py-2.5">
                            <Badge tone="neutral" className="capitalize">{tool.category}</Badge>
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              </Card>
            )}
          </div>
        </section>
      )}
    </div>
  );
}
