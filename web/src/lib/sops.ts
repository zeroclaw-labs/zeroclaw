import { apiFetch } from './api';
import type { components } from './api-generated';
import { fieldHelp } from './api-descriptions';
import { enumMembers } from './api-enums';

type Schemas = components['schemas'];

/// Help text for a field of a generated SOP schema, sourced from Rust `///`
/// docs via the OpenAPI spec. Thin re-export so SOP surfaces have one import.
export function sopFieldHelp(schema: string, field: string): string | undefined {
  return fieldHelp(schema, field);
}

export const sopPriorities = enumMembers('SopPriority') as readonly SopPriority[];
export const sopExecutionModes = enumMembers('SopExecutionMode') as readonly SopExecutionMode[];
export const sopStepKinds = enumMembers('SopStepKind') as readonly SopStepKind[];

type ServerDefaultedSopFields = 'admission_policy' | 'max_pending_approvals';

export type Sop = Omit<Schemas['Sop'], ServerDefaultedSopFields> &
  Partial<Pick<Schemas['Sop'], ServerDefaultedSopFields>>;
export type SopStep = Schemas['SopStep'];
export type SopTrigger = Schemas['SopTrigger'];
export type SopPriority = Schemas['SopPriority'];
export type SopExecutionMode = Schemas['SopExecutionMode'];
export type SopStepKind = Schemas['SopStepKind'];
export type StepRouting = Schemas['StepRouting'];
export type SwitchRule = Schemas['SwitchRule'];
export type StepFailure = Schemas['StepFailure'];
export type StepSchema = Schemas['StepSchema'];
export type StepToolScope = Schemas['StepToolScope'];
export type PlannedToolCall = Schemas['PlannedToolCall'];
export type StepToolCall = Schemas['StepToolCall'];

export type SopGraph = Schemas['SopGraph'];
export type GraphNode = Schemas['GraphNode'];
export type GraphPin = Schemas['GraphPin'];
export type GraphWire = Schemas['GraphWire'];
export type GraphDiagnostic = Schemas['GraphDiagnostic'];
export type GraphLayout = Schemas['GraphLayout'];
export type LayoutGeometry = Schemas['LayoutGeometry'];
export type NodePosition = Schemas['NodePosition'];
export type NodeKind = Schemas['NodeKind'];
export type PinClass = GraphPin['class'];
export type FlowRole = NonNullable<GraphWire['flow_role']>;
export type GraphSeverity = GraphDiagnostic['severity'];

export type RunOverlay = Schemas['RunOverlay'];
export type SopApprovalDecision = Schemas['ApprovalDecision'];
export type TriggerSourceRegistry = Schemas['TriggerSourceRegistry'];
export type BoundTriggerSource = Schemas['BoundTriggerSource'];
export type TriggerField = Schemas['TriggerField'];
export type ChannelTriggerKind = Schemas['ChannelTriggerKind'];
export type ChannelAlias = Schemas['ChannelAlias'];
export type PayloadContract = Schemas['PayloadContract'];
export type ConditionField = Schemas['ConditionField'];
export type ConditionOpSpec = Schemas['ConditionOpSpec'];
export type ConditionValueType = Schemas['ConditionValueType'];
export type NodeRunOverlay = Schemas['NodeRunOverlay'];
export type NodeRunState = Schemas['NodeRunState'];
export type SopRunStatus = Schemas['SopRunStatus'];
export type GraphLegend = Schemas['GraphLegend'];
export type LegendEntry = Schemas['LegendEntry'];

export interface SopSummary {
  name: string;
  description: string;
  version: string;
}

/// One row on the Runs page: a run the engine currently holds (a live run
/// from its active set or a retained terminal run). Mirrors the Rust
/// `SopRunSummary` serde shape. Not sourced from the generated OpenAPI
/// schema because the runs listing is served as a plain JSON envelope, not a
/// schemars-exported type.
export interface SopRunSummary {
  run_id: string;
  sop_name: string;
  status: SopRunStatus;
  current_step: number;
  total_steps: number;
  started_at: string;
  completed_at: string | null;
  trigger_source: string;
  active: boolean;
}

