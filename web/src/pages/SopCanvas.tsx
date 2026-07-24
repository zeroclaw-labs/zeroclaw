import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Plus, Undo2 } from 'lucide-react';
import { t } from '@/lib/i18n';
import {
  runStateTone,
  flowRoleTone,
  layoutGeometry,
  getGraphLegend,
  indexLegend,
  indexLegendLabels,
  CANONICAL_LAYOUT_GEOMETRY,
  type RunStateTone,
  type WireTone,
  type Sop,
  type SopStep,
  type NodeRunState,
  type SopGraph,
  type GraphNode,
  type GraphPin,
  type GraphWire,
  type GraphLegend,
  type FlowRole,
} from '@/lib/sops';

type XY = { x: number; y: number };

// Node box dimensions read from the shared geometry registry
// (`zeroclaw-sop-graph::LayoutGeometry`). Per-graph placement pitch/origin come
// off `graph.layout.geometry` in seedPositions; the box size is fixed-canonical
// and only drives local rendering math, so it binds to the canonical fallback.
const NODE_W = CANONICAL_LAYOUT_GEOMETRY.node_w;
const NODE_H = CANONICAL_LAYOUT_GEOMETRY.node_h;

// Wire and node colors read from the gateway theme's semantic status tokens
// and accent, so the canvas follows light/dark and the active palette instead
// of fixed hex. The tone semantics come from the shared `wireTone`/
// `runStateTone` maps; this file only binds tones to CSS variables.
const WIRE_STROKE: Record<WireTone, string> = {
  data: 'var(--color-status-info)',
  error: 'var(--color-status-error)',
  warning: 'var(--color-status-warning)',
  switch: 'var(--pc-accent-light)',
  accent: 'var(--pc-accent)',
  success: 'var(--color-status-success)',
};

function wireStroke(kind: FlowRole): string {
  return WIRE_STROKE[flowRoleTone(kind)];
}

const NODE_STROKE: Record<RunStateTone, string> = {
  accent: 'var(--pc-accent)',
  success: 'var(--color-status-success)',
  error: 'var(--color-status-error)',
  warning: 'var(--color-status-warning)',
  neutral: 'var(--pc-border-strong)',
};

function nodeStateStroke(state: NodeRunState | undefined): string {
  return NODE_STROKE[runStateTone(state)];
}

/// Seed positions from the backend layout. The layout (columns/rows walked from
/// the projected edges) is the single source of node placement; the canvas only
/// maps grid slots onto pixels and lets the user drag from there. Placement
/// pitch and origin come from the shared geometry registry carried on
/// `graph.layout.geometry`, so no layout constant is duplicated client-side.
function seedPositions(graph: SopGraph): Map<number, XY> {
  const pos = new Map<number, XY>();
  const g = layoutGeometry(graph);
  const colPitch = g.node_w + g.col_gap;
  const rowPitch = g.node_h + g.row_gap;
  for (const p of graph.layout.positions) {
    if (p.x != null && p.y != null) {
      pos.set(p.step, { x: p.x, y: p.y });
    } else {
      pos.set(p.step, {
        x: g.origin + p.col * colPitch,
        y: g.origin + p.row * rowPitch,
      });
    }
  }
  return pos;
}

function edgePath(a: XY, b: XY, sourceY?: number, targetY?: number): string {
  const x1 = a.x + NODE_W;
  const y1 = sourceY ?? a.y + NODE_H / 2;
  const x2 = b.x;
  const y2 = targetY ?? b.y + NODE_H / 2;
  const dx = Math.max(40, Math.abs(x2 - x1) / 2);
  return `M ${x1} ${y1} C ${x1 + dx} ${y1}, ${x2 - dx} ${y2}, ${x2} ${y2}`;
}

// Vertical lanes inside a node, laid out top-to-bottom with no overlap:
//   header band          title (0..26)
//   flow lane             failure / sequence / dependency handles (both edges)
//   switch-port lane      one handle per switch rule (right edge, switch only)
//   data-pin lane         data in/out pins (left/right edges)
// Every zone gets its own y-range so handles never collide across kinds.

// Flow handle lane. Three fixed rows under the header, spaced far enough that
// r<=6 dots never touch. Order top-to-bottom: failure, sequence, dependency.
const FLOW_LANE_TOP = 42;
const FLOW_LANE_GAP = 16;
const FLOW_ROW: Record<'failure' | 'sequence' | 'dependency', number> = {
  failure: 0,
  sequence: 1,
  dependency: 2,
};
function flowLaneY(nodeY: number, kind: 'failure' | 'sequence' | 'dependency'): number {
  return nodeY + FLOW_LANE_TOP + FLOW_ROW[kind] * FLOW_LANE_GAP;
}
// Bottom of the flow lane (last row center + a little breathing room).
const FLOW_LANE_BOTTOM = FLOW_LANE_TOP + FLOW_ROW.dependency * FLOW_LANE_GAP + 10;

// Switch-port lane sits directly below the flow lane on switch nodes.
const SWITCH_PORT_GAP = 16;
function switchPortTop(): number {
  return FLOW_LANE_BOTTOM + 6;
}
function switchPortY(nodeY: number, index: number): number {
  return nodeY + switchPortTop() + index * SWITCH_PORT_GAP;
}

function switchPortLaneBottom(ruleCount: number): number {
  if (ruleCount <= 0) return FLOW_LANE_BOTTOM;
  return switchPortTop() + (ruleCount - 1) * SWITCH_PORT_GAP + 10;
}

const DATA_PIN_GAP = 16;
// Output data pins share the right edge with switch ports, so they land below
// the switch-port lane. Input data pins share the left edge with only the
// inbound flow handles, so they anchor just under the inbound flow lane and do
// not inherit the right-side switch-port offset.
function dataPinTop(side: 'inputs' | 'outputs', ruleCount: number): number {
  return side === 'inputs' ? FLOW_LANE_BOTTOM + 8 : switchPortLaneBottom(ruleCount) + 8;
}
function dataPinY(
  nodeY: number,
  side: 'inputs' | 'outputs',
  index: number,
  ruleCount: number,
): number {
  return nodeY + dataPinTop(side, ruleCount) + index * DATA_PIN_GAP;
}

