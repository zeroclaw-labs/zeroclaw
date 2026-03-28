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
import { t } from '@/lib/i18n';

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

function severityBg(severity: DiagResult['severity']): string {
  switch (severity) {
    case 'ok':
      return 'rgba(0, 230, 138, 0.04)';
    case 'warn':
      return 'rgba(255, 170, 0, 0.04)';
    case 'error':
      return 'rgba(239, 68, 68, 0.04)';
  }
}

function severityBorder(severity: DiagResult['severity']): string {
  switch (severity) {
    case 'ok':
      return 'border-[rgba(0,230,138,0.3)]';
    case 'warn':
      return 'border-[rgba(255,170,0,0.3)]';
    case 'error':
      return 'border-[rgba(239,68,68,0.3)]';
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
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Stethoscope className="h-5 w-5" style={{ color: 'var(--pc-accent)' }} />
          <h2 className="text-sm font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-primary)' }}>{t('doctor.diagnostics_title')}</h2>
        </div>
        <button
          onClick={handleRun}
          disabled={loading}
          className="btn-electric flex items-center gap-2 text-sm px-4 py-2"
        >
          {loading ? (
            <>
            <Loader2 className="h-4 w-4 animate-spin" />
              {t('doctor.running_btn')}
            </>
          ) : (
            <>
            <Play className="h-4 w-4" />
              {t('doctor.run_diagnostics')}
            </>
          )}
        </button>
      </div>

      {/* Error */}
      {error && (
        <div className="rounded-xl p-4 border animate-fade-in" style={{ background: 'rgba(239,68,68,0.06)', borderColor: 'rgba(239,68,68,0.2)', color: '#f87171' }}>
          {error}
        </div>
      )}

      {/* Loading spinner */}
      {loading && (
        <div className="flex flex-col items-center justify-center py-16 animate-fade-in">
          <div className="h-12 w-12 border-2 rounded-full animate-spin mb-4" style={{ borderColor: 'rgba(255,255,255,0.1)', borderTopColor: 'var(--pc-accent)' }}/>
          <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>{t('doctor.running_desc')}</p>
          <p className="text-[13px] mt-1" style={{ color: 'var(--pc-text-faint)' }}>
            {t('doctor.running_hint')}
          </p>
        </div>
      )}

      {/* Results */}
      {results && !loading && (
        <>
          {/* Summary Bar */}
          <div className="flex items-center gap-4 p-4 rounded-xl border animate-fade-in" style={{ background: 'var(--pc-bg-surface)', borderColor: 'var(--pc-border)' }}>
            <div className="flex items-center gap-2">
              <CheckCircle className="h-5 w-5" style={{ color: 'var(--color-status-success)' }} />
              <span className="text-sm font-medium" style={{ color: 'var(--pc-text-primary)' }}>
                {okCount}{' '}<span className="text-sm font-normal" style={{ color: 'var(--pc-text-muted)' }}>ok</span>
              </span>
            </div>
            <div className="w-px h-5" style={{ background: 'var(--pc-border)' }} />
            <div className="flex items-center gap-2">
              <AlertTriangle className="h-5 w-5" style={{ color: 'var(--color-status-warning)' }} />
              <span className="text-sm font-medium" style={{ color: 'var(--pc-text-primary)' }}>
                {warnCount}{' '}
                <span className="text-sm font-normal" style={{ color: 'var(--pc-text-muted)' }}>
                  warning{warnCount !== 1 ? 's' : ''}
                </span>
              </span>
            </div>
            <div className="w-px h-5" style={{ background: 'var(--pc-border)' }} />
            <div className="flex items-center gap-2">
              <XCircle className="h-5 w-5" style={{ color: 'var(--color-status-error)' }} />
              <span className="text-sm font-medium" style={{ color: 'var(--pc-text-primary)' }}>
                {errorCount}{' '}
                <span className="text-sm font-normal" style={{ color: 'var(--pc-text-muted)' }}>
                  error{errorCount !== 1 ? 's' : ''}
                </span>
              </span>
            </div>

            {/* Overall indicator */}
            <div className="ml-auto">
              {errorCount > 0 ? (
                <span className="inline-flex items-center gap-1.5 px-3 py-1 rounded-full text-xs font-medium border" style={{ background: 'rgba(239,68,68,0.06)', borderColor: 'rgba(239,68,68,0.3)', color: '#f87171' }}>
                  {t('doctor.issues_found')}
                </span>
              ) : warnCount > 0 ? (
                <span className="inline-flex items-center gap-1.5 px-3 py-1 rounded-full text-xs font-medium border" style={{ background: 'rgba(255,170,0,0.06)', borderColor: 'rgba(255,170,0,0.3)', color: '#fbbf24' }}>
                  {t('doctor.warnings_summary')}
                </span>
              ) : (
                <span className="inline-flex items-center gap-1.5 px-3 py-1 rounded-full text-xs font-medium border" style={{ background: 'rgba(0,230,138,0.06)', borderColor: 'rgba(0,230,138,0.3)', color: '#34d399' }}>
                  {t('doctor.all_clear')}
                </span>
              )}
            </div>
          </div>

          {/* Grouped Results */}
          {Object.entries(grouped)
            .sort(([a], [b]) => a.localeCompare(b))
            .map(([category, items]) => (
              <div key={category}>
                <h3 className="text-sm font-semibold uppercase tracking-wider mb-3 capitalize" style={{ color: 'var(--pc-text-muted)' }}>
                  {category}
                </h3>
                <div className="space-y-2">
                  {items.map((result, idx) => (
                    <div
                      key={`${category}-${idx}`}
                      className={`flex items-start gap-3 rounded-xl border p-3 ${severityBorder(result.severity,)} ${severityBg(result.severity)}`}
                    >
                      {severityIcon(result.severity)}
                      <div className="min-w-0">
                        <p className="text-sm" style={{ color: 'var(--pc-text-primary)' }}>{result.message}</p>
                        <p className="text-xs capitalize mt-0.5" style={{ color: 'var(--pc-text-faint)' }}>
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

      {/* Empty state */}
      {!results && !loading && !error && (
        <div className="flex flex-col items-center justify-center py-16 text-[var(--pc-text-muted)]">
          <div className="h-16 w-16 rounded-2xl flex items-center justify-center mb-4 animate-float" style={{ background: 'linear-gradient(135deg, var(--pc-accent-glow), transparent)' }}>
            <Stethoscope className="h-8 w-8" style={{ color: 'var(--pc-accent)' }} />
          </div>
          <p className="text-lg font-semibold mb-1" style={{ color: 'var(--pc-text-primary)' }}>{t('doctor.system_diagnostics')}</p>
          <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
            {t('doctor.empty_hint')}
          </p>
        </div>
      )}
    </div>
  );
}
