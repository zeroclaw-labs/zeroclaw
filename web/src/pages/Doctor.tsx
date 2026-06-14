import { useState } from 'react';
import { Link } from 'react-router-dom';
import {
  CheckCircle,
  AlertTriangle,
  XCircle,
  Loader2,
  Play,
  Stethoscope,
  ArrowRight,
} from 'lucide-react';
import type { DiagResult } from '@/types/api';
import { runDoctor } from '@/lib/api';
import { Badge, Button, Card, PageHeader } from '@/components/ui';
import ReloadDaemonButton from '@/components/sections/ReloadDaemonButton';
import { t } from '@/lib/i18n';

type Severity = DiagResult['severity'];

const SEVERITY_TONE: Record<Severity, 'ok' | 'warn' | 'error'> = {
  ok: 'ok',
  warn: 'warn',
  error: 'error',
};

/**
 * Best-effort remediation route for a diagnostic. `DiagResult` carries no
 * per-finding target, so we first try to PARSE a config entity out of the
 * message (config diagnostics are phrased "<type>.<alias>: <problem>", e.g.
 * "openai.ss: no model configured", "discord.gnosis: …") and deep-link
 * straight to that entity; otherwise we fall back to the coarse per-category
 * route. Returns `[href, label]`, or `null` when no in-app link is sensible.
 *  - parsed model finding   → /config/providers.models/<type>/<alias>
 *  - parsed channel finding → /config/channels/<type>/<alias>
 *  - other config/workspace → /config (the navigator)
 *  - daemon → null (covered by the "Reload daemon" header action)
 *  - environment, cli-tools → null (system-level; nothing to open in the UI)
 */
function remediationLink(result: DiagResult): [string, string] | null {
  if (result.severity === 'ok') return null;
  const msg = result.message;
  // Leading "<type>.<alias>" entity reference, if present.
  const m = msg.match(/^\s*([a-z0-9_-]+)\.([a-z0-9_-]+)\b/i);
  if (m && m[1] && m[2]) {
    const type = encodeURIComponent(m[1]);
    const alias = encodeURIComponent(m[2]);
    if (/\bmodel\b|api[\s_-]?key|provider/i.test(msg)) {
      return [`/config/providers.models/${type}/${alias}`, 'Open config'];
    }
    if (/\bchannel\b/i.test(msg)) {
      return [`/config/channels/${type}/${alias}`, 'Open config'];
    }
  }
  switch (result.category) {
    case 'config':
    case 'workspace':
      return ['/config', 'Open config'];
    default:
      return null;
  }
}

