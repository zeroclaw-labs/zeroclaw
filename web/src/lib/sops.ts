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
export type PinClass = GraphPin['class'];
export type FlowRole = NonNullable<GraphWire['flow_role']>;
export type GraphSeverity = GraphDiagnostic['severity'];

export type RunOverlay = Schemas['RunOverlay'];
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

export function getRunOverlay(name: string, runId: string): Promise<RunOverlay> {
  return apiFetch<RunOverlay>(
    `/api/sops/${encodeURIComponent(name)}/runs/${encodeURIComponent(runId)}/overlay`,
  );
}
