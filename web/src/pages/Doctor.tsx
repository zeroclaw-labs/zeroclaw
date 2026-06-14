import { useState, type ReactNode } from 'react';
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
import DoctorFixModal from '@/components/DoctorFixModal';
import { t } from '@/lib/i18n';

type Severity = DiagResult['severity'];

const SEVERITY_TONE: Record<Severity, 'ok' | 'warn' | 'error'> = {
  ok: 'ok',
  warn: 'warn',
  error: 'error',
};

/**
 * A remediable config entity parsed out of a diagnostic message, paired with
 * its deep-link. The same parse drives both the inline "fix in a modal" flow
 * (via `prefix`, which FieldForm fetches its fields under) and the "Open
 * config" / "Open full page →" deep-link (`href`).
 *  - `prefix` — dotted config entity prefix, e.g. `providers.models.openai.ss`
 *               or `channels.discord.gnosis`.
 *  - `href`   — the in-app route to the full config page for that entity
 *               (carries `?tab=model` for model findings).
 *  - `label`  — the action label ("Open config").
 */
interface RemediationTarget {
  prefix: string;
  href: string;
  label: string;
}

/**
 * Best-effort remediation TARGET for a diagnostic. `DiagResult` carries no
 * per-finding target, so we PARSE a config entity out of the message (config
 * diagnostics are phrased "<type>.<alias>: <problem>", e.g.
 * "openai.ss: no model configured", "discord.gnosis: …") and resolve both the
 * editable entity prefix and its deep-link. Returns `null` when no parseable
 * entity is present (the caller then falls back to the coarse `/config` link).
 *  - parsed model finding   → prefix `providers.models.<type>.<alias>`,
 *                             href `/config/providers.models/<type>/<alias>[?tab=model]`
 *  - parsed channel finding → prefix `channels.<type>.<alias>`,
 *                             href `/config/channels/<type>/<alias>`
 */
function remediationTarget(result: DiagResult): RemediationTarget | null {
  if (result.severity === 'ok') return null;
  const msg = result.message;
  // Leading "<type>.<alias>" entity reference, if present.
  const m = msg.match(/^\s*([a-z0-9_-]+)\.([a-z0-9_-]+)\b/i);
  if (!m || !m[1] || !m[2]) return null;
  const rawType = m[1];
  const rawAlias = m[2];
  const type = encodeURIComponent(rawType);
  const alias = encodeURIComponent(rawAlias);
  if (/\bmodel\b|api[\s_-]?key|provider/i.test(msg)) {
    // A "no model configured" finding belongs on the Model tab; api-key /
    // connection issues default to the Connection tab (no ?tab needed). The
    // dotted entity prefix FieldForm edits uses dots throughout; the href uses
    // a slash after the section so Config's router can split type/alias.
    const tab = /\bmodel\b/i.test(msg) ? '?tab=model' : '';
    return {
      prefix: `providers.models.${rawType}.${rawAlias}`,
      href: `/config/providers.models/${type}/${alias}${tab}`,
      label: 'Open config',
    };
  }
  if (/\bchannel\b/i.test(msg)) {
    return {
      prefix: `channels.${rawType}.${rawAlias}`,
      href: `/config/channels/${type}/${alias}`,
      label: 'Open config',
    };
  }
  return null;
}

/**
 * Coarse fallback link for a finding with NO parseable entity. Config/workspace
 * findings point at the navigator; everything else (daemon, environment,
 * cli-tools) has no sensible in-app target. Returns `[href, label]` or `null`.
 */
function fallbackLink(result: DiagResult): [string, string] | null {
  if (result.severity === 'ok') return null;
  switch (result.category) {
    case 'config':
    case 'workspace':
      return ['/config', 'Open config'];
    default:
      return null;
  }
}

/**
 * One clickable count in the summary bar. Toggles its severity on/off as a
 * filter. `active` (the severity is currently SHOWN) gets accent/selected
 * styling; an inactive (filtered-out) toggle dims and de-accents so the
 * operator can see at a glance which severities the list is scoped to. The
 * count text stays visible in both states. Acts as a toggle button
 * (`aria-pressed`) for assistive tech.
 */
