import { useEffect, useImperativeHandle, useLayoutEffect, useRef, useState, type Ref } from 'react';
import { Mic, MicOff, Volume2, VolumeX } from 'lucide-react';
import type { ChatMessage } from '@/contexts/AgentContext';
import { t } from '@/lib/i18n';
import { localRecognitionStatus, recognitionConstructor, shortPressAction, useBrowserVoices, useVoicePreferences, type Recognition } from '@/lib/browserVoice';

export default function BrowserVoiceControls({ messages, hydrated, typing, onTranscript, captureRef }: {
  messages: ChatMessage[]; hydrated: boolean; typing: boolean; onTranscript: (text: string) => void;
  captureRef?: Ref<() => void>;
}) {
  const { preferences: prefs, ready } = useVoicePreferences();
  const voices = useBrowserVoices();
  const [override, setOverride] = useState<boolean | null>(null);
  const continuous = override ?? prefs.automatic;
  const [listening, setListening] = useState(false);
  const [speaking, setSpeaking] = useState(false);
  const [error, setError] = useState('');
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
  const stopCapture = () => {
    generation.current++;
    finishing.current = false;
    if (recognition.current) {
      recognition.current.onresult = null; recognition.current.onerror = null;
      recognition.current.onend = null; recognition.current.abort(); recognition.current = null;
    }
    setListening(false);
  };
  useImperativeHandle(captureRef, () => stopCapture);
  const stopAll = () => { stopCapture(); stopSpeech(); };
  const speak = (text: string) => {
    if (!ready || !prefs.reading || !visible() || recognition.current) return false;
    const voice = voices.find(v => v.voiceURI === prefs.voice);
    if (!voice || !('speechSynthesis' in window)) { setError(t('voice.select_voice')); return false; }
    stopSpeech();
    const next = new SpeechSynthesisUtterance(text);
    next.voice = voice; next.lang = prefs.language; next.rate = prefs.rate;
    next.onend = () => { utterance.current = null; setSpeaking(false); };
    next.onerror = () => { utterance.current = null; setSpeaking(false); setError(t('voice.speech_error')); };
    utterance.current = next; setSpeaking(true); setError(''); window.speechSynthesis.speak(next);
    return true;
  };
  const replies = messages.filter(m => m.role === 'agent' && !m.toolCall && !m.ephemeral && !m.notice && m.content.trim());
  const latest = replies[replies.length - 1];
  useEffect(() => {
    if (!hydrated) { baseline.current = null; return; }
    const ids = new Set(replies.map(m => m.id));
    if (baseline.current === null || !continuous || !prefs.reading || !visible()) {
      baseline.current = ids;
      return;
    }
    if (typing) return;
    const unseen = replies.filter(m => !m.fromHistory && !baseline.current!.has(m.id));
    const next = unseen[0];
    // Only consume the reply whose playback actually started. Later replies
    // stay pending until the current utterance ends.
    if (next) {
      if (!ready || listening || speaking || !speak(next.content)) return;
      baseline.current.add(next.id);
    }
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
      finishing.current = false;
      setListening(false);
      if (!receivedText && visible()) setError(t('voice.no_transcript'));
    };
    recognition.current = capture;
    try { capture.start(); } catch { stopCapture(); setError(t('voice.capture_error')); }
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
    {prefs.dictation && <button type="button" className={listening ? css + ' bg-pc-accent/20 ring-2 ring-pc-accent' : css} aria-label={t('voice.microphone')} title={t('voice.microphone')} aria-pressed={listening} onClick={() => void toggleMicrophone()}>{listening ? <Mic size={20} /> : <MicOff size={20} />}</button>}
    {prefs.reading && <button type="button" className={`${css} ${speaking || continuous ? 'bg-pc-accent/20 ring-2 ring-pc-accent' : ''}`} aria-label={t('voice.speaker')} title={t('voice.gesture')} aria-pressed={continuous} aria-busy={speaking}
      onPointerDown={event => {
        if (event.button !== 0) return;
        event.currentTarget.setPointerCapture(event.pointerId); longPressed.current = false;
        timer.current = setTimeout(() => { longPressed.current = true; setOverride(true); }, 600);
      }} onPointerUp={release} onPointerCancel={() => { release(); longPressed.current = true; }} onLostPointerCapture={release}
      onKeyDown={event => { if (event.key === 'Enter' && event.shiftKey) { event.preventDefault(); setOverride(!continuous); stopSpeech(); } }}
      onContextMenu={event => event.preventDefault()} onClick={shortPress}>{speaking || continuous ? <Volume2 size={20} /> : <VolumeX size={20} />}</button>}
    {error && <span role="alert" className="text-xs text-status-error w-full">{error}</span>}
  </div>;
}
