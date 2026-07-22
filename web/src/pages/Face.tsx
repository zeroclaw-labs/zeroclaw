/**
 * Face — the fullscreen conversation experience.
 *
 * Just Clawd on black. Hold Space (or press the mascot) to talk, or switch
 * to continuous listening. The mascot's motion is driven by the live audio
 * envelope and the agent's lifecycle (listening / thinking / working /
 * speaking), with idle fidgets so it always feels alive.
 *
 * Keyboard: SPACE hold = talk · C = continuous mode · V = vision source ·
 * T = type instead · ESC = interrupt.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useParams, useNavigate } from 'react-router-dom';
import ClawdAvatar, { type ClawdHandle } from '../components/clawd/ClawdAvatar';
import type { EmotionName } from '../lib/clawd/engine';
import { useVoiceSession, type VoicePhase } from '../hooks/useVoiceSession';
import {
  VOICE_EFFECT_PRESETS,
  storedVoiceEffect,
  type VoiceEffectPreset,
} from '../lib/voice/robotVoice';
import { getAgentOptions } from '../lib/api';

const PHASE_LABEL: Record<VoicePhase, string> = {
  boot: 'waking up…',
  idle: '',
  listening: 'listening',
  transcribing: '…',
  thinking: '',
  speaking: '',
  error: '',
};

function FaceSession({ alias }: { alias: string }) {
  const clawd = useRef<ClawdHandle>(null);
  const session = useVoiceSession(alias);
  const { state } = session;
  const [showHints, setShowHints] = useState(true);
  const [typing, setTyping] = useState(false);
  const [voiceEffect, setVoiceEffectState] = useState<VoiceEffectPreset>(() => storedVoiceEffect());
  const [effectToast, setEffectToast] = useState('');
  const effectToastTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [draft, setDraft] = useState('');
  const greetedRef = useRef(false);
  const spaceDownRef = useRef(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const [avatarSize, setAvatarSize] = useState(() =>
    Math.min(560, window.innerWidth * 0.7, window.innerHeight * 0.72),
  );

  useEffect(() => {
    const onResize = () =>
      setAvatarSize(Math.min(560, window.innerWidth * 0.7, window.innerHeight * 0.72));
    window.addEventListener('resize', onResize);
    return () => window.removeEventListener('resize', onResize);
  }, []);

  // ---- mascot ← lifecycle ----
  useEffect(() => {
    const c = clawd.current;
    if (!c) return;
    c.setListening(state.phase === 'listening');
    c.setTalking(state.phase === 'speaking');
    switch (state.phase) {
      case 'listening':
        c.setEmotion('curious');
        break;
      case 'transcribing':
      case 'thinking':
        c.setEmotion('thinking');
        void c.play('think');
        break;
      case 'speaking':
        c.setEmotion('happy');
        c.stopAction();
        break;
      case 'idle':
        c.setEmotion('neutral');
        c.stopAction();
        break;
      default:
        break;
    }
  }, [state.phase]);

  // Tool activity → hard-at-work typing
  useEffect(() => {
    const c = clawd.current;
    if (!c) return;
    if (state.toolActivity) {
      c.setEmotion('focused');
      void c.play('typeFuriously');
    } else if (state.phase === 'thinking') {
      void c.play('think');
    } else {
      c.stopAction();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state.toolActivity]);

  // Mirror toolActivity into a ref so the mascot-cue subscriber (mounted
  // once) always sees the latest value without resubscribing every time it
  // changes.
  const toolActivityRef = useRef<string | null>(null);
  useEffect(() => {
    toolActivityRef.current = state.toolActivity;
  }, [state.toolActivity]);

  // Inline control-tag cues (contract B/C): emotion applies immediately;
  // gesture plays as a one-shot action unless toolActivity already owns
  // the action slot (typeFuriously) — same precedence as the effect above.
  useEffect(() => {
    return session.subscribeMascotCue((cue) => {
      const c = clawd.current;
      if (!c) return;
      if (cue.emotion) c.setEmotion(cue.emotion as EmotionName);
      if (cue.gesture && !toolActivityRef.current) void c.play(cue.gesture);
    });
  }, [session.subscribeMascotCue]);

  // Greet once the session is live
  useEffect(() => {
    if (state.micReady && !greetedRef.current) {
      greetedRef.current = true;
      void clawd.current?.play('greetSequence');
    }
  }, [state.micReady]);

  // ---- audio envelope → body ----
  useEffect(() => {
    let raf = 0;
    const tick = () => {
      clawd.current?.setTalkLevel(session.outputLevel());
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [session]);

  // Auto-hide hints after 8s of no interaction
  useEffect(() => {
    const t = setTimeout(() => setShowHints(false), 8000);
    return () => clearTimeout(t);
  }, []);

  const cycleVoiceEffect = useCallback(() => {
    const order = VOICE_EFFECT_PRESETS.map((p) => p.id);
    const next = order[(order.indexOf(voiceEffect) + 1) % order.length] ?? 'droid';
    setVoiceEffectState(next);
    session.setVoiceEffect(next);
    const label = VOICE_EFFECT_PRESETS.find((p) => p.id === next);
    setEffectToast(label ? `voice · ${label.name.toLowerCase()}` : '');
    if (effectToastTimer.current) clearTimeout(effectToastTimer.current);
    effectToastTimer.current = setTimeout(() => setEffectToast(''), 1800);
  }, [session, voiceEffect]);

  const cycleVision = useCallback(() => {
    const next = state.vision === 'off' ? 'camera' : state.vision === 'camera' ? 'screen' : 'off';
    session.setVision(next);
  }, [session, state.vision]);

  // ---- keyboard ----
  useEffect(() => {
    const down = (e: KeyboardEvent) => {
      if (typing) {
        if (e.key === 'Escape') setTyping(false);
        return;
      }
      if (e.code === 'Space' && !e.repeat && state.mode === 'push') {
        e.preventDefault();
        spaceDownRef.current = true;
        setShowHints(false);
        session.pressTalk();
      } else if (e.key === 'Escape') {
        session.bargeIn();
      } else if (e.key === 'c' || e.key === 'C') {
        session.setMode(state.mode === 'push' ? 'continuous' : 'push');
      } else if (e.key === 'v' || e.key === 'V') {
        cycleVision();
      } else if (e.key === 'r' || e.key === 'R') {
        cycleVoiceEffect();
      } else if (e.key === 't' || e.key === 'T') {
        e.preventDefault();
        setTyping(true);
        setTimeout(() => inputRef.current?.focus(), 0);
      }
    };
    const up = (e: KeyboardEvent) => {
      if (e.code === 'Space' && spaceDownRef.current) {
        spaceDownRef.current = false;
        session.releaseTalk();
      }
    };
    // Losing focus while holding Space would strand the capture — treat
    // blur as key-release.
    const blur = () => {
      if (spaceDownRef.current) {
        spaceDownRef.current = false;
        session.releaseTalk();
      }
    };
    window.addEventListener('keydown', down);
    window.addEventListener('keyup', up);
    window.addEventListener('blur', blur);
    return () => {
      window.removeEventListener('keydown', down);
      window.removeEventListener('keyup', up);
      window.removeEventListener('blur', blur);
    };
  }, [session, state.mode, typing, cycleVision, cycleVoiceEffect]);

  const statusLabel = useMemo(() => {
    if (state.phase === 'error') return state.error ?? 'error';
    if (state.toolActivity) return `working · ${state.toolActivity}`;
    if (state.phase === 'thinking') return state.transcript ? '' : '';
    return PHASE_LABEL[state.phase];
  }, [state]);

  const caption = state.phase === 'speaking' || state.phase === 'thinking' ? state.reply : '';

  return (
    <div
      className="fixed inset-0 z-50 flex flex-col items-center justify-center overflow-hidden select-none"
      style={{ background: '#000' }}
    >
      {/* top status line */}
      <div className="absolute top-5 left-0 right-0 flex items-center justify-center gap-2 text-[11px] tracking-[0.25em] uppercase">
        <span
          className="inline-block h-1.5 w-1.5 rounded-full"
          style={{
            background: state.connected ? '#D97757' : '#444',
            boxShadow: state.connected ? '0 0 8px #D9775788' : 'none',
          }}
        />
        <span style={{ color: '#666' }}>{alias}</span>
        {state.mode === 'continuous' && (
          <span style={{ color: '#D97757' }}>· always listening</span>
        )}
        {state.vision !== 'off' && (
          <span style={{ color: '#D97757' }}>· seeing {state.vision}</span>
        )}
      </div>

      {/* the mascot */}
      <button
        type="button"
        aria-label="Hold to talk"
        className="bg-transparent border-0 p-0 cursor-pointer outline-none"
        onPointerDown={(e) => {
          e.preventDefault();
          if (state.mode === 'push') session.pressTalk();
        }}
        onPointerUp={() => state.mode === 'push' && session.releaseTalk()}
        onPointerLeave={() => state.mode === 'push' && spaceDownRef.current === false && session.releaseTalk()}
      >
        <ClawdAvatar ref={clawd} size={avatarSize} />
      </button>

      {/* voice-effect toast */}
      <div
        className="absolute top-12 left-0 right-0 text-center text-[11px] tracking-[0.3em] uppercase transition-opacity duration-300 pointer-events-none"
        style={{ color: '#D97757', opacity: effectToast ? 1 : 0 }}
      >
        {effectToast}
      </div>

      {/* status word under the mascot */}
      <div
        className="mt-2 h-5 text-[12px] tracking-[0.3em] uppercase transition-opacity duration-500"
        style={{ color: '#555', opacity: statusLabel ? 1 : 0 }}
      >
        {statusLabel}
      </div>

      {/* streaming caption */}
      <div
        className="absolute bottom-24 left-1/2 -translate-x-1/2 w-[min(720px,86vw)] text-center transition-opacity duration-300"
        style={{ opacity: caption ? 1 : 0 }}
      >
        <p
          className="text-[15px] leading-relaxed line-clamp-3"
          style={{ color: '#FAF9F5', textShadow: '0 2px 12px #000' }}
        >
          {caption}
        </p>
      </div>

      {/* camera thumbnail */}
      {session.cameraStream && (
        <video
          className="absolute bottom-6 right-6 w-36 rounded-lg border"
          style={{ borderColor: '#222' }}
          autoPlay
          muted
          playsInline
          ref={(el) => {
            if (el && el.srcObject !== session.cameraStream) el.srcObject = session.cameraStream;
          }}
        />
      )}

      {/* text input escape hatch */}
      {typing && (
        <form
          className="absolute bottom-8 left-1/2 -translate-x-1/2 w-[min(560px,80vw)]"
          onSubmit={(e) => {
            e.preventDefault();
            session.sendText(draft);
            setDraft('');
            setTyping(false);
          }}
        >
          <input
            ref={inputRef}
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            placeholder="say it silently…"
            className="w-full px-4 py-3 rounded-xl text-[14px] outline-none"
            style={{
              background: '#0d0d0d',
              border: '1px solid #262626',
              color: '#FAF9F5',
              caretColor: '#D97757',
            }}
          />
        </form>
      )}

      {/* hints */}
      <div
        className="absolute bottom-8 left-0 right-0 text-center text-[11px] tracking-[0.2em] uppercase transition-opacity duration-1000 pointer-events-none"
        style={{ color: '#3a3a3a', opacity: showHints && !typing ? 1 : 0 }}
      >
        hold space to talk · c continuous · v vision · r voice · t type · esc interrupt
      </div>
    </div>
  );
}