function dataPins(node: GraphNode, side: 'inputs' | 'outputs'): GraphPin[] {
  return node[side].filter((p) => p.class === 'data');
}

function contentBottom(node: GraphNode, ruleCount: number): number {
  const inputs = dataPins(node, 'inputs').length;
  const outputs = dataPins(node, 'outputs').length;
  const inputBottom = inputs > 0 ? dataPinTop('inputs', ruleCount) + (inputs - 1) * DATA_PIN_GAP + 6 : 0;
  const outputBottom =
    outputs > 0 ? dataPinTop('outputs', ruleCount) + (outputs - 1) * DATA_PIN_GAP + 6 : 0;
  const switchBottom = ruleCount > 0 ? switchPortLaneBottom(ruleCount) + 6 : 0;
  const flowBottom = flowLaneY(0, 'dependency') + 8;
  return Math.max(inputBottom, outputBottom, switchBottom, flowBottom);
}

const TOOLS_BAR_H = 22;

function toolsBarTop(node: GraphNode, ruleCount: number): number {
  return contentBottom(node, ruleCount) + 4;
}

function nodeHeight(node: GraphNode, ruleCount: number, expandedRows: number): number {
  const barBlock = TOOLS_BAR_H + (expandedRows > 0 ? expandedRows * 16 + 8 : 0);
  return toolsBarTop(node, ruleCount) + barBlock + 6;
}

function dataTypesCompatible(from: string | null, to: string | null): boolean {
  return from === null || to === null || from === to;
}

function argsEntries(args: unknown): [string, string][] {
  if (args === null || args === undefined) return [];
  if (typeof args === 'object' && !Array.isArray(args)) {
    return Object.entries(args as Record<string, unknown>).map(([k, v]) => {
      let val: string;
      try {
        val = typeof v === 'string' ? v : JSON.stringify(v);
      } catch {
        val = String(v);
      }
      return [k, val.length > 32 ? `${val.slice(0, 32)}…` : val];
    });
  }
  let s: string;
  try {
    s = typeof args === 'string' ? args : JSON.stringify(args);
  } catch {
    return [];
  }
  return s ? [['', s.length > 40 ? `${s.slice(0, 40)}…` : s]] : [];
}

function flowOutY(nodeY: number, kind: FlowRole): number | undefined {
  if (kind === 'sequence' || kind === 'failure' || kind === 'dependency') {
    return flowLaneY(nodeY, kind);
  }
  return undefined;
}

// Inbound flow anchors mirror the outbound lane so a wire lands on a visible
// handle instead of the node's bare edge.
function flowInY(nodeY: number, kind: FlowRole): number {
  if (kind === 'sequence' || kind === 'failure' || kind === 'dependency') {
    return flowLaneY(nodeY, kind);
  }
  return nodeY + FLOW_LANE_TOP + FLOW_ROW.sequence * FLOW_LANE_GAP;
}

// Pointer travel (px) allowed between a wire press and release before the
// gesture is treated as a pan instead of a delete-click.
const WIRE_CLICK_SLOP = 4;

const EMPTY_RUN_STATE: Map<number, NodeRunState> = new Map();

interface Props {
  draft: Sop;
  graph: SopGraph;
  selectedStep: number | null;
  runStateByStep?: Map<number, NodeRunState>;
  readOnly?: boolean;
  onSelectStep: (n: number) => void;
  onSelectTrigger: (index: number) => void;
  onAddStep: () => void;
  onRemoveStep?: (n: number) => void;
  onConnect: (from: number, to: number, kind: FlowRole, portIndex?: number) => void;
  onDisconnect: (from: number, to: number, kind: FlowRole, portIndex?: number) => void;
  onConnectData: (fromStep: number, fromPin: string, toStep: number, toPin: string) => void;
  onDisconnectData: (toStep: number, toPin: string) => void;
  onMoveNode?: (step: number, x: number, y: number) => void;
  onUndo?: () => void;
  canUndo?: boolean;
}

type ContextMenu = { x: number; y: number; step: number | null };

const LEGEND_WIRE_DASH: Partial<Record<FlowRole, string>> = {
  dependency: '5 4',
  trigger: '4 3',
};

function CanvasLegend({ legend }: { legend: GraphLegend | null }) {
  const [open, setOpen] = useState(false);
  const flowRoles = legend?.flow_roles ?? [];
  const dataDesc =
    legend?.pin_classes.find((p) => p.key === 'data')?.description ?? t('sops.legend_data');
  return (
    <div className="absolute bottom-2 left-2 z-10">
      {open ? (
        <div className="rounded-[var(--radius-lg)] border border-pc-border bg-pc-surface p-2 text-xs shadow-lg">
          <div className="mb-1 flex items-center justify-between gap-4">
            <span className="font-medium text-pc-text">{t('sops.legend_title')}</span>
            <button
              type="button"
              onClick={() => setOpen(false)}
              className="text-pc-text-muted hover:text-pc-text"
              aria-label={t('sops.cancel')}
            >
              ×
            </button>
          </div>
          <div className="space-y-1">
            {flowRoles.map((row) => (
              <div key={row.key} className="flex items-center gap-2" title={row.description}>
                <svg width="28" height="8" aria-hidden>
                  <line
                    x1="0"
                    y1="4"
                    x2="28"
                    y2="4"
                    stroke={wireStroke(row.key as FlowRole)}
                    strokeWidth="2"
                    strokeDasharray={LEGEND_WIRE_DASH[row.key as FlowRole]}
                  />
                </svg>
                <span className="text-pc-text-secondary">{row.description}</span>
              </div>
            ))}
            <div className="flex items-center gap-2" title={dataDesc}>
              <svg width="28" height="10" aria-hidden>
                <line x1="0" y1="5" x2="28" y2="5" stroke={WIRE_STROKE.data} strokeWidth="2" strokeDasharray="2 3" />
              </svg>
              <span className="text-pc-text-secondary">{dataDesc}</span>
            </div>
            <div className="mt-1 border-t border-pc-border pt-1 text-pc-text-muted">
              {t('sops.legend_handles_hint')}
            </div>
          </div>
        </div>
      ) : (
        <button
          type="button"
          onClick={() => setOpen(true)}
          className="rounded border border-pc-border bg-pc-surface px-2 py-1 text-xs text-pc-text-muted hover:text-pc-text"
        >
          {t('sops.legend_title')}
        </button>
      )}
    </div>
  );
}

