/**
 * ClawdAvatar — the animated mascot.
 *
 * Renders the Clawd rig (blocky terracotta body, two stub arms, four legs,
 * expressive eyes) as SVG and drives it from a ClawdAnimator on an adaptive
 * frame loop (see lib/clawd/adaptiveFrameLoop.ts — throttles to 60/30/20fps
 * and pauses when the tab is hidden). Transforms are applied imperatively via
 * refs so React never re-renders during animation; only eye-shape swaps go
 * through state (a few times a minute at most).
 */
import {
  forwardRef,
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
} from 'react';
import {
  ClawdAnimator,
  type EmotionName,
  type EyeShape,
  type Pose,
} from '../../lib/clawd/engine';
import { ACTIONS, IDLE_FIDGETS } from '../../lib/clawd/actions';
import { startAdaptiveFrameLoop } from '../../lib/clawd/adaptiveFrameLoop';

export interface ClawdHandle {
  play: (action: string) => Promise<void>;
  stopAction: () => void;
  setEmotion: (e: EmotionName) => void;
  setTalking: (on: boolean) => void;
  setTalkLevel: (level: number) => void;
  setListening: (on: boolean) => void;
  lookAt: (x: number, y: number) => void;
  listActions: () => string[];
}

interface Props {
  /** overall pixel size; the rig keeps its aspect ratio inside */
  size?: number;
  /** body fill — defaults to Clawd terracotta */
  bodyColor?: string;
  eyeColor?: string;
  /** enable random idle fidgets */
  fidget?: boolean;
  className?: string;
}

const BODY = '#D97757';
const EYE = '#FAF9F5';
const GLOW = '#F0894C';

/** Renders one eye's shape centered on (0,0); `right` mirrors asymmetric shapes. */
function EyeGlyph({ shape, color, right }: { shape: EyeShape; color: string; right: boolean }) {
  const stroke = { stroke: color, strokeWidth: 6, strokeLinecap: 'round' as const, fill: 'none' };
  switch (shape) {
    case 'rect':
      return <rect x={-6} y={-15} width={12} height={30} rx={5} fill={color} />;
    case 'wide':
      return <rect x={-8} y={-17} width={16} height={34} rx={6} fill={color} />;
    case 'happy':
      return <path d="M -11 6 Q 0 -12 11 6" {...stroke} />;
    case 'squint':
      // > < — inner-pointing chevrons
      return right ? (
        <path d="M 8 -10 L -6 0 L 8 10" {...stroke} />
      ) : (
        <path d="M -8 -10 L 6 0 L -8 10" {...stroke} />
      );
    case 'closed':
      return <line x1={-10} y1={0} x2={10} y2={0} {...stroke} />;
    case 'sleepy':
      return <rect x={-7} y={-2} width={14} height={14} rx={5} fill={color} />;
    case 'sad':
      return (
        <g transform={`rotate(${right ? -14 : 14})`}>
          <rect x={-5.5} y={-12} width={11} height={26} rx={5} fill={color} />
        </g>
      );
    case 'angry':
      return (
        <g transform={`rotate(${right ? 18 : -18})`}>
          <rect x={-6} y={-13} width={12} height={26} rx={5} fill={color} />
        </g>
      );
    case 'heart':
      return (
        <path
          d="M 0 8 C -14 -2 -10 -14 -1 -9 L 0 -8 L 1 -9 C 10 -14 14 -2 0 8 Z"
          fill={color}
          transform="scale(1.35)"
        />
      );
    case 'star':
      return (
        <path
          d="M 0 -13 L 3.5 -4 L 13 -4 L 5.5 2 L 8.5 12 L 0 6 L -8.5 12 L -5.5 2 L -13 -4 L -3.5 -4 Z"
          fill={color}
        />
      );
    case 'dizzy':
      return (
        <path
          d="M 0 0 m 0 -2 a 2 2 0 1 1 -2 2 a 4 4 0 1 1 4 -4 a 8 8 0 1 1 -8 8 a 12 12 0 1 1 12 -12"
          {...stroke}
          strokeWidth={4}
        />
      );
    case 'o':
      return <circle r={11} {...stroke} />;
    case 'x':
      return (
        <g {...{ stroke: color, strokeWidth: 6, strokeLinecap: 'round' as const }}>
          <line x1={-9} y1={-9} x2={9} y2={9} />
          <line x1={9} y1={-9} x2={-9} y2={9} />
        </g>
      );
    case 'wink':
      return right ? (
        <rect x={-6} y={-15} width={12} height={30} rx={5} fill={color} />
      ) : (
        <line x1={-10} y1={0} x2={10} y2={0} {...stroke} />
      );
    case 'suspicious':
      return <rect x={-8} y={-5} width={16} height={10} rx={4} fill={color} />;
    case 'sparkle':
      return (
        <path
          d="M 0 -14 Q 2 -2 14 0 Q 2 2 0 14 Q -2 2 -14 0 Q -2 -2 0 -14 Z"
          fill={color}
        />
      );
    default:
      return <rect x={-6} y={-15} width={12} height={30} rx={5} fill={color} />;
  }
}

