import { useState, useEffect, useCallback } from 'react';
import { Sparkles, Search, ChevronDown, ChevronRight, Download, Trash2, ShieldCheck } from 'lucide-react';
import { getSkills, installSkill, removeSkill, auditSkill } from '../lib/api';
import type { SkillsListResponse } from '../types/api';
import { t } from '@/lib/i18n';

export default function Skills() {
  const [data, setData] = useState<SkillsListResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [installSource, setInstallSource] = useState('');
  const [installing, setInstalling] = useState(false);
  const [expandedSkill, setExpandedSkill] = useState<string | null>(null);
  const [search, setSearch] = useState('');

  const load = useCallback(async () => {
    try {
      setLoading(true);
      const result = await getSkills();
      setData(result);
      setError(null);
    } catch (e: any) {
      setError(e.message);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { load(); }, [load]);

  const handleInstall = async () => {
    if (!installSource.trim()) return;
    setInstalling(true);
    try {
      await installSkill(installSource.trim());
      setInstallSource('');
      await load();
    } catch (e: any) {
      setError(e.message);
    } finally {
      setInstalling(false);
    }
  };

  const handleRemove = async (name: string) => {
    if (!confirm(`Remove skill "${name}"?`)) return;
    try {
      await removeSkill(name);
      await load();
    } catch (e: any) {
      setError(e.message);
    }
  };

  const handleAudit = async (name: string) => {
    try {
      const result = await auditSkill(name);
      alert(
        result.is_clean
          ? `✅ ${name}: Clean (${result.files_scanned} files scanned)`
          : `⚠️ ${name}: ${result.findings.join(', ')}`
      );
    } catch (e: any) {
      setError(e.message);
    }
  };

  const filteredSkills = data?.skills.filter((skill) =>
    skill.name.toLowerCase().includes(search.toLowerCase()) ||
    skill.description.toLowerCase().includes(search.toLowerCase()),
  ) ?? [];

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="h-8 w-8 border-2 rounded-full animate-spin" style={{ borderColor: 'var(--pc-border)', borderTopColor: 'var(--pc-accent)' }} />
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6 animate-fade-in">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <Sparkles className="h-6 w-6" style={{ color: 'var(--pc-accent)' }} />
          <div>
            <h1 className="text-xl font-bold" style={{ color: 'var(--pc-text-primary)' }}>{t('nav.skills')}</h1>
            <p className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
              {data?.total ?? 0} {t('skills.installed')}
              {data?.open_skills_enabled && ` · ${t('skills.open_enabled')}`}
            </p>
          </div>
        </div>
      </div>

      {/* Search and Install */}
      <div className="flex gap-3">
        <div className="relative flex-1 max-w-md">
          <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4" style={{ color: 'var(--pc-text-faint)' }} />
          <input
            type="text"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder={t('skills.search')}
            className="input-electric w-full pl-10 pr-4 py-2.5 text-sm"
          />
        </div>
        <div className="flex gap-2 flex-1 max-w-xl">
          <input
            type="text"
            value={installSource}
            onChange={(e) => setInstallSource(e.target.value)}
            placeholder={t('skills.install_placeholder')}
            className="input-electric flex-1 px-3 py-2.5 text-sm"
            onKeyDown={(e) => e.key === 'Enter' && handleInstall()}
          />
          <button
            onClick={handleInstall}
            disabled={installing || !installSource.trim()}
            className="btn-electric px-4 py-2.5 text-sm font-medium flex items-center gap-2 disabled:opacity-50"
          >
            <Download className="h-4 w-4" />
            {installing ? t('skills.installing') : t('skills.install')}
          </button>
        </div>
      </div>

      {/* Error */}
      {error && (
        <div className="rounded-2xl border p-4 animate-fade-in" style={{ background: 'rgba(239, 68, 68, 0.08)', borderColor: 'rgba(239, 68, 68, 0.2)', color: '#f87171' }}>
          <div className="flex items-start justify-between gap-3">
            <span className="text-sm">{error}</span>
            <button
              onClick={() => setError(null)}
              className="text-xs font-medium hover:underline"
              style={{ color: '#f87171' }}
            >
              {t('common.dismiss')}
            </button>
          </div>
        </div>
      )}

      {/* Skills List */}
      {filteredSkills.length === 0 ? (
        <div className="text-center py-12">
          <Sparkles className="h-12 w-12 mx-auto mb-4" style={{ color: 'var(--pc-text-faint)' }} />
          <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
            {search ? t('skills.no_match') : t('skills.no_skills')}
          </p>
        </div>
      ) : (
        <div className="grid grid-cols-1 gap-4 stagger-children">
          {filteredSkills.map((skill) => {
            const isExpanded = expandedSkill === skill.name;
            return (
              <div
                key={skill.name}
                className="card overflow-hidden animate-slide-in-up"
              >
                <button
                  onClick={() => setExpandedSkill(isExpanded ? null : skill.name)}
                  className="w-full text-left p-4 transition-all"
                  style={{ background: 'transparent' }}
                  onMouseEnter={(e) => { e.currentTarget.style.background = 'var(--pc-hover)'; }}
                  onMouseLeave={(e) => { e.currentTarget.style.background = 'transparent'; }}
                >
                  <div className="flex items-start justify-between gap-3">
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-2 flex-wrap">
                        <h3 className="text-sm font-semibold" style={{ color: 'var(--pc-text-primary)' }}>{skill.name}</h3>
                        <span className="inline-flex items-center px-2 py-0.5 rounded-full text-[10px] font-semibold border" style={{ borderColor: 'var(--pc-border)', color: 'var(--pc-text-secondary)', background: 'var(--pc-accent-glow)' }}>
                          v{skill.version}
                        </span>
                        {skill.always && (
                          <span className="inline-flex items-center px-2 py-0.5 rounded-full text-[10px] font-semibold border" style={{ borderColor: 'var(--pc-accent-dim)', color: 'var(--pc-accent-light)', background: 'var(--pc-accent-glow)' }}>
                            {t('skills.always')}
                          </span>
                        )}
                      </div>
                      <p className="text-sm mt-1 line-clamp-2" style={{ color: 'var(--pc-text-muted)' }}>
                        {skill.description}
                      </p>
                      <div className="flex items-center gap-3 mt-2 text-xs" style={{ color: 'var(--pc-text-faint)' }}>
                        <span>{skill.tools.length} {t('skills.tools')}</span>
                        <span>{skill.prompts_count} {t('skills.prompts')}</span>
                        {skill.author && <span>· {t('skills.author')}: {skill.author}</span>}
                      </div>
                    </div>
                    <div className="flex items-center gap-2">
                      {isExpanded
                        ? <ChevronDown className="h-4 w-4" style={{ color: 'var(--pc-accent)' }} />
                        : <ChevronRight className="h-4 w-4" style={{ color: 'var(--pc-text-faint)' }} />
                      }
                    </div>
                  </div>
                </button>

                {isExpanded && (
                  <div className="border-t p-4 animate-fade-in" style={{ borderColor: 'var(--pc-border)', background: 'var(--pc-bg-base)' }}>
                    {skill.location && (
                      <div className="mb-3">
                        <p className="text-[10px] font-semibold uppercase tracking-wider mb-1" style={{ color: 'var(--pc-text-muted)' }}>
                          {t('skills.location')}
                        </p>
                        <code className="text-xs font-mono block truncate" style={{ color: 'var(--pc-text-secondary)' }}>
                          {skill.location}
                        </code>
                      </div>
                    )}

                    {skill.tools.length > 0 && (
                      <div className="mb-4">
                        <p className="text-[10px] font-semibold uppercase tracking-wider mb-2" style={{ color: 'var(--pc-text-muted)' }}>
                          {t('skills.tools')}
                        </p>
                        <div className="space-y-1.5">
                          {skill.tools.map((tool) => (
                            <div key={tool.name} className="flex items-center gap-2 text-sm">
                              <code className="px-2 py-0.5 rounded text-xs font-mono" style={{ background: 'var(--pc-accent-glow)', color: 'var(--pc-text-secondary)', border: '1px solid var(--pc-border)' }}>
                                {tool.name}
                              </code>
                              <span className="inline-flex items-center px-1.5 py-0.5 rounded text-[10px] font-semibold capitalize border" style={{ borderColor: 'var(--pc-border)', color: 'var(--pc-text-secondary)', background: 'var(--pc-bg-elevated)' }}>
                                {tool.kind}
                              </span>
                              <span className="text-xs truncate" style={{ color: 'var(--pc-text-muted)' }}>{tool.description}</span>
                            </div>
                          ))}
                        </div>
                      </div>
                    )}

                    <div className="flex gap-2">
                      <button
                        onClick={() => handleAudit(skill.name)}
                        className="btn-electric px-3 py-1.5 text-xs font-medium flex items-center gap-1.5"
                      >
                        <ShieldCheck className="h-3.5 w-3.5" />
                        {t('skills.audit')}
                      </button>
                      <button
                        onClick={() => handleRemove(skill.name)}
                        className="px-3 py-1.5 text-xs font-medium flex items-center gap-1.5 rounded-xl border transition-all"
                        style={{ borderColor: 'var(--pc-border)', color: '#f87171', background: 'transparent' }}
                        onMouseEnter={(e) => { e.currentTarget.style.background = 'rgba(239, 68, 68, 0.08)'; e.currentTarget.style.borderColor = 'rgba(239, 68, 68, 0.3)'; }}
                        onMouseLeave={(e) => { e.currentTarget.style.background = 'transparent'; e.currentTarget.style.borderColor = 'var(--pc-border)'; }}
                      >
                        <Trash2 className="h-3.5 w-3.5" />
                        {t('skills.remove')}
                      </button>
                    </div>
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
