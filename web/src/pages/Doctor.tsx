import { useState } from 'react';
import { Stethoscope, Play, CheckCircle, AlertTriangle, XCircle, Loader2 } from 'lucide-react';
import type { DiagResult } from '@/types/api';
import { runDoctor } from '@/lib/api';
import { t } from '@/lib/i18n';

function severityIcon(severity: DiagResult['severity']) {
  switch (severity) {
    case 'ok': return <CheckCircle className="h-4 w-4 flex-shrink-0" style={{ color: 'var(--color-status-success)' }} />;
    case 'warn': return <AlertTriangle className="h-4 w-4 flex-shrink-0" style={{ color: 'var(--color-status-warning)' }} />;
    case 'error': return <XCircle className="h-4 w-4 flex-shrink-0" style={{ color: 'var(--color-status-error)' }} />;
  }
}

function severityBg(severity: DiagResult['severity']): string {
  switch (severity) {
    case 'ok': return 'rgba(0, 230, 138, 0.04)';
    case 'warn': return 'rgba(255, 170, 0, 0.04)';
    case 'error': return 'rgba(239, 68, 68, 0.04)';
  }
}

export default function Doctor() {
  const [results, setResults] = useState<DiagResult[]>([]);
  const [running, setRunning] = useState(false);
  const [done, setDone] = useState(false);

  const handleRun = async () => {
    setRunning(true);
    setDone(false);
    setResults([]);
    try {
      const r = await runDoctor();
      setResults(r);
    } finally {
      setRunning(false);
      setDone(true);
    }
  };

  const summary = {
    ok: results.filter((r) => r.severity === 'ok').length,
    warn: results.filter((r) => r.severity === 'warn').length,
    error: results.filter((r) => r.severity === 'error').length,
  };

  return (
    <div className="p-6 space-y-6 animate-fade-in">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Stethoscope className="h-5 w-5" style={{ color: 'var(--pc-accent)' }} />
          <h2 className="text-sm font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-primary)' }}>
            {t('doctor.diagnostics_title')}
          </h2>
        </div>
        <button onClick={handleRun} disabled={running} className="btn-electric flex items-center gap-2 text-sm px-4 py-2">
          {running ? <Loader2 className="h-4 w-4 animate-spin" /> : <Play className="h-4 w-4" />}
          {running ? t('doctor.running') : t('doctor.run_diagnostics')}
        </button>
      </div>

      {done && (
        <div className="flex items-center gap-4">
          <div className="flex items-center gap-1.5 text-xs" style={{ color: 'var(--color-status-success)' }}>
            <CheckCircle className="h-3.5 w-3.5" />{summary.ok} {t('doctor.ok')}
          </div>
          <div className="flex items-center gap-1.5 text-xs" style={{ color: 'var(--color-status-warning)' }}>
            <AlertTriangle className="h-3.5 w-3.5" />{summary.warn} {t('doctor.warnings')}
          </div>
          <div className="flex items-center gap-1.5 text-xs" style={{ color: 'var(--color-status-error)' }}>
            <XCircle className="h-3.5 w-3.5" />{summary.error} {t('doctor.errors')}
          </div>
        </div>
      )}

      {results.length === 0 && !running ? (
        <div className="card p-8 text-center">
          <Stethoscope className="h-10 w-10 mx-auto mb-3" style={{ color: 'var(--pc-text-faint)' }} />
          <p style={{ color: 'var(--pc-text-muted)' }}>{t('doctor.empty')}</p>
        </div>
      ) : (
        <div className="space-y-3">
          {results.map((result, i) => (
            <div
              key={i}
              className="card rounded-2xl p-4"
              style={{ borderLeft: `3px solid ${severityBg(result.severity) === 'rgba(0, 230, 138, 0.04)' ? 'var(--color-status-success)' : severityBg(result.severity) === 'rgba(255, 170, 0, 0.04)' ? 'var(--color-status-warning)' : 'var(--color-status-error)'}` }}
            >
              <div className="flex items-start gap-3">
                {severityIcon(result.severity)}
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2 mb-1">
                    <span className="text-sm font-medium" style={{ color: 'var(--pc-text-primary)' }}>{result.category}</span>
                    <span className="text-[10px] uppercase tracking-wider px-2 py-0.5 rounded-full border" style={result.severity === 'ok' ? { color: 'var(--color-status-success)', borderColor: 'rgba(0, 230, 138, 0.2)', background: 'rgba(0, 230, 138, 0.06)' } : result.severity === 'warn' ? { color: 'var(--color-status-warning)', borderColor: 'rgba(255, 170, 0, 0.2)', background: 'rgba(255, 170, 0, 0.06)' } : { color: 'var(--color-status-error)', borderColor: 'rgba(239, 68, 68, 0.2)', background: 'rgba(239, 68, 68, 0.06)' }}>
                      {result.severity}
                    </span>
                  </div>
                  <p className="text-sm" style={{ color: 'var(--pc-text-secondary)' }}>{result.message}</p>
                </div>
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
