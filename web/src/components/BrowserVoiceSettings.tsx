import { useEffect, useRef, useState } from 'react';
import { t } from '@/lib/i18n';
import { Button } from '@/components/ui';
import { localRecognitionStatus, recognitionConstructor, saveVoicePreferences, useBrowserVoices, useVoicePreferences, type VoicePreferences } from '@/lib/browserVoice';

export default function BrowserVoiceSettings() {
  const { preferences: saved, ready, error: loadError } = useVoicePreferences();
  const [draft, setDraft] = useState<VoicePreferences | null>(null);
  const prefs = draft ?? saved;
  const [saving, setSaving] = useState(false);
  const voices = useBrowserVoices();
  const [localStatus, setLocalStatus] = useState('checking');
  const [error, setError] = useState('');
  const [installing, setInstalling] = useState(false);
  const sample = useRef<SpeechSynthesisUtterance | null>(null);
  useEffect(() => () => { if (sample.current) window.speechSynthesis.cancel(); }, []);
  useEffect(() => {
    let active = true;
    setLocalStatus('checking');
    void localRecognitionStatus(prefs.language).then(status => { if (active) setLocalStatus(status); });
    return () => { active = false; };
  }, [prefs.language]);
  const update = async (patch: Partial<typeof prefs>) => {
    const next = { ...prefs, ...patch };
    setDraft(next); setSaving(true);
    try { await saveVoicePreferences(next); setError(''); }
    catch { setError(t('voice.storage_error')); }
    finally { setDraft(null); setSaving(false); }
  };
  const install = async () => {
    setInstalling(true);
    try {
      const ok = await recognitionConstructor()?.install?.({ langs: [prefs.language] });
      if (!ok) setError(t('voice.install_error'));
      setLocalStatus(await localRecognitionStatus(prefs.language));
    } catch { setError(t('voice.install_error')); }
    finally { setInstalling(false); }
  };
  return (
    <details className="shrink-0 max-h-[70vh] overflow-y-auto border-b border-pc-border p-3 text-pc-text bg-pc-surface">
      <summary className="cursor-pointer font-medium">{t('voice.settings')}</summary>
      <p className="mt-3 max-w-2xl text-sm text-pc-text-muted">{t('voice.help_intro')}</p>
      <details className="mt-3 max-w-2xl rounded-lg border border-pc-border p-3 text-sm">
        <summary className="cursor-pointer font-medium">{t('voice.help_title')}</summary>
        <ol className="list-decimal pl-5 mt-3 space-y-3">
          <li>{t('voice.help_setup')}</li>
          <li>{t('voice.help_microphone')}</li>
          <li>{t('voice.help_once')}</li>
          <li>{t('voice.help_continuous')}</li>
          <li>{t('voice.help_default')}</li>
        </ol>
        <p className="mt-3">{t('voice.help_privacy')}</p>
        <p className="mt-3">{t('voice.help_limits')}</p>
        <p className="mt-3 text-pc-text-muted">{t('voice.help_identity')}</p>
      </details>
      <fieldset disabled={!ready || saving} className="grid gap-3 max-w-2xl mt-3 text-sm">
        <p className="text-pc-text-muted">{t('voice.device_note')}</p>
        <label><input type="checkbox" checked={prefs.dictation} onChange={e => update({ dictation: e.target.checked })} /> {t('voice.dictation')}</label>
        <label className="grid gap-1">{t('voice.language')}
          <select className="border border-pc-border rounded p-2 bg-pc-base" value={prefs.language} onChange={e => void update({ language: e.target.value })}>
            {Array.from(new Set([prefs.language, 'en-US', 'en-GB', 'pt-BR', 'pt-PT', 'es-ES', 'fr-FR', 'de-DE'])).map(lang => <option key={lang} value={lang}>{lang}</option>)}
          </select>
        </label>
        <label className="grid gap-1">{t('voice.recognition')}
          <select className="border border-pc-border rounded p-2 bg-pc-base" value={prefs.recognition} onChange={e => update({ recognition: e.target.value as 'local' | 'browser' })}>
            <option value="local">{t('voice.local')}</option>
            <option value="browser">{t('voice.browser')}</option>
          </select>
        </label>
        <p role="status">{t('voice.local_status')} {t('voice.status_' + localStatus)}</p>
        <p>{recognitionConstructor() && window.isSecureContext ? t('voice.browser_available') : t('voice.unsupported')}</p>
        {prefs.recognition === 'browser' && <p className="text-status-warning">{t('voice.remote_warning')}</p>}
        {(localStatus === 'downloadable' || localStatus === 'downloading') && <Button size="sm" disabled={installing || localStatus === 'downloading'} onClick={() => void install()}>{t('voice.install')}</Button>}
        <label><input type="checkbox" checked={prefs.reading} onChange={e => update({ reading: e.target.checked })} /> {t('voice.reading')}</label>
        <label className="grid gap-1">{t('voice.voice')}
          <select aria-label={t('voice.voice')} className="border border-pc-border rounded p-2 bg-pc-base" value={prefs.voice} onChange={e => update({ voice: e.target.value })}>
            <option value="">{t('voice.select_voice')}</option>
            {voices.map(v => <option key={v.voiceURI} value={v.voiceURI}>{v.name} · {v.lang} · {t(v.localService ? 'voice.local' : 'voice.online')}</option>)}
          </select>
        </label>
        {!voices.length && <p>{t('voice.no_voices')}</p>}
        <label>{t('voice.rate')} {prefs.rate}×
          <input className="block w-full" type="range" min="0.5" max="2" step="0.1" value={prefs.rate} onChange={e => update({ rate: Number(e.target.value) })} />
        </label>
        <Button size="sm" disabled={!prefs.voice || !voices.some(v => v.voiceURI === prefs.voice)} onClick={() => {
          const utterance = new SpeechSynthesisUtterance(t('voice.sample_text'));
          utterance.voice = voices.find(v => v.voiceURI === prefs.voice) || null;
          utterance.lang = prefs.language; utterance.rate = prefs.rate;
          sample.current = utterance;
          utterance.onend = () => { sample.current = null; };
          utterance.onerror = () => { sample.current = null; setError(t('voice.speech_error')); };
          window.speechSynthesis.cancel(); window.speechSynthesis.speak(utterance);
        }}>{t('voice.sample')}</Button>
        <label><input type="checkbox" checked={prefs.automatic} onChange={e => update({ automatic: e.target.checked })} /> {t('voice.automatic')}</label>
        <p className="text-pc-text-muted">{t('voice.gesture')}</p>
        {error && <p role="alert" className="text-status-error">{error}</p>}
      </fieldset>
      {loadError && <p role="alert">{t('voice.pair_required')}</p>}
    </details>
  );
}