// Canonical canvas geometry fallback, mirroring `LayoutGeometry::CANONICAL` in
// `zeroclaw-sop-graph`. Every projected graph carries `layout.geometry` on the
// wire; this is only used when deserializing a response from an older daemon
// that predates the field. The Rust registry is the source of truth.
export const CANONICAL_LAYOUT_GEOMETRY: LayoutGeometry = {
  node_w: 210,
  node_h: 84,
  col_gap: 130,
  row_gap: 46,
  origin: 24,
};

export function layoutGeometry(graph: SopGraph): LayoutGeometry {
  return graph.layout.geometry ?? CANONICAL_LAYOUT_GEOMETRY;
}

export async function listSops(): Promise<SopSummary[]> {
  const body = await apiFetch<{ sops: SopSummary[] }>('/api/sops');
  return body.sops ?? [];
}

export async function listRuns(sop?: string): Promise<SopRunSummary[]> {
  const qs = sop ? `?sop=${encodeURIComponent(sop)}` : '';
  const body = await apiFetch<{ runs: SopRunSummary[] }>(`/api/sops/runs${qs}`);
  return body.runs ?? [];
}

export function getSopGraph(name: string): Promise<SopGraph> {
  return apiFetch<SopGraph>(`/api/sops/${encodeURIComponent(name)}/graph`);
}

let legendCache: Promise<GraphLegend> | null = null;

/// Canonical graph legend, cached for the session (static registry).
export function getGraphLegend(): Promise<GraphLegend> {
  if (!legendCache) legendCache = apiFetch<GraphLegend>('/api/sops/graph-legend');
  return legendCache;
}

/// Index a legend section by its stable `key` for hover/description lookup.
export function indexLegend(entries: LegendEntry[] | undefined): Map<string, string> {
  const map = new Map<string, string>();
  for (const e of entries ?? []) map.set(e.key, e.description);
  return map;
}

export function indexLegendLabels(entries: LegendEntry[] | undefined): Map<string, string> {
  const map = new Map<string, string>();
  for (const e of entries ?? []) map.set(e.key, e.label);
  return map;
}

export function getSop(name: string): Promise<Sop> {
  return apiFetch<Sop>(`/api/sops/${encodeURIComponent(name)}/full`);
}

export function createSop(sop: Sop): Promise<{ created: string }> {
  return apiFetch<{ created: string }>('/api/sops', {
    method: 'POST',
    body: JSON.stringify(sop),
  });
}

export function saveSop(sop: Sop): Promise<{ saved: string }> {
  return apiFetch<{ saved: string }>(`/api/sops/${encodeURIComponent(sop.name)}`, {
    method: 'PUT',
    body: JSON.stringify(sop),
  });
}

export function deleteSop(name: string): Promise<{ deleted: string }> {
  return apiFetch<{ deleted: string }>(`/api/sops/${encodeURIComponent(name)}`, {
    method: 'DELETE',
  });
}

export type WireOp = 'connect' | 'disconnect';
export type WireRole = FlowRole;

export interface WireEdit {
  op: WireOp;
  from: number;
  to: number;
  role: WireRole;
  port?: number;
}

export interface WireResult {
  sop: Sop;
  graph: SopGraph;
}

/// Edge mutation against an unsaved draft. Writes nothing; returns the mutated
/// draft. The backend `apply_wire` mapping is the single source of truth for
/// how each edge kind maps onto step routing.
export function wireDraft(sop: Sop, edit: WireEdit): Promise<WireResult> {
  return apiFetch<WireResult>('/api/sops/wire-draft', {
    method: 'POST',
    body: JSON.stringify({ sop, edit }),
  });
}

