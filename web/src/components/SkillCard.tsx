import { t } from '@/lib/i18n';
import type { AgentSkillEntry, SkillDocument } from '@/lib/api';
import {
  BookOpen,
  ChevronDown,
  ChevronRight,
  Pencil,
} from 'lucide-react';

interface SkillCardProps {
  skill: AgentSkillEntry;
  skillDetail?: SkillDocument;
  /** Only wired up for bundle skills (`skill.editable`). */
  onExpand?: (skill: AgentSkillEntry) => void;
  isExpanded: boolean;
}

/** Short, display-ready origin label for the badge. */
function originLabel(skill: AgentSkillEntry): string {
  switch (skill.origin) {
    case 'plugin':
      return skill.plugin ? `plugin:${skill.plugin}` : 'plugin';
    case 'bundle':
      return skill.bundle ?? 'bundle';
    default:
      return skill.origin;
  }
}

export const SkillCard = ({ skill, onExpand, isExpanded, skillDetail }: SkillCardProps) => {
  // Only bundle skills can be expanded for detail / edited; other origins are
  // read-only list entries (#6700 read-only browser).
  const canExpand = skill.editable && !!onExpand;

  const header = (
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
      {canExpand &&
        (isExpanded ? (
          <ChevronDown
            className="h-4 w-4 shrink-0"
            style={{ color: 'var(--pc-accent)' }}
          />
        ) : (
          <ChevronRight
            className="h-4 w-4 shrink-0"
            style={{ color: 'var(--pc-text-faint)' }}
          />
        ))}
    </div>
  );

  const description = skill.description && (
    <p
      className="text-sm mt-2 line-clamp-2"
      style={{ color: 'var(--pc-text-muted)' }}
    >
      {skill.description}
    </p>
  );

  return (
    <div
      className="card overflow-hidden animate-slide-in-up flex flex-col justify-between"
    >
      {/* Card header — expand trigger (bundle skills only) */}
      {canExpand ? (
        <button
          onClick={() => onExpand?.(skill)}
          className="w-full text-left p-4 transition-all h-full flex flex-col"
          style={{ background: 'transparent' }}
          onMouseEnter={(e) => {
            e.currentTarget.style.background = 'var(--pc-hover)';
          }}
          onMouseLeave={(e) => {
            e.currentTarget.style.background = 'transparent';
          }}
        >
          {header}
          {description}
        </button>
      ) : (
        <div className="w-full text-left p-4 h-full flex flex-col">
          {header}
          {description}
        </div>
      )}

      {/* Origin / meta row */}
      <div
        className="flex items-center gap-2 px-4 py-3 border-t"
        style={{ borderColor: 'var(--pc-border)' }}
      >
        <span
          className="text-[10px] font-mono truncate"
          style={{ color: 'var(--pc-text-faint)' }}
        >
          {originLabel(skill)}
        </span>
        {skill.shadowed && skill.shadowed.length > 0 && (
          <span
            className="text-[10px] font-semibold px-2 py-0.5 rounded shrink-0"
            style={{ background: 'rgba(251, 146, 60, 0.15)', color: '#f97316' }}
            title={skill.shadowed
              .map((s) => `${s.origin}:${s.name}`)
              .join(', ')}
          >
            {t('skills.shadows')} {skill.shadowed.map((s) => s.origin).join(', ')}
          </span>
        )}
        {skill.editable && (
          <Pencil
            className="h-3 w-3 shrink-0 ml-auto"
            style={{ color: 'var(--pc-accent)' }}
            aria-label={t('skills.editable')}
          />
        )}
      </div>

      {/* Expanded detail (bundle skills only) */}
      {canExpand && isExpanded && (
        <div
          className="border-t p-4 space-y-3 animate-fade-in"
          style={{ borderColor: 'var(--pc-border)' }}
        >
          {skillDetail?.frontmatter.version && (
            <div className="flex gap-2 text-xs" style={{ color: 'var(--pc-text-muted)' }}>
              <span className="font-semibold" style={{ color: 'var(--pc-text-secondary)' }}>v</span>
              {skillDetail.frontmatter.version}
            </div>
          )}
          {skillDetail?.frontmatter.author && (
            <div className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
              {skillDetail.frontmatter.author}
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
