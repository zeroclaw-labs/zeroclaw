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

export default function Tools() {
  const [tools, setTools] = useState<ToolSpec[]>([]);
  const [cliTools, setCliTools] = useState<CliTool[]>([]);
  const [search, setSearch] = useState('');
  const [expandedTool, setExpandedTool] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    Promise.all([getTools(), getCliTools()])
      .then(([t, c]) => {
        setTools(t);
        setCliTools(c);
      })
      .catch((err) => setError(err.message))
      .finally(() => setLoading(false));
  }, []);

  const filtered = tools.filter(
    (t) =>
      t.name.toLowerCase().includes(search.toLowerCase()) ||
      t.description.toLowerCase().includes(search.toLowerCase()),
  );

  const filteredCli = cliTools.filter(
    (t) =>
      t.name.toLowerCase().includes(search.toLowerCase()) ||
      t.category.toLowerCase().includes(search.toLowerCase()),
  );

  if (error) {
    return (
      <div className="p-6 animate-fade-in">
        <div className="rounded-xl p-4" style={{ backgroundColor: 'var(--color-status-error)', opacity: 0.1, border: '1px solid var(--color-status-error)', color: 'var(--color-status-error)' }}>
          Failed to load tools: {error}
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
      <div className="relative max-w-md">
        <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4" style={{ color: 'var(--color-text-muted)' }} />
        <input
          type="text"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder="Search tools..."
          className="input-electric w-full pl-10 pr-4 py-2.5 text-sm"
        />
      </div>

      <div>
        <div className="flex items-center gap-2 mb-4">
          <Wrench className="h-5 w-5" style={{ color: 'var(--color-accent-blue)' }} />
          <h2 className="text-sm font-semibold uppercase tracking-wider" style={{ color: 'var(--color-text-primary)' }}>
            Agent Tools ({filtered.length})
          </h2>
        </div>

        {filtered.length === 0 ? (
          <p className="text-sm" style={{ color: 'var(--color-text-muted)' }}>No tools match your search.</p>
        ) : (
          <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-4 stagger-children">
            {filtered.map((tool) => {
              const isExpanded = expandedTool === tool.name;
              return (
                <div
                  key={tool.name}
                  className="glass-card overflow-hidden animate-slide-in-up"
                >
                  <button
                    onClick={() =>
                      setExpandedTool(isExpanded ? null : tool.name)
                    }
                    className="w-full text-left p-4 transition-all duration-300 hover:opacity-80"
                  >
                    <div className="flex items-start justify-between gap-2">
                      <div className="flex items-center gap-2 min-w-0">
                        <Package className="h-4 w-4 flex-shrink-0 mt-0.5" style={{ color: 'var(--color-accent-blue)' }} />
                        <h3 className="text-sm font-semibold truncate" style={{ color: 'var(--color-text-primary)' }}>
                          {tool.name}
                        </h3>
                      </div>
                      {isExpanded ? (
                        <ChevronDown className="h-4 w-4 flex-shrink-0 transition-transform" style={{ color: 'var(--color-accent-blue)' }} />
                      ) : (
                        <ChevronRight className="h-4 w-4 flex-shrink-0 transition-transform" style={{ color: 'var(--color-text-muted)' }} />
                      )}
                    </div>
                    <p className="text-sm mt-2 line-clamp-2" style={{ color: 'var(--color-text-muted)' }}>
                      {tool.description}
                    </p>
                  </button>

                  {isExpanded && tool.parameters && (
                    <div className="border-t p-4 animate-fade-in" style={{ borderColor: 'var(--color-border-default)' }}>
                      <p className="text-xs font-semibold uppercase tracking-wider mb-2" style={{ color: 'var(--color-text-muted)' }}>
                        Parameter Schema
                      </p>
                      <pre className="text-xs rounded-xl p-3 overflow-x-auto max-h-64 overflow-y-auto" style={{ backgroundColor: 'var(--color-bg-primary)', color: 'var(--color-text-secondary)' }}>
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

      {filteredCli.length > 0 && (
        <div className="animate-slide-in-up" style={{ animationDelay: '200ms' }}>
          <div className="flex items-center gap-2 mb-4">
            <Terminal className="h-5 w-5" style={{ color: 'var(--color-status-success)' }} />
            <h2 className="text-sm font-semibold uppercase tracking-wider" style={{ color: 'var(--color-text-primary)' }}>
              CLI Tools ({filteredCli.length})
            </h2>
          </div>

          <div className="glass-card overflow-hidden">
            <table className="table-electric">
              <thead>
                <tr>
                  <th className="text-left">Name</th>
                  <th className="text-left">Path</th>
                  <th className="text-left">Version</th>
                  <th className="text-left">Category</th>
                </tr>
              </thead>
              <tbody>
                {filteredCli.map((tool) => (
                  <tr key={tool.name}>
                    <td className="px-4 py-3 font-medium text-sm" style={{ color: 'var(--color-text-primary)' }}>
                      {tool.name}
                    </td>
                    <td className="px-4 py-3 font-mono text-xs truncate max-w-[200px]" style={{ color: 'var(--color-text-muted)' }}>
                      {tool.path}
                    </td>
                    <td className="px-4 py-3 text-sm" style={{ color: 'var(--color-text-muted)' }}>
                      {tool.version ?? '-'}
                    </td>
                    <td className="px-4 py-3">
                      <span className="inline-flex items-center px-2.5 py-0.5 rounded-full text-[10px] font-semibold capitalize border" style={{ borderColor: 'var(--color-border-default)', color: 'var(--color-text-secondary)', backgroundColor: 'var(--color-accent-blue)', opacity: 0.1 }}>
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
