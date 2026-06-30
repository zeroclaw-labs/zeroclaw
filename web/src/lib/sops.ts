import { apiFetch } from './api';

export type PinClass = 'flow' | 'data';
export type FlowRole = 'sequence' | 'dependency' | 'failure';
export type GraphSeverity = 'warning' | 'error';

export interface GraphPin {
  class: PinClass;
  name: string;
  data_type?: string;
  required: boolean;
}

export interface GraphNode {
  step: number;
  title: string;
  inputs: GraphPin[];
  outputs: GraphPin[];
}

export interface GraphWire {
  class: PinClass;
  from_step: number;
  to_step: number;
  flow_role?: FlowRole;
  from_pin?: string;
  to_pin?: string;
}

export interface GraphDiagnostic {
  severity: GraphSeverity;
  step: number;
  message: string;
}

export interface SopGraph {
  nodes: GraphNode[];
  wires: GraphWire[];
  diagnostics: GraphDiagnostic[];
}

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
