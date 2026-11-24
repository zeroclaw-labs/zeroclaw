import { useEffect, useState, useSyncExternalStore } from 'react';
import { apiFetch, getProp, putProp } from './api';
import { getOidcSession, getToken, subscribeOidcSession } from './auth';

export const voicePlaybackIdle = 'zeroclaw-voice-playback-idle';
let activeVoicePlayback: SpeechSynthesisUtterance | null = null;

// Browser audio has one owner: microphone capture and narration must not
// overlap across split panes, including the settings preview.
let activeVoiceCapture: object | null = null;
export function beginVoiceCapture(): (() => void) | null {
  if (voicePlaybackBusy()) return null;
  const owner = {};
  activeVoiceCapture = owner;
  return () => {
    if (activeVoiceCapture !== owner) return;
    activeVoiceCapture = null;
    window.dispatchEvent(new Event(voicePlaybackIdle));
  };
}

export function voicePlaybackBusy(): boolean {
  // The page leases are canonical. Some engines keep speaking/pending true
  // briefly after cancelling our own utterance; those stale flags must not
  // reject its owner's immediate switch to dictation or a replacement reply.
  return activeVoiceCapture !== null || activeVoicePlayback !== null;
}

export function playVoice(utterance: SpeechSynthesisUtterance): boolean {
  if (voicePlaybackBusy()) return false;
  activeVoicePlayback = utterance;
  window.speechSynthesis.speak(utterance);
  return true;
}

export function releaseVoice(utterance: SpeechSynthesisUtterance, cancel = false) {
  if (activeVoicePlayback !== utterance) return;
  if (cancel) window.speechSynthesis.cancel();
  activeVoicePlayback = null;
  window.dispatchEvent(new Event(voicePlaybackIdle));
}


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
const oidcPreferenceEvent = 'zeroclaw-oidc-voice-updated';
interface VoicePreferenceState {
  pairedIdentity?: string | null;
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

// The verified principal is stable across sign-ins. Browser storage keeps
// preferences local to this origin and profile, with a separate key per account.
export function useOidcVoiceSession() {
  return useSyncExternalStore(subscribeOidcSession, getOidcSession, () => null);
}

async function oidcPreferenceKey(principal: string): Promise<string> {
  const bytes = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(principal));
  const digest = Array.from(new Uint8Array(bytes), byte => byte.toString(16).padStart(2, '0')).join('');
  return `zeroclaw-oidc-voice:v2:${digest}`;
}

function readOidcPreferences(key: string): string | null {
  try { return localStorage.getItem(key); }
  catch { return null; }
}

function parseOidcPreferences(raw: string | null): VoicePreferences {
  try {
    const value = JSON.parse(raw ?? 'null');
    if (value && typeof value.dictation === 'boolean' && typeof value.reading === 'boolean'
      && typeof value.automatic === 'boolean' && typeof value.language === 'string'
      && (value.recognition === 'local' || value.recognition === 'browser')
      && typeof value.voice === 'string' && Number.isFinite(value.rate)
      && value.rate >= 0.5 && value.rate <= 2) return value;
  } catch { /* Invalid local settings fall back to opt-in defaults. */ }
  return { ...defaults };
}

export async function saveVoicePreferences(changes: Partial<VoicePreferences>, deviceId?: string): Promise<void> {
  const session = getOidcSession();
  if (session && !deviceId) {
    if (!session.features.chat || !session.features.config) throw new Error('Invalid OIDC voice preference target');
    const key = await oidcPreferenceKey(session.principal_id);
    const current = getOidcSession();
    if (current?.principal_id !== session.principal_id || !current.features.chat || !current.features.config) {
      throw new Error('OIDC voice session changed');
    }
    const saved = parseOidcPreferences(readOidcPreferences(key));
    localStorage.setItem(key, JSON.stringify({ ...saved, ...changes }));
    window.dispatchEvent(new Event(oidcPreferenceEvent));
    return;
  }
  if (session && !session.admin) throw new Error('Invalid OIDC voice preference target');
  const identity = getToken();
  const result = await apiFetch<DevicePreferences>(preferencePath(deviceId), {
    method: 'PUT', cache: 'no-store', body: JSON.stringify(changes),
  });
  if (getOidcSession() !== session || identity !== getToken() || (deviceId && result.device_id !== deviceId)) throw new Error('Paired device changed');
  window.dispatchEvent(new CustomEvent(preferenceEvent, { detail: result }));
  const channel = notificationChannel();
  if (channel) {
    // Notify only: other tabs read their authenticated record from ZeroClaw.
    try { channel.postMessage({ device_id: result.device_id }); }
    catch { console.warn('Voice preference notification failed; settings reload on tab return.'); }
    finally { channel.close(); }
  }
}
function subscribePairedToken(changed: () => void) {
  window.addEventListener('storage', changed);
  window.addEventListener('focus', changed);
  return () => {
    window.removeEventListener('storage', changed);
    window.removeEventListener('focus', changed);
  };
}

