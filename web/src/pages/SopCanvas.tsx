import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Plus } from 'lucide-react';
import { t } from '@/lib/i18n';
import type { Sop, SopStep, NodeRunState } from '@/lib/sops';

type XY = { x: number; y: number };
type WireKind = 'sequence' | 'dependency' | 'failure' | 'switch';

interface CanvasWire {
  from: number;
  to: number;
  kind: WireKind;
  label?: string;
  port?: string;
  portIndex?: number;
}

const NODE_W = 210;
const NODE_H = 84;
const COL_GAP = 130;
const ROW_GAP = 46;

function wireStroke(kind: WireKind): string {
  switch (kind) {
    case 'failure':
      return 'var(--pc-danger, #f43f5e)';
    case 'dependency':
      return '#f59e0b';
    case 'switch':
      return '#a78bfa';
    default:
      return '#10b981';
  }
}

function nodeStateStroke(state: NodeRunState | undefined): string {
  switch (state) {
    case 'active':
      return 'var(--pc-accent)';
    case 'completed':
      return '#10b981';
    case 'failed':
      return '#f43f5e';
    case 'skipped':
      return '#f59e0b';
    default:
      return 'var(--pc-border-strong)';
  }
}

/// Layered layout: a node's column is 1 + max(column of every predecessor),
/// so dependency fan-in and explicit branches push successors rightward.
/// Rows pack per column. UI-only; never persisted (the model has no coords).
function autoLayout(steps: SopStep[]): Map<number, XY> {
  const byNum = new Map(steps.map((s) => [s.number, s]));
  const preds = new Map<number, number[]>();
  const ordered = [...steps].sort((a, b) => a.number - b.number);
  for (let i = 0; i < ordered.length; i += 1) {
    const s = ordered[i];
    if (!s) continue;
    const list: number[] = [];
    for (const d of s.routing?.depends_on ?? []) if (byNum.has(d)) list.push(d);
    const prev = ordered[i - 1];
    const explicitInbound = ordered.some((o) => o.routing?.next === s.number);
    if (list.length === 0 && prev && !explicitInbound) list.push(prev.number);
    for (const o of ordered) if (o.routing?.next === s.number) list.push(o.number);
    preds.set(s.number, list);
  }
  const col = new Map<number, number>();
  const resolve = (n: number, seen: Set<number>): number => {
    if (col.has(n)) return col.get(n) as number;
    if (seen.has(n)) return 0;
    seen.add(n);
    const ps = preds.get(n) ?? [];
    const c = ps.length === 0 ? 0 : 1 + Math.max(...ps.map((p) => resolve(p, seen)));
    col.set(n, c);
    return c;
  };
  for (const s of ordered) resolve(s.number, new Set());
  const rowByCol = new Map<number, number>();
  const pos = new Map<number, XY>();
  for (const s of ordered) {
    const c = col.get(s.number) ?? 0;
    const r = rowByCol.get(c) ?? 0;
    rowByCol.set(c, r + 1);
    pos.set(s.number, {
      x: 24 + c * (NODE_W + COL_GAP),
      y: 24 + r * (NODE_H + ROW_GAP),
    });
  }
  return pos;
}

function deriveWires(steps: SopStep[]): CanvasWire[] {
  const nums = new Set(steps.map((s) => s.number));
  const ordered = [...steps].sort((a, b) => a.number - b.number);
  const wires: CanvasWire[] = [];
  for (let i = 0; i < ordered.length; i += 1) {
    const s = ordered[i];
    if (!s) continue;
    const routing = s.routing ?? {};
    const rules = routing.switch ?? [];
    if (rules.length > 0) {
      rules.forEach((rule, ri) => {
        if (rule.goto !== undefined && rule.goto !== null && nums.has(rule.goto)) {
          wires.push({
            from: s.number,
            to: rule.goto,
            kind: 'switch',
            port: rule.name,
            portIndex: ri,
            label: rule.when ?? rule.name,
          });
        }
      });
    } else {
      const next = routing.next ?? undefined;
      const hasExplicit = next !== undefined && nums.has(next);
      if (hasExplicit) {
        wires.push({ from: s.number, to: next as number, kind: 'sequence', label: routing.when ?? undefined });
      } else if (!routing.when) {
        const nx = ordered[i + 1];
        if (nx) wires.push({ from: s.number, to: nx.number, kind: 'sequence' });
      }
    }
    for (const dep of routing.depends_on ?? []) {
      if (nums.has(dep)) wires.push({ from: dep, to: s.number, kind: 'dependency' });
    }
    const fail = s.on_failure;
    if (fail && typeof fail === 'object' && 'goto' in fail) {
      const target = fail.goto.step;
      if (nums.has(target)) wires.push({ from: s.number, to: target, kind: 'failure' });
    }
  }
  return wires;
}

