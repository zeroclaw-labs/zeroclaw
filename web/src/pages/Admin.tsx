import { useState } from 'react';
import { Link } from 'react-router-dom';
import { ArrowRight, Search, Settings2, ShieldCheck } from 'lucide-react';
import { useWorkspaceSettings } from '@/components/WorkspaceSettings';
import { usePolling } from '@/hooks/usePolling';
import { getSections, type SectionInfo } from '@/lib/api';
import { destinations, type NavItem } from '@/lib/navigation';
import { t } from '@/lib/i18n';

// These are shortcut preferences only. The server's section catalogue owns
// availability, labels, help text, and the editor each shortcut opens.
const preferredSettings = [
  'gateway', 'providers.models', 'risk_profiles', 'runtime_profiles',
  'channels', 'observability', 'security', 'users', 'oidc',
];

export default function Admin() {
  const openSettings = useWorkspaceSettings();
  const [query, setQuery] = useState('');
  const [sections, setSections] = useState<SectionInfo[] | null>(null);
  const [error, setError] = useState(false);
  usePolling(async (stale) => {
    try {
      const response = await getSections();
      if (!stale()) { setSections(response.sections); setError(false); }
    } catch {
      if (!stale()) { setSections(null); setError(true); }
    }
  }, 30000);

  const words = query.trim().replace(/[._/-]/g, ' ').toLocaleLowerCase().split(/\s+/).filter(Boolean);
  const matches = (...values: string[]) => {
    const text = values.join(' ').replace(/[._/-]/g, ' ').toLocaleLowerCase();
    return words.every((word) => text.includes(word));
  };
  const shortcuts = (sections ?? []).filter((section) => words.length
    ? matches(section.label, section.key, section.help, section.group)
    : preferredSettings.includes(section.key));
  const pages = destinations.filter((item) => item.adminGroup && matches(
    t(item.labelKey), item.descriptionKey ? t(item.descriptionKey) : '',
  ));

  return (
    <div className="mx-auto max-w-6xl space-y-7 px-5 py-7 sm:px-8 sm:py-9">
      <div className="flex flex-wrap items-start justify-between gap-4">
        <div>
          <h1 className="flex items-center gap-3 text-2xl font-semibold tracking-tight sm:text-3xl">
            <ShieldCheck className="h-6 w-6 text-pc-accent" />
            {t('workspace.admin')}
          </h1>
          <p className="mt-2 text-sm leading-relaxed text-pc-text-secondary">
            {t('admin.description')}
          </p>
        </div>
        <button type="button" onClick={() => openSettings('/config')}
          className="inline-flex items-center gap-2 rounded-lg border border-pc-border bg-pc-surface px-3 py-2 text-sm hover:bg-pc-elevated">
          <Settings2 className="h-4 w-4 text-pc-accent" />
          {t('config.all_settings')}
        </button>
      </div>

      <label className="flex items-center gap-3 rounded-xl border border-pc-border bg-pc-surface px-4 py-3 focus-within:border-pc-accent">
        <Search className="h-4 w-4 shrink-0 text-pc-text-muted" />
        <input type="search" aria-label={t('admin.search')} placeholder={t('admin.search')}
          value={query} onChange={(event) => setQuery(event.target.value)}
          className="min-w-0 flex-1 bg-transparent text-sm outline-none placeholder:text-pc-text-muted" />
      </label>

      <section aria-labelledby="admin-settings" className="rounded-xl border border-pc-border bg-pc-surface p-5">
        <h2 id="admin-settings" className="text-sm font-medium">{t('admin.quick_settings')}</h2>
        <p className="mt-1 text-xs leading-relaxed text-pc-text-muted">{t('admin.settings_hint')}</p>
        {error ? (
          <p role="status" className="mt-4 text-sm text-pc-text-muted">{t('admin.settings_unavailable')}</p>
        ) : sections === null ? (
          <p role="status" className="mt-4 text-sm text-pc-text-muted">{t('common.loading')}</p>
        ) : (
          <div className="mt-4 flex flex-wrap gap-2">
            {shortcuts.map((section) => (
              <button key={section.key} type="button" title={section.help}
                onClick={() => openSettings(`/config/${encodeURIComponent(section.key)}`)}
                className="inline-flex items-center gap-2 rounded-lg border border-pc-border px-3 py-2 text-sm text-pc-text-secondary hover:border-pc-accent/40 hover:bg-pc-elevated hover:text-pc-text">
                {section.label}
                <ArrowRight className="h-3 w-3" />
              </button>
            ))}
            {words.length > 0 && shortcuts.length === 0 && (
              <p className="text-sm text-pc-text-muted">{t('nav.cmdk.empty')}</p>
            )}
          </div>
        )}
      </section>

      {(['dashboards', 'management', 'advanced'] as const).map((group) => {
        const items = pages.filter((item) => item.adminGroup === group);
        if (!items.length) return null;
        return (
          <section key={group} aria-labelledby={`admin-${group}`}>
            <h2 id={`admin-${group}`} className="mb-3 text-sm font-medium">{t(`admin.${group}`)}</h2>
            <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
              {items.map((item) => <Destination key={item.to} item={item} />)}
            </div>
          </section>
        );
      })}
      {words.length > 0 && sections !== null && !shortcuts.length && !pages.length && (
        <p role="status" className="py-6 text-center text-sm text-pc-text-muted">{t('admin.no_matches')}</p>
      )}
    </div>
  );
}

function Destination({ item }: { item: NavItem }) {
  const Icon = item.icon;
  return (
    <Link to={item.to} className="group flex items-start gap-3 rounded-xl border border-pc-border bg-pc-surface p-4 transition-colors hover:border-pc-accent/40 hover:bg-pc-elevated">
      <Icon className="mt-0.5 h-4 w-4 shrink-0 text-pc-accent" />
      <span className="min-w-0 flex-1">
        <span className="block text-sm font-medium">{t(item.labelKey)}</span>
        {item.descriptionKey && <span className="mt-1.5 block text-xs leading-relaxed text-pc-text-muted">{t(item.descriptionKey)}</span>}
      </span>
      <ArrowRight className="mt-0.5 h-3.5 w-3.5 shrink-0 text-pc-text-muted group-hover:text-pc-accent" />
    </Link>
  );
}