function SeverityFilterToggle({
  active,
  count,
  label,
  icon,
  onToggle,
}: {
  active: boolean;
  count: number;
  label: string;
  icon: ReactNode;
  onToggle: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onToggle}
      aria-pressed={active}
      title={active ? `Hide ${label}` : `Show ${label}`}
      className={[
        'inline-flex items-center gap-2 rounded-[var(--radius-md)] border px-2.5 py-1 transition-colors duration-150 cursor-pointer select-none',
        'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)] focus-visible:ring-offset-2 focus-visible:ring-offset-pc-base',
        active
          ? 'border-pc-accent bg-pc-accent/10 text-pc-text'
          : 'border-pc-border bg-transparent text-pc-text-muted opacity-60 hover:opacity-100 hover:border-pc-border-strong',
      ].join(' ')}
    >
      {icon}
      <span className="text-sm font-medium text-pc-text">
        {count} <span className="font-normal text-pc-text-muted">{label}</span>
      </span>
    </button>
  );
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
  // Severity filter: a severity present in the set is HIDDEN. Empty = show all
  // (the default). Each summary count toggles its own severity independently.
  const [hidden, setHidden] = useState<Set<Severity>>(new Set());
  // The config entity currently being fixed in the modal, or null when closed.
  const [fixTarget, setFixTarget] = useState<RemediationTarget | null>(null);

  const handleRun = async () => {
    setLoading(true);
    setError(null);
    setResults(null);
    setHidden(new Set());
    try {
      const data = await runDoctor();
      setResults(data);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to run diagnostics');
    } finally {
      setLoading(false);
    }
  };

  const toggleSeverity = (severity: Severity) => {
    setHidden((prev) => {
      const next = new Set(prev);
      if (next.has(severity)) next.delete(severity);
      else next.add(severity);
      return next;
    });
  };

  const okCount = results?.filter((r) => r.severity === 'ok').length ?? 0;
  const warnCount = results?.filter((r) => r.severity === 'warn').length ?? 0;
  const errorCount = results?.filter((r) => r.severity === 'error').length ?? 0;

  // Apply the severity filter, then group. Grouping the FILTERED list lets a
  // category whose every finding is hidden drop out of the list entirely.
  const filtered = results?.filter((r) => !hidden.has(r.severity)) ?? [];

  const grouped = filtered.reduce<Record<string, DiagResult[]>>((acc, item) => {
    const key = item.category;
    if (!acc[key]) acc[key] = [];
    acc[key].push(item);
    return acc;
  }, {});

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
          {/* Summary bar — the counts double as severity filters. Click a
              count to toggle that severity in/out of the list below; toggles
              are independent and default to all-shown. */}
          <Card className="flex items-center gap-2 flex-wrap">
            <SeverityFilterToggle
              active={!hidden.has('ok')}
              count={okCount}
              label="ok"
              icon={<CheckCircle className="h-5 w-5 text-status-success" />}
              onToggle={() => toggleSeverity('ok')}
            />
            <SeverityFilterToggle
              active={!hidden.has('warn')}
              count={warnCount}
              label={`warning${warnCount !== 1 ? 's' : ''}`}
              icon={<AlertTriangle className="h-5 w-5 text-status-warning" />}
              onToggle={() => toggleSeverity('warn')}
            />
            <SeverityFilterToggle
              active={!hidden.has('error')}
              count={errorCount}
              label={`error${errorCount !== 1 ? 's' : ''}`}
              icon={<XCircle className="h-5 w-5 text-status-error" />}
              onToggle={() => toggleSeverity('error')}
            />

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

          {/* All severities toggled off → nothing to show. */}
          {filtered.length === 0 && results.length > 0 && (
            <Card className="text-sm text-center text-pc-text-muted py-8">
              No findings match the active severity filter.
            </Card>
          )}

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
                    // Findings WITH a parseable config entity open the inline
                    // fix modal (no navigation, no re-run). Findings WITHOUT
                    // one fall back to the coarse /config link.
                    const target = remediationTarget(result);
                    const link = target ? null : fallbackLink(result);
                    return (
                      <Card
                        key={`${category}-${idx}`}
                        className="flex items-start gap-3 p-3"
                      >
                        {severityIcon(result.severity)}
                        <div className="min-w-0 flex-1">
                          <p className="text-sm text-pc-text">{result.message}</p>
                        </div>
                        {target && (
                          <button
                            type="button"
                            onClick={() => setFixTarget(target)}
                            className="inline-flex h-7 flex-shrink-0 items-center gap-1 rounded-[var(--radius-md)] border border-pc-border bg-transparent px-2.5 text-[13px] font-medium text-pc-text-secondary transition-colors duration-150 hover:bg-[var(--pc-hover)] hover:text-pc-text hover:border-pc-border-strong focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)] focus-visible:ring-offset-2 focus-visible:ring-offset-pc-base cursor-pointer"
                          >
                            {target.label}
                            <ArrowRight className="h-3.5 w-3.5" />
                          </button>
                        )}
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

      {/* Fix-in-place modal. Mounted once at the page root; opens when a
          finding with a parseable entity is actioned. Closing returns the
          operator to the Doctor list as-is — no navigation, no re-run. */}
      <DoctorFixModal
        open={fixTarget !== null}
        prefix={fixTarget?.prefix ?? ''}
        entity={fixTarget?.prefix.split('.').slice(-2).join('.') ?? ''}
        href={fixTarget?.href ?? ''}
        onClose={() => setFixTarget(null)}
      />

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
