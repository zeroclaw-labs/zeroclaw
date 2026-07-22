/**
 * StepAwaken — the wizard's deep-integration loading screen.
 *
 * Runs automatically: writes the soul generated from the wizard's name +
 * seed into the agent's workspace, then runs the first real turn through
 * the model — it absorbs the identity, stores a "Who I am" Core memory,
 * and speaks its first words, which we show before "Meet them".
 *
 * Every stage is resilient: a failure marks the stage degraded and lets
 * the user continue (or retry) — awakening never bricks the wizard.
 */
import { useCallback, useEffect, useRef, useState } from 'react';
import ClawdAvatar, { type ClawdHandle } from '../../components/clawd/ClawdAvatar';
import {
  bootstrapPrompt,
  runBootstrapTurn,
  writeSoulFromSeed,
} from '../../lib/companionSetup';
import { GhostButton, PrimaryButton } from './ui';

type StageState = 'pending' | 'running' | 'done' | 'degraded';

interface Stage {
  id: 'soul' | 'mind' | 'ready';
  label: string;
  state: StageState;
  detail?: string;
}

const C = {
  accent: '#D97757',
  text: '#FAF9F5',
  muted: '#9a938c',
  faint: '#5c5854',
  border: '#242220',
  surface: '#0d0c0b',
};

interface Props {
  agentAlias: string;
  name: string;
  seed: string;
  onBack: () => void;
  onDone: (firstWords: string) => void;
}

export default function StepAwaken({ agentAlias, name, seed, onBack, onDone }: Props) {
  const clawd = useRef<ClawdHandle>(null);
  const startedRef = useRef(false);
  const [stages, setStages] = useState<Stage[]>([
    { id: 'soul', label: 'Writing their soul', state: 'pending' },
    { id: 'mind', label: 'Waking their mind', state: 'pending' },
    { id: 'ready', label: 'First words', state: 'pending' },
  ]);
  const [firstWords, setFirstWords] = useState('');
  const [activity, setActivity] = useState('');
  const [finished, setFinished] = useState(false);
  const [anyDegraded, setAnyDegraded] = useState(false);

  const setStage = useCallback((id: Stage['id'], state: StageState, detail?: string) => {
    setStages((prev) => prev.map((s) => (s.id === id ? { ...s, state, detail } : s)));
    if (state === 'degraded') setAnyDegraded(true);
  }, []);

  const run = useCallback(async () => {
    setFinished(false);
    setAnyDegraded(false);
    setFirstWords('');
    setStages((prev) => prev.map((s) => ({ ...s, state: 'pending', detail: undefined })));

    clawd.current?.setEmotion('sleepy');
    void clawd.current?.play('sleep');

    // ── 1. Soul files ──
    setStage('soul', 'running');
    try {
      await writeSoulFromSeed({ agentAlias, name, seed });
      setStage('soul', 'done');
    } catch (e) {
      setStage('soul', 'degraded', e instanceof Error ? e.message : String(e));
    }

    // ── 2. First turn through the model ──
    setStage('mind', 'running');
    clawd.current?.setEmotion('thinking');
    void clawd.current?.play('wakeUp');
    const result = await runBootstrapTurn(
      agentAlias,
      bootstrapPrompt(name, seed),
      (label) => setActivity(label),
    );
    setActivity('');
    if (result.degraded) {
      setStage('mind', 'degraded', result.detail);
    } else {
      setStage('mind', 'done');
    }

    // ── 3. First words ──
    if (result.firstWords) {
      setStage('ready', 'done');
      setFirstWords(result.firstWords);
      clawd.current?.setEmotion('happy');
      void clawd.current?.play('greetSequence');
    } else {
      setStage('ready', 'degraded', 'no reply — they will still know who they are');
      clawd.current?.setEmotion('neutral');
      void clawd.current?.play('wakeUp');
    }
    setFinished(true);
  }, [agentAlias, name, seed, setStage]);

  useEffect(() => {
    if (startedRef.current) return;
    startedRef.current = true;
    void run();
  }, [run]);

  return (
    <div className="wlc-fade-up" style={{ textAlign: 'center' }}>
      <p
        style={{
          fontSize: 11,
          letterSpacing: '0.28em',
          textTransform: 'uppercase',
          color: C.accent,
          marginBottom: 6,
        }}
      >
        Awakening
      </p>
      <h2 style={{ fontSize: 26, fontWeight: 700, color: C.text, marginBottom: 22 }}>
        {name || 'Your companion'} is waking up
      </h2>

      <div style={{ display: 'flex', justifyContent: 'center', marginBottom: 8 }}>
        <ClawdAvatar ref={clawd} size={230} fidget={false} />
      </div>

      {/* stages */}
      <div
        style={{
          maxWidth: 380,
          margin: '0 auto 18px',
          textAlign: 'left',
          background: C.surface,
          border: `1px solid ${C.border}`,
          borderRadius: 12,
          padding: '14px 18px',
        }}
      >
        {stages.map((s) => (
          <div
            key={s.id}
            style={{ display: 'flex', alignItems: 'center', gap: 10, padding: '7px 0' }}
          >
            <span style={{ width: 16, display: 'inline-flex', justifyContent: 'center' }}>
              {s.state === 'running' ? (
                <span className="wlc-spinner" />
              ) : s.state === 'done' ? (
                <span style={{ color: C.accent, fontWeight: 700 }}>✓</span>
              ) : s.state === 'degraded' ? (
                <span style={{ color: '#a8623f' }}>!</span>
              ) : (
                <span style={{ color: C.faint }}>·</span>
              )}
            </span>
            <span
              style={{
                fontSize: 13.5,
                color: s.state === 'pending' ? C.faint : C.text,
                flex: 1,
              }}
            >
              {s.label}
              {s.id === 'mind' && s.state === 'running' && activity ? (
                <span style={{ color: C.muted }}> — {activity}…</span>
              ) : null}
            </span>
            {s.detail ? (
              <span style={{ fontSize: 11, color: C.faint, maxWidth: 150 }}>{s.detail}</span>
            ) : null}
          </div>
        ))}
      </div>

      {/* first words */}
      {firstWords ? (
        <blockquote
          className="wlc-fade-up"
          style={{
            maxWidth: 420,
            margin: '0 auto 20px',
            fontSize: 16,
            lineHeight: 1.5,
            color: C.text,
            fontStyle: 'italic',
          }}
        >
          “{firstWords}”
        </blockquote>
      ) : null}

      <div style={{ display: 'flex', justifyContent: 'center', gap: 10 }}>
        {finished ? (
          <>
            {anyDegraded ? (
              <GhostButton onClick={() => void run()}>Try again</GhostButton>
            ) : null}
            <PrimaryButton onClick={() => onDone(firstWords)}>Continue</PrimaryButton>
          </>
        ) : (
          <GhostButton onClick={onBack}>Back</GhostButton>
        )}
      </div>
    </div>
  );
}
