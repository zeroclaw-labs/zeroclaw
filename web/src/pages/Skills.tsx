import { useState, useEffect, useCallback } from 'react';
import { Sparkles, Check, FileCode, Package, Download, Trash2, Tag, Wrench, AlertCircle } from 'lucide-react';
import { getSkills, installSkill, removeSkill } from '@/lib/api';
import { t } from '@/lib/i18n';

interface Skill {
  name: string;
  version: string;
  description: string;
  tools: string[];
  tags: string[];
}

type SkillStatus = 'active' | 'installed' | 'script';

function skillStatus(skill: Skill): SkillStatus {
  if (skill.tools.length > 0) return 'active';
  return 'installed';
}

function statusBadge(status: SkillStatus) {
  switch (status) {
    case 'active':
      return {
        icon: Check,
        label: 'Active',
        color: 'var(--color-status-success)',
        border: 'rgba(0, 230, 138, 0.2)',
        bg: 'rgba(0, 230, 138, 0.06)',
      };
    case 'installed':
      return {
        icon: Package,
        label: 'Installed',
        color: 'var(--pc-accent)',
        border: 'var(--pc-accent-dim)',
        bg: 'var(--pc-accent-glow)',
      };
    case 'script':
      return {
        icon: FileCode,
        label: 'Script',
        color: 'var(--pc-text-muted)',
        border: 'var(--pc-border)',
        bg: 'transparent',
      };
  }
}

