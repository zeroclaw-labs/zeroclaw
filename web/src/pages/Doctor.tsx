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
      return <CheckCircle className="h-4 w-4 text-[#00e68a] flex-shrink-0" />;
    case 'warn':
      return <AlertTriangle className="h-4 w-4 text-[#ffaa00] flex-shrink-0" />;
    case 'error':
      return <XCircle className="h-4 w-4 text-[#ff4466] flex-shrink-0" />;
  }
}

function severityBorder(severity: DiagResult['severity']): string {
  switch (severity) {
    case 'ok':
      return 'border-[#00e68a20]';
    case 'warn':
      return 'border-[#ffaa0020]';
    case 'error':
      return 'border-[#ff446620]';
  }
}

function severityBg(severity: DiagResult['severity']): string {
  switch (severity) {
    case 'ok':
      return 'rgba(0,230,138,0.04)';
    case 'warn':
      return 'rgba(255,170,0,0.04)';
    case 'error':
      return 'rgba(255,68,102,0.04)';
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
          <Stethoscope className="h-5 w-5 text-[#0080ff]" />
          <h2 className="text-sm font-semibold text-white uppercase tracking-wider">{t('doctor.diagnostics_title')}</h2>
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
        <div className="rounded-xl bg-[#ff446615] border border-[#ff446630] p-4 text-[#ff6680] animate-fade-in">
          {error}
        </div>
      )}

      {/* Loading spinner */}
      {loading && (
        <div className="flex flex-col items-center justify-center py-16 animate-fade-in">
          <div className="h-12 w-12 border-2 border-[#0080ff30] border-t-[#0080ff] rounded-full animate-spin mb-4" />
          <p className="text-[#8892a8]">{t('doctor.running_desc')}</p>
          <p className="text-sm text-[#334060] mt-1">
            {t('doctor.running_hint')}
          </p>
        </div>
      )}

      {/* Results */}
      {results && !loading && (
        <>
          {/* Summary Bar */}
          <div className="glass-card flex items-center gap-4 p-4 animate-slide-in-up">
            <div className="flex items-center gap-2">
              <CheckCircle className="h-5 w-5 text-[#00e68a]" />
              <span className="text-sm text-white font-medium">
                {okCount} <span className="text-[#556080] font-normal">ok</span>
              </span>
            </div>
            <div className="w-px h-5 bg-[#1a1a3e]" />
            <div className="flex items-center gap-2">
              <AlertTriangle className="h-5 w-5 text-[#ffaa00]" />
              <span className="text-sm text-white font-medium">
                {warnCount}{' '}
                <span className="text-[#556080] font-normal">
                  warning{warnCount !== 1 ? 's' : ''}
                </span>
              </span>
            </div>
            <div className="w-px h-5 bg-[#1a1a3e]" />
            <div className="flex items-center gap-2">
              <XCircle className="h-5 w-5 text-[#ff4466]" />
              <span className="text-sm text-white font-medium">
                {errorCount}{' '}
                <span className="text-[#556080] font-normal">
                  error{errorCount !== 1 ? 's' : ''}
                </span>
              </span>
            </div>

            {/* Overall indicator */}
            <div className="ml-auto">
              {errorCount > 0 ? (
                <span className="inline-flex items-center gap-1.5 px-3 py-1 rounded-full text-[10px] font-semibold border text-[#ff4466] border-[#ff446630]" style={{ background: 'rgba(255,68,102,0.06)' }}>
                  {t('doctor.issues_found')}
                </span>
              ) : warnCount > 0 ? (
                <span className="inline-flex items-center gap-1.5 px-3 py-1 rounded-full text-[10px] font-semibold border text-[#ffaa00] border-[#ffaa0030]" style={{ background: 'rgba(255,170,0,0.06)' }}>
                  {t('doctor.warnings_summary')}
                </span>
              ) : (
                <span className="inline-flex items-center gap-1.5 px-3 py-1 rounded-full text-[10px] font-semibold border text-[#00e68a] border-[#00e68a30]" style={{ background: 'rgba(0,230,138,0.06)' }}>
                  {t('doctor.all_clear')}
                </span>
              )}
            </div>
          </div>

          {/* Grouped Results */}
          {Object.entries(grouped)
            .sort(([a], [b]) => a.localeCompare(b))
            .map(([category, items], catIdx) => (
              <div key={category} className="animate-slide-in-up" style={{ animationDelay: `${(catIdx + 1) * 100}ms` }}>
                <h3 className="text-[10px] font-semibold text-[#334060] uppercase tracking-wider mb-3 capitalize">
                  {category}
                </h3>
                <div className="space-y-2 stagger-children">
                  {items.map((result, idx) => (
                    <div
                      key={`${category}-${idx}`}
                      className={`flex items-start gap-3 rounded-xl border p-3 transition-all duration-300 hover:translate-x-1 ${severityBorder(result.severity)} animate-slide-in-left`}
                      style={{ background: severityBg(result.severity) }}
                    >
                      {severityIcon(result.severity)}
                      <div className="min-w-0">
                        <p className="text-sm text-white">{result.message}</p>
                        <p className="text-[10px] text-[#334060] mt-0.5 capitalize uppercase tracking-wider">
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
        <div className="flex flex-col items-center justify-center py-16 text-[#334060] animate-fade-in">
          <div className="h-16 w-16 rounded-2xl flex items-center justify-center mb-4 animate-float" style={{ background: 'linear-gradient(135deg, #0080ff15, #0080ff08)' }}>
            <Stethoscope className="h-8 w-8 text-[#0080ff]" />
          </div>
          <p className="text-lg font-semibold text-white mb-1">{t('doctor.system_diagnostics')}</p>
          <p className="text-sm text-[#556080]">
            {t('doctor.empty_hint')}
          </p>
        </div>
      )}
    </div>
  );
}