/// Reproject an unsaved draft to its graph. Read-only counterpart to
/// `wireDraft`: the canvas calls it after any non-wire field edit so trigger
/// fan-in, data wires, pins, and layout stay single-sourced from the backend.
export function graphDraft(sop: Sop): Promise<SopGraph> {
  return apiFetch<SopGraph>('/api/sops/graph-draft', {
    method: 'POST',
    body: JSON.stringify({ sop }),
  });
}

/// The trigger-source registry: bound sources plus every inbound-capable
/// channel kind with its configured aliases. Walked from the backend registry;
/// the surface renders whatever it returns and never hardcodes a channel list.
export function triggerSources(): Promise<TriggerSourceRegistry> {
  return apiFetch<TriggerSourceRegistry>('/api/sops/trigger-sources');
}

/// A condition string decomposed into the three parts the guided builder edits:
/// an optional JSON path (absent for `direct` scalar payloads), an operator
/// token, and a raw comparand. `raw` preserves the original string so a value
/// the builder cannot parse round-trips untouched in advanced mode.
export interface ParsedCondition {
  path: string | null;
  op: string;
  value: string;
  raw: string;
}

/// Split a stored condition into (path, op, value) against the walked operator
/// catalog. Operators are matched longest token first so `>=` wins over `>`.
/// A `$.`-prefixed path is JSON-path form; anything else is a direct scalar
/// comparison with no path. Unparseable input yields a null op so the caller
/// can fall back to the raw text field.
export function parseCondition(
  condition: string | null | undefined,
  operators: ConditionOpSpec[],
): ParsedCondition {
  const raw = condition ?? '';
  const trimmed = raw.trim();
  const tokens = [...operators]
    .map((o) => o.token)
    .sort((a, b) => b.length - a.length);
  const hasPath = trimmed.startsWith('$');
  const scanFrom = hasPath ? trimmed.replace(/^\$\.?/, '') : trimmed;
  for (const token of tokens) {
    const at = scanFrom.indexOf(token);
    if (at < 0) continue;
    const left = scanFrom.slice(0, at).trim();
    const right = scanFrom.slice(at + token.length).trim();
    return {
      path: hasPath ? left : null,
      op: token,
      value: right,
      raw,
    };
  }
  return { path: hasPath ? scanFrom.trim() : null, op: '', value: '', raw };
}

/// Reassemble a condition string from builder parts. JSON-path form emits
/// `$.<path> <op> <value>`; direct scalar form emits `<op> <value>`. An empty
/// operator collapses to `null` (fire on every event).
export function buildCondition(part: {
  path: string | null;
  op: string;
  value: string;
}): string | null {
  if (part.op.length === 0) return null;
  const rhs = `${part.op} ${part.value}`.trim();
  if (part.path === null) return rhs;
  const path = part.path.trim();
  return path.length > 0 ? `$.${path} ${rhs}`.trim() : rhs;
}

export function getRunOverlay(name: string, runId: string): Promise<RunOverlay> {
  return apiFetch<RunOverlay>(
    `/api/sops/${encodeURIComponent(name)}/runs/${encodeURIComponent(runId)}/overlay`,
  );
}

/// Fire a Manual trigger for the named SOP and return the started run id, which
/// feeds straight into `getRunOverlay` to animate the run. `payload` is an
/// optional JSON string passed as the step-1 input; the backend rejects
/// malformed JSON with a clear error. Requires the SOP to declare a manual
/// trigger.
export function runSop(name: string, payload?: string): Promise<{ run_id: string }> {
  return apiFetch<{ run_id: string }>(`/api/sops/${encodeURIComponent(name)}/run`, {
    method: 'POST',
    body: JSON.stringify({ payload: payload ?? null }),
  });
}

