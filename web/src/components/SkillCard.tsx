import { t } from '@/lib/i18n';
import type { SkillDocument, SkillEntry } from '@/lib/api';
import {
  BookOpen,
  ChevronDown,
  ChevronRight,
} from 'lucide-react';

interface SkillCardProps {
  skill: SkillEntry;
  skillDetail?: SkillDocument;
  onExpand: (skill: SkillEntry) => void;
  isExpanded: boolean;
}

export const SkillCard = ({ skill, onExpand, isExpanded, skillDetail }: SkillCardProps) => {
  const fm = skill.frontmatter;

  return (
    <div
      className="card overflow-hidden animate-slide-in-up flex flex-col justify-between"
    >
      {/* Card header — expand trigger */}
      <button
        onClick={() => onExpand(skill)}
        className="w-full text-left p-4 transition-all h-full flex flex-col"
        style={{ background: 'transparent' }}
        onMouseEnter={(e) => {
          e.currentTarget.style.background = 'var(--pc-hover)';
        }}
        onMouseLeave={(e) => {
          e.currentTarget.style.background = 'transparent';
        }}
      >
        <div className="flex items-start justify-between gap-2">
          <div className="flex items-center gap-2 min-w-0">
            <BookOpen
              className="h-4 w-4 shrink-0"
              style={{ color: 'var(--pc-accent)' }}
            />
            <h3
              className="text-sm font-semibold truncate"
              style={{ color: 'var(--pc-text-primary)' }}
            >
              {skill.name}
            </h3>
          </div>
          {isExpanded ? (
            <ChevronDown
              className="h-4 w-4 shrink-0"
              style={{ color: 'var(--pc-accent)' }}
            />
          ) : (
            <ChevronRight
              className="h-4 w-4 shrink-0"
              style={{ color: 'var(--pc-text-faint)' }}
            />
          )}
        </div>

        {fm.description && (
          <p
            className="text-sm mt-2 line-clamp-2"
            style={{ color: 'var(--pc-text-muted)' }}
          >
            {fm.description}
          </p>
        )}
      </button>

      {/* Bundle / meta row */}
      <div
        className="flex items-center gap-2 px-4 py-3 border-t"
        style={{ borderColor: 'var(--pc-border)' }}
      >
        <span
          className="text-[10px] font-mono truncate"
          style={{ color: 'var(--pc-text-faint)' }}
        >
          {skill.bundle}
        </span>
        {fm.category && (
          <span
            className="text-[10px] font-semibold uppercase tracking-wider ml-auto"
            style={{ color: 'var(--pc-accent)' }}
          >
            {fm.category}
          </span>
        )}
      </div>

      {/* Expanded detail */}
      {isExpanded && (
        <div
          className="border-t p-4 space-y-3 animate-fade-in"
          style={{ borderColor: 'var(--pc-border)' }}
        >
          {fm.version && (
            <div className="flex gap-2 text-xs" style={{ color: 'var(--pc-text-muted)' }}>
              <span className="font-semibold" style={{ color: 'var(--pc-text-secondary)' }}>v</span>
              {fm.version}
            </div>
          )}
          {fm.author && (
            <div className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
              {fm.author}
            </div>
          )}
          {skillDetail?.body && (
            <div>
              <p
                className="text-[10px] font-semibold uppercase tracking-wider mb-2"
                style={{ color: 'var(--pc-text-muted)' }}
              >
                {t('skills.skill_md')}
              </p>
              <pre
                className="text-xs rounded-xl p-3 overflow-x-auto max-h-64 overflow-y-auto font-mono whitespace-pre-wrap"
                style={{ background: 'var(--pc-bg-base)', color: 'var(--pc-text-secondary)' }}
              >
                {skillDetail.body}
              </pre>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
