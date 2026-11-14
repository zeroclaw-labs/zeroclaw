import { useEffect, useState } from 'react';
import { apiFetch, getProp, putProp } from './api';
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
interface VoicePreferenceState {
  path: string;
  deviceId?: string;
  preferences: VoicePreferences;
  ready: boolean;
  error: string;
}
function preferencePath(deviceId?: string): string {
  return deviceId ? `/api/devices/${encodeURIComponent(deviceId)}/voice` : '/api/devices/me/voice';
}
const notificationTopic = 'zeroclaw-device-voice-invalidated';
function notificationChannel(): BroadcastChannel | undefined {
  if (!('BroadcastChannel' in window)) return;
  try { return new window.BroadcastChannel(notificationTopic); }
  catch { console.warn('Voice preference notifications unavailable; settings reload on tab return.'); }
}

export async function saveVoicePreferences(prefs: VoicePreferences, deviceId?: string): Promise<void> {
  const identity = getToken();
  const result = await apiFetch<DevicePreferences>(preferencePath(deviceId), {
    method: 'PUT', cache: 'no-store', body: JSON.stringify(prefs),
  });
  if (identity !== getToken() || (deviceId && result.device_id !== deviceId)) throw new Error('Paired device changed');
  window.dispatchEvent(new CustomEvent(preferenceEvent, { detail: result }));
  const channel = notificationChannel();
  if (channel) {
    // Notify only: other tabs read their authenticated record from ZeroClaw.
    try { channel.postMessage({ device_id: result.device_id }); }
    catch { console.warn('Voice preference notification failed; settings reload on tab return.'); }
    finally { channel.close(); }
  }
}
export function useVoicePreferences(targetDeviceId?: string): VoicePreferenceState {
  const path = preferencePath(targetDeviceId);
  const [state, setState] = useState<VoicePreferenceState>({ path, preferences: { ...defaults }, ready: false, error: '' });
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
      setState(previous => previous.path === path
        ? { ...previous, ready: false, error: '' }
        : { path, preferences: { ...defaults }, ready: false, error: '' });
      try {
        const result = await apiFetch<DevicePreferences>(path, { cache: 'no-store', signal: controller.signal });
        if (!result?.device_id || !result.preferences || (targetDeviceId && result.device_id !== targetDeviceId)) throw new Error('Invalid device preferences response');
        if (!active || controller.signal.aborted || version !== requestVersion || identity !== getToken()) return;
        deviceId = result.device_id;
        setState({ path, deviceId: result.device_id, preferences: result.preferences, ready: true, error: '' });
      } catch {
        if (active && !controller.signal.aborted && version === requestVersion) {
          setState({ path, preferences: { ...defaults }, ready: false, error: 'unavailable' });
        }
      }
    };
    const changed = (event: Event) => {
      const result = (event as CustomEvent<DevicePreferences>).detail;
      if (active && identity === getToken() && result.device_id === deviceId) {
        requestVersion++;
        pending?.abort();
        setState({ path, deviceId: result.device_id, preferences: result.preferences, ready: true, error: '' });
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
  }, [path]);
  return state.path === path ? state : { path, preferences: { ...defaults }, ready: false, error: '' };
}
export interface Recognition {
  lang: string;
  continuous: boolean;
  interimResults: boolean;
  processLocally?: boolean;
  onresult: ((event: { resultIndex: number; results: ArrayLike<{ isFinal: boolean; 0: { transcript: string } }> }) => void) | null;
  onerror: ((event: { error: string }) => void) | null;
  onend: (() => void) | null;
  onspeechstart?: (() => void) | null;
  onspeechend?: (() => void) | null;
  start(): void;
  stop(): void;
  abort(): void;
}
export type RecognitionConstructor = {
  new(): Recognition;
  available?: (options: { langs: string[]; processLocally: boolean }) => Promise<string>;
  install?: (options: { langs: string[]; processLocally: true }) => Promise<boolean>;
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


export async function readGlobalVoiceTimeout(): Promise<number> {
  const result = await getProp('gateway.voice_silence_timeout_secs');
  const value = typeof result.value === 'string' && /^\d+$/.test(result.value.trim()) ? Number(result.value) : result.value;
  if (typeof value !== 'number' || !Number.isInteger(value) || value < 0 || value > 300) throw new Error('Invalid global voice timeout');
  return value;
}

type GlobalVoiceState = { seconds: number | null; error: boolean };
let globalVoiceState: GlobalVoiceState = { seconds: null, error: false };
const globalVoiceListeners = new Set<(state: GlobalVoiceState) => void>();
let stopGlobalVoiceRefresh: (() => void) | undefined;

function subscribeGlobalVoice(listener: (state: GlobalVoiceState) => void) {
  globalVoiceListeners.add(listener);
  listener(globalVoiceState);
  if (!stopGlobalVoiceRefresh) {
    let active = true;
    let version = 0;
    let pending = false;
    const refresh = async () => {
      if (pending || document.visibilityState !== 'visible') return;
      pending = true;
      const current = ++version;
      let next: GlobalVoiceState;
      try { next = { seconds: await readGlobalVoiceTimeout(), error: false }; }
      catch { next = { seconds: null, error: true }; }
      pending = false;
      if (active && current === version && (next.seconds !== globalVoiceState.seconds || next.error !== globalVoiceState.error)) {
        globalVoiceState = next;
        for (const notify of globalVoiceListeners) notify(next);
      }
    };
    window.addEventListener('focus', refresh);
    window.addEventListener('zeroclaw-config-mutated', refresh);
    document.addEventListener('visibilitychange', refresh);
    const poll = setInterval(() => void refresh(), 2000);
    stopGlobalVoiceRefresh = () => {
      active = false;
      clearInterval(poll);
      window.removeEventListener('focus', refresh);
      window.removeEventListener('zeroclaw-config-mutated', refresh);
      document.removeEventListener('visibilitychange', refresh);
    };
    void refresh();
  }
  return () => {
    globalVoiceListeners.delete(listener);
    if (!globalVoiceListeners.size) {
      stopGlobalVoiceRefresh?.(); stopGlobalVoiceRefresh = undefined;
      globalVoiceState = { seconds: null, error: false };
    }
  };
}

export function useGlobalVoiceTimeout(enabled = true) {
  const [state, setState] = useState<GlobalVoiceState>({ seconds: null, error: false });
  useEffect(() => {
    if (!enabled) { setState({ seconds: null, error: false }); return; }
    return subscribeGlobalVoice(setState);
  }, [enabled]);
  return state;
}

export async function saveGlobalVoiceTimeout(seconds: number) {
  await putProp('gateway.voice_silence_timeout_secs', seconds);
  window.dispatchEvent(new Event('zeroclaw-config-mutated'));
}
