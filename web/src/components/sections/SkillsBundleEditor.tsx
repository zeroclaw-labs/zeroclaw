// Drill-in editor for a single skill-bundle: list the skills in the
// bundle's resolved directory, scaffold new ones (strict-spec: name +
// description required), open a selected skill in a CodeMirror SKILL.md
// editor, and archive a skill on demand.
//
// Lazy-load pattern mirrors PersonalityEditor — list endpoint hydrates
// the picker, individual SKILL.md content is fetched on selection.

import { useCallback, useEffect, useRef, useState } from 'react';
import { t } from '@/lib/i18n';
import { markdown } from '@codemirror/lang-markdown';
import { oneDark } from '@codemirror/theme-one-dark';
import { githubLight } from '@uiw/codemirror-theme-github';
import CodeMirror from '@uiw/react-codemirror';
import { useLocation } from 'react-router-dom';
import { useTheme } from '@/hooks/useTheme';
import {
  createSkill,
  deleteSkill,
  listSkillsInBundle,
  listSlashOptionKinds,
  readSkill,
  writeSkill,
  type SkillEntry,
  type SkillFrontmatter,
  type SkillSlashChoice,
  type SkillSlashOption,
  type SlashOptionKindDescriptor,
} from '../../lib/api';

interface Props {
  bundle: string;
}

interface EditorBuffer {
  loaded: { frontmatter: SkillFrontmatter; body: string };
  draft: { frontmatter: SkillFrontmatter; body: string };
}

