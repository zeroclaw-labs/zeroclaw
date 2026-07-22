/**
 * ClawdLab — animation workbench for the mascot.
 *
 * Fires any of the 60+ action clips, emotions, and live talk/listen states
 * against the real rig. Used to tune motion design; also a fun easter egg
 * (/clawd-lab).
 */
import { useMemo, useRef, useState } from 'react';
import ClawdAvatar, { type ClawdHandle } from '../components/clawd/ClawdAvatar';
import { ACTIONS } from '../lib/clawd/actions';
import { EMOTIONS, type EmotionName } from '../lib/clawd/engine';

const GROUPS: Record<string, string[]> = {
  Social: ['wave', 'waveLeft', 'waveBoth', 'greetSequence', 'salute', 'bow', 'highFive', 'fistPump', 'point', 'shrug', 'blowKiss', 'wink'],
  Motion: ['rollLeft', 'rollRight', 'jump', 'doubleJump', 'hop', 'bounce', 'walkLeft', 'walkRight', 'walkBack', 'run', 'moonwalk', 'scoot', 'stomp', 'tapFoot', 'float', 'land'],
  Flips: ['spin', 'spinJump', 'backflip', 'pirouette'],
  Feelings: ['nod', 'shakeHead', 'cheer', 'celebrate', 'dance', 'headbang', 'wiggle', 'wobble', 'shiver', 'laugh', 'giggle', 'gasp', 'surprise', 'pout', 'cry', 'angry', 'facepalm', 'nervous'],
  Mind: ['think', 'hmm', 'confused', 'dizzy', 'idea', 'search', 'ponder', 'agree', 'disagree', 'attention'],
  Gaze: ['lookLeft', 'lookRight', 'lookUp', 'lookDown', 'lookAround', 'doubleTake', 'sideEye', 'peek', 'hide'],
  Rituals: ['yawn', 'stretch', 'squashJelly', 'sleep', 'wakeUp', 'sneeze', 'typeFuriously'],
  Flair: ['heartEyes', 'starEyes', 'sparkle', 'powerUp', 'glitch'],
};

export default function ClawdLab() {
  const clawd = useRef<ClawdHandle>(null);
  const [active, setActive] = useState<string | null>(null);
  const [emotion, setEmotion] = useState<EmotionName>('neutral');
  const [talking, setTalking] = useState(false);
  const [listening, setListening] = useState(false);

  const uncategorized = useMemo(() => {
    const listed = new Set(Object.values(GROUPS).flat());
    return Object.keys(ACTIONS).filter((a) => !listed.has(a));
  }, []);

  const fire = (name: string) => {
    setActive(name);
    clawd.current?.stopAction();
    void clawd.current?.play(name)?.then(() => setActive((a) => (a === name ? null : a)));
  };

  // fake talk envelope so the talking toggle looks real
  const talkTimer = useRef<ReturnType<typeof setInterval> | null>(null);
  const toggleTalking = () => {
    const next = !talking;
    setTalking(next);
    clawd.current?.setTalking(next);
    if (talkTimer.current) {
      clearInterval(talkTimer.current);
      talkTimer.current = null;
    }
    if (next) {
      talkTimer.current = setInterval(() => {
        clawd.current?.setTalkLevel(0.25 + Math.random() * 0.6);
      }, 90);
    } else {
      clawd.current?.setTalkLevel(0);
    }
  };

  return (
    <div className="flex h-full min-h-[85vh] gap-6 p-6" style={{ background: '#000' }}>
      {/* stage */}
      <div className="flex-1 flex flex-col items-center justify-center rounded-2xl" style={{ background: '#050505', border: '1px solid #1a1a1a' }}>
        <ClawdAvatar ref={clawd} size={420} />
        <div className="mt-4 h-5 text-[12px] tracking-[0.3em] uppercase" style={{ color: '#D97757' }}>
          {active ?? emotion}
        </div>
        <div className="mt-4 flex gap-2">
          <button
            type="button"
            onClick={toggleTalking}
            className="px-4 py-1.5 rounded-full text-xs font-medium"
            style={{
              background: talking ? '#D97757' : 'transparent',
              color: talking ? '#000' : '#D97757',
              border: '1px solid #D97757',
            }}
          >
            talking
          </button>
          <button
            type="button"
            onClick={() => {
              const next = !listening;
              setListening(next);
              clawd.current?.setListening(next);
            }}
            className="px-4 py-1.5 rounded-full text-xs font-medium"
            style={{
              background: listening ? '#FAF9F5' : 'transparent',
              color: listening ? '#000' : '#FAF9F5',
              border: '1px solid #FAF9F5',
            }}
          >
            listening
          </button>
        </div>
        <div className="mt-4 flex flex-wrap justify-center gap-1.5 max-w-md">
          {(Object.keys(EMOTIONS) as EmotionName[]).map((e) => (
            <button
              key={e}
              type="button"
              onClick={() => {
                setEmotion(e);
                clawd.current?.setEmotion(e);
              }}
              className="px-2.5 py-1 rounded-md text-[11px]"
              style={{
                background: emotion === e ? '#1f1f1f' : 'transparent',
                color: emotion === e ? '#D97757' : '#777',
                border: '1px solid #1f1f1f',
              }}
            >
              {e}
            </button>
          ))}
        </div>
      </div>

      {/* action palette */}
      <div className="w-80 overflow-y-auto rounded-2xl p-4 space-y-4" style={{ background: '#050505', border: '1px solid #1a1a1a' }}>
        {[...Object.entries(GROUPS), ...(uncategorized.length ? [['Other', uncategorized] as const] : [])].map(
          ([group, names]) => (
            <div key={group}>
              <div className="text-[10px] tracking-[0.25em] uppercase mb-2" style={{ color: '#555' }}>
                {group}
              </div>
              <div className="flex flex-wrap gap-1.5">
                {names.map((name) => (
                  <button
                    key={name}
                    type="button"
                    onClick={() => fire(name)}
                    className="px-2.5 py-1 rounded-md text-[11px] transition-colors"
                    style={{
                      background: active === name ? '#D97757' : '#0f0f0f',
                      color: active === name ? '#000' : '#bbb',
                      border: '1px solid #1f1f1f',
                    }}
                  >
                    {name}
                  </button>
                ))}
              </div>
            </div>
          ),
        )}
      </div>
    </div>
  );
}
