import { useState } from 'react';
import {
  Stethoscope,
  Play,
  CheckCircle,
  AlertTriangle,
  XCircle,
  Loader2,
} from 'lucide-react';
import type { DiagResult } from '@/types/api';
import { runDoctor } from '@/lib/api';

function severityIcon(severity: DiagResult['severity']) {
  switch (severity) {
    case 'ok':
      return <CheckCircle className="h-4 w-4 flex-shrink-0" style={{ color: 'var(--color-status-success)' }} />;
    case 'warn':
      return <AlertTriangle className="h-4 w-4 flex-shrink-0" style={{ color: 'var(--color-status-warning)' }} />;
    case 'error':
      return <XCircle className="h-4 w-4 flex-shrink-0" style={{ color: 'var(--color-status-error)' }} />;
  }
}

function severityBorder(severity: DiagResult['severity']): string {
  switch (severity) {
    case 'ok':
      return 'var(--color-status-success)';
    case 'warn':
      return 'var(--color-status-warning)';
    case 'error':
      return 'var(--color-status-error)';
  }
}

function severityBg(severity: DiagResult['severity']): string {
  switch (severity) {
    case 'ok':
      return 'var(--color-status-success)';
    case 'warn':
      return 'var(--color-status-warning)';
    case 'error':
      return 'var(--color-status-error)';
  }
}