export default function SkillsBundleEditor({ bundle }: Props) {
  const location = useLocation();
  const requestedSkill = new URLSearchParams(location.search).get('skill');
  const appliedRequestedSkill = useRef<string | null>(null);
  const shouldFocusRequestedSkill = useRef(false);
  const editorRef = useRef<HTMLDivElement | null>(null);

  // Match the SKILL.md editor to the active console scheme — a dark CodeMirror
  // theme inside a light palette is the light-mode bug we're fixing.
  // `resolvedTheme` is 'dark' | 'light' | 'oled'; only 'light' is a light scheme.
  const { resolvedTheme } = useTheme();
  const cmTheme = resolvedTheme === 'light' ? githubLight : oneDark;

  const [skills, setSkills] = useState<SkillEntry[]>([]);
  const [active, setActive] = useState<string | null>(null);
  const [buffer, setBuffer] = useState<EditorBuffer | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [creating, setCreating] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [newName, setNewName] = useState('');
  const [newDescription, setNewDescription] = useState('');

  const loadList = useCallback(async () => {
    try {
      const resp = await listSkillsInBundle(bundle);
      setSkills(resp.skills);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [bundle]);

  useEffect(() => {
    void loadList();
  }, [loadList]);

  useEffect(() => {
    if (!requestedSkill) return;
    const requestedSkillKey = `${bundle}:${requestedSkill}`;
    if (!skills.some((s) => s.name === requestedSkill)) {
      setActive(null);
      setBuffer(null);
      return;
    }
    if (appliedRequestedSkill.current === requestedSkillKey) return;
    appliedRequestedSkill.current = requestedSkillKey;
    shouldFocusRequestedSkill.current = true;
    setActive(requestedSkill);
  }, [bundle, requestedSkill, skills]);

  useEffect(() => {
    if (!buffer || !shouldFocusRequestedSkill.current) return;
    shouldFocusRequestedSkill.current = false;
    editorRef.current?.focus();
  }, [buffer]);

  const loadActive = useCallback(async () => {
    if (!active) return;
    setError(null);
    setConfirmDelete(false);
    try {
      const doc = await readSkill(bundle, active);
      const loaded = { frontmatter: doc.frontmatter, body: doc.body };
      setBuffer({ loaded, draft: structuredClone(loaded) });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setBuffer(null);
    }
  }, [active, bundle]);

  useEffect(() => {
    void loadActive();
  }, [loadActive]);

  const dirty =
    buffer !== null && JSON.stringify(buffer.draft) !== JSON.stringify(buffer.loaded);

  const onSave = async () => {
    if (!active || !buffer) return;
    setBusy(true);
    setError(null);
    try {
      await writeSkill(bundle, active, buffer.draft);
      setBuffer({ loaded: structuredClone(buffer.draft), draft: buffer.draft });
      void loadList();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const onCreate = async () => {
    const name = newName.trim();
    const description = newDescription.trim();
    if (!name) {
      setError(t('skills_bundle.name_required'));
      return;
    }
    if (!description) {
      setError(t('skills_bundle.description_required'));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await createSkill(bundle, {
        name,
        frontmatter: {
          name,
          description,
          version: '0.1.0',
        },
        body: '',
      });
      setNewName('');
      setNewDescription('');
      setCreating(false);
      await loadList();
      setActive(name);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const onDelete = async () => {
    if (!active) return;
    setBusy(true);
    setError(null);
    try {
      await deleteSkill(bundle, active, false);
      setBuffer(null);
      setActive(null);
      setConfirmDelete(false);
      await loadList();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const setFrontmatterField = (field: keyof SkillFrontmatter, value: string) => {
    setBuffer((prev) =>
      prev
        ? {
            ...prev,
            draft: {
              ...prev.draft,
              frontmatter: {
                ...prev.draft.frontmatter,
                [field]: value || (field === 'name' || field === 'description' ? '' : null),
              },
            },
          }
        : prev,
    );
  };

  const setTags = (tags: string[]) => {
    setBuffer((prev) => {
      if (!prev) return prev;
      const frontmatter = { ...prev.draft.frontmatter, tags };
      // Dropping the `slash` tag retires the command, so clear its now-orphaned
      // typed options: the options editor is gated on the tag, so otherwise they
      // would persist invisibly and silently reappear if the tag is re-added.
      if (!tags.includes('slash')) frontmatter.slash_options = [];
      return { ...prev, draft: { ...prev.draft, frontmatter } };
    });
  };

  const setSlashOptions = (slash_options: SkillSlashOption[]) => {
    setBuffer((prev) =>
      prev
        ? {
            ...prev,
            draft: {
              ...prev.draft,
              frontmatter: { ...prev.draft.frontmatter, slash_options },
            },
          }
        : prev,
    );
  };

  return (
    <div className="flex flex-col gap-3">
      <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
        {t('skills_bundle.intro_before_skill_md')}
        <code>SKILL.md</code> {t('skills_bundle.intro_after_skill_md')}{' '}
        <code> scripts/</code>, <code>references/</code>,{' '}
        {t('skills_bundle.intro_and')} <code>assets/</code>{' '}
        {t('skills_bundle.intro_subdirs')}
      </p>

      {/* Skill picker strip */}
      <div className="flex flex-wrap gap-2 items-center">
        {skills.map((s) => (
          <button
            key={s.name}
            type="button"
            aria-pressed={s.name === active}
            onClick={() => setActive(s.name)}
            className="text-xs px-3 py-1.5 rounded-lg border transition-colors"
            style={{
              borderColor: 'var(--pc-border)',
              background: s.name === active ? 'var(--pc-accent-glow)' : 'transparent',
              color: s.name === active ? 'var(--pc-accent)' : 'var(--pc-text-secondary)',
              fontWeight: s.name === active ? 600 : 400,
            }}
          >
            {s.name}
          </button>
        ))}
        {!creating && (
          <button
            type="button"
            onClick={() => {
              setCreating(true);
              setError(null);
            }}
            className="text-xs px-3 py-1.5 rounded-lg border-dashed border"
            style={{ borderColor: 'var(--pc-border)', color: 'var(--pc-text-muted)' }}
          >
            {t('skills_bundle.new_skill')}
          </button>
        )}
        {skills.length === 0 && !creating && (
          <span className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
            {t('skills_bundle.no_skills_installed')}
          </span>
        )}
      </div>

      {error && (
        <div
          className="rounded-lg border p-3 text-sm"
          style={{
            background: 'rgba(239, 68, 68, 0.08)',
            borderColor: 'rgba(239, 68, 68, 0.2)',
            color: '#f87171',
          }}
        >
          {error}
        </div>
      )}

      {/* Create form */}
      {creating && (
        <div
          className="rounded-xl border p-4 flex flex-col gap-3"
          style={{ borderColor: 'var(--pc-border)', background: 'var(--pc-bg-surface)' }}
        >
          <div className="flex flex-col gap-1">
            <label className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
              {t('skills_bundle.name_label_create')}
            </label>
            <input
              type="text"
              value={newName}
              onChange={(e) => setNewName(e.target.value)}
              placeholder={t('skills_bundle.name_placeholder')}
              className="rounded-md border bg-transparent px-3 py-1.5 text-sm"
              style={{ borderColor: 'var(--pc-border)' }}
            />
          </div>
          <div className="flex flex-col gap-1">
            <label className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
              {t('skills_bundle.description_label_create')}
            </label>
            <textarea
              value={newDescription}
              onChange={(e) => setNewDescription(e.target.value)}
              rows={3}
              placeholder={t('skills_bundle.description_placeholder')}
              className="rounded-md border bg-transparent px-3 py-1.5 text-sm font-mono"
              style={{ borderColor: 'var(--pc-border)' }}
            />
          </div>
          <div className="flex gap-2">
            <button
              type="button"
              disabled={busy}
              onClick={() => void onCreate()}
              className="btn-primary text-sm"
            >
              {t('skills_bundle.create_skill')}
            </button>
            <button
              type="button"
              onClick={() => {
                setCreating(false);
                setNewName('');
                setNewDescription('');
                setError(null);
              }}
              className="btn-secondary text-sm"
            >
              {t('common.cancel')}
            </button>
          </div>
        </div>
      )}

      {/* Editor */}
      {active && buffer && (
        <div
          ref={editorRef}
          tabIndex={-1}
          aria-live="polite"
          aria-label={`${t('common.edit')} ${active}`}
          className="flex flex-col gap-3 focus:outline-none"
        >
          <FrontmatterForm
            value={buffer.draft.frontmatter}
            onChange={setFrontmatterField}
            onTagsChange={setTags}
          />
          {(buffer.draft.frontmatter.tags ?? []).includes('slash') && (
            <SlashOptionsEditor
              options={buffer.draft.frontmatter.slash_options ?? []}
              onChange={setSlashOptions}
            />
          )}
          <div
            className="rounded-xl border overflow-hidden"
            style={{ borderColor: 'var(--pc-border)' }}
          >
            <CodeMirror
              value={buffer.draft.body}
              height="320px"
              theme={cmTheme}
              extensions={[markdown()]}
              onChange={(v) =>
                setBuffer((prev) =>
                  prev ? { ...prev, draft: { ...prev.draft, body: v } } : prev,
                )
              }
            />
          </div>
          <div className="flex items-center gap-2">
            <button
              type="button"
              disabled={!dirty || busy}
              onClick={() => void onSave()}
              className="btn-primary text-sm"
            >
              {busy ? t('skills_bundle.saving') : t('common.save')}
            </button>
            <button
              type="button"
              disabled={!dirty || busy}
              onClick={() =>
                setBuffer((prev) =>
                  prev ? { ...prev, draft: structuredClone(prev.loaded) } : prev,
                )
              }
              className="btn-secondary text-sm"
            >
              {t('skills_bundle.discard')}
            </button>
            <div className="flex-1" />
            {!confirmDelete ? (
              <button
                type="button"
                onClick={() => setConfirmDelete(true)}
                className="btn-secondary text-sm"
                style={{ color: '#f87171' }}
              >
                {t('skills_bundle.archive_skill')}
              </button>
            ) : (
              <>
                <span className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
                  {t('skills_bundle.move_to_prefix')}{' '}
                  <code>shared/skills/_deleted/</code>{t('skills_bundle.move_to_suffix')}
                </span>
                <button
                  type="button"
                  disabled={busy}
                  onClick={() => void onDelete()}
                  className="btn-primary text-sm"
                  style={{ background: '#dc2626' }}
                >
                  {t('skills_bundle.confirm_archive')}
                </button>
                <button
                  type="button"
                  onClick={() => setConfirmDelete(false)}
                  className="btn-secondary text-sm"
                >
                  {t('common.cancel')}
                </button>
              </>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

interface FrontmatterFormProps {
  value: SkillFrontmatter;
  onChange: (field: keyof SkillFrontmatter, value: string) => void;
  onTagsChange: (tags: string[]) => void;
}

function FrontmatterForm({ value, onChange, onTagsChange }: FrontmatterFormProps) {
  return (
    <div
      className="rounded-xl border p-4 grid gap-3 md:grid-cols-2"
      style={{ borderColor: 'var(--pc-border)', background: 'var(--pc-bg-surface)' }}
    >
      <Field
        label={t('skills_bundle.name_label_required')}
        value={value.name}
        onChange={(v) => onChange('name', v)}
      />
      <Field
        label={t('skills_bundle.version_label')}
        value={value.version ?? ''}
        onChange={(v) => onChange('version', v)}
        placeholder="0.1.0"
      />
      <div className="md:col-span-2 flex flex-col gap-1">
        <label className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
          {t('skills_bundle.description_label_required')}
        </label>
        <textarea
          value={value.description}
          onChange={(e) => onChange('description', e.target.value)}
          rows={2}
          className="rounded-md border bg-transparent px-3 py-1.5 text-sm font-mono"
          style={{ borderColor: 'var(--pc-border)' }}
        />
      </div>
      <Field
        label={t('skills_bundle.license_label')}
        value={value.license ?? ''}
        onChange={(v) => onChange('license', v)}
        placeholder="MIT"
      />
      <Field
        label={t('skills_bundle.author_label')}
        value={value.author ?? ''}
        onChange={(v) => onChange('author', v)}
      />
      <Field
        label={t('skills_bundle.category_label')}
        value={value.category ?? ''}
        onChange={(v) => onChange('category', v)}
        placeholder="coding, ops, …"
      />
      <TagsField tags={value.tags ?? []} onTagsChange={onTagsChange} />
    </div>
  );
}

interface TagsFieldProps {
  tags: string[];
  onTagsChange: (tags: string[]) => void;
}

/**
 * Tags editor + the slash-command opt-in. The `slash` tag is surfaced as a
 * boolean toggle (it makes the skill a Discord slash command — see
 * zeroclaw-labs/zeroclaw#7490); `open-skills` is loader-managed and shown
 * read-only. Everything else is an editable badge. The full tag list (including
 * `slash`/`open-skills`) is preserved on save.
 */
function TagsField({ tags, onTagsChange }: TagsFieldProps) {
  const [tagInput, setTagInput] = useState('');
  const slashOn = tags.includes('slash');
  const isOpenSkills = tags.includes('open-skills');
  const editableTags = tags.filter((t) => t !== 'slash' && t !== 'open-skills');

  const setSlash = (on: boolean) =>
    onTagsChange(on ? [...tags, 'slash'] : tags.filter((t) => t !== 'slash'));
  const removeTag = (tag: string) => onTagsChange(tags.filter((t) => t !== tag));
  const addTag = () => {
    const next = tagInput.trim().toLowerCase();
    setTagInput('');
    if (!next || next === 'slash' || next === 'open-skills' || tags.includes(next)) return;
    onTagsChange([...tags, next]);
  };

  return (
    <div
      className="md:col-span-2 flex flex-col gap-2 border-t pt-3"
      style={{ borderColor: 'var(--pc-border)' }}
    >
      <label className="flex items-center gap-2 cursor-pointer select-none">
        <input
          type="checkbox"
          checked={slashOn}
          onChange={(e) => setSlash(e.target.checked)}
        />
        <span className="text-sm" style={{ color: 'var(--pc-text-secondary)' }}>
          Slash command
        </span>
        <span className="text-xs" style={{ color: 'var(--pc-text-faint)' }}>
          — expose this skill as a <code>/command</code> in Discord (adds the{' '}
          <code>slash</code> tag)
        </span>
      </label>
      <div className="flex flex-col gap-1">
        <label className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
          Tags
        </label>
        <div className="flex flex-wrap items-center gap-1.5">
          {editableTags.map((tag) => (
            <span
              key={tag}
              className="inline-flex items-center gap-1 text-xs px-2 py-0.5 rounded-md border"
              style={{ borderColor: 'var(--pc-border)', color: 'var(--pc-text-secondary)' }}
            >
              {tag}
              <button
                type="button"
                onClick={() => removeTag(tag)}
                aria-label={`Remove tag ${tag}`}
                className="leading-none"
                style={{ color: 'var(--pc-text-muted)' }}
              >
                ×
              </button>
            </span>
          ))}
          {isOpenSkills && (
            <span
              className="inline-flex items-center text-xs px-2 py-0.5 rounded-md border opacity-60"
              title="Loader-managed: community-synced skill (open-skills)"
              style={{ borderColor: 'var(--pc-border)', color: 'var(--pc-text-faint)' }}
            >
              open-skills
            </span>
          )}
          <input
            type="text"
            value={tagInput}
            onChange={(e) => setTagInput(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') {
                e.preventDefault();
                addTag();
              }
            }}
            placeholder="add tag…"
            aria-label="Add tag"
            className="text-xs bg-transparent border rounded-md px-2 py-0.5 w-24"
            style={{ borderColor: 'var(--pc-border)', color: 'var(--pc-text)' }}
          />
        </div>
      </div>
    </div>
  );
}

interface FieldProps {
  label: string;
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
}

function Field({ label, value, onChange, placeholder }: FieldProps) {
  return (
    <div className="flex flex-col gap-1">
      <label className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
        {label}
      </label>
      <input
        type="text"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder}
        className="rounded-md border bg-transparent px-3 py-1.5 text-sm"
        style={{ borderColor: 'var(--pc-border)' }}
      />
    </div>
  );
}

// The typed slash-option model (zeroclaw-labs/zeroclaw#8021), shaped after
// Discord's application command option types. The kind list and which
// constraints each kind carries (choices / numeric bounds / length bounds) are
// NOT restated here: the editor fetches the backend registry
// (`listSlashOptionKinds`, built by walking the backend `SlashOptionKind` enum)
// and walks it. The Discord layer drops bounds/choices that don't match the
// type and sorts required options first, so the editor only gates which inputs
// are *offered* per kind; it does not enforce order.

interface SlashOptionsEditorProps {
  options: SkillSlashOption[];
  onChange: (options: SkillSlashOption[]) => void;
}

/**
 * Bespoke editor for a `slash`-tagged skill's typed slash-command options.
 * The flat frontmatter form can't express this nested list, so it lives here
 * (the data model is round-tripped by the runtime; see
 * SkillFrontmatter::slash_options). Only rendered when the `slash` tag is on.
 */
function SlashOptionsEditor({ options, onChange }: SlashOptionsEditorProps) {
  const [kinds, setKinds] = useState<SlashOptionKindDescriptor[]>([]);
  useEffect(() => {
    let live = true;
    listSlashOptionKinds()
      .then((r) => {
        if (live) setKinds(r.kinds);
      })
      .catch(() => {
        if (live) setKinds([]);
      });
    return () => {
      live = false;
    };
  }, []);

  const update = (i: number, patch: Partial<SkillSlashOption>) =>
    onChange(options.map((o, idx) => (idx === i ? { ...o, ...patch } : o)));
  const remove = (i: number) => onChange(options.filter((_, idx) => idx !== i));
  const move = (i: number, dir: -1 | 1) => {
    const j = i + dir;
    if (j < 0 || j >= options.length) return;
    const next = options.slice();
    const a = next[i];
    const b = next[j];
    if (a === undefined || b === undefined) return;
    next[i] = b;
    next[j] = a;
    onChange(next);
  };
  const add = () => {
    const first = kinds[0]?.manifest_name;
    if (first === undefined) return;
    onChange([
      ...options,
      { name: '', description: '', type: first, required: false },
    ]);
  };

  return (
    <div
      className="rounded-xl border p-4 flex flex-col gap-3"
      style={{ borderColor: 'var(--pc-border)', background: 'var(--pc-bg-surface)' }}
    >
      <div className="flex items-center justify-between">
        <div className="flex flex-col">
          <span className="text-sm" style={{ color: 'var(--pc-text-secondary)' }}>
            Slash command options
          </span>
          <span className="text-xs" style={{ color: 'var(--pc-text-faint)' }}>
            Typed parameters this <code>/command</code> accepts. With none, the
            skill runs with a single free-text argument.
          </span>
        </div>
        <button
          type="button"
          onClick={add}
          disabled={kinds.length === 0}
          className="btn-secondary text-xs whitespace-nowrap disabled:opacity-40"
        >
          + Add option
        </button>
      </div>

      {options.length === 0 ? (
        <p className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
          No options yet.
        </p>
      ) : (
        <div className="flex flex-col gap-3">
          {options.map((opt, i) => (
            <SlashOptionCard
              key={i}
              option={opt}
              index={i}
              count={options.length}
              kinds={kinds}
              onChange={(patch) => update(i, patch)}
              onRemove={() => remove(i)}
              onMove={(dir) => move(i, dir)}
            />
          ))}
        </div>
      )}
    </div>
  );
}

interface SlashOptionCardProps {
  option: SkillSlashOption;
  index: number;
  count: number;
  kinds: SlashOptionKindDescriptor[];
  onChange: (patch: Partial<SkillSlashOption>) => void;
  onRemove: () => void;
  onMove: (dir: -1 | 1) => void;
}

function SlashOptionCard({
  option,
  index,
  count,
  kinds,
  onChange,
  onRemove,
  onMove,
}: SlashOptionCardProps) {
  const kind = kinds.find((k) => k.manifest_name === option.type);
  const isNumeric = kind?.supports_numeric_bounds ?? false;
  const isString = kind?.supports_length_bounds ?? false;
  const hasChoices = kind?.supports_choices ?? false;

  // Switching to a kind that no longer supports a constraint clears it, so the
  // saved frontmatter never carries a bound the channel would silently drop.
  // The capability flags come from the registry, not a hardcoded per-type map.
  const onType = (type: string) => {
    const next = kinds.find((k) => k.manifest_name === type);
    const patch: Partial<SkillSlashOption> = { type };
    if (!(next?.supports_numeric_bounds ?? false)) {
      patch.min = null;
      patch.max = null;
    }
    if (!(next?.supports_length_bounds ?? false)) {
      patch.min_length = null;
      patch.max_length = null;
    }
    if (!(next?.supports_choices ?? false)) patch.choices = [];
    onChange(patch);
  };

  return (
    <div
      className="rounded-lg border p-3 flex flex-col gap-2"
      style={{ borderColor: 'var(--pc-border)' }}
    >
      <div className="flex flex-wrap items-end gap-2">
        <div className="flex flex-col gap-1 flex-1 min-w-[8rem]">
          <label className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
            Name
          </label>
          <input
            type="text"
            value={option.name}
            onChange={(e) => onChange({ name: e.target.value })}
            placeholder="lowercase_name"
            aria-label={`Option ${index + 1} name`}
            className="rounded-md border bg-transparent px-2 py-1 text-sm"
            style={{ borderColor: 'var(--pc-border)' }}
          />
        </div>
        <div className="flex flex-col gap-1">
          <label className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
            Type
          </label>
          <select
            value={option.type}
            onChange={(e) => onType(e.target.value)}
            aria-label={`Option ${index + 1} type`}
            className="rounded-md border bg-transparent px-2 py-1 text-sm"
            style={{ borderColor: 'var(--pc-border)', color: 'var(--pc-text)' }}
          >
            {kinds.map((k) => (
              <option
                key={k.manifest_name}
                value={k.manifest_name}
                style={{ color: '#000', background: '#fff' }}
              >
                {k.manifest_name}
              </option>
            ))}
          </select>
        </div>
        <label className="flex items-center gap-1.5 cursor-pointer select-none pb-1.5">
          <input
            type="checkbox"
            checked={option.required ?? false}
            onChange={(e) => onChange({ required: e.target.checked })}
          />
          <span className="text-xs" style={{ color: 'var(--pc-text-secondary)' }}>
            Required
          </span>
        </label>
        <div className="flex-1" />
        <div className="flex items-center gap-1 pb-1">
          <button
            type="button"
            onClick={() => onMove(-1)}
            disabled={index === 0}
            aria-label={`Move option ${index + 1} up`}
            className="text-xs px-1.5 py-0.5 rounded border disabled:opacity-30"
            style={{ borderColor: 'var(--pc-border)', color: 'var(--pc-text-muted)' }}
          >
            ↑
          </button>
          <button
            type="button"
            onClick={() => onMove(1)}
            disabled={index === count - 1}
            aria-label={`Move option ${index + 1} down`}
            className="text-xs px-1.5 py-0.5 rounded border disabled:opacity-30"
            style={{ borderColor: 'var(--pc-border)', color: 'var(--pc-text-muted)' }}
          >
            ↓
          </button>
          <button
            type="button"
            onClick={onRemove}
            aria-label="Remove option"
            className="text-xs px-1.5 py-0.5 rounded border"
            style={{ borderColor: 'var(--pc-border)', color: '#f87171' }}
          >
            ×
          </button>
        </div>
      </div>

      <div className="flex flex-col gap-1">
        <label className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
          Description
        </label>
        <input
          type="text"
          value={option.description}
          onChange={(e) => onChange({ description: e.target.value })}
          placeholder="Shown to the user when picking this option"
          aria-label={`Option ${index + 1} description`}
          className="rounded-md border bg-transparent px-2 py-1 text-sm"
          style={{ borderColor: 'var(--pc-border)' }}
        />
      </div>

      {isNumeric && (
        <div className="flex flex-wrap gap-2">
          <NumField
            label="Min"
            value={option.min}
            onChange={(v) => onChange({ min: v })}
          />
          <NumField
            label="Max"
            value={option.max}
            onChange={(v) => onChange({ max: v })}
          />
        </div>
      )}

      {isString && (
        <div className="flex flex-wrap gap-2">
          <NumField
            label="Min length"
            value={option.min_length}
            onChange={(v) => onChange({ min_length: clampLen(v) })}
            integer
          />
          <NumField
            label="Max length"
            value={option.max_length}
            onChange={(v) => onChange({ max_length: clampLen(v) })}
            integer
          />
        </div>
      )}

      {hasChoices && (
        <ChoicesEditor
          choices={option.choices ?? []}
          onChange={(choices) => onChange({ choices })}
        />
      )}
    </div>
  );
}

// Length bounds are u32 on the backend; keep them non-negative integers.
function clampLen(v: number | null): number | null {
  if (v === null) return null;
  return Math.max(0, Math.round(v));
}

interface ChoicesEditorProps {
  choices: SkillSlashChoice[];
  onChange: (choices: SkillSlashChoice[]) => void;
}

/** Predefined choices (a dropdown in Discord). `value` is the text handed to
 *  the skill; `name` is what the user sees. */
function ChoicesEditor({ choices, onChange }: ChoicesEditorProps) {
  const update = (i: number, patch: Partial<SkillSlashChoice>) =>
    onChange(choices.map((c, idx) => (idx === i ? { ...c, ...patch } : c)));
  const remove = (i: number) => onChange(choices.filter((_, idx) => idx !== i));
  const add = () => onChange([...choices, { name: '', value: '' }]);

  return (
    <div
      className="flex flex-col gap-1.5 border-t pt-2"
      style={{ borderColor: 'var(--pc-border)' }}
    >
      <div className="flex items-center justify-between">
        <label className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
          Choices <span style={{ color: 'var(--pc-text-faint)' }}>(optional, a fixed dropdown)</span>
        </label>
        <button
          type="button"
          onClick={add}
          className="text-xs px-2 py-0.5 rounded border"
          style={{ borderColor: 'var(--pc-border)', color: 'var(--pc-text-muted)' }}
        >
          + Choice
        </button>
      </div>
      {choices.map((c, i) => (
        <div key={i} className="flex items-center gap-2">
          <input
            type="text"
            value={c.name}
            onChange={(e) => update(i, { name: e.target.value })}
            placeholder="label"
            aria-label={`Choice ${i + 1} label`}
            className="rounded-md border bg-transparent px-2 py-1 text-xs flex-1"
            style={{ borderColor: 'var(--pc-border)' }}
          />
          <span className="text-xs" style={{ color: 'var(--pc-text-faint)' }}>
            →
          </span>
          <input
            type="text"
            value={c.value}
            onChange={(e) => update(i, { value: e.target.value })}
            placeholder="value"
            aria-label={`Choice ${i + 1} value`}
            className="rounded-md border bg-transparent px-2 py-1 text-xs flex-1 font-mono"
            style={{ borderColor: 'var(--pc-border)' }}
          />
          <button
            type="button"
            onClick={() => remove(i)}
            aria-label={`Remove choice ${i + 1}`}
            className="text-xs leading-none"
            style={{ color: 'var(--pc-text-muted)' }}
          >
            ×
          </button>
        </div>
      ))}
    </div>
  );
}

interface NumFieldProps {
  label: string;
  value: number | null | undefined;
  onChange: (value: number | null) => void;
  integer?: boolean;
}

// A clearable numeric input: empty string maps to null (the option carries no
// bound), any parseable number maps through. `integer` switches the step hint.
function NumField({ label, value, onChange, integer }: NumFieldProps) {
  return (
    <div className="flex flex-col gap-1">
      <label className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
        {label}
      </label>
      <input
        type="number"
        step={integer ? 1 : 'any'}
        value={value ?? ''}
        onChange={(e) => {
          const raw = e.target.value;
          if (raw === '') {
            onChange(null);
            return;
          }
          const n = Number(raw);
          // Reject NaN and +/-Infinity (the backend bound is a finite f64).
          onChange(Number.isFinite(n) ? n : null);
        }}
        className="rounded-md border bg-transparent px-2 py-1 text-sm w-24"
        style={{ borderColor: 'var(--pc-border)' }}
      />
    </div>
  );
}