function MenuItem({
  label,
  tone,
  onClick,
}: {
  label: string;
  tone?: 'danger';
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`block w-full px-3 py-1.5 text-left hover:bg-pc-elevated ${
        tone === 'danger' ? 'text-status-error' : 'text-pc-text'
      }`}
    >
      {label}
    </button>
  );
}

export default function SopCanvas({
  draft,
  graph,
  selectedStep,
  runStateByStep = EMPTY_RUN_STATE,
  readOnly = false,
  onSelectStep,
  onSelectTrigger,
  onAddStep,
  onRemoveStep,
  onConnect,
  onDisconnect,
  onConnectData,
  onDisconnectData,
  onMoveNode,
  onUndo,
  canUndo = false,
}: Props) {
  const [pos, setPos] = useState<Map<number, XY>>(() => seedPositions(graph));
  const [drag, setDrag] = useState<{ step: number; dx: number; dy: number } | null>(null);
  const [linkFrom, setLinkFrom] = useState<number | null>(null);
  const [linkKind, setLinkKind] = useState<FlowRole>('sequence');
  const [linkPort, setLinkPort] = useState<number | undefined>(undefined);
  const [dataLink, setDataLink] = useState<{ step: number; pin: string; dataType: string | null } | null>(null);
  const [cursor, setCursor] = useState<XY | null>(null);
  const [hoverWire, setHoverWire] = useState<number | null>(null);
  const [menu, setMenu] = useState<ContextMenu | null>(null);
  const [toolsOpen, setToolsOpen] = useState<Set<number>>(() => new Set());
  const svgRef = useRef<SVGSVGElement | null>(null);
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const panRef = useRef<{ x: number; y: number; left: number; top: number } | null>(null);
  const [panning, setPanning] = useState(false);
  // A wire is deleted only by a deliberate click: press and release on the same
  // wire without moving past this threshold. This keeps a click-and-hold pan
  // that happens to start on a wire from destroying it.
  const wireClickRef = useRef<{ id: number; x: number; y: number } | null>(null);
  const [legend, setLegend] = useState<GraphLegend | null>(null);

  useEffect(() => {
    let active = true;
    getGraphLegend()
      .then((l) => {
        if (active) setLegend(l);
      })
      .catch(() => {});
    return () => {
      active = false;
    };
  }, []);

  const flowRoleDesc = useMemo(() => indexLegend(legend?.flow_roles), [legend]);
  const flowRoleLabel = useMemo(() => indexLegendLabels(legend?.flow_roles), [legend]);
  const pinClassDesc = useMemo(() => indexLegend(legend?.pin_classes), [legend]);
  const runStateDesc = useMemo(() => indexLegend(legend?.run_states), [legend]);
  const dataWireTitle = pinClassDesc.get('data') ?? t('sops.wire_kind_data');

  // Abandon an in-progress wire draw (flow or data) and clear the ghost line.
  const cancelLink = useCallback(() => {
    setLinkFrom(null);
    setDataLink(null);
    setCursor(null);
  }, []);

  // Escape cancels a wire currently being drawn.
  useEffect(() => {
    if (linkFrom === null && dataLink === null) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      e.preventDefault();
      cancelLink();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [linkFrom, dataLink, cancelLink]);

  const handleTitle = useCallback(
    (direction: 'out' | 'in', role: FlowRole): string => {
      const label = flowRoleLabel.get(role) ?? role;
      const chrome = (direction === 'out' ? t('sops.handle_out') : t('sops.handle_in')).replace(
        '{label}',
        label,
      );
      const desc = flowRoleDesc.get(role);
      return desc ? `${chrome} — ${desc}` : chrome;
    },
    [flowRoleDesc, flowRoleLabel],
  );

  const roleLabel = useCallback(
    (role: FlowRole): string => flowRoleLabel.get(role) ?? role,
    [flowRoleLabel],
  );

  // Reseed from the backend layout when the projection changes, preserving any
  // positions the user has dragged for nodes that still exist.
  useEffect(() => {
    setPos((prev) => {
      const seeded = seedPositions(graph);
      const merged = new Map(seeded);
      for (const [k, v] of prev) if (seeded.has(k)) merged.set(k, v);
      return merged;
    });
  }, [graph]);

  const stepByNum = useMemo(() => new Map(draft.steps.map((s) => [s.number, s])), [draft.steps]);
  const nodeByStep = useMemo(() => new Map(graph.nodes.map((n) => [n.step, n])), [graph.nodes]);

  // Switch port index per wire, resolved against the source step's rules so the
  // edge can leave the correct port handle. Backend carries the port label in
  // `from_pin`; the index is a render concern only.
  const switchPortIndex = useCallback(
    (w: GraphWire): number | undefined => {
      if (w.flow_role !== 'switch' || !w.from_pin) return undefined;
      const src = stepByNum.get(w.from_step);
      const idx = src?.routing?.switch?.findIndex((r) => r.name === w.from_pin);
      return idx !== undefined && idx >= 0 ? idx : undefined;
    },
    [stepByNum],
  );

  // Switch-rule count for a step, the single source that drives the switch-port
  // lane and the data-pin lane offset below it.
  const rulesForStep = useCallback(
    (step: number): number => (stepByNum.get(step)?.routing?.switch ?? []).length,
    [stepByNum],
  );

  const toLocal = useCallback((clientX: number, clientY: number): XY => {
    const rect = svgRef.current?.getBoundingClientRect();
    return { x: clientX - (rect?.left ?? 0), y: clientY - (rect?.top ?? 0) };
  }, []);

  const onPointerMove = useCallback(
    (e: React.PointerEvent) => {
      if (panRef.current && scrollRef.current) {
        scrollRef.current.scrollLeft = panRef.current.left - (e.clientX - panRef.current.x);
        scrollRef.current.scrollTop = panRef.current.top - (e.clientY - panRef.current.y);
        return;
      }
      const p = toLocal(e.clientX, e.clientY);
      if (drag) {
        setPos((prev) => {
          const next = new Map(prev);
          next.set(drag.step, { x: p.x - drag.dx, y: p.y - drag.dy });
          return next;
        });
      }
      if (linkFrom !== null || dataLink !== null) setCursor(p);
    },
    [drag, linkFrom, dataLink, toLocal],
  );

  const endDrag = useCallback(() => {
    if (drag !== null && onMoveNode) {
      const p = pos.get(drag.step);
      if (p) onMoveNode(drag.step, p.x, p.y);
    }
    setDrag(null);
    panRef.current = null;
    wireClickRef.current = null;
    setPanning(false);
  }, [drag, pos, onMoveNode]);

  // Left-click on empty canvas background starts a drag-scroll pan. Nodes,
  // handles, and wires stop propagation on their own pointerdown, so this only
  // fires on the bare SVG.
  const startPan = useCallback(
    (e: React.PointerEvent) => {
      setMenu(null);
      if (e.button !== 0 || linkFrom !== null || !scrollRef.current) return;
      panRef.current = {
        x: e.clientX,
        y: e.clientY,
        left: scrollRef.current.scrollLeft,
        top: scrollRef.current.scrollTop,
      };
      setPanning(true);
    },
    [linkFrom],
  );

  const openMenu = useCallback(
    (e: React.MouseEvent, step: number | null) => {
      if (readOnly) return;
      e.preventDefault();
      e.stopPropagation();
      const p = toLocal(e.clientX, e.clientY);
      setMenu({ x: p.x, y: p.y, step });
    },
    [readOnly, toLocal],
  );

  const startLink = useCallback((step: number, kind: FlowRole, port?: number) => {
    setLinkKind(kind);
    setLinkPort(port);
    setLinkFrom(step);
  }, []);

  const startDataLink = useCallback((step: number, pin: string, dataType: string | null) => {
    setDataLink({ step, pin, dataType });
  }, []);

  const completeDataLink = useCallback(
    (toStep: number, toPin: string, toType: string | null) => {
      if (
        dataLink !== null &&
        dataLink.step !== toStep &&
        dataTypesCompatible(dataLink.dataType, toType)
      ) {
        onConnectData(dataLink.step, dataLink.pin, toStep, toPin);
      }
      setDataLink(null);
      setCursor(null);
    },
    [dataLink, onConnectData],
  );

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

  return (
    <div ref={scrollRef} className="relative overflow-auto rounded-[var(--radius-lg)] border border-pc-border bg-pc-bg-base">
      {readOnly ? null : (
        <div className="absolute right-2 top-2 z-10 flex gap-1">
          {onUndo ? (
            <button
              type="button"
              onClick={onUndo}
              disabled={!canUndo}
              title={t('sops.undo_hint')}
              className="inline-flex items-center gap-1 rounded border border-pc-border bg-pc-surface px-2 py-1 text-xs text-pc-text-muted hover:text-pc-text disabled:cursor-not-allowed disabled:opacity-40"
            >
              <Undo2 className="h-3.5 w-3.5" aria-hidden /> {t('sops.undo')}
            </button>
          ) : null}
          <button
            type="button"
            onClick={onAddStep}
            className="inline-flex items-center gap-1 rounded bg-pc-accent px-2 py-1 text-xs text-[#0b1220] hover:bg-pc-accent-light"
          >
            <Plus className="h-3.5 w-3.5" aria-hidden /> {t('sops.add_step')}
          </button>
        </div>
      )}
      <CanvasLegend legend={legend} />
      {linkFrom !== null || dataLink !== null ? (
        <div className="absolute left-2 top-2 z-10 rounded bg-pc-elevated px-2 py-1 text-xs text-pc-text">
          {t('sops.linking')}: {dataLink !== null ? dataWireTitle : roleLabel(linkKind)}.{' '}
          {t('sops.link_hint')}
          <button
            type="button"
            onClick={cancelLink}
            className="ml-2 text-pc-text-muted underline"
          >
            {t('sops.cancel')}
          </button>
        </div>
      ) : null}
      {menu ? (
        <div
          className="absolute z-20 w-44 rounded-[var(--radius-lg)] border border-pc-border bg-pc-surface py-1 text-sm shadow-lg"
          style={{ left: menu.x, top: menu.y }}
          onContextMenu={(e) => e.preventDefault()}
        >
          {menu.step !== null ? (
            <>
              <MenuItem
                label={t('sops.menu_edit_step')}
                onClick={() => {
                  onSelectStep(menu.step as number);
                  setMenu(null);
                }}
              />
              <MenuItem
                label={t('sops.menu_wire_sequence')}
                onClick={() => {
                  startLink(menu.step as number, 'sequence');
                  setMenu(null);
                }}
              />
              <MenuItem
                label={t('sops.menu_wire_failure')}
                onClick={() => {
                  startLink(menu.step as number, 'failure');
                  setMenu(null);
                }}
              />
              <MenuItem
                label={t('sops.menu_wire_dependency')}
                onClick={() => {
                  startLink(menu.step as number, 'dependency');
                  setMenu(null);
                }}
              />
              {onRemoveStep ? (
                <MenuItem
                  label={t('sops.menu_remove_step')}
                  tone="danger"
                  onClick={() => {
                    onRemoveStep(menu.step as number);
                    setMenu(null);
                  }}
                />
              ) : null}
            </>
          ) : (
            <MenuItem
              label={t('sops.add_step')}
              onClick={() => {
                onAddStep();
                setMenu(null);
              }}
            />
          )}
        </div>
      ) : null}
      <svg
        ref={svgRef}
        width={extent.w}
        height={extent.h}
        onPointerMove={onPointerMove}
        onPointerUp={endDrag}
        onPointerLeave={endDrag}
        onPointerDown={startPan}
        onContextMenu={(e) => openMenu(e, null)}
        className={`block touch-none select-none ${panning ? 'cursor-grabbing' : linkFrom !== null ? '' : 'cursor-grab'}`}
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
        {graph.wires
          .filter((w) => w.class === 'flow')
          .map((w, i) => {
            const a = pos.get(w.from_step);
            const b = pos.get(w.to_step);
            if (!a || !b) return null;
            const kind = (w.flow_role ?? 'sequence') as FlowRole;
            const active = runStateByStep.get(w.to_step) === 'active';
            const portIndex = switchPortIndex(w);
            const srcY =
              portIndex !== undefined ? switchPortY(a.y, portIndex) : flowOutY(a.y, kind);
            const dstY = kind === 'trigger' ? flowInY(b.y, 'sequence') : flowInY(b.y, kind);
            const d = edgePath(a, b, srcY, dstY);
            const hovered = hoverWire === i;
            const wireLabel = kind === 'trigger' ? nodeByStep.get(w.from_step)?.subtitle : undefined;
            return (
              <g key={`wire-${i}`}>
                <path
                  d={d}
                  fill="none"
                  stroke="transparent"
                  strokeWidth={14}
                  pointerEvents="stroke"
                  className={readOnly || kind === 'trigger' ? '' : 'cursor-pointer'}
                  onPointerEnter={() => setHoverWire(i)}
                  onPointerLeave={() => setHoverWire((h) => (h === i ? null : h))}
                  onPointerDown={(e) => {
                    // Trigger edges are derived from the SOP's triggers; they
                    // are not hand-wired and cannot be deleted from the canvas.
                    if (readOnly || kind === 'trigger') return;
                    // Arm a delete-click; deletion only fires if the pointer is
                    // released on the same wire without travelling (a real
                    // click, not a click-and-hold pan).
                    e.stopPropagation();
                    wireClickRef.current = { id: i, x: e.clientX, y: e.clientY };
                  }}
                  onPointerUp={(e) => {
                    if (readOnly || kind === 'trigger') return;
                    const armed = wireClickRef.current;
                    wireClickRef.current = null;
                    if (!armed || armed.id !== i) return;
                    if (
                      Math.abs(e.clientX - armed.x) > WIRE_CLICK_SLOP ||
                      Math.abs(e.clientY - armed.y) > WIRE_CLICK_SLOP
                    )
                      return;
                    e.stopPropagation();
                    onDisconnect(w.from_step, w.to_step, kind, portIndex);
                  }}
                >
                  {kind === 'trigger' ? (
                    <title>{flowRoleDesc.get('trigger') ?? roleLabel('trigger')}</title>
                  ) : (
                    <title>
                      {flowRoleDesc.get(kind) ?? roleLabel(kind)}
                      {readOnly ? '' : ` — ${t('sops.wire_delete_hint')}`}
                    </title>
                  )}
                </path>
                <path
                  d={d}
                  fill="none"
                  stroke={hovered && !readOnly && kind !== 'trigger' ? 'var(--color-status-error)' : wireStroke(kind)}
                  strokeWidth={active ? 3 : hovered ? 2.5 : 1.75}
                  strokeDasharray={
                    hovered && !readOnly && kind !== 'trigger'
                      ? '6 3'
                      : kind === 'dependency'
                        ? '5 4'
                        : kind === 'trigger'
                          ? '4 3'
                          : undefined
                  }
                  markerEnd="url(#sop-arrow)"
                  opacity={active ? 1 : hovered ? 1 : kind === 'trigger' ? 1 : 0.85}
                  pointerEvents="none"
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
                {hovered && !readOnly && kind !== 'trigger' ? (
                  <g pointerEvents="none">
                    <circle cx={(a.x + NODE_W + b.x) / 2} cy={(a.y + b.y) / 2 + NODE_H / 2} r={8} fill="var(--color-status-error)" />
                    <text
                      x={(a.x + NODE_W + b.x) / 2}
                      y={(a.y + b.y) / 2 + NODE_H / 2 + 3}
                      fill="#fff"
                      fontSize="11"
                      fontWeight="bold"
                      textAnchor="middle"
                    >
                      ×
                    </text>
                  </g>
                ) : wireLabel ? (
                  (() => {
                    const cx = (a.x + NODE_W + b.x) / 2;
                    const cy = (a.y + b.y) / 2 + NODE_H / 2 - 10;
                    const label = wireLabel.length > 28 ? `${wireLabel.slice(0, 27)}…` : wireLabel;
                    const chipW = label.length * 5.6 + 12;
                    return (
                      <g pointerEvents="none">
                        <rect
                          x={cx - chipW / 2}
                          y={cy - 10}
                          width={chipW}
                          height={15}
                          rx={4}
                          fill="var(--pc-bg-base)"
                          stroke={wireStroke(kind)}
                          strokeOpacity={0.4}
                        />
                        <text
                          x={cx}
                          y={cy + 1}
                          fill={wireStroke(kind)}
                          fontSize="10"
                          textAnchor="middle"
                          dominantBaseline="middle"
                        >
                          {label}
                        </text>
                      </g>
                    );
                  })()
                ) : null}
              </g>
            );
          })}
        {graph.wires
          .filter((w) => w.class === 'data')
          .map((w, i) => {
            const a = pos.get(w.from_step);
            const b = pos.get(w.to_step);
            if (!a || !b) return null;
            const fromNode = nodeByStep.get(w.from_step);
            const toNode = nodeByStep.get(w.to_step);
            const fromIdx = fromNode
              ? dataPins(fromNode, 'outputs').findIndex((p) => p.name === w.from_pin)
              : -1;
            const toIdx = toNode
              ? dataPins(toNode, 'inputs').findIndex((p) => p.name === w.to_pin)
              : -1;
            // A data wire whose endpoints do not resolve to real pins would
            // otherwise anchor to the node's bare center and render as a
            // phantom pipe leaving from nowhere. Drop it instead.
            if (fromIdx < 0 || toIdx < 0) return null;
            const srcY = dataPinY(a.y, 'outputs', fromIdx, rulesForStep(w.from_step));
            const dstY = dataPinY(b.y, 'inputs', toIdx, rulesForStep(w.to_step));
            const d = edgePath(a, b, srcY, dstY);
            const hovered = hoverWire === -(i + 1);
            return (
              <g key={`data-wire-${i}`}>
                <path
                  d={d}
                  fill="none"
                  stroke="transparent"
                  strokeWidth={12}
                  pointerEvents="stroke"
                  className={readOnly ? '' : 'cursor-pointer'}
                  onPointerEnter={() => setHoverWire(-(i + 1))}
                  onPointerLeave={() => setHoverWire((h) => (h === -(i + 1) ? null : h))}
                  onPointerDown={(e) => {
                    if (readOnly || !w.to_pin) return;
                    e.stopPropagation();
                    wireClickRef.current = { id: -(i + 1), x: e.clientX, y: e.clientY };
                  }}
                  onPointerUp={(e) => {
                    if (readOnly || !w.to_pin) return;
                    const armed = wireClickRef.current;
                    wireClickRef.current = null;
                    if (!armed || armed.id !== -(i + 1)) return;
                    if (
                      Math.abs(e.clientX - armed.x) > WIRE_CLICK_SLOP ||
                      Math.abs(e.clientY - armed.y) > WIRE_CLICK_SLOP
                    )
                      return;
                    e.stopPropagation();
                    onDisconnectData(w.to_step, w.to_pin);
                  }}
                >
                  <title>
                    {dataWireTitle}
                    {readOnly ? '' : ` — ${t('sops.data_wire_delete_hint')}`}
                  </title>
                </path>
                <path
                  d={d}
                  fill="none"
                  stroke={hovered && !readOnly ? 'var(--color-status-error)' : WIRE_STROKE.data}
                  strokeWidth={hovered && !readOnly ? 2.5 : 1.75}
                  strokeDasharray="2 3"
                  markerEnd="url(#sop-arrow)"
                  opacity={hovered && !readOnly ? 1 : 0.8}
                  pointerEvents="none"
                />
              </g>
            );
          })}
        {linkFrom !== null && cursor && pos.get(linkFrom) ? (
          <path
            d={edgePath(
              pos.get(linkFrom) as XY,
              { x: cursor.x - NODE_W, y: cursor.y - NODE_H / 2 },
              linkPort !== undefined
                ? switchPortY((pos.get(linkFrom) as XY).y, linkPort)
                : flowOutY((pos.get(linkFrom) as XY).y, linkKind),
            )}
            fill="none"
            stroke={wireStroke(linkKind)}
            strokeWidth={1.75}
            strokeDasharray="4 4"
          />
        ) : null}
        {dataLink !== null && cursor && pos.get(dataLink.step) ? (
          (() => {
            const src = pos.get(dataLink.step) as XY;
            const node = nodeByStep.get(dataLink.step);
            const idx = node ? dataPins(node, 'outputs').findIndex((p) => p.name === dataLink.pin) : -1;
            return (
              <path
                d={edgePath(
                  src,
                  { x: cursor.x - NODE_W, y: cursor.y - NODE_H / 2 },
                  idx >= 0 ? dataPinY(src.y, 'outputs', idx, rulesForStep(dataLink.step)) : undefined,
                )}
                fill="none"
                stroke={WIRE_STROKE.data}
                strokeWidth={1.75}
                strokeDasharray="2 3"
              />
            );
          })()
        ) : null}
        {graph.nodes.map((node) => {
          const p = pos.get(node.step);
          if (!p) return null;
          if (node.kind === 'trigger') return renderTrigger(node, p);
          return renderStep(node);
        })}
      </svg>
    </div>
  );

  function renderTrigger(node: GraphNode, p: XY) {
    const idx = node.trigger_index;
    return (
      <g
        key={`trigger-${node.step}`}
        transform={`translate(${p.x}, ${p.y})`}
        onClick={() => {
          if (idx != null) onSelectTrigger(idx);
        }}
        className={idx != null ? 'cursor-pointer' : undefined}
      >
        <title>{t('sops.trigger_edit_hint')}</title>
        <rect
          width={NODE_W}
          height={NODE_H}
          rx={12}
          fill="var(--pc-bg-surface)"
          stroke={wireStroke('trigger')}
          strokeWidth={1.5}
          strokeDasharray="4 3"
        />
        <rect width={NODE_W} height={26} rx={12} fill="var(--pc-bg-elevated)" />
        <rect y={16} width={NODE_W} height={10} fill="var(--pc-bg-elevated)" />
        <circle cx={16} cy={13} r={9} fill={wireStroke('trigger')} opacity={0.3} />
        <text x={16} y={17} fontSize="11" textAnchor="middle" fill={wireStroke('trigger')}>
          ⚡
        </text>
        <text x={32} y={17} fontSize="12" fill="var(--pc-text-primary)">
          {node.title}
        </text>
        <text x={12} y={46} fontSize="10" fill="var(--pc-text-muted)">
          {(node.subtitle ?? '').slice(0, 30)}
        </text>
        <circle cx={NODE_W} cy={NODE_H / 2} r={6} fill={wireStroke('trigger')} />
      </g>
    );
  }

  function renderStep(node: GraphNode) {
    const step: SopStep | undefined = stepByNum.get(node.step);
    const p = pos.get(node.step);
    if (!p) return null;
    const state = runStateByStep.get(node.step);
    const selected = selectedStep === node.step;
    const isCheckpoint = step?.kind === 'checkpoint';
    const switchRules = step?.routing?.switch ?? [];
    const ruleCount = switchRules.length;
    const allowedTools = step?.suggested_tools ?? [];
    const plannedCalls = step?.calls ?? [];
    const callArgRows = plannedCalls.map((c) => argsEntries(c.args));
    const toolRows = allowedTools.length + plannedCalls.length;
    const expandedRowCount =
      allowedTools.length +
      plannedCalls.length +
      callArgRows.reduce((sum, rows) => sum + rows.length, 0);
    const expanded = toolsOpen.has(node.step);
    const expandedRows = expanded ? expandedRowCount : 0;
    return (
      <g
        key={node.step}
        transform={`translate(${p.x}, ${p.y})`}
        onContextMenu={(e) => openMenu(e, node.step)}
        onPointerDown={(e) => {
          if (e.button !== 0) return;
          e.stopPropagation();
          if (linkFrom !== null) {
            completeLink(node.step);
            return;
          }
          onSelectStep(node.step);
          if (readOnly) return;
          const local = toLocal(e.clientX, e.clientY);
          setDrag({ step: node.step, dx: local.x - p.x, dy: local.y - p.y });
        }}
        className={readOnly ? 'cursor-pointer' : 'cursor-grab'}
      >
        <rect
          width={NODE_W}
          height={nodeHeight(node, ruleCount, expandedRows)}
          rx={10}
          fill="var(--pc-bg-surface)"
          stroke={selected ? 'var(--pc-accent)' : nodeStateStroke(state)}
          strokeWidth={selected ? 2.5 : 1.5}
        />
        <rect width={NODE_W} height={26} rx={10} fill="var(--pc-bg-elevated)" />
        <rect y={16} width={NODE_W} height={10} fill="var(--pc-bg-elevated)" />
        <circle cx={16} cy={13} r={9} fill="var(--pc-accent)" />
        <text x={16} y={17} fontSize="11" textAnchor="middle" fill="#0b1220" fontWeight="600">
          {node.step}
        </text>
        <text x={32} y={17} fontSize="12" fill="var(--pc-text-primary)">
          {(node.title || t('sops.untitled')).slice(0, 22)}
        </text>
        {isCheckpoint ? (
          <text x={NODE_W - 10} y={17} fontSize="10" textAnchor="end" fill="var(--color-status-warning)">
            ⏸ {t('sops.checkpoint')}
          </text>
        ) : switchRules.length > 0 ? (
          <text x={NODE_W - 10} y={17} fontSize="10" textAnchor="end" fill="var(--pc-accent-light)">
            ⋔ {t('sops.switch')}
          </text>
        ) : null}
        {state ? (
          <text x={12} y={64} fontSize="10" fill={nodeStateStroke(state)}>
            {t(`sops.run_state.${state}`)}
            {runStateDesc.get(state) ? <title>{runStateDesc.get(state)}</title> : null}
          </text>
        ) : null}
        {switchRules.length > 0 ? (
          <g>
            <circle
              cx={NODE_W}
              cy={flowLaneY(0, 'failure')}
              r={5}
              fill={wireStroke('failure')}
              onPointerDown={(e) => {
                e.stopPropagation();
                if (!readOnly) startLink(node.step, 'failure');
              }}
              className={readOnly ? '' : 'cursor-crosshair'}
            >
              <title>{handleTitle('out', 'failure')}</title>
            </circle>
            <text x={NODE_W + 9} y={flowLaneY(0, 'failure') + 3} fontSize="8" fill={wireStroke('failure')}>
              {roleLabel('failure')}
            </text>
            <circle
              cx={NODE_W}
              cy={flowLaneY(0, 'dependency')}
              r={5}
              fill={wireStroke('dependency')}
              onPointerDown={(e) => {
                e.stopPropagation();
                if (!readOnly) startLink(node.step, 'dependency');
              }}
              className={readOnly ? '' : 'cursor-crosshair'}
            >
              <title>{handleTitle('out', 'dependency')}</title>
            </circle>
            <text x={NODE_W + 9} y={flowLaneY(0, 'dependency') + 3} fontSize="8" fill={wireStroke('dependency')}>
              {roleLabel('dependency')}
            </text>
            {switchRules.map((rule, ri) => (
              <g key={`port-${ri}`}>
                <text
                  x={NODE_W - 16}
                  y={switchPortTop() + ri * SWITCH_PORT_GAP + 3}
                  fontSize="9"
                  textAnchor="end"
                  fill="var(--pc-accent-light)"
                >
                  {(rule.name || `port ${ri + 1}`).slice(0, 16)}
                </text>
                <circle
                  cx={NODE_W}
                  cy={switchPortTop() + ri * SWITCH_PORT_GAP}
                  r={5}
                  fill={wireStroke('switch')}
                  onPointerDown={(e) => {
                    e.stopPropagation();
                    if (!readOnly) startLink(node.step, 'switch', ri);
                  }}
                  className={readOnly ? '' : 'cursor-crosshair'}
                >
                  <title>
                    {handleTitle('out', 'switch')}: {rule.name}
                  </title>
                </circle>
              </g>
            ))}
          </g>
        ) : (
          <g>
            <circle
              cx={NODE_W}
              cy={flowLaneY(0, 'sequence')}
              r={6}
              fill={wireStroke('sequence')}
              onPointerDown={(e) => {
                e.stopPropagation();
                if (!readOnly) startLink(node.step, 'sequence');
              }}
              className={readOnly ? '' : 'cursor-crosshair'}
            >
              <title>{handleTitle('out', 'sequence')}</title>
            </circle>
            <text x={NODE_W + 9} y={flowLaneY(0, 'sequence') + 3} fontSize="8" fill={wireStroke('sequence')}>
              {roleLabel('sequence')}
            </text>
            <circle
              cx={NODE_W}
              cy={flowLaneY(0, 'failure')}
              r={5}
              fill={wireStroke('failure')}
              onPointerDown={(e) => {
                e.stopPropagation();
                if (!readOnly) startLink(node.step, 'failure');
              }}
              className={readOnly ? '' : 'cursor-crosshair'}
            >
              <title>{handleTitle('out', 'failure')}</title>
            </circle>
            <text x={NODE_W + 9} y={flowLaneY(0, 'failure') + 3} fontSize="8" fill={wireStroke('failure')}>
              {roleLabel('failure')}
            </text>
            <circle
              cx={NODE_W}
              cy={flowLaneY(0, 'dependency')}
              r={5}
              fill={wireStroke('dependency')}
              onPointerDown={(e) => {
                e.stopPropagation();
                if (!readOnly) startLink(node.step, 'dependency');
              }}
              className={readOnly ? '' : 'cursor-crosshair'}
            >
              <title>{handleTitle('out', 'dependency')}</title>
            </circle>
            <text x={NODE_W + 9} y={flowLaneY(0, 'dependency') + 3} fontSize="8" fill={wireStroke('dependency')}>
              {roleLabel('dependency')}
            </text>
          </g>
        )}
        <circle cx={0} cy={flowLaneY(0, 'sequence')} r={5} fill={wireStroke('sequence')} stroke="var(--pc-bg-surface)" strokeWidth={1}>
          <title>{handleTitle('in', 'sequence')}</title>
        </circle>
        <text x={-9} y={flowLaneY(0, 'sequence') + 3} fontSize="8" textAnchor="end" fill={wireStroke('sequence')}>
          {roleLabel('sequence')}
        </text>
        <circle cx={0} cy={flowLaneY(0, 'failure')} r={4} fill={wireStroke('failure')} stroke="var(--pc-bg-surface)" strokeWidth={1}>
          <title>{handleTitle('in', 'failure')}</title>
        </circle>
        <text x={-9} y={flowLaneY(0, 'failure') + 3} fontSize="8" textAnchor="end" fill={wireStroke('failure')}>
          {roleLabel('failure')}
        </text>
        <circle cx={0} cy={flowLaneY(0, 'dependency')} r={4} fill={wireStroke('dependency')} stroke="var(--pc-bg-surface)" strokeWidth={1}>
          <title>{handleTitle('in', 'dependency')}</title>
        </circle>
        <text x={-9} y={flowLaneY(0, 'dependency') + 3} fontSize="8" textAnchor="end" fill={wireStroke('dependency')}>
          {roleLabel('dependency')}
        </text>
        {dataPins(node, 'inputs').map((pin, di) => {
          const active = dataLink !== null && dataTypesCompatible(dataLink.dataType, pin.data_type ?? null);
          return (
            <g key={`din-${pin.name}`}>
              <circle
                cx={0}
                cy={dataPinY(0, 'inputs', di, ruleCount)}
                r={5}
                fill={active ? WIRE_STROKE.data : 'var(--pc-bg-surface)'}
                stroke={WIRE_STROKE.data}
                strokeWidth={1.5}
                onPointerDown={(e) => {
                  e.stopPropagation();
                  if (readOnly) return;
                  if (dataLink !== null) completeDataLink(node.step, pin.name, pin.data_type ?? null);
                }}
                onPointerUp={(e) => {
                  e.stopPropagation();
                  if (!readOnly && dataLink !== null) {
                    completeDataLink(node.step, pin.name, pin.data_type ?? null);
                  }
                }}
                className={readOnly ? '' : 'cursor-crosshair'}
              >
                <title>
                  {pin.name}: {pin.data_type ?? t('sops.pin_any')}
                  {pin.required ? ` (${t('sops.pin_required')})` : ''}
                </title>
              </circle>
              <text x={10} y={dataPinY(0, 'inputs', di, ruleCount) + 3} fontSize="9" fill="var(--pc-text-muted)">
                {pin.name.slice(0, 18)}
              </text>
            </g>
          );
        })}
        {dataPins(node, 'outputs').map((pin, di) => (
          <g key={`dout-${pin.name}`}>
            <circle
              cx={NODE_W}
              cy={dataPinY(0, 'outputs', di, ruleCount)}
              r={5}
              fill={WIRE_STROKE.data}
              onPointerDown={(e) => {
                e.stopPropagation();
                if (!readOnly) startDataLink(node.step, pin.name, pin.data_type ?? null);
              }}
              className={readOnly ? '' : 'cursor-crosshair'}
            >
              <title>
                {pin.name}: {pin.data_type ?? t('sops.pin_any')}
              </title>
            </circle>
            <text x={NODE_W - 10} y={dataPinY(0, 'outputs', di, ruleCount) + 3} fontSize="9" textAnchor="end" fill="var(--pc-text-muted)">
              {pin.name.slice(0, 18)}
            </text>
          </g>
        ))}
        <foreignObject
          x={6}
          y={toolsBarTop(node, ruleCount)}
          width={NODE_W - 12}
          height={nodeHeight(node, ruleCount, expandedRows) - toolsBarTop(node, ruleCount) - 4}
        >
          <div className="text-[10px] leading-tight text-pc-text-muted">
            <button
              type="button"
              onPointerDown={(e) => e.stopPropagation()}
              onClick={(e) => {
                e.stopPropagation();
                setToolsOpen((prev) => {
                  const next = new Set(prev);
                  if (next.has(node.step)) next.delete(node.step);
                  else next.add(node.step);
                  return next;
                });
              }}
              className="flex w-full items-center gap-1 truncate text-left hover:text-pc-text"
              title={t('sops.tools_accordion_hint')}
            >
              <span className="text-pc-text-muted">{expanded ? '▾' : '▸'}</span>
              {toolRows === 0 ? (
                <span>{t('sops.no_tools')}</span>
              ) : (
                <span className="truncate">
                  {[...allowedTools, ...plannedCalls.map((c) => c.tool)].join(', ')}
                </span>
              )}
            </button>
            {expanded && toolRows > 0 ? (
              <ul className="mt-1 space-y-0.5">
                {allowedTools.map((name) => (
                  <li key={`allow-${name}`} className="flex items-center gap-1 truncate">
                    <span className="text-pc-accent-light">•</span>
                    <span className="text-pc-text-secondary">{name}</span>
                    <span className="text-pc-text-muted">— {t('sops.tool_allowed')}</span>
                  </li>
                ))}
                {plannedCalls.map((call, ci) => {
                  const argRows = callArgRows[ci] ?? [];
                  return (
                    <li key={`call-${ci}`}>
                      <div className="flex items-center gap-1 truncate">
                        <span className="text-pc-accent">⚙</span>
                        <span className="text-pc-text-secondary">{call.tool}</span>
                      </div>
                      {argRows.length > 0 ? (
                        <ul className="ml-3 space-y-0.5">
                          {argRows.map(([k, v], ai) => (
                            <li key={`arg-${ci}-${ai}`} className="flex items-center gap-1 truncate">
                              {k ? <span className="text-pc-text-muted">{k}:</span> : null}
                              <span className="truncate text-pc-text-secondary">{v}</span>
                            </li>
                          ))}
                        </ul>
                      ) : null}
                    </li>
                  );
                })}
              </ul>
            ) : null}
          </div>
        </foreignObject>
      </g>
    );
  }
}
