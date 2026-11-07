import { useCallback, useEffect, useRef, useState } from 'react';
import { useSearchParams } from 'react-router-dom';
import { getProviderRegistry, saveProviderRegistry, type ProviderRegistryDocument } from '@/lib/api';
import { t } from '@/lib/i18n';
import { useConfigDraft } from '@/lib/draftStore';
import { Button } from '@/components/ui';

const sections = ['connections', 'models', 'model_presets', 'fallback'] as const;
type Section = typeof sections[number];

type RegistrySections = Record<Section, string>;

function sectionDraft(document: ProviderRegistryDocument): RegistrySections {
  return Object.fromEntries(
    sections.map((key) => [key, JSON.stringify(document[key], null, 2)]),
  ) as RegistrySections;
}

/** Edit one atomic registry document so model/preset/connection references can
 * change together. The server validates the typed candidate and preserves
 * unchanged masked credentials; the client never reconstructs dotted paths. */
export default function ProviderRegistryEditor({ onSaved }: { onSaved: () => void }) {
  const [params] = useSearchParams();
  const selectedModel = params.get('model');
  const [section, setSection] = useState<Section>(selectedModel ? 'models' : 'connections');
  const [error, setError] = useState('');
  const [saving, setSaving] = useState(false);
  const loadedThisMount = useRef(false);
  const {
    providerRegistryDraft,
    setProviderRegistryDraft,
    clearProviderRegistryDraft,
  } = useConfigDraft();

  const load = useCallback(async () => {
    try {
      const document = await getProviderRegistry();
      setProviderRegistryDraft({
        expected: document,
        sections: sectionDraft(document),
        dirty: false,
      });
      setError('');
    } catch (cause) {
      setError(String(cause));
    }
  }, [setProviderRegistryDraft]);

  useEffect(() => {
    if (loadedThisMount.current) return;
    loadedThisMount.current = true;
    if (!providerRegistryDraft?.dirty) void load();
  }, [load, providerRegistryDraft?.dirty]);
  useEffect(() => { if (selectedModel) setSection('models'); }, [selectedModel]);

  const save = async () => {
    if (!providerRegistryDraft) return;
    setSaving(true);
    try {
      const registrySections = providerRegistryDraft.sections as RegistrySections;
      const candidate = Object.fromEntries(
        sections.map((key) => [key, JSON.parse(registrySections[key])]),
      ) as ProviderRegistryDocument;
      await saveProviderRegistry(providerRegistryDraft.expected, candidate);
      setProviderRegistryDraft({
        expected: candidate,
        sections: sectionDraft(candidate),
        dirty: false,
      });
      setError('');
      onSaved();
    } catch (cause) { setError(String(cause)); }
    finally { setSaving(false); }
  };

  const reload = () => {
    clearProviderRegistryDraft();
    setError('');
    void load();
  };

  const registry = providerRegistryDraft;
  const drafts = registry?.sections as RegistrySections | undefined;

  return <div className="flex flex-col gap-4 min-h-0">
    <p className="text-sm text-pc-text-muted">{t('config.registry_help')}</p>
    {selectedModel && <p className="font-mono text-sm break-all">{selectedModel}</p>}
    <div className="flex flex-wrap gap-2" role="tablist">
      {sections.map((key) => <Button key={key} role="tab" aria-selected={section === key} variant={section === key ? 'primary' : 'ghost'} onClick={() => setSection(key)}>{t(`config.registry_${key}`)}</Button>)}
    </div>
    {error && <p role="alert" className="text-status-error whitespace-pre-wrap">{error}</p>}
    {registry && drafts && <textarea
      aria-label={t(`config.registry_${section}`)}
      className="w-full min-h-96 resize-y rounded border border-pc-border bg-pc-base text-pc-text p-3 font-mono text-sm"
      value={drafts[section]}
      disabled={saving}
      spellCheck={false}
      onChange={(event) => setProviderRegistryDraft({
        expected: registry.expected,
        sections: { ...drafts, [section]: event.target.value },
        dirty: true,
      })}
    />}
    <div className="flex gap-2">
      <Button disabled={!drafts || saving} onClick={() => void save()}>{t('config.registry_save')}</Button>
      <Button variant="ghost" disabled={saving} onClick={reload}>{t('config.registry_reload')}</Button>
    </div>
  </div>;
}
