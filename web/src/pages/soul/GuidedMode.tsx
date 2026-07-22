/**
 * Guided mode — structured soul authoring form with a live preview of the
 * generated SOUL.md + IDENTITY.md (web/src/lib/soulTemplates.ts).
 */

import { useMemo, useState } from 'react';
import { Link } from 'react-router-dom';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { Check, Loader2, Mic, Plus, Sparkles, Trash2 } from 'lucide-react';
import {
  PersonalityConflictError,
  getPersonalityFile,
  putPersonalityFile,
} from '@/lib/api';
import {
  RELATIONSHIP_LABELS,
  defaultSoulSpec,
  generateIdentityMd,
  generateSoulMd,
  type RelationshipPreset,
  type SoulSpec,
} from '@/lib/soulTemplates';
import {
  AxisSlider,
  Chip,
  ErrorNote,
  S,
  SectionCard,
  TextArea,
  TextInput,
  Toggle,
} from './studioUi';

const VALUE_PRESETS = [
  'Honesty',
  'Curiosity',
  'Loyalty',
  'Craftsmanship',
  'Kindness',
  'Directness',
  'Humor',
  'Privacy',
  'Calm',
  'Ambition',
];

const RELATIONSHIP_PRESETS: RelationshipPreset[] = [
  'companion',
  'chief_of_staff',
  'coach',
  'friend',
];

interface Props {
  agent: string;
  /** ?seed= from the welcome wizard, if present. */
  seed: string | null;
  /** ?name= from the welcome wizard, if present. */
  seedName?: string | null;
}

