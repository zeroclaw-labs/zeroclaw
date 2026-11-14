import { useCallback, useEffect, useRef, useState } from 'react';
import { useSearchParams } from 'react-router-dom';
import { getProviderRegistry, type ProviderRegistryDocument } from '@/lib/api';
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
  const requestedSection = selectedModel
    ? 'models'
    : sections.find((key) => key === params.get('tab'));
  const [section, setSection] = useState<Section>(requestedSection ?? 'connections');
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(false);
  const editVersion = useRef(0);
  const loadedThisMount = useRef(false);
  const {
    providerRegistryDraft,
    providerRegistrySaveState,
    saveProviderRegistryDraft,
    setProviderRegistryDraft,
  } = useConfigDraft();
  const saving = providerRegistrySaveState === 'saving';
  const refreshRequired = providerRegistrySaveState === 'refresh-required';

  const load = useCallback(async () => {
    const versionAtStart = editVersion.current;
    setLoading(true);
    try {
      const document = await getProviderRegistry();
      // A reload is allowed to replace the current draft only when the user
      // did not type while the request was in flight.
      if (editVersion.current !== versionAtStart) return false;
      setProviderRegistryDraft({
        expected: document,
        sections: sectionDraft(document),
        dirty: false,
      });
      setError('');
      return true;
    } catch (cause) {
      setError(String(cause));
      throw cause;
    } finally {
      setLoading(false);
    }
  }, [setProviderRegistryDraft]);

  useEffect(() => {
    if (!loadedThisMount.current) {
      loadedThisMount.current = true;
      // A dirty draft belongs to the shared store and must survive remounts.
      if (!providerRegistryDraft?.dirty) void load().catch(() => undefined);
      return;
    }
    // Discard all clears the shared draft while this editor stays mounted.
    // Re-read the masked server document so the editor remains usable.
    if (!providerRegistryDraft) void load().catch(() => undefined);
  }, [load, providerRegistryDraft]);
  useEffect(() => { if (requestedSection) setSection(requestedSection); }, [requestedSection, selectedModel]);

  const save = async () => {
    try {
      await saveProviderRegistryDraft();
      setError('');
      onSaved();
    } catch (cause) { setError(String(cause)); }
  };

  const reload = () => {
    setError('');
    void load().catch(() => undefined);
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
      disabled={saving || loading || refreshRequired}
      spellCheck={false}
      onChange={(event) => {
        if (saving || loading || refreshRequired) return;
        editVersion.current += 1;
        setProviderRegistryDraft({
          expected: registry.expected,
          sections: { ...drafts, [section]: event.target.value },
          dirty: true,
        });
      }}
    />}
    <div className="flex gap-2">
      <Button disabled={!drafts || saving || loading || refreshRequired} onClick={() => void save()}>{t('config.registry_save')}</Button>
      <Button variant="ghost" disabled={saving || loading} onClick={reload}>{t('config.registry_reload')}</Button>
    </div>
  </div>;
}
