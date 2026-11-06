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
export async function saveVoicePreferences(prefs: VoicePreferences): Promise<void> {
  const identity = getToken();
  const result = await apiFetch<DevicePreferences>('/api/devices/me/voice', {
    method: 'PUT', cache: 'no-store', body: JSON.stringify(prefs),
  });
  if (identity !== getToken()) throw new Error('Paired device changed');
  window.dispatchEvent(new CustomEvent(preferenceEvent, { detail: result }));
}
export function useVoicePreferences() {
  const [state, setState] = useState({ preferences: { ...defaults }, ready: false, error: '' });
  useEffect(() => {
    let active = true;
    let deviceId: string | undefined;
    const identity = getToken();
    const changed = (event: Event) => {
      const result = (event as CustomEvent<DevicePreferences>).detail;
      if (active && identity === getToken() && result.device_id === deviceId) {
        setState({ preferences: result.preferences, ready: true, error: '' });
      }
    };
    window.addEventListener(preferenceEvent, changed);
    void apiFetch<DevicePreferences>('/api/devices/me/voice', { cache: 'no-store' }).then(result => {
      if (!result?.device_id || !result.preferences) throw new Error('Invalid device preferences response');
      if (!active || identity !== getToken()) return;
      deviceId = result.device_id;
      setState({ preferences: result.preferences, ready: true, error: '' });
    }).catch(() => {
      if (active) setState({ preferences: { ...defaults }, ready: false, error: 'unavailable' });
    });
    return () => { active = false; window.removeEventListener(preferenceEvent, changed); };
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