export default function Skills() {
  const [skills, setSkills] = useState<Skill[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [activeTag, setActiveTag] = useState<string>('all');
  const [installSource, setInstallSource] = useState('');
  const [installing, setInstalling] = useState(false);
  const [installError, setInstallError] = useState<string | null>(null);
  const [removing, setRemoving] = useState<string | null>(null);

  const fetchSkills = useCallback(() => {
    setLoading(true);
    getSkills()
      .then(setSkills)
      .catch((err) => setError(err.message))
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    fetchSkills();
  }, [fetchSkills]);

  const allTags = Array.from(
    new Set(skills.flatMap((s) => s.tags).filter(Boolean)),
  ).sort();
  const categories = ['all', ...allTags];

  const filtered =
    activeTag === 'all'
      ? skills
      : skills.filter((s) => s.tags.includes(activeTag));

  const handleInstall = async () => {
    const source = installSource.trim();
    if (!source) return;
    setInstalling(true);
    setInstallError(null);
    try {
      await installSkill(source);
      setInstallSource('');
      fetchSkills();
    } catch (err: unknown) {
      setInstallError(err instanceof Error ? err.message : 'Install failed');
    } finally {
      setInstalling(false);
    }
  };

  const handleRemove = async (name: string) => {
    setRemoving(name);
    try {
      await removeSkill(name);
      fetchSkills();
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Remove failed');
    } finally {
      setRemoving(null);
    }
  };

  if (error && skills.length === 0) {
    return (
      <div className="p-6 animate-fade-in">
        <div
          className="rounded-2xl border p-6"
          style={{
            background: 'var(--pc-bg-elevated)',
            borderColor: 'var(--pc-border)',
          }}
        >
          <div className="flex items-center gap-2 mb-3">
            <Sparkles className="h-5 w-5" style={{ color: 'var(--pc-accent)' }} />
            <h2
              className="text-sm font-semibold uppercase tracking-wider"
              style={{ color: 'var(--pc-text-primary)' }}
            >
              Skills
            </h2>
          </div>
          <p className="text-sm mb-4" style={{ color: 'var(--pc-text-muted)' }}>
            Skills API not available. Install skills via the command line:
          </p>
          <pre
            className="text-xs rounded-lg p-3 font-mono"
            style={{
              background: 'var(--pc-bg-base)',
              color: 'var(--pc-text-secondary)',
            }}
          >
{`# Install from ClawHub
zeroclaw skills install clawhub:weather
zeroclaw skills install clawhub:memory

# Install from GitHub
zeroclaw skills install https://github.com/user/skill

# List installed
zeroclaw skills list`}
          </pre>
        </div>
      </div>
    );
  }

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div
          className="h-8 w-8 border-2 rounded-full animate-spin"
          style={{
            borderColor: 'var(--pc-border)',
            borderTopColor: 'var(--pc-accent)',
          }}
        />
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6 animate-fade-in">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Sparkles className="h-5 w-5" style={{ color: 'var(--pc-accent)' }} />
          <h2
            className="text-sm font-semibold uppercase tracking-wider"
            style={{ color: 'var(--pc-text-primary)' }}
          >
            Skills ({skills.length})
          </h2>
        </div>
      </div>

      {/* Install bar */}
      <div
        className="rounded-2xl border p-4"
        style={{
          background: 'var(--pc-bg-elevated)',
          borderColor: 'var(--pc-border)',
        }}
      >
        <div className="flex items-center gap-3">
          <Download className="h-4 w-4 shrink-0" style={{ color: 'var(--pc-text-muted)' }} />
          <input
            type="text"
            value={installSource}
            onChange={(e) => setInstallSource(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') handleInstall();
            }}
            placeholder="clawhub:weather or https://github.com/..."
            className="input-electric flex-1 px-3 py-2 text-sm"
          />
          <button
            onClick={handleInstall}
            disabled={installing || !installSource.trim()}
            className="btn-electric px-4 py-2 text-sm font-medium shrink-0 rounded-xl"
            style={{ color: 'white' }}
          >
            {installing ? (
              <span className="flex items-center gap-2">
                <span className="h-3 w-3 border-2 border-white/30 border-t-white rounded-full animate-spin" />
                Installing...
              </span>
            ) : (
              'Install'
            )}
          </button>
        </div>
        {installError && (
          <div
            className="flex items-center gap-2 mt-2 text-xs"
            style={{ color: 'var(--color-status-error)' }}
          >
            <AlertCircle className="h-3 w-3 shrink-0" />
            {installError}
          </div>
        )}
      </div>

      {/* Tag filter tabs */}
      {allTags.length > 0 && (
        <div className="flex flex-wrap gap-2">
          {categories.map((tag) => (
            <button
              key={tag}
              onClick={() => setActiveTag(tag)}
              className="px-3.5 py-1.5 rounded-xl text-xs font-semibold transition-all capitalize"
              style={
                activeTag === tag
                  ? { background: 'var(--pc-accent)', color: 'white' }
                  : {
                      color: 'var(--pc-text-muted)',
                      border: '1px solid var(--pc-border)',
                      background: 'transparent',
                    }
              }
            >
              {tag}
            </button>
          ))}
        </div>
      )}

      {/* Skill cards */}
      {filtered.length === 0 ? (
        <div className="card p-8 text-center">
          <Sparkles
            className="h-10 w-10 mx-auto mb-3"
            style={{ color: 'var(--pc-text-faint)' }}
          />
          <p style={{ color: 'var(--pc-text-muted)' }}>
            No skills installed. Use the install bar above or run{' '}
            <code className="text-xs px-1 py-0.5 rounded" style={{ background: 'var(--pc-bg-base)' }}>
              zeroclaw skills install clawhub:name
            </code>
          </p>
        </div>
      ) : (
        <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-4 stagger-children">
          {filtered.map((skill) => {
            const status = skillStatus(skill);
            const badge = statusBadge(status);
            const BadgeIcon = badge.icon;
            return (
              <div key={skill.name} className="card p-5 animate-slide-in-up group relative">
                <div className="flex items-start justify-between gap-3">
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-2">
                      <h4
                        className="text-sm font-semibold truncate"
                        style={{ color: 'var(--pc-text-primary)' }}
                      >
                        {skill.name}
                      </h4>
                      <span
                        className="text-[10px] font-mono"
                        style={{ color: 'var(--pc-text-faint)' }}
                      >
                        v{skill.version}
                      </span>
                    </div>
                    <p
                      className="text-sm mt-1 line-clamp-2"
                      style={{ color: 'var(--pc-text-muted)' }}
                    >
                      {skill.description}
                    </p>
                  </div>
                  <span
                    className="flex-shrink-0 inline-flex items-center gap-1 px-2.5 py-1 rounded-full text-[10px] font-semibold border"
                    style={badge}
                  >
                    <BadgeIcon className="h-3 w-3" />
                    {badge.label}
                  </span>
                </div>

                {/* Tools */}
                {skill.tools.length > 0 && (
                  <div className="flex items-center gap-1.5 mt-3">
                    <Wrench
                      className="h-3 w-3 shrink-0"
                      style={{ color: 'var(--pc-text-faint)' }}
                    />
                    <span
                      className="text-[10px] truncate"
                      style={{ color: 'var(--pc-text-faint)' }}
                    >
                      {skill.tools.length} tool{skill.tools.length !== 1 ? 's' : ''}:{' '}
                      {skill.tools.slice(0, 3).join(', ')}
                      {skill.tools.length > 3 ? ` +${skill.tools.length - 3}` : ''}
                    </span>
                  </div>
                )}

                {/* Tags */}
                {skill.tags.length > 0 && (
                  <div className="flex items-center gap-1.5 mt-2 flex-wrap">
                    <Tag
                      className="h-3 w-3 shrink-0"
                      style={{ color: 'var(--pc-text-faint)' }}
                    />
                    {skill.tags.map((tag) => (
                      <span
                        key={tag}
                        className="text-[10px] px-1.5 py-0.5 rounded-md"
                        style={{
                          background: 'var(--pc-bg-base)',
                          color: 'var(--pc-text-muted)',
                        }}
                      >
                        {tag}
                      </span>
                    ))}
                  </div>
                )}

                {/* Remove button (hover) */}
                <button
                  onClick={() => handleRemove(skill.name)}
                  disabled={removing === skill.name}
                  className="absolute top-3 right-3 opacity-0 group-hover:opacity-100 transition-all p-1.5 rounded-xl"
                  style={{
                    background: 'var(--pc-bg-base)',
                    border: '1px solid var(--pc-border)',
                    color: 'var(--pc-text-muted)',
                  }}
                  title={`Remove ${skill.name}`}
                >
                  {removing === skill.name ? (
                    <span
                      className="h-3 w-3 border border-current border-t-transparent rounded-full animate-spin block"
                    />
                  ) : (
                    <Trash2 className="h-3 w-3" />
                  )}
                </button>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
