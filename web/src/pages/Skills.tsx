import { SkillCard } from '@/components/SkillCard';
import { getAgentOptions, listAgentSkills, readSkill } from '@/lib/api';
import { t } from '@/lib/i18n';
import type { AgentSkillEntry, DroppedSkillEntry, SkillDocument } from '@/lib/api';
import {
  BookOpen,
  RefreshCw,
  Search
} from 'lucide-react';
import { useCallback, useEffect, useState } from 'react';

export default function Skills() {
  const [agents, setAgents] = useState<string[]>([]);
  const [selectedAlias, setSelectedAlias] = useState<string>('');
  const [skills, setSkills] = useState<AgentSkillEntry[]>([]);
  const [dropped, setDropped] = useState<DroppedSkillEntry[]>([]);
  const [search, setSearch] = useState('');
  const [loading, setLoading] = useState(true);
  const [reloading, setReloading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [expandedKey, setExpandedKey] = useState<string | null>(null);
  const [detailMap, setDetailMap] = useState<Record<string, SkillDocument>>({});

  // Load the agent list once; default to the first agent.
  useEffect(() => {
    let cancelled = false;
    getAgentOptions()
      .then(({ agents: as }) => {
        if (cancelled) return;
        setAgents(as);
        setSelectedAlias((prev) => prev || as[0] || '');
        if (as.length === 0) setLoading(false);
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        setError(err instanceof Error ? err.message : String(err));
        setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const loadSkills = useCallback((alias: string) => {
    return listAgentSkills(alias).then(({ skills: ss, dropped: dd }) => {
      setSkills(ss);
      setDropped(dd ?? []);
    });
  }, []);

  // Re-fetch whenever the selected agent changes.
  useEffect(() => {
    if (!selectedAlias) return;
    setLoading(true);
    setError(null);
    setExpandedKey(null);
    loadSkills(selectedAlias)
      .catch((err: unknown) => setError(err instanceof Error ? err.message : String(err)))
      .finally(() => setLoading(false));
  }, [selectedAlias, loadSkills]);

  const handleReload = () => {
    if (!selectedAlias) return;
    setReloading(true);
    loadSkills(selectedAlias)
      .catch((err: unknown) => setError(err instanceof Error ? err.message : String(err)))
      .finally(() => setReloading(false));
  };

  // Only bundle skills can be expanded — detail comes from the bundle endpoint.
  const handleExpand = (skill: AgentSkillEntry) => {
    if (!skill.editable || !skill.bundle) return;
    const key = `${skill.bundle}/${skill.name}`;
    if (expandedKey === key) {
      setExpandedKey(null);
      return;
    }
    setExpandedKey(key);
    if (!detailMap[key]) {
      readSkill(skill.bundle, skill.name)
        .then((doc) => setDetailMap((prev) => ({ ...prev, [key]: doc })))
        .catch(() => { /* detail is best-effort */ });
    }
  };

  const skillKey = (skill: AgentSkillEntry): string =>
    skill.editable && skill.bundle
      ? `${skill.bundle}/${skill.name}`
      : `${skill.origin}:${skill.plugin ?? ''}/${skill.name}`;

  const filtered = skills.filter((s) => {
    const q = search.toLowerCase();
    return (
      s.name.toLowerCase().includes(q) ||
      s.description.toLowerCase().includes(q) ||
      s.origin.toLowerCase().includes(q) ||
      (s.bundle ?? '').toLowerCase().includes(q) ||
      (s.plugin ?? '').toLowerCase().includes(q)
    );
  });

  if (error) {
    return (
      <div className="p-6 animate-fade-in">
        <div
          className="rounded-2xl border p-4"
          style={{
            background: 'rgba(239, 68, 68, 0.08)',
            borderColor: 'rgba(239, 68, 68, 0.2)',
            color: '#f87171',
          }}
        >
          {t('skills.load_error')}: {error}
        </div>
      </div>
    );
  }

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div
          className="h-8 w-8 border-2 rounded-full animate-spin"
          style={{ borderColor: 'var(--pc-border)', borderTopColor: 'var(--pc-accent)' }}
        />
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6 animate-fade-in">
      {/* Header row */}
      <div className="flex items-center justify-between gap-4 flex-wrap">
        <div className="flex items-center gap-3 flex-1 flex-wrap">
          <select
            value={selectedAlias}
            onChange={(e) => setSelectedAlias(e.target.value)}
            className="input-electric px-3 py-2.5 text-sm"
            aria-label={t('skills.agent')}
            title={t('skills.agent')}
          >
            {agents.map((a) => (
              <option key={a} value={a}>
                {a}
              </option>
            ))}
          </select>

          <div className="relative max-w-md flex-1">
            <Search
              className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4"
              style={{ color: 'var(--pc-text-faint)' }}
            />
            <input
              type="text"
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              placeholder={t('skills.search')}
              className="input-electric w-full pl-10 pr-4 py-2.5 text-sm"
            />
          </div>
        </div>

        <button
          onClick={handleReload}
          disabled={reloading}
          className="btn-electric flex items-center gap-2 px-4 py-2 text-sm"
          style={{ opacity: reloading ? 0.6 : 1 }}
          title={t('skills.reload')}
        >
          <RefreshCw className={`h-4 w-4 ${reloading ? 'animate-spin' : ''}`} />
          {t('skills.reload')}
        </button>
      </div>

      {/* Section header */}
      <div className="flex items-center gap-2">
        <BookOpen className="h-5 w-5" style={{ color: 'var(--pc-accent)' }} />
        <span
          className="text-sm font-semibold uppercase tracking-wider"
          style={{ color: 'var(--pc-text-primary)' }}
        >
          {t('skills.title')} ({filtered.length})
        </span>
      </div>

      {/* Dropped-skill warning — surfaces skills the resolver skipped during
          the security audit, so an empty list isn't mistaken for "none configured". */}
      {dropped.length > 0 && (
        <div
          className="rounded-lg p-3 text-sm"
          style={{
            background: 'rgba(251, 191, 36, 0.1)',
            borderLeft: '4px solid #fbbf24',
            color: '#fbbf24',
          }}
        >
          {dropped.length} {t('skills.skipped_count')}
          <ul className="mt-2 space-y-1">
            {dropped.map((d) => (
              <li
                key={`${d.origin}/${d.name}`}
                className="text-xs"
                style={{ color: 'var(--pc-text-muted)' }}
              >
                <span className="font-mono">{d.name}</span> ({d.origin}) — {d.reason}
              </li>
            ))}
          </ul>
        </div>
      )}

      {/* Empty state — only when there are no skills AND nothing was dropped;
          the dropped banner above already explains the "all skipped" case. */}
      {filtered.length === 0 && dropped.length === 0 && (
        <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
          {t('skills.empty')}
        </p>
      )}

      {/* Skill cards */}
      <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-4 stagger-children">
        {filtered.map((skill) => {
          const key = skillKey(skill);
          const isExpanded = expandedKey === key;
          const detail = detailMap[key];

          return (
            <SkillCard
              key={key}
              skill={skill}
              onExpand={skill.editable ? handleExpand : undefined}
              isExpanded={isExpanded}
              skillDetail={detail}
            />
          );
        })}
      </div>
    </div>
  );
}