/** Sleeping Clawd + message, used for the no-agent / unreachable states. */
function FaceFallback({
  message,
  actionLabel,
  onAction,
}: {
  message: string;
  actionLabel: string;
  onAction: () => void;
}) {
  const clawd = useRef<ClawdHandle>(null);
  useEffect(() => {
    clawd.current?.setEmotion('sleepy');
    void clawd.current?.play('sleep');
  }, []);
  return (
    <div
      className="fixed inset-0 z-50 flex flex-col items-center justify-center gap-5"
      style={{ background: '#000' }}
    >
      <ClawdAvatar ref={clawd} size={300} fidget={false} />
      <p className="text-[13px] tracking-[0.15em] uppercase" style={{ color: '#666' }}>
        {message}
      </p>
      <button
        type="button"
        className="px-5 py-2 rounded-lg text-sm font-medium"
        style={{ background: '#D97757', color: '#000' }}
        onClick={onAction}
      >
        {actionLabel}
      </button>
    </div>
  );
}

export default function Face() {
  const { alias: aliasParam } = useParams<{ alias?: string }>();
  const [alias, setAlias] = useState<string | null>(aliasParam ?? null);
  const [error, setError] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  const navigate = useNavigate();

  useEffect(() => {
    if (aliasParam) {
      setAlias(aliasParam);
      return;
    }
    setError(null);
    getAgentOptions()
      .then((opts) => {
        if (opts.agents.length > 0) setAlias(opts.agents[0] ?? null);
        else setError('no-agents');
      })
      .catch(() => setError('load-failed'));
  }, [aliasParam, attempt]);

  if (error === 'no-agents') {
    return (
      <FaceFallback
        message="No companion configured yet"
        actionLabel="Run setup"
        onAction={() => navigate('/welcome')}
      />
    );
  }
  if (error === 'load-failed') {
    return (
      <FaceFallback
        message="Can't reach your companion — is the daemon running?"
        actionLabel="Try again"
        onAction={() => setAttempt((n) => n + 1)}
      />
    );
  }
  if (!alias) {
    return <div className="fixed inset-0 z-50" style={{ background: '#000' }} />;
  }
  return <FaceSession alias={alias} />;
}