/// Resolve a paused checkpoint on a live run. `decision` is the canonical
/// `ApprovalDecision` wire shape (generated from the runtime enum): `'approve'`
/// or `{ deny: { reason? } }`. Returns the refreshed overlay so the caller
/// re-renders the post-decision run state.
export function decideSop(
  name: string,
  runId: string,
  decision: SopApprovalDecision,
): Promise<RunOverlay> {
  return apiFetch<RunOverlay>(
    `/api/sops/${encodeURIComponent(name)}/runs/${encodeURIComponent(runId)}/decide`,
    {
      method: 'POST',
      body: JSON.stringify(decision),
    },
  );
}

/// Index a run overlay's node states by step number. Shared by every view
/// that projects run state onto graph nodes.
export function overlayStateByStep(
  overlay: RunOverlay | null | undefined,
): Map<number, NodeRunState> {
  const map = new Map<number, NodeRunState>();
  for (const n of overlay?.nodes ?? []) map.set(n.step, n.state);
  return map;
}

/// Index a run overlay's captured tool calls by step number. Feeds the
/// call inspector and lets planned calls pin a sample output from a run.
export function overlayCallsByStep(
  overlay: RunOverlay | null | undefined,
): Map<number, StepToolCall[]> {
  const map = new Map<number, StepToolCall[]>();
  for (const n of overlay?.nodes ?? []) {
    if (n.tool_calls && n.tool_calls.length > 0) map.set(n.step, n.tool_calls);
  }
  return map;
}

/// Semantic tone for a node run state. Single mapping shared by every SOP
/// surface; each renderer maps the tone onto its own representation
/// (Tailwind class, SVG stroke, badge variant) without re-deciding which
/// state means what.
export type RunStateTone = 'accent' | 'success' | 'error' | 'warning' | 'neutral';

/// The one tone-to-badge binding. Every surface that renders a run state or
/// run status as a badge goes through this map; nothing re-declares it.
export const RUN_TONE_BADGE = {
  accent: 'neutral',
  success: 'ok',
  error: 'error',
  warning: 'warn',
  neutral: 'neutral',
} as const satisfies Record<RunStateTone, string>;

export type RunToneBadge = (typeof RUN_TONE_BADGE)[RunStateTone];

export function runStateBadge(state: NodeRunState | undefined): RunToneBadge {
  return RUN_TONE_BADGE[runStateTone(state)];
}

export function runStatusBadge(status: SopRunStatus | undefined): RunToneBadge {
  return RUN_TONE_BADGE[runStatusTone(status)];
}

export function runStateTone(state: NodeRunState | undefined): RunStateTone {
  switch (state) {
    case 'active':
      return 'accent';
    case 'completed':
      return 'success';
    case 'failed':
      return 'error';
    case 'skipped':
      return 'warning';
    default:
      return 'neutral';
  }
}

/// Semantic tone for a whole-run status (the Runs page's status badge).
/// Distinct from `runStateTone`, which tones an individual node's state.
export function runStatusTone(status: SopRunStatus | undefined): RunStateTone {
  switch (status) {
    case 'running':
    case 'pending':
      return 'accent';
    case 'completed':
      return 'success';
    case 'failed':
      return 'error';
    case 'waiting_approval':
    case 'paused_checkpoint':
      return 'warning';
    default:
      return 'neutral';
  }
}

export function isTerminalRunStatus(status: SopRunStatus | undefined): boolean {
  switch (status) {
    case 'completed':
    case 'failed':
    case 'cancelled':
      return true;
    default:
      return false;
  }
}

/// Semantic tone for a graph wire. Same single-mapping rationale as
/// `runStateTone`.
export type WireTone = 'data' | 'error' | 'warning' | 'switch' | 'accent' | 'success';

export function flowRoleTone(role: FlowRole | null | undefined): WireTone {
  switch (role) {
    case 'failure':
      return 'error';
    case 'dependency':
      return 'warning';
    case 'switch':
      return 'switch';
    case 'trigger':
      return 'accent';
    default:
      return 'success';
  }
}

export function wireTone(wire: GraphWire): WireTone {
  if (wire.class === 'data') return 'data';
  return flowRoleTone(wire.flow_role);
}