export default function Doctor() {
  const [results, setResults] = useState<DiagResult[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleRun = async () => {
    setLoading(true);
    setError(null);
    setResults(null);
    try {
      const data = await runDoctor();
      setResults(data);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to run diagnostics');
    } finally {
      setLoading(false);
    }
  };

  const okCount = results?.filter((r) => r.severity === 'ok').length ?? 0;
  const warnCount = results?.filter((r) => r.severity === 'warn').length ?? 0;
  const errorCount = results?.filter((r) => r.severity === 'error').length ?? 0;

  const grouped =
    results?.reduce<Record<string, DiagResult[]>>((acc, item) => {
      const key = item.category;
      if (!acc[key]) acc[key] = [];
      acc[key].push(item);
      return acc;
    }, {}) ?? {};

  return (
    <div className="p-6 space-y-6 animate-fade-in">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Stethoscope className="h-5 w-5" style={{ color: 'var(--color-accent-blue)' }} />
          <h2 className="text-sm font-semibold uppercase tracking-wider" style={{ color: 'var(--color-text-primary)' }}>Diagnostics</h2>
        </div>
        <button
          onClick={handleRun}
          disabled={loading}
          className="btn-electric flex items-center gap-2 text-sm px-4 py-2"
        >
          {loading ? (
            <>
              <Loader2 className="h-4 w-4 animate-spin" />
              Running...
            </>
          ) : (
            <>
              <Play className="h-4 w-4" />
              Run Diagnostics
            </>
          )}
        </button>
      </div>

      {error && (
        <div className="rounded-xl p-4 animate-fade-in" style={{ backgroundColor: 'var(--color-status-error)', opacity: 0.1, border: '1px solid var(--color-status-error)', color: 'var(--color-status-error)' }}>
          {error}
        </div>
      )}

      {loading && (
        <div className="flex flex-col items-center justify-center py-16 animate-fade-in">
          <div className="h-12 w-12 border-2 rounded-full animate-spin mb-4" style={{ borderColor: 'var(--color-glow-blue)', borderTopColor: 'var(--color-accent-blue)' }} />
          <p style={{ color: 'var(--color-text-secondary)' }}>Running diagnostics...</p>
          <p className="text-sm mt-1" style={{ color: 'var(--color-text-muted)' }}>
            This may take a few seconds.
          </p>
        </div>
      )}

      {results && !loading && (
        <>
          <div className="glass-card flex items-center gap-4 p-4 animate-slide-in-up">
            <div className="flex items-center gap-2">
              <CheckCircle className="h-5 w-5" style={{ color: 'var(--color-status-success)' }} />
              <span className="text-sm font-medium" style={{ color: 'var(--color-text-primary)' }}>
                {okCount} <span style={{ color: 'var(--color-text-muted)', fontWeight: 'normal' }}>ok</span>
              </span>
            </div>
            <div className="w-px h-5" style={{ backgroundColor: 'var(--color-border-default)' }} />
            <div className="flex items-center gap-2">
              <AlertTriangle className="h-5 w-5" style={{ color: 'var(--color-status-warning)' }} />
              <span className="text-sm font-medium" style={{ color: 'var(--color-text-primary)' }}>
                {warnCount}
                <span style={{ color: 'var(--color-text-muted)', fontWeight: 'normal' }}>
                  warning{warnCount !== 1 ? 's' : ''}
                </span>
              </span>
            </div>
            <div className="w-px h-5" style={{ backgroundColor: 'var(--color-border-default)' }} />
            <div className="flex items-center gap-2">
              <XCircle className="h-5 w-5" style={{ color: 'var(--color-status-error)' }} />
              <span className="text-sm font-medium" style={{ color: 'var(--color-text-primary)' }}>
                {errorCount}
                <span style={{ color: 'var(--color-text-muted)', fontWeight: 'normal' }}>
                  error{errorCount !== 1 ? 's' : ''}
                </span>
              </span>
            </div>

            <div className="ml-auto">
              {errorCount > 0 ? (
                <span className="inline-flex items-center gap-1.5 px-3 py-1 rounded-full text-xs font-semibold border" style={{ color: 'var(--color-status-error)', borderColor: 'var(--color-status-error)', backgroundColor: 'var(--color-status-error)', opacity: 0.06 }}>
                  Issues Found
                </span>
              ) : warnCount > 0 ? (
                <span className="inline-flex items-center gap-1.5 px-3 py-1 rounded-full text-xs font-semibold border" style={{ color: 'var(--color-status-warning)', borderColor: 'var(--color-status-warning)', backgroundColor: 'var(--color-status-warning)', opacity: 0.06 }}>
                  Warnings
                </span>
              ) : (
                <span className="inline-flex items-center gap-1.5 px-3 py-1 rounded-full text-xs font-semibold border" style={{ color: 'var(--color-status-success)', borderColor: 'var(--color-status-success)', backgroundColor: 'var(--color-status-success)', opacity: 0.06 }}>
                  All Clear
                </span>
              )}
            </div>
          </div>

          {Object.entries(grouped)
            .sort(([a], [b]) => a.localeCompare(b))
            .map(([category, items], catIdx) => (
              <div key={category} className="animate-slide-in-up" style={{ animationDelay: `${(catIdx + 1) * 100}ms` }}>
                <h3 className="text-xs font-semibold uppercase tracking-wider mb-3 capitalize" style={{ color: 'var(--color-text-muted)' }}>
                  {category}
                </h3>
                <div className="space-y-2 stagger-children">
                  {items.map((result, idx) => (
                    <div
                      key={`${category}-${idx}`}
                      className="flex items-start gap-3 rounded-xl border p-3 transition-all duration-300 hover:translate-x-1 animate-slide-in-left"
                      style={{ 
                        backgroundColor: severityBg(result.severity), 
                        opacity: 0.04,
                        borderColor: severityBorder(result.severity)
                      }}
                    >
                      {severityIcon(result.severity)}
                      <div className="min-w-0">
                        <p className="text-sm" style={{ color: 'var(--color-text-primary)' }}>{result.message}</p>
                        <p className="text-xs mt-0.5 capitalize uppercase tracking-wider" style={{ color: 'var(--color-text-muted)' }}>
                          {result.severity}
                        </p>
                      </div>
                    </div>
                  ))}
                </div>
              </div>
            ))}
        </>
      )}

      {!results && !loading && !error && (
        <div className="flex flex-col items-center justify-center py-16 animate-fade-in" style={{ color: 'var(--color-text-muted)' }}>
          <div className="h-16 w-16 rounded-2xl flex items-center justify-center mb-4 animate-float" style={{ background: 'linear-gradient(135deg, var(--color-accent-blue), var(--color-accent-cyan))', opacity: 0.1 }}>
            <Stethoscope className="h-8 w-8" style={{ color: 'var(--color-accent-blue)' }} />
          </div>
          <p className="text-lg font-semibold mb-1" style={{ color: 'var(--color-text-primary)' }}>System Diagnostics</p>
          <p className="text-sm" style={{ color: 'var(--color-text-muted)' }}>
            Click "Run Diagnostics" to check your ZeroClaw installation.
          </p>
        </div>
      )}
    </div>
  );
}
