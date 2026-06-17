import { SkillCard } from '@/components/SkillCard';
import { listSkillBundles, listSkillsInBundle, readSkill } from '@/lib/api';
import { t } from '@/lib/i18n';
import type { SkillBundleEntry, SkillDocument, SkillEntry } from '@/lib/api';
import {
  BookOpen,
  RefreshCw,
  Search
} from 'lucide-react';
import { useCallback, useEffect, useState } from 'react';

export default function Skills() {
  const [bundles, setBundles] = useState<SkillBundleEntry[]>([]);
  const [skillsByBundle, setSkillsByBundle] = useState<Record<string, SkillEntry[]>>({});
  const [search, setSearch] = useState('');
  const [loading, setLoading] = useState(true);
  const [reloading, setReloading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [expandedKey, setExpandedKey] = useState<string | null>(null);
  const [detailMap, setDetailMap] = useState<Record<string, SkillDocument>>({});

  const loadBundles = useCallback(() => {
    return listSkillBundles()
      .then(({ bundles: bs }) => {
        setBundles(bs);
        return Promise.all(
          bs.map((b) =>
            listSkillsInBundle(b.alias).then(({ skills }) => ({ alias: b.alias, skills })),
          ),
        );
      })
      .then((results) => {
        const map: Record<string, SkillEntry[]> = {};
        for (const { alias, skills } of results) {
          map[alias] = skills;
        }
        setSkillsByBundle(map);
      });
  }, []);

  const fetchAll = useCallback(() => {
    setLoading(true);
    setError(null);
    loadBundles()
      .catch((err: unknown) => setError(err instanceof Error ? err.message : String(err)))
      .finally(() => setLoading(false));
  }, [loadBundles]);

  useEffect(() => {
    fetchAll();
  }, [fetchAll]);

  const handleReload = () => {
    setReloading(true);
    loadBundles()
      .catch((err: unknown) => setError(err instanceof Error ? err.message : String(err)))
      .finally(() => setReloading(false));
  };

  const handleExpand = (skill: SkillEntry) => {
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

  const allSkills = bundles.flatMap((b) => skillsByBundle[b.alias] ?? []);

  const filtered = allSkills.filter((s) => {
    const q = search.toLowerCase();
    return (
      s.name.toLowerCase().includes(q) ||
      s.frontmatter.description.toLowerCase().includes(q) ||
      s.bundle.toLowerCase().includes(q) ||
      (s.frontmatter.category ?? '').toLowerCase().includes(q)
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

      {/* Empty state */}
      {filtered.length === 0 && (
        <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
          {t('skills.empty')}
        </p>
      )}

      {/* Skill cards */}
      <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-4 stagger-children">
        {filtered.map((skill) => {
          const key = `${skill.bundle}/${skill.name}`;
          const isExpanded = expandedKey === key;
          const detail = detailMap[key];

          return (
            <SkillCard
              key={key}
              skill={skill}
              onExpand={handleExpand}
              isExpanded={isExpanded}
              skillDetail={detail}
            />
          );
        })}
      </div>
    </div>
  );
}
