import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { ChevronDown, Mic, MicOff, Volume2, VolumeX } from 'lucide-react';
import type { ChatMessage } from '@/contexts/AgentContext';
import { t } from '@/lib/i18n';
import { localRecognitionStatus, recognitionConstructor, shortPressAction, useBrowserVoices, useVoicePreferences, type Recognition } from '@/lib/browserVoice';

export default function BrowserVoiceControls({ messages, hydrated, typing, onTranscript }: {
  messages: ChatMessage[]; hydrated: boolean; typing: boolean; onTranscript: (text: string) => void;
}) {
  const { preferences: prefs, ready } = useVoicePreferences();
  const voices = useBrowserVoices();
  const [override, setOverride] = useState<boolean | null>(null);
  const continuous = override ?? prefs.automatic;
  const [listening, setListening] = useState(false);
  const [speaking, setSpeaking] = useState(false);
  const [error, setError] = useState('');
  const [showInputs, setShowInputs] = useState(false);
  const [inputs, setInputs] = useState<MediaDeviceInfo[]>([]);
  const [inputId, setInputId] = useState('');
  const inputStream = useRef<MediaStream | null>(null);
  const root = useRef<HTMLDivElement>(null);
  const recognition = useRef<Recognition | null>(null);
  const finishing = useRef(false);
  const utterance = useRef<SpeechSynthesisUtterance | null>(null);
  const generation = useRef(0);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const longPressed = useRef(false);
  const baseline = useRef<Set<string> | null>(null);
  const visible = () => document.visibilityState === 'visible' && !!root.current?.getClientRects().length;
  const stopSpeech = () => {
    if (utterance.current) {
      utterance.current.onend = null; utterance.current.onerror = null;
      window.speechSynthesis.cancel(); utterance.current = null;
    }
    setSpeaking(false);
  };
  const releaseInput = () => {
    inputStream.current?.getTracks().forEach(track => track.stop());
    inputStream.current = null;
  };
  const stopCapture = () => {
    generation.current++;
    finishing.current = false;
    if (recognition.current) {
      recognition.current.onresult = null; recognition.current.onerror = null;
      recognition.current.onend = null; recognition.current.abort(); recognition.current = null;
    }
    releaseInput();
    setListening(false);
  };
  const stopAll = () => { stopCapture(); stopSpeech(); };
  const speak = (text: string) => {
    if (!ready || !prefs.reading || !visible() || recognition.current) return;
    const voice = voices.find(v => v.voiceURI === prefs.voice);
    if (!voice || !('speechSynthesis' in window)) { setError(t('voice.select_voice')); return; }
    stopSpeech();
    const next = new SpeechSynthesisUtterance(text);
    next.voice = voice; next.lang = prefs.language; next.rate = prefs.rate;
    next.onend = () => { utterance.current = null; setSpeaking(false); };
    next.onerror = () => { utterance.current = null; setSpeaking(false); setError(t('voice.speech_error')); };
    utterance.current = next; setSpeaking(true); setError(''); window.speechSynthesis.speak(next);
  };
  const replies = messages.filter(m => m.role === 'agent' && !m.toolCall && !m.ephemeral && !m.notice && m.content.trim());
  const latest = replies[replies.length - 1];
  useEffect(() => {
    if (!hydrated) { baseline.current = null; return; }
    if (typing) return;
    const ids = new Set(replies.map(m => m.id));
    if (baseline.current !== null && continuous && ready && prefs.reading) {
      const unseen = replies.filter(m => !m.fromHistory && !baseline.current!.has(m.id));
      const newest = unseen[unseen.length - 1];
      if (newest) speak(newest.content);
    }
    baseline.current = ids;
  });
  // Cancel synchronously at commit so a late recognition event cannot reach a new draft.
  useLayoutEffect(() => {
    if (!root.current) return;
    const hidden = () => { if (!visible()) stopAll(); };
    document.addEventListener('visibilitychange', hidden);
    // Workspace tabs stay mounted; stop a hidden pane as well as a hidden document.
    const observer = new MutationObserver(hidden);
    let parent = root.current?.parentElement;
    while (parent) { observer.observe(parent, { attributes: true, attributeFilter: ['class', 'style', 'hidden'] }); parent = parent.parentElement; }
    return () => {
      document.removeEventListener('visibilitychange', hidden); observer.disconnect();
      if (timer.current) clearTimeout(timer.current);
      stopAll();
    };
  }, [ready, prefs.dictation, prefs.reading]);
  useEffect(() => {
    if (!prefs.dictation) stopCapture();
    if (!prefs.reading) stopSpeech();
  }, [prefs.dictation, prefs.reading]);
  const listInputs = async () => {
    try {
      const devices = await navigator.mediaDevices.enumerateDevices();
      setInputs(devices.filter(device => device.kind === 'audioinput' && device.deviceId && device.deviceId !== 'default'));
    } catch {
      setError(t('voice.input_error'));
    }
  };
  const revealInputs = async () => {
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      try { await listInputs(); }
      finally { stream.getTracks().forEach(track => track.stop()); }
    } catch {
      setError(t('voice.input_error'));
    }
  };
  const toggleMicrophone = async () => {
    if (recognition.current) {
      if (finishing.current) return;
      finishing.current = true;
      // End capture and let the recognizer deliver the final utterance.
      try { recognition.current.stop(); }
      catch { stopCapture(); setError(t('voice.capture_error')); }
      return;
    }
    if (listening) { stopCapture(); return; }
    stopSpeech(); setError('');
    const Constructor = recognitionConstructor();
    if (!Constructor || !window.isSecureContext) { setError(t('voice.unsupported')); return; }
    const version = ++generation.current;
    setListening(true);
    if (prefs.recognition === 'local') {
      const status = await localRecognitionStatus(prefs.language);
      if (version !== generation.current) return;
      if (status !== 'available') {
        setListening(false);
        setError(t(status === 'downloadable' ? 'voice.pack_required'
          : status === 'downloading' ? 'voice.status_downloading' : 'voice.local_unavailable'));
        return;
      }
    }
    if (version !== generation.current || !visible()) {
      if (version === generation.current) setListening(false);
      return;
    }
    const capture = new Constructor();
    if (prefs.recognition === 'local' && !('processLocally' in capture)) {
      setListening(false); setError(t('voice.local_unavailable')); return;
    }
    capture.lang = prefs.language; capture.continuous = true; capture.interimResults = false;
    if (prefs.recognition === 'local') capture.processLocally = true;
    let receivedText = false;
    capture.onresult = event => {
      if (generation.current !== version || !visible()) return;
      for (let i = event.resultIndex; i < event.results.length; i++) {
        const result = event.results[i];
        const text = result?.isFinal ? result[0].transcript.trim() : '';
        if (text) {
          receivedText = true;
          onTranscript(text);
        }
      }
    };
    capture.onerror = ({ error: code }) => {
      if (generation.current !== version) return;
      stopCapture();
      setError(t(code === 'no-speech' ? 'voice.no_transcript' : 'voice.capture_error'));
    };
    capture.onend = () => {
      if (generation.current !== version) return;
      recognition.current = null;
      releaseInput();
      finishing.current = false;
      setListening(false);
      if (!receivedText && visible()) setError(t('voice.no_transcript'));
    };
    let track: MediaStreamTrack | undefined;
    if (inputId) {
      try {
        const stream = await navigator.mediaDevices.getUserMedia({ audio: { deviceId: { exact: inputId } } });
        if (version !== generation.current || !visible()) {
          stream.getTracks().forEach(entry => entry.stop());
          if (version === generation.current) setListening(false);
          return;
        }
        inputStream.current = stream;
        track = stream.getAudioTracks()[0];
        if (!track) throw new Error('Missing audio track');
      } catch {
        if (version === generation.current) { stopCapture(); setError(t('voice.input_error')); }
        return;
      }
    }
    recognition.current = capture;
    try {
      if (track) capture.start(track);
      else capture.start();
    } catch { stopCapture(); setError(t('voice.capture_error')); }
  };
  const shortPress = () => {
    if (longPressed.current) { longPressed.current = false; return; }
    if (shortPressAction(continuous, speaking) === 'off') { setOverride(false); stopSpeech(); }
    else if (latest) { stopCapture(); speak(latest.content); }
  };
  const release = () => { if (timer.current) clearTimeout(timer.current); timer.current = null; };
  const css = 'touch-manipulation select-none p-2 rounded-lg border border-pc-border hover:bg-pc-hover text-pc-text-secondary disabled:opacity-40';
  if (!ready || (!prefs.dictation && !prefs.reading)) return null;
  return <div ref={root} className="flex flex-wrap items-center gap-1 max-w-48">
    {prefs.dictation && <button type="button" className={css} aria-label={t('voice.microphone')} title={t('voice.microphone')} aria-pressed={listening} onClick={() => void toggleMicrophone()}>{listening ? <MicOff size={20} /> : <Mic size={20} />}</button>}
    {prefs.dictation && typeof navigator.mediaDevices?.enumerateDevices === 'function' && (
      <button type="button" className={css} aria-label={t('voice.choose_input')} title={t('voice.choose_input')}
        aria-expanded={showInputs} disabled={listening} onClick={() => {
          setShowInputs(!showInputs);
          if (!showInputs) { setError(''); void listInputs(); }
        }}><ChevronDown size={16} /></button>
    )}
    {prefs.dictation && showInputs && (
      <div className="flex flex-col gap-1 w-full">
        <select aria-label={t('voice.choose_input')} value={inputId} disabled={listening}
          className="max-w-full rounded border border-pc-border bg-pc-base p-1 text-xs"
          onChange={event => { setInputId(event.target.value); setError(''); setShowInputs(false); }}>
          <option value="">{t('voice.default_input')}</option>
          {inputs.map((device, i) => <option key={device.deviceId} value={device.deviceId}>
            {device.label || t('voice.unnamed_input') + ' ' + (i + 1)}
          </option>)}
        </select>
        {(!inputs.length || inputs.some(device => !device.label)) && (
          <button type="button" className={css + ' text-xs'} disabled={listening}
            onClick={() => { setError(''); void revealInputs(); }}>{t('voice.allow_inputs')}</button>
        )}
      </div>
    )}
    {prefs.reading && <button type="button" className={`${css} ${continuous ? 'bg-pc-accent/20 ring-2 ring-pc-accent' : ''}`} aria-label={t('voice.speaker')} title={t('voice.gesture')} aria-pressed={continuous} aria-busy={speaking}
      onPointerDown={event => {
        if (event.button !== 0) return;
        event.currentTarget.setPointerCapture(event.pointerId); longPressed.current = false;
        timer.current = setTimeout(() => { longPressed.current = true; setOverride(true); }, 600);
      }} onPointerUp={release} onPointerCancel={() => { release(); longPressed.current = true; }} onLostPointerCapture={release}
      onKeyDown={event => { if (event.key === 'Enter' && event.shiftKey) { event.preventDefault(); setOverride(!continuous); stopSpeech(); } }}
      onContextMenu={event => event.preventDefault()} onClick={shortPress}>{speaking ? <VolumeX size={20} /> : <Volume2 size={20} />}</button>}
    {error && <span role="alert" className="text-xs text-status-error w-full">{error}</span>}
  </div>;
}