export const ClawdAvatar = forwardRef<ClawdHandle, Props>(function ClawdAvatar(
  { size = 480, bodyColor = BODY, eyeColor = EYE, fidget = true, className },
  ref,
) {
  const animRef = useRef<ClawdAnimator | null>(null);
  if (!animRef.current) animRef.current = new ClawdAnimator(ACTIONS);
  const anim = animRef.current;

  const rootRef = useRef<SVGGElement>(null);
  const armLRef = useRef<SVGGElement>(null);
  const armRRef = useRef<SVGGElement>(null);
  const legRefs = [
    useRef<SVGRectElement>(null),
    useRef<SVGRectElement>(null),
    useRef<SVGRectElement>(null),
    useRef<SVGRectElement>(null),
  ];
  const eyeLRef = useRef<SVGGElement>(null);
  const eyeRRef = useRef<SVGGElement>(null);
  const glowRef = useRef<SVGEllipseElement>(null);
  const shadowRef = useRef<SVGEllipseElement>(null);

  const [eyeShape, setEyeShape] = useState<EyeShape>('rect');
  const eyeShapeRef = useRef<EyeShape>('rect');

  useImperativeHandle(ref, () => ({
    play: (a) => anim.play(a),
    stopAction: () => anim.stopAction(),
    setEmotion: (e) => anim.setEmotion(e),
    setTalking: (on) => anim.setTalking(on),
    setTalkLevel: (l) => anim.setTalkLevel(l),
    setListening: (on) => anim.setListening(on),
    lookAt: (x, y) => anim.lookAt(x, y),
    listActions: () => anim.listActions(),
  }), [anim]);

  useEffect(() => {
    anim.setFidgetPicker(
      fidget
        ? () => IDLE_FIDGETS[Math.floor(Math.random() * IDLE_FIDGETS.length)] ?? null
        : null,
    );
  }, [anim, fidget]);

  useEffect(() => {
    const legsY = [132, 132, 132, 132];

    const stop = startAdaptiveFrameLoop((dt) => {
      const p: Pose = anim.update(dt);

      if (rootRef.current) {
        rootRef.current.setAttribute(
          'transform',
          `translate(${120 + p.body.x} ${100 + p.body.y}) rotate(${p.body.rot}) scale(${p.body.sx} ${p.body.sy}) translate(-120 -100)`,
        );
      }
      armLRef.current?.setAttribute('transform', `rotate(${p.armL.rot} 50 89)`);
      armRRef.current?.setAttribute('transform', `rotate(${p.armR.rot} 190 89)`);
      for (let i = 0; i < 4; i++) {
        legRefs[i]?.current?.setAttribute('y', String((legsY[i] ?? 132) + (p.legs[i] ?? 0)));
      }
      const eyeScale = p.eyes.size;
      const openScale = Math.max(0.06, p.eyes.open);
      eyeLRef.current?.setAttribute(
        'transform',
        `translate(${82 + p.eyes.dx} ${74 + p.eyes.dy}) scale(${eyeScale} ${eyeScale * openScale})`,
      );
      eyeRRef.current?.setAttribute(
        'transform',
        `translate(${158 + p.eyes.dx} ${74 + p.eyes.dy}) scale(${eyeScale} ${eyeScale * openScale})`,
      );
      if (glowRef.current) {
        glowRef.current.setAttribute('opacity', String(p.glow * 0.55));
        const gs = 1 + p.glow * 0.12;
        glowRef.current.setAttribute('transform', `translate(120 95) scale(${gs})`);
      }
      if (shadowRef.current) {
        // shadow shrinks and fades as Clawd leaves the ground
        const lift = Math.max(0, -p.body.y);
        const s = Math.max(0.45, 1 - lift / 70);
        shadowRef.current.setAttribute('transform', `translate(${120 + p.body.x} 176) scale(${s} ${s})`);
        shadowRef.current.setAttribute('opacity', String(0.35 * s));
      }
      if (p.eyes.shape !== eyeShapeRef.current) {
        eyeShapeRef.current = p.eyes.shape;
        setEyeShape(p.eyes.shape);
      }
    });
    return stop;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [anim]);

  return (
    <svg
      viewBox="0 0 240 200"
      width={size}
      height={(size * 200) / 240}
      className={className}
      role="img"
      aria-label="Clawd"
      style={{ overflow: 'visible' }}
    >
      <defs>
        <radialGradient id="clawd-glow" cx="50%" cy="50%" r="50%">
          <stop offset="0%" stopColor={GLOW} stopOpacity="0.9" />
          <stop offset="60%" stopColor={GLOW} stopOpacity="0.25" />
          <stop offset="100%" stopColor={GLOW} stopOpacity="0" />
        </radialGradient>
      </defs>

      {/* ambient glow ring (listening / celebration energy) */}
      <ellipse ref={glowRef} rx={118} ry={100} fill="url(#clawd-glow)" opacity={0} />

      {/* ground shadow */}
      <ellipse ref={shadowRef} rx={78} ry={9} fill="#000" opacity={0.35} />

      <g ref={rootRef}>
        {/* arms behind body edges */}
        <g ref={armLRef}>
          <rect x={22} y={76} width={30} height={26} rx={5} fill={bodyColor} />
        </g>
        <g ref={armRRef}>
          <rect x={188} y={76} width={30} height={26} rx={5} fill={bodyColor} />
        </g>

        {/* legs */}
        <rect ref={legRefs[0]} x={58} y={132} width={18} height={34} rx={4} fill={bodyColor} />
        <rect ref={legRefs[1]} x={94} y={132} width={18} height={34} rx={4} fill={bodyColor} />
        <rect ref={legRefs[2]} x={128} y={132} width={18} height={34} rx={4} fill={bodyColor} />
        <rect ref={legRefs[3]} x={164} y={132} width={18} height={34} rx={4} fill={bodyColor} />

        {/* body slab — blocky with pixel-step top corners */}
        <path
          d={[
            'M 60 30',
            'L 180 30',
            'Q 186 30 186 36',
            'L 186 40 L 192 40 Q 196 40 196 44',
            'L 196 130 Q 196 136 190 136',
            'L 50 136 Q 44 136 44 130',
            'L 44 44 Q 44 40 48 40',
            'L 54 40 L 54 36 Q 54 30 60 30',
            'Z',
          ].join(' ')}
          fill={bodyColor}
        />

        {/* eyes */}
        <g ref={eyeLRef}>
          <EyeGlyph shape={eyeShape} color={eyeColor} right={false} />
        </g>
        <g ref={eyeRRef}>
          <EyeGlyph shape={eyeShape} color={eyeColor} right={true} />
        </g>
      </g>
    </svg>
  );
});

export default ClawdAvatar;
