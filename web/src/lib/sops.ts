import { apiFetch } from './api';
import type { components } from './api-generated';

type Schemas = components['schemas'];

export type Sop = Schemas['Sop'];
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

export type SopGraph = Schemas['SopGraph'];
export type GraphNode = Schemas['GraphNode'];
export type GraphPin = Schemas['GraphPin'];
export type GraphWire = Schemas['GraphWire'];
export type GraphDiagnostic = Schemas['GraphDiagnostic'];
export type GraphLayout = Schemas['GraphLayout'];
export type NodePosition = Schemas['NodePosition'];
export type NodeKind = Schemas['NodeKind'];
export type PinClass = GraphPin['class'];
export type FlowRole = NonNullable<GraphWire['flow_role']>;
export type GraphSeverity = GraphDiagnostic['severity'];

export type RunOverlay = Schemas['RunOverlay'];
export type TriggerSourceRegistry = Schemas['TriggerSourceRegistry'];
export type BoundTriggerSource = Schemas['BoundTriggerSource'];
export type TriggerField = Schemas['TriggerField'];
export type ChannelTriggerKind = Schemas['ChannelTriggerKind'];
export type ChannelAlias = Schemas['ChannelAlias'];
export type NodeRunOverlay = Schemas['NodeRunOverlay'];
export type NodeRunState = Schemas['NodeRunState'];
export type SopRunStatus = Schemas['SopRunStatus'];

export interface SopSummary {
  name: string;
  description: string;
  version: string;
}

export async function listSops(): Promise<SopSummary[]> {
  const body = await apiFetch<{ sops: SopSummary[] }>('/api/sops');
  return body.sops ?? [];
}

export function getSopGraph(name: string): Promise<SopGraph> {
  return apiFetch<SopGraph>(`/api/sops/${encodeURIComponent(name)}/graph`);
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

export function getRunOverlay(name: string, runId: string): Promise<RunOverlay> {
  return apiFetch<RunOverlay>(
    `/api/sops/${encodeURIComponent(name)}/runs/${encodeURIComponent(runId)}/overlay`,
  );
}