export default function GuidedMode({ agent, seed, seedName }: Props) {
  const [spec, setSpec] = useState<SoulSpec>(() => {
    const base = defaultSoulSpec();
    if (seed) {
      base.essence = seed;
      base.seed = seed;
    }
    if (seedName) base.name = seedName;
    return base;
  });
  const [customValue, setCustomValue] = useState('');
  const [rawPreview, setRawPreview] = useState(false);
  const [previewFile, setPreviewFile] = useState<'SOUL.md' | 'IDENTITY.md'>('SOUL.md');
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [conflictAsk, setConflictAsk] = useState<null | {
    resume: () => void;
    cancel: () => void;
  }>(null);

  const soulMd = useMemo(() => generateSoulMd(spec), [spec]);
  const identityMd = useMemo(() => generateIdentityMd(spec), [spec]);
  const previewText = previewFile === 'SOUL.md' ? soulMd : identityMd;

  const patch = (p: Partial<SoulSpec>) => {
    setSaved(false);
    setSpec((prev) => ({ ...prev, ...p }));
  };

  const toggleValue = (v: string) => {
    patch({
      values: spec.values.includes(v)
        ? spec.values.filter((x) => x !== v)
        : [...spec.values, v],
    });
  };

  const addCustomValue = () => {
    const v = customValue.trim();
    if (!v || spec.values.includes(v)) return;
    patch({ values: [...spec.values, v] });
    setCustomValue('');
  };

  /**
   * Write one file: fetch the current mtime, PUT with it. On a 409 (someone
   * changed the file between fetch and save, or another editor is open) ask
   * the user before overwriting.
   */
  const writeFile = async (filename: string, content: string) => {
    const current = await getPersonalityFile(filename, agent);
    try {
      await putPersonalityFile(filename, content, current.mtime_ms, agent);
    } catch (e) {
      if (e instanceof PersonalityConflictError) {
        const conflictMtime = e.conflict.current_mtime_ms;
        const overwrite = await new Promise<boolean>((resolve) => {
          setConflictAsk({
            resume: () => resolve(true),
            cancel: () => resolve(false),
          });
        });
        setConflictAsk(null);
        if (!overwrite) throw new Error(`${filename}: save cancelled`);
        await putPersonalityFile(filename, content, conflictMtime, agent);
      } else {
        throw e;
      }
    }
  };

  const onSave = async () => {
    setSaving(true);
    setError(null);
    setSaved(false);
    try {
      await writeFile('SOUL.md', soulMd);
      await writeFile('IDENTITY.md', identityMd);
      setSaved(true);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="grid gap-4 xl:grid-cols-[minmax(0,1fr)_minmax(0,1fr)]">
      {/* ── Form column ─────────────────────────────────────────── */}
      <div className="flex min-w-0 flex-col gap-4">
        <SectionCard title="Name & essence" hint="Who is this being, in a breath?">
          <div className="flex flex-col gap-2">
            <TextInput
              value={spec.name}
              onChange={(v) => patch({ name: v })}
              placeholder="Name — e.g. Iris"
            />
            <TextInput
              value={spec.essence}
              onChange={(v) => patch({ essence: v })}
              placeholder="Essence, one line — e.g. A dry-witted night owl who runs this house."
            />
            <TextInput
              value={spec.userName}
              onChange={(v) => patch({ userName: v })}
              placeholder="Your name (who they belong to) — optional"
            />
          </div>
        </SectionCard>

        <SectionCard title="Temperament" hint="Slide each axis toward the pole that feels right.">
          <div className="flex flex-col gap-4">
            <AxisSlider
              left="Warm"
              right="Dry"
              value={spec.temperament.warmDry}
              onChange={(v) => patch({ temperament: { ...spec.temperament, warmDry: v } })}
            />
            <AxisSlider
              left="Playful"
              right="Serious"
              value={spec.temperament.playfulSerious}
              onChange={(v) => patch({ temperament: { ...spec.temperament, playfulSerious: v } })}
            />
            <AxisSlider
              left="Concise"
              right="Expansive"
              value={spec.temperament.conciseExpansive}
              onChange={(v) => patch({ temperament: { ...spec.temperament, conciseExpansive: v } })}
            />
            <AxisSlider
              left="Bold"
              right="Careful"
              value={spec.temperament.boldCareful}
              onChange={(v) => patch({ temperament: { ...spec.temperament, boldCareful: v } })}
            />
          </div>
        </SectionCard>

        <SectionCard title="Values" hint="What they hold dear. Pick chips or add your own.">
          <div className="flex flex-wrap gap-2">
            {VALUE_PRESETS.map((v) => (
              <Chip
                key={v}
                label={v}
                selected={spec.values.includes(v)}
                onClick={() => toggleValue(v)}
              />
            ))}
            {spec.values
              .filter((v) => !VALUE_PRESETS.includes(v))
              .map((v) => (
                <Chip key={v} label={v} selected onRemove={() => toggleValue(v)} />
              ))}
          </div>
          <div className="flex items-center gap-2">
            <TextInput
              value={customValue}
              onChange={setCustomValue}
              placeholder="Add a value…"
              onKeyDown={(e) => {
                if (e.key === 'Enter') {
                  e.preventDefault();
                  addCustomValue();
                }
              }}
            />
            <button
              type="button"
              onClick={addCustomValue}
              className="inline-flex shrink-0 items-center gap-1 rounded-lg border px-3 py-2 text-xs"
              style={{ borderColor: S.border, color: S.muted }}
            >
              <Plus size={13} /> Add
            </button>
          </div>
        </SectionCard>

        <SectionCard
          title="Speech style"
          hint="The voice-first contract is always included; these tune its emphasis."
        >
          <div className="grid gap-2.5 sm:grid-cols-2">
            <Toggle
              label="Short spoken sentences"
              checked={spec.speech.shortSpokenSentences}
              onChange={(v) => patch({ speech: { ...spec.speech, shortSpokenSentences: v } })}
            />
            <Toggle
              label="Contractions"
              checked={spec.speech.contractions}
              onChange={(v) => patch({ speech: { ...spec.speech, contractions: v } })}
            />
            <Toggle
              label="Filler words allowed"
              checked={spec.speech.fillerWordsAllowed}
              onChange={(v) => patch({ speech: { ...spec.speech, fillerWordsAllowed: v } })}
            />
            <Toggle
              label="No lists in voice"
              checked={spec.speech.noListsInVoice}
              onChange={(v) => patch({ speech: { ...spec.speech, noListsInVoice: v } })}
            />
          </div>
        </SectionCard>

        <SectionCard title="Relationship to you" hint="What are they, to you?">
          <div className="flex flex-wrap gap-2">
            {RELATIONSHIP_PRESETS.map((r) => (
              <Chip
                key={r}
                label={RELATIONSHIP_LABELS[r]}
                selected={spec.relationship === r}
                onClick={() => patch({ relationship: r })}
              />
            ))}
          </div>
          <TextArea
            value={spec.relationshipNotes}
            onChange={(v) => patch({ relationshipNotes: v })}
            placeholder="In your own words — e.g. They keep my mornings sane and call me out when I drift."
            rows={2}
          />
        </SectionCard>

        <SectionCard title="Boundaries" hint="Lines they never cross.">
          <TextArea
            value={spec.boundaries}
            onChange={(v) => patch({ boundaries: v })}
            placeholder="e.g. Never share what I tell you with anyone else. Never spend money without asking."
            rows={3}
          />
        </SectionCard>

        <SectionCard title="Quirks & running jokes" hint="Small habits that make them them.">
          <div className="flex flex-col gap-2">
            {spec.quirks.map((q, i) => (
              <div key={i} className="flex items-center gap-2">
                <TextInput
                  value={q}
                  onChange={(v) =>
                    patch({ quirks: spec.quirks.map((x, j) => (j === i ? v : x)) })
                  }
                  placeholder="e.g. Blames every bug on cosmic rays."
                />
                <button
                  type="button"
                  onClick={() => patch({ quirks: spec.quirks.filter((_, j) => j !== i) })}
                  className="shrink-0 rounded-lg border p-2"
                  style={{ borderColor: S.border, color: S.faint }}
                  aria-label="Remove quirk"
                >
                  <Trash2 size={13} />
                </button>
              </div>
            ))}
            <button
              type="button"
              onClick={() => patch({ quirks: [...spec.quirks, ''] })}
              className="inline-flex items-center gap-1 self-start rounded-lg border px-3 py-1.5 text-xs"
              style={{ borderColor: S.border, color: S.muted }}
            >
              <Plus size={13} /> Add a quirk
            </button>
          </div>
        </SectionCard>

        <SectionCard
          title="How you evolve"
          hint="Memory and the nightly dream. The aliveness block is always included."
        >
          <TextArea
            value={spec.evolution}
            onChange={(v) => patch({ evolution: v })}
            rows={3}
          />
        </SectionCard>
      </div>

      {/* ── Preview column ──────────────────────────────────────── */}
      <div className="flex min-w-0 flex-col gap-3 xl:sticky xl:top-4 xl:self-start">
        <div
          className="flex flex-col rounded-xl border"
          style={{ background: S.surface, borderColor: S.border }}
        >
          <div
            className="flex flex-wrap items-center justify-between gap-2 border-b px-4 py-3"
            style={{ borderColor: S.border }}
          >
            <div className="flex items-center gap-1">
              {(['SOUL.md', 'IDENTITY.md'] as const).map((f) => (
                <button
                  key={f}
                  type="button"
                  onClick={() => setPreviewFile(f)}
                  className="rounded-md px-2.5 py-1 font-mono text-xs transition-colors"
                  style={{
                    background: previewFile === f ? S.accentSoft : 'transparent',
                    color: previewFile === f ? S.accent : S.muted,
                  }}
                >
                  {f}
                </button>
              ))}
            </div>
            <button
              type="button"
              onClick={() => setRawPreview((v) => !v)}
              className="rounded-md border px-2.5 py-1 text-xs"
              style={{ borderColor: S.border, color: S.muted }}
            >
              {rawPreview ? 'Rendered' : 'Raw'}
            </button>
          </div>
          <div className="max-h-[70vh] overflow-y-auto px-5 py-4">
            {rawPreview ? (
              <pre
                className="whitespace-pre-wrap break-words font-mono text-xs leading-relaxed"
                style={{ color: S.muted }}
              >
                {previewText}
              </pre>
            ) : (
              <div
                className="prose prose-invert prose-sm max-w-none prose-headings:tracking-tight"
                style={{ color: S.text }}
              >
                <ReactMarkdown remarkPlugins={[remarkGfm]}>{previewText}</ReactMarkdown>
              </div>
            )}
          </div>
        </div>

        {error && <ErrorNote>{error}</ErrorNote>}

        {conflictAsk && (
          <div
            className="flex flex-col gap-3 rounded-xl border p-4 text-sm"
            style={{
              background: 'rgba(245, 158, 11, 0.08)',
              borderColor: 'rgba(245, 158, 11, 0.3)',
              color: S.text,
            }}
          >
            <span>
              This file changed on disk since it was read (another editor may be
              open). Overwrite it with the version generated here?
            </span>
            <div className="flex gap-2">
              <button
                type="button"
                onClick={() => conflictAsk.resume()}
                className="rounded-lg px-3 py-1.5 text-sm font-semibold"
                style={{ background: S.accent, color: '#000' }}
              >
                Overwrite
              </button>
              <button
                type="button"
                onClick={() => conflictAsk.cancel()}
                className="rounded-lg border px-3 py-1.5 text-sm"
                style={{ borderColor: S.border, color: S.muted }}
              >
                Cancel
              </button>
            </div>
          </div>
        )}

        <div className="flex flex-wrap items-center gap-3">
          <button
            type="button"
            disabled={saving}
            onClick={() => void onSave()}
            className="inline-flex items-center gap-2 rounded-lg px-4 py-2 text-sm font-semibold transition-opacity disabled:opacity-60"
            style={{ background: S.accent, color: '#000' }}
          >
            {saving ? (
              <Loader2 size={15} className="animate-spin" />
            ) : (
              <Sparkles size={15} />
            )}
            {saving ? 'Breathing life…' : 'Save soul & identity'}
          </button>
          {saved && (
            <>
              <span
                className="inline-flex items-center gap-1.5 text-sm"
                style={{ color: S.accent }}
              >
                <Check size={15} /> Saved
              </span>
              <Link
                to={agent ? `/face/${encodeURIComponent(agent)}` : '/face'}
                className="inline-flex items-center gap-2 rounded-lg border px-4 py-2 text-sm font-semibold transition-colors"
                style={{ borderColor: S.accentBorder, color: S.accent }}
              >
                <Mic size={15} /> Talk to them →
              </Link>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