export function useVoicePreferences(targetDeviceId?: string): VoicePreferenceState {
  const session = useOidcVoiceSession();
  const identity = useSyncExternalStore(subscribePairedToken, getToken, () => null);
  const principal = !targetDeviceId && session?.features.chat && session.features.config ? session.principal_id : null;
  const [keyState, setKeyState] = useState<{ principal: string; key: string } | null>(null);
  const key = principal && keyState?.principal === principal ? keyState.key : null;
  useEffect(() => {
    if (!principal) return;
    let active = true;
    void oidcPreferenceKey(principal).then(derivedKey => {
      if (!active || getOidcSession()?.principal_id !== principal) return;
      try {
        const previous = `zeroclaw-oidc-voice:${encodeURIComponent(principal)}`;
        const raw = readOidcPreferences(previous);
        if (raw !== null && readOidcPreferences(derivedKey) === null) localStorage.setItem(derivedKey, raw);
        localStorage.removeItem(previous);
      } catch { /* Storage can be disabled in this browser profile. */ }
      setKeyState({ principal, key: derivedKey });
    }).catch(() => { /* Without Web Crypto, OIDC preferences stay disabled. */ });
    return () => { active = false; };
  }, [principal]);
  const path = key ?? preferencePath(targetDeviceId);
  const [, refreshLocal] = useState(0);
  useEffect(() => {
    if (!key) return;
    const refresh = () => refreshLocal(revision => revision + 1);
    const storageChanged = (event: StorageEvent) => {
      if (event.key === key || event.key === null) refresh();
    };
    window.addEventListener(oidcPreferenceEvent, refresh);
    window.addEventListener('storage', storageChanged);
    return () => {
      window.removeEventListener(oidcPreferenceEvent, refresh);
      window.removeEventListener('storage', storageChanged);
    };
  }, [key]);
  const [state, setState] = useState<VoicePreferenceState>({ path, pairedIdentity: identity, preferences: { ...defaults }, ready: false, error: '' });
  useEffect(() => {
    if (session && (!targetDeviceId || !session.admin)) return;
    let active = true;
    let deviceId: string | undefined;
    let requestVersion = 0;
    let pending: AbortController | undefined;
    const refresh = async () => {
      const version = ++requestVersion;
      pending?.abort();
      const controller = new AbortController();
      pending = controller;
      // A returning tab cannot use stale microphone/reading choices while loading.
      setState(previous => previous.path === path && previous.pairedIdentity === identity
        ? { ...previous, ready: false, error: '' }
        : { path, pairedIdentity: identity, preferences: { ...defaults }, ready: false, error: '' });
      try {
        const result = await apiFetch<DevicePreferences>(path, { cache: 'no-store', signal: controller.signal });
        if (!result?.device_id || !result.preferences || (targetDeviceId && result.device_id !== targetDeviceId)) throw new Error('Invalid device preferences response');
        if (!active || controller.signal.aborted || version !== requestVersion || getOidcSession() !== session || identity !== getToken()) return;
        deviceId = result.device_id;
        setState({ path, pairedIdentity: identity, deviceId: result.device_id, preferences: result.preferences, ready: true, error: '' });
      } catch {
        if (active && !controller.signal.aborted && version === requestVersion) {
          setState({ path, pairedIdentity: identity, preferences: { ...defaults }, ready: false, error: 'unavailable' });
        }
      }
    };
    const changed = (event: Event) => {
      const result = (event as CustomEvent<DevicePreferences>).detail;
      if (result && active && getOidcSession() === session && identity === getToken() && result.device_id === deviceId) {
        requestVersion++;
        pending?.abort();
        setState({ path, pairedIdentity: identity, deviceId: result.device_id, preferences: result.preferences, ready: true, error: '' });
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
  }, [session, path, targetDeviceId, identity]);
  if (session && (!targetDeviceId || !session.admin)) {
    const ready = key !== null && !targetDeviceId;
    return { path, pairedIdentity: identity, preferences: ready && key ? parseOidcPreferences(readOidcPreferences(key)) : { ...defaults }, ready, error: '' };
  }
  return state.path === path && state.pairedIdentity === identity ? state : { path, preferences: { ...defaults }, ready: false, error: '' };
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
    const poll = setInterval(() => void refresh(), 30_000);
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