function edgePath(a: XY, b: XY, sourceY?: number): string {
  const x1 = a.x + NODE_W;
  const y1 = sourceY ?? a.y + NODE_H / 2;
  const x2 = b.x;
  const y2 = b.y + NODE_H / 2;
  const dx = Math.max(40, Math.abs(x2 - x1) / 2);
  return `M ${x1} ${y1} C ${x1 + dx} ${y1}, ${x2 - dx} ${y2}, ${x2} ${y2}`;
}

const SWITCH_PORT_TOP = 34;
const SWITCH_PORT_GAP = 14;
function switchPortY(nodeY: number, index: number): number {
  return nodeY + SWITCH_PORT_TOP + index * SWITCH_PORT_GAP;
}

interface Props {
  draft: Sop;
  selectedStep: number | null;
  runStateByStep: Map<number, NodeRunState>;
  onSelectStep: (n: number) => void;
  onAddStep: () => void;
  onConnect: (from: number, to: number, kind: WireKind, portIndex?: number) => void;
}

export default function SopCanvas({
  draft,
  selectedStep,
  runStateByStep,
  onSelectStep,
  onAddStep,
  onConnect,
}: Props) {
  const [pos, setPos] = useState<Map<number, XY>>(() => autoLayout(draft.steps));
  const [drag, setDrag] = useState<{ step: number; dx: number; dy: number } | null>(null);
  const [linkFrom, setLinkFrom] = useState<number | null>(null);
  const [linkKind, setLinkKind] = useState<WireKind>('sequence');
  const [linkPort, setLinkPort] = useState<number | undefined>(undefined);
  const [cursor, setCursor] = useState<XY | null>(null);
  const svgRef = useRef<SVGSVGElement | null>(null);

  useEffect(() => {
    setPos((prev) => {
      const laid = autoLayout(draft.steps);
      const merged = new Map(laid);
      for (const [k, v] of prev) if (laid.has(k)) merged.set(k, v);
      return merged;
    });
  }, [draft.steps]);

  const wires = useMemo(() => deriveWires(draft.steps), [draft.steps]);

  const toLocal = useCallback((clientX: number, clientY: number): XY => {
    const rect = svgRef.current?.getBoundingClientRect();
    return { x: clientX - (rect?.left ?? 0), y: clientY - (rect?.top ?? 0) };
  }, []);

  const onPointerMove = useCallback(
    (e: React.PointerEvent) => {
      const p = toLocal(e.clientX, e.clientY);
      if (drag) {
        setPos((prev) => {
          const next = new Map(prev);
          next.set(drag.step, { x: p.x - drag.dx, y: p.y - drag.dy });
          return next;
        });
      }
      if (linkFrom !== null) setCursor(p);
    },
    [drag, linkFrom, toLocal],
  );

  const endDrag = useCallback(() => setDrag(null), []);

  const startLink = useCallback((step: number, kind: WireKind, port?: number) => {
    setLinkKind(kind);
    setLinkPort(port);
    setLinkFrom(step);
  }, []);

  const completeLink = useCallback(
    (target: number) => {
      if (linkFrom !== null && linkFrom !== target) onConnect(linkFrom, target, linkKind, linkPort);
      setLinkFrom(null);
      setCursor(null);
    },
    [linkFrom, linkKind, linkPort, onConnect],
  );

  const extent = useMemo(() => {
    let w = 640;
    let h = 320;
    for (const p of pos.values()) {
      w = Math.max(w, p.x + NODE_W + 48);
      h = Math.max(h, p.y + NODE_H + 48);
    }
    return { w, h };
  }, [pos]);

  const ordered = useMemo(() => [...draft.steps].sort((a, b) => a.number - b.number), [draft.steps]);

  return (
    <div className="relative overflow-auto rounded-[var(--radius-lg)] border border-pc-border bg-pc-bg-base">
      <div className="absolute right-2 top-2 z-10 flex gap-1">
        <button
          type="button"
          onClick={onAddStep}
          className="inline-flex items-center gap-1 rounded bg-pc-accent px-2 py-1 text-xs text-white"
        >
          <Plus className="h-3.5 w-3.5" aria-hidden /> {t('sops.add_step')}
        </button>
      </div>
      {linkFrom !== null ? (
        <div className="absolute left-2 top-2 z-10 rounded bg-pc-elevated px-2 py-1 text-xs text-pc-text">
          {t('sops.linking')}: {linkKind} — {t('sops.link_hint')}
          <button
            type="button"
            onClick={() => {
              setLinkFrom(null);
              setCursor(null);
            }}
            className="ml-2 text-pc-text-muted underline"
          >
            {t('sops.cancel')}
          </button>
        </div>
      ) : null}
      <svg
        ref={svgRef}
        width={extent.w}
        height={extent.h}
        onPointerMove={onPointerMove}
        onPointerUp={endDrag}
        onPointerLeave={endDrag}
        className="block touch-none select-none"
      >
        <defs>
          <marker
            id="sop-arrow"
            viewBox="0 0 10 10"
            refX="9"
            refY="5"
            markerWidth="7"
            markerHeight="7"
            orient="auto-start-reverse"
          >
            <path d="M 0 0 L 10 5 L 0 10 z" fill="context-stroke" />
          </marker>
        </defs>
        {wires.map((w, i) => {
          const a = pos.get(w.from);
          const b = pos.get(w.to);
          if (!a || !b) return null;
          const active = runStateByStep.get(w.to) === 'active';
          const srcY =
            w.kind === 'switch' && w.portIndex !== undefined
              ? switchPortY(a.y, w.portIndex)
              : undefined;
          return (
            <g key={`wire-${i}`}>
              <path
                d={edgePath(a, b, srcY)}
                fill="none"
                stroke={wireStroke(w.kind)}
                strokeWidth={active ? 3 : 1.75}
                strokeDasharray={w.kind === 'dependency' ? '5 4' : undefined}
                markerEnd="url(#sop-arrow)"
                opacity={active ? 1 : 0.85}
              >
                {active ? (
                  <animate
                    attributeName="stroke-dashoffset"
                    from="18"
                    to="0"
                    dur="0.6s"
                    repeatCount="indefinite"
                  />
                ) : null}
              </path>
              {w.label ? (
                <text
                  x={(a.x + NODE_W + b.x) / 2}
                  y={(a.y + b.y) / 2 + NODE_H / 2 - 6}
                  fill={wireStroke(w.kind)}
                  fontSize="10"
                  textAnchor="middle"
                >
                  {w.label}
                </text>
              ) : null}
            </g>
          );
        })}
        {linkFrom !== null && cursor && pos.get(linkFrom) ? (
          <path
            d={edgePath(pos.get(linkFrom) as XY, { x: cursor.x - NODE_W, y: cursor.y - NODE_H / 2 })}
            fill="none"
            stroke={wireStroke(linkKind)}
            strokeWidth={1.75}
            strokeDasharray="4 4"
          />
        ) : null}
        {ordered.map((step) => {
          const p = pos.get(step.number);
          if (!p) return null;
          const state = runStateByStep.get(step.number);
          const selected = selectedStep === step.number;
          const isCheckpoint = step.kind === 'checkpoint';
          return (
            <g
              key={step.number}
              transform={`translate(${p.x}, ${p.y})`}
              onPointerDown={(e) => {
                if (linkFrom !== null) {
                  completeLink(step.number);
                  return;
                }
                const local = toLocal(e.clientX, e.clientY);
                setDrag({ step: step.number, dx: local.x - p.x, dy: local.y - p.y });
                onSelectStep(step.number);
              }}
              className="cursor-grab"
            >
              <rect
                width={NODE_W}
                height={NODE_H}
                rx={10}
                fill="var(--pc-bg-surface)"
                stroke={nodeStateStroke(state)}
                strokeWidth={selected ? 2.5 : 1.5}
              />
              <rect width={NODE_W} height={26} rx={10} fill="var(--pc-bg-elevated)" />
              <rect y={16} width={NODE_W} height={10} fill="var(--pc-bg-elevated)" />
              <circle cx={16} cy={13} r={9} fill="var(--pc-accent-dim)" />
              <text x={16} y={17} fontSize="11" textAnchor="middle" fill="var(--pc-accent)">
                {step.number}
              </text>
              <text x={32} y={17} fontSize="12" fill="var(--pc-text-primary)">
                {(step.title || t('sops.untitled')).slice(0, 22)}
              </text>
              {isCheckpoint ? (
                <text x={NODE_W - 10} y={17} fontSize="10" textAnchor="end" fill="#f59e0b">
                  ⏸ {t('sops.checkpoint')}
                </text>
              ) : (step.routing?.switch?.length ?? 0) > 0 ? (
                <text x={NODE_W - 10} y={17} fontSize="10" textAnchor="end" fill="#a78bfa">
                  ⋔ {t('sops.switch')}
                </text>
              ) : null}
              <text x={12} y={46} fontSize="10" fill="var(--pc-text-muted)">
                {step.suggested_tools && step.suggested_tools.length > 0
                  ? step.suggested_tools.slice(0, 3).join(', ')
                  : t('sops.no_tools')}
              </text>
              {state ? (
                <text x={12} y={64} fontSize="10" fill={nodeStateStroke(state)}>
                  {t(`sops.run_state.${state}`)}
                </text>
              ) : null}
              {(step.routing?.switch?.length ?? 0) > 0 ? (
                <g>
                  {(step.routing?.switch ?? []).map((rule, ri) => (
                    <g key={`port-${ri}`}>
                      <text
                        x={NODE_W - 16}
                        y={SWITCH_PORT_TOP + ri * SWITCH_PORT_GAP + 3}
                        fontSize="9"
                        textAnchor="end"
                        fill="#a78bfa"
                      >
                        {(rule.name || `port ${ri + 1}`).slice(0, 16)}
                      </text>
                      <circle
                        cx={NODE_W}
                        cy={SWITCH_PORT_TOP + ri * SWITCH_PORT_GAP}
                        r={5}
                        fill={wireStroke('switch')}
                        onPointerDown={(e) => {
                          e.stopPropagation();
                          startLink(step.number, 'switch', ri);
                        }}
                        className="cursor-crosshair"
                      >
                        <title>
                          {t('sops.handle_switch')}: {rule.name}
                        </title>
                      </circle>
                    </g>
                  ))}
                </g>
              ) : (
                <g>
                  <circle
                    cx={NODE_W}
                    cy={NODE_H / 2}
                    r={6}
                    fill={wireStroke('sequence')}
                    onPointerDown={(e) => {
                      e.stopPropagation();
                      startLink(step.number, 'sequence');
                    }}
                    className="cursor-crosshair"
                  >
                    <title>{t('sops.handle_sequence')}</title>
                  </circle>
                  <circle
                    cx={NODE_W}
                    cy={NODE_H / 2 + 18}
                    r={5}
                    fill={wireStroke('dependency')}
                    onPointerDown={(e) => {
                      e.stopPropagation();
                      startLink(step.number, 'dependency');
                    }}
                    className="cursor-crosshair"
                  >
                    <title>{t('sops.handle_dependency')}</title>
                  </circle>
                  <circle
                    cx={NODE_W}
                    cy={NODE_H / 2 - 18}
                    r={5}
                    fill={wireStroke('failure')}
                    onPointerDown={(e) => {
                      e.stopPropagation();
                      startLink(step.number, 'failure');
                    }}
                    className="cursor-crosshair"
                  >
                    <title>{t('sops.handle_failure')}</title>
                  </circle>
                </g>
              )}
              <circle cx={0} cy={NODE_H / 2} r={5} fill="var(--pc-border-strong)" />
            </g>
          );
        })}
      </svg>
    </div>
  );
}
