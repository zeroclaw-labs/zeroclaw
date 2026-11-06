import { useEffect, useRef, useState } from 'react';
import { t } from '@/lib/i18n';
import { apiFetch } from '@/lib/api';
import { Button } from '@/components/ui';
import { localRecognitionStatus, recognitionConstructor, saveVoicePreferences, useBrowserVoices, useVoicePreferences, type VoicePreferences } from '@/lib/browserVoice';

interface PairedDevice { id: string; name: string | null; device_type: string | null }
interface DeviceInventory { devices: PairedDevice[]; current_device_id: string | null }

export default function BrowserVoiceSettings() {
  const [inventory, setInventory] = useState<DeviceInventory>({ devices: [], current_device_id: null });
  const [selectedDeviceId, setSelectedDeviceId] = useState<string>();
  const [inventoryLoading, setInventoryLoading] = useState(true);
  const [inventoryError, setInventoryError] = useState('');
  const { preferences: saved, ready, error: loadError, deviceId: loadedDeviceId } = useVoicePreferences(selectedDeviceId);
  const currentDevice = !!selectedDeviceId && selectedDeviceId === loadedDeviceId && loadedDeviceId === inventory.current_device_id;
  useEffect(() => {
    let active = true;
    let pending: AbortController | undefined;
    const load = async () => {
      pending?.abort();
      const controller = new AbortController(); pending = controller;
      setInventoryLoading(true);
      try {
        const result = await apiFetch<DeviceInventory>('/api/devices', { cache: 'no-store', signal: controller.signal });
        if (!Array.isArray(result.devices)) throw new Error('Invalid device inventory');
        if (!active || controller.signal.aborted) return;
        setInventory(result);
        setSelectedDeviceId(previous => result.devices.some(device => device.id === previous) ? previous
          : result.current_device_id ?? result.devices[0]?.id);
        setInventoryError('');
      } catch {
        if (active && !controller.signal.aborted) setInventoryError(t('voice.devices_error'));
      } finally {
        if (active && !controller.signal.aborted) setInventoryLoading(false);
      }
    };
    void load();
    const focused = () => { void load(); };
    window.addEventListener('focus', focused);
    return () => { active = false; pending?.abort(); window.removeEventListener('focus', focused); };
  }, []);
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
    if (!currentDevice) { setLocalStatus('unavailable'); return; }
    setLocalStatus('checking');
    void localRecognitionStatus(prefs.language).then(status => { if (active) setLocalStatus(status); });
    return () => { active = false; };
  }, [prefs.language, currentDevice]);
  const update = async (patch: Partial<typeof prefs>) => {
    const next = { ...prefs, ...patch };
    setDraft(next); setSaving(true);
    try { await saveVoicePreferences(next, selectedDeviceId); setError(''); }
    catch { setError(t('voice.storage_error')); }
    finally { setDraft(null); setSaving(false); }
  };
  const install = async () => {
    setInstalling(true);
    setError('');
    try {
      const ok = await recognitionConstructor()?.install?.({ langs: [prefs.language], processLocally: true });
      if (!ok) setError(t('voice.install_error'));
      setLocalStatus(await localRecognitionStatus(prefs.language));
    } catch { setError(t('voice.install_error')); }
    finally { setInstalling(false); }
  };
  return (
    <div className="text-pc-text">
      <p className="mt-3 max-w-2xl text-sm text-pc-text-muted">{t('voice.help_intro')}</p>
      <label className="grid gap-1 mt-3 max-w-2xl text-sm">{t('voice.device')}
        <select aria-label={t('voice.device')} size={Math.min(5, Math.max(2, inventory.devices.length))}
          className="border border-pc-border rounded p-2 bg-pc-base w-full"
          value={selectedDeviceId ?? ''} disabled={saving || inventoryLoading || !inventory.devices.length}
          onChange={event => { setDraft(null); setError(''); setSelectedDeviceId(event.target.value); }}>
          {!inventory.devices.length && <option value="">{t(inventoryLoading ? 'voice.loading' : 'voice.no_devices')}</option>}
          {inventory.devices.map(device => <option key={device.id} value={device.id}>
            {device.name || device.device_type || t('voice.unnamed_device')} · {device.id.slice(0, 8)}{device.id === inventory.current_device_id ? ` · ${t('voice.current_device')}` : ''}
          </option>)}
        </select>
      </label>
      {inventoryError && <p role="alert">{inventoryError}</p>}
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
      <fieldset disabled={!ready || saving || inventoryLoading || !selectedDeviceId} className="grid gap-3 max-w-2xl mt-3 text-sm">
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
        {currentDevice && <p role="status">{t('voice.local_status')} {t('voice.status_' + localStatus)}</p>}
        {currentDevice && <p>{recognitionConstructor() && window.isSecureContext ? t('voice.browser_available') : t('voice.unsupported')}</p>}
        {prefs.recognition === 'browser' && <p className="text-status-warning">{t('voice.remote_warning')}</p>}
        {currentDevice && (localStatus === 'downloadable' || localStatus === 'downloading') && <Button size="sm" disabled={installing || localStatus === 'downloading'} onClick={() => void install()}>{t('voice.install')}</Button>}
        <label><input type="checkbox" checked={prefs.reading} onChange={e => update({ reading: e.target.checked })} /> {t('voice.reading')}</label>
        <label className="grid gap-1">{t('voice.voice')}
          {currentDevice ? <select aria-label={t('voice.voice')} className="border border-pc-border rounded p-2 bg-pc-base" value={prefs.voice} onChange={e => update({ voice: e.target.value })}>
            <option value="">{t('voice.select_voice')}</option>
            {voices.map(v => <option key={v.voiceURI} value={v.voiceURI}>{v.name} · {v.lang} · {t(v.localService ? 'voice.local' : 'voice.online')}</option>)}
          </select> : <span className="rounded border border-pc-border p-2 break-words">{prefs.voice || t('voice.select_voice')}</span>}
        </label>
        {currentDevice && !voices.length && <p>{t('voice.no_voices')}</p>}
        {!currentDevice && <p>{t('voice.remote_device_resources')}</p>}
        <label>{t('voice.rate')} {prefs.rate}×
          <input className="block w-full" type="range" min="0.5" max="2" step="0.1" value={prefs.rate} onChange={e => update({ rate: Number(e.target.value) })} />
        </label>
        {currentDevice && <Button size="sm" disabled={!prefs.voice || !voices.some(v => v.voiceURI === prefs.voice)} onClick={() => {
          const utterance = new SpeechSynthesisUtterance(t('voice.sample_text'));
          utterance.voice = voices.find(v => v.voiceURI === prefs.voice) || null;
          utterance.lang = prefs.language; utterance.rate = prefs.rate;
          sample.current = utterance;
          utterance.onend = () => { sample.current = null; };
          utterance.onerror = () => { sample.current = null; setError(t('voice.speech_error')); };
          window.speechSynthesis.cancel(); window.speechSynthesis.speak(utterance);
        }}>{t('voice.sample')}</Button>}
        <label><input type="checkbox" checked={prefs.automatic} onChange={e => update({ automatic: e.target.checked })} /> {t('voice.automatic')}</label>
        <p className="text-pc-text-muted">{t('voice.gesture')}</p>
        {error && <p role="alert" className="text-status-error">{error}</p>}
      </fieldset>
      {loadError && <p role="alert">{t(selectedDeviceId ? 'voice.device_load_error' : 'voice.pair_required')}</p>}
    </div>
  );
}