function severityIcon(severity: Severity) {
  switch (severity) {
    case 'ok':
      return <CheckCircle className="h-4 w-4 flex-shrink-0 text-status-success" />;
    case 'warn':
      return <AlertTriangle className="h-4 w-4 flex-shrink-0 text-status-warning" />;
    case 'error':
      return <XCircle className="h-4 w-4 flex-shrink-0 text-status-error" />;
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
    <div className="p-6 space-y-6">
      <PageHeader
        title={t('doctor.diagnostics_title')}
        description={t('doctor.system_diagnostics')}
        actions={
          <>
            {/* Many config/daemon findings only clear after the daemon
                re-consumes config. Re-run diagnostics once it's back. */}
            <ReloadDaemonButton onReloaded={() => void handleRun()} />
            <Button onClick={handleRun} disabled={loading}>
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
            </Button>
          </>
        }
      />

      {/* Error */}
      {error && (
        <Card className="text-sm border-status-error/25 bg-status-error/10 text-status-error">
          {error}
        </Card>
      )}

      {/* Loading state */}
      {loading && (
        <Card className="flex flex-col items-center justify-center py-16">
          <Loader2 className="h-8 w-8 animate-spin text-pc-accent mb-4" />
          <p className="text-sm text-pc-text-secondary">{t('doctor.running_desc')}</p>
          <p className="text-[13px] mt-1 text-pc-text-faint">{t('doctor.running_hint')}</p>
        </Card>
      )}

      {/* Results */}
      {results && !loading && (
        <>
          {/* Summary bar */}
          <Card className="flex items-center gap-4 flex-wrap">
            <div className="flex items-center gap-2">
              <CheckCircle className="h-5 w-5 text-status-success" />
              <span className="text-sm font-medium text-pc-text">
                {okCount} <span className="font-normal text-pc-text-muted">ok</span>
              </span>
            </div>
            <div className="w-px h-5 bg-pc-border" />
            <div className="flex items-center gap-2">
              <AlertTriangle className="h-5 w-5 text-status-warning" />
              <span className="text-sm font-medium text-pc-text">
                {warnCount}{' '}
                <span className="font-normal text-pc-text-muted">
                  warning{warnCount !== 1 ? 's' : ''}
                </span>
              </span>
            </div>
            <div className="w-px h-5 bg-pc-border" />
            <div className="flex items-center gap-2">
              <XCircle className="h-5 w-5 text-status-error" />
              <span className="text-sm font-medium text-pc-text">
                {errorCount}{' '}
                <span className="font-normal text-pc-text-muted">
                  error{errorCount !== 1 ? 's' : ''}
                </span>
              </span>
            </div>

            {/* Overall indicator */}
            <div className="ml-auto">
              {errorCount > 0 ? (
                <Badge tone="error">{t('doctor.issues_found')}</Badge>
              ) : warnCount > 0 ? (
                <Badge tone="warn">{t('doctor.warnings_summary')}</Badge>
              ) : (
                <Badge tone="ok">{t('doctor.all_clear')}</Badge>
              )}
            </div>
          </Card>

          {/* Grouped results */}
          {Object.entries(grouped)
            .sort(([a], [b]) => a.localeCompare(b))
            .map(([category, items]) => (
              <div key={category}>
                <h3 className="text-sm font-semibold uppercase tracking-wider mb-3 capitalize text-pc-text-muted">
                  {category}
                </h3>
                <div className="space-y-2">
                  {items.map((result, idx) => {
                    const link = remediationLink(result);
                    return (
                      <Card
                        key={`${category}-${idx}`}
                        className="flex items-start gap-3 p-3"
                      >
                        {severityIcon(result.severity)}
                        <div className="min-w-0 flex-1">
                          <p className="text-sm text-pc-text">{result.message}</p>
                        </div>
                        {link && (
                          <Link
                            to={link[0]}
                            className="inline-flex h-7 flex-shrink-0 items-center gap-1 rounded-[var(--radius-md)] border border-pc-border bg-transparent px-2.5 text-[13px] font-medium text-pc-text-secondary transition-colors duration-150 hover:bg-[var(--pc-hover)] hover:text-pc-text hover:border-pc-border-strong focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)] focus-visible:ring-offset-2 focus-visible:ring-offset-pc-base"
                          >
                            {link[1]}
                            <ArrowRight className="h-3.5 w-3.5" />
                          </Link>
                        )}
                        <Badge tone={SEVERITY_TONE[result.severity]}>
                          {result.severity}
                        </Badge>
                      </Card>
                    );
                  })}
                </div>
              </div>
            ))}
        </>
      )}

      {/* Empty state */}
      {!results && !loading && !error && (
        <Card className="flex flex-col items-center justify-center py-16">
          <div className="h-16 w-16 rounded-[var(--radius-lg)] flex items-center justify-center mb-4 bg-pc-elevated border border-pc-border">
            <Stethoscope className="h-8 w-8 text-pc-accent" />
          </div>
          <p className="text-lg font-semibold mb-1 text-pc-text">
            {t('doctor.system_diagnostics')}
          </p>
          <p className="text-sm text-pc-text-muted">{t('doctor.empty_hint')}</p>
        </Card>
      )}
    </div>
  );
}
