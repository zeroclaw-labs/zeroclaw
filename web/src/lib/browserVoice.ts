import { useEffect, useState } from 'react';
import { apiFetch } from './api';
import { getToken } from './auth';

export interface VoicePreferences {
  dictation: boolean;
  reading: boolean;
  automatic: boolean;
  language: string;
  recognition: 'local' | 'browser';
  voice: string;
  rate: number;
}
const defaults: VoicePreferences = {
  dictation: false, reading: false, automatic: false, language: 'en-US',
  recognition: 'local', voice: '', rate: 1,
};
interface DevicePreferences { device_id: string; preferences: VoicePreferences }
const preferenceEvent = 'zeroclaw-device-voice-updated';
const notificationTopic = 'zeroclaw-device-voice-invalidated';
function notificationChannel(): BroadcastChannel | undefined {
  if (!('BroadcastChannel' in window)) return;
  try { return new window.BroadcastChannel(notificationTopic); }
  catch { console.warn('Voice preference notifications unavailable; settings reload on tab return.'); }
}

export async function saveVoicePreferences(prefs: VoicePreferences): Promise<void> {
  const identity = getToken();
  const result = await apiFetch<DevicePreferences>('/api/devices/me/voice', {
    method: 'PUT', cache: 'no-store', body: JSON.stringify(prefs),
  });
  if (identity !== getToken()) throw new Error('Paired device changed');
  window.dispatchEvent(new CustomEvent(preferenceEvent, { detail: result }));
  const channel = notificationChannel();
  if (channel) {
    // Notify only: other tabs read their authenticated record from ZeroClaw.
    try { channel.postMessage({ device_id: result.device_id }); }
    catch { console.warn('Voice preference notification failed; settings reload on tab return.'); }
    finally { channel.close(); }
  }
}
export function useVoicePreferences() {
  const [state, setState] = useState({ preferences: { ...defaults }, ready: false, error: '' });
  useEffect(() => {
    let active = true;
    let deviceId: string | undefined;
    let requestVersion = 0;
    let pending: AbortController | undefined;
    const identity = getToken();
    const refresh = async () => {
      const version = ++requestVersion;
      pending?.abort();
      const controller = new AbortController();
      pending = controller;
      // A returning tab cannot use stale microphone/reading choices while loading.
      setState(previous => ({ ...previous, ready: false, error: '' }));
      try {
        const result = await apiFetch<DevicePreferences>('/api/devices/me/voice', { cache: 'no-store', signal: controller.signal });
        if (!result?.device_id || !result.preferences) throw new Error('Invalid device preferences response');
        if (!active || controller.signal.aborted || version !== requestVersion || identity !== getToken()) return;
        deviceId = result.device_id;
        setState({ preferences: result.preferences, ready: true, error: '' });
      } catch {
        if (active && !controller.signal.aborted && version === requestVersion) {
          setState({ preferences: { ...defaults }, ready: false, error: 'unavailable' });
        }
      }
    };
    const changed = (event: Event) => {
      const result = (event as CustomEvent<DevicePreferences>).detail;
      if (active && identity === getToken() && result.device_id === deviceId) {
        requestVersion++;
        pending?.abort();
        setState({ preferences: result.preferences, ready: true, error: '' });
      }
    };
    const channel = notificationChannel();
    if (channel) channel.onmessage = (event: MessageEvent) => {
      const notice = event.data as { device_id?: unknown } | null;
      if (notice && typeof notice.device_id === 'string' && (!deviceId || notice.device_id === deviceId)) void refresh();
    };
    const focused = () => { void refresh(); };
    const visible = () => { if (document.visibilityState === 'visible') void refresh(); };
    window.addEventListener(preferenceEvent, changed);
    window.addEventListener('focus', focused);
    document.addEventListener('visibilitychange', visible);
    void refresh();
    return () => {
      active = false; pending?.abort(); channel?.close();
      window.removeEventListener(preferenceEvent, changed);
      window.removeEventListener('focus', focused);
      document.removeEventListener('visibilitychange', visible);
    };
  }, []);
  return state;
}
export interface Recognition {
  lang: string;
  continuous: boolean;
  interimResults: boolean;
  processLocally?: boolean;
  onresult: ((event: { resultIndex: number; results: ArrayLike<{ isFinal: boolean; 0: { transcript: string } }> }) => void) | null;
  onerror: ((event: { error: string }) => void) | null;
  onend: (() => void) | null;
  start(): void;
  stop(): void;
  abort(): void;
}
export type RecognitionConstructor = {
  new(): Recognition;
  available?: (options: { langs: string[]; processLocally: boolean }) => Promise<string>;
  install?: (options: { langs: string[] }) => Promise<boolean>;
};
export function recognitionConstructor(): RecognitionConstructor | undefined {
  const w = window as unknown as { SpeechRecognition?: RecognitionConstructor; webkitSpeechRecognition?: RecognitionConstructor };
  return w.SpeechRecognition || w.webkitSpeechRecognition;
}
export async function localRecognitionStatus(language: string): Promise<string> {
  const constructor = recognitionConstructor();
  if (!window.isSecureContext || !constructor?.available) return 'unavailable';
  try { return await constructor.available({ langs: [language], processLocally: true }); }
  catch { return 'unavailable'; }
}
export function useBrowserVoices() {
  const [voices, setVoices] = useState<SpeechSynthesisVoice[]>([]);
  useEffect(() => {
    if (!('speechSynthesis' in window)) return;
    const update = () => setVoices(window.speechSynthesis.getVoices());
    update();
    window.speechSynthesis.addEventListener('voiceschanged', update);
    return () => window.speechSynthesis.removeEventListener('voiceschanged', update);
  }, []);
  return voices;
}
export function shortPressAction(continuous: boolean, speaking: boolean): 'off' | 'once' {
  return continuous || speaking ? 'off' : 'once';
}
