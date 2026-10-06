import { apiFetch } from "./api";
import type { components } from "./api-generated";

// The gateway's OpenAPI schemas are the contract. These aliases create no
// separate client policy model or persisted state. The config input schema
// permits omitted serde-default fields. Response DTOs serialize those fields;
// materialize their required output shape without inventing client defaults.
type Serialized<T> = T extends (infer V)[]
  ? Serialized<V>[]
  : T extends object
    ? { [K in keyof T]-?: Serialized<Exclude<T[K], undefined>> }
    : T;
export type ColonyConfig = Serialized<components["schemas"]["ColonyConfig"]>;
export type ColonyDetail = Serialized<components["schemas"]["ColonyDetail"]>;
export type ColonyAgent = Serialized<components["schemas"]["ColonyAgent"]>;
export type ColonySnapshot = Serialized<
  components["schemas"]["ColonySnapshot"]
>;
export type QueenProposal = Serialized<components["schemas"]["QueenProposal"]>;
export type ColonyGoal = Serialized<components["schemas"]["GoalView"]>;
export type ColonyMessage = Serialized<components["schemas"]["ColonyMessage"]>;
export type ColonyCreateRequest = components["schemas"]["ColonyCreateRequest"];
export type ColonySaveRequest = components["schemas"]["ColonySaveRequest"];
export type ColonyClarifyRequest =
  components["schemas"]["ColonyClarifyRequest"];
export type ColonyControlRequest =
  components["schemas"]["ColonyControlRequest"];
export type GoalRequest = components["schemas"]["GoalRequest"];
export type ColonyMessageRequest =
  components["schemas"]["ColonyMessageRequest"];

const path = (id: string) => `/api/colonies/${encodeURIComponent(id)}`;
const json = (method: string, body: unknown): RequestInit => ({
  method,
  body: JSON.stringify(body),
});
export const listColonies = (signal?: AbortSignal) =>
  apiFetch<ColonySnapshot>("/api/colonies", {
    signal: signal ?? new AbortController().signal,
  });
export const getColony = (id: string, signal?: AbortSignal) =>
  apiFetch<ColonyDetail>(path(id), {
    signal: signal ?? new AbortController().signal,
  });
export const createColony = (body: ColonyCreateRequest) =>
  apiFetch<ColonyDetail>("/api/colonies", json("POST", body));
export const saveColony = (id: string, body: ColonySaveRequest) =>
  apiFetch<ColonyDetail>(path(id), json("PUT", body));
export const clarifyColony = (id: string, body: ColonyClarifyRequest) =>
  apiFetch<QueenProposal>(`${path(id)}/clarify`, json("POST", body));
export const createColonyGoal = (id: string, body: GoalRequest) =>
  apiFetch<ColonyGoal>(`${path(id)}/goals`, json("POST", body));
export const controlColonyGoal = (
  id: string,
  goal: string,
  body: ColonyControlRequest,
) =>
  apiFetch<ColonyGoal>(
    `${path(id)}/goals/${encodeURIComponent(goal)}/control`,
    json("POST", body),
  );
export const clarifyColonyGoal = (
  id: string,
  goal: string,
  body: components["schemas"]["ColonyGoalClarifyRequest"],
) =>
  apiFetch<ColonyGoal>(
    `${path(id)}/goals/${encodeURIComponent(goal)}/clarify`,
    json("POST", body),
  );
export const colonyMessages = (
  id: string,
  recipient: string,
  signal?: AbortSignal,
) =>
  apiFetch<{ messages: ColonyMessage[] }>(
    `${path(id)}/messages?recipient=${encodeURIComponent(recipient)}`,
    { signal: signal ?? new AbortController().signal },
  );
export const sendColonyMessage = (id: string, body: ColonyMessageRequest) =>
  apiFetch<{ messages: ColonyMessage[] }>(
    `${path(id)}/messages`,
    json("POST", body),
  );

export function blankColony(
  name: string,
  queen: string,
  members: string[],
): ColonyConfig {
  return {
    name,
    queen,
    members,
    autonomy: "supervised",
    start_mode: "review",
    connections: [],
    prompts: [],
    rooms: [],
    channels: [],
    baseline_context: [],
    instruction_revision: 0,
    positions: {},
  };
}
export function teamAliases(colony: ColonyConfig): string[] {
  return [colony.queen, ...colony.members];
}
export function activeGoal(colony: ColonyDetail): ColonyGoal | undefined {
  return colony.goals.find(
    (g) => g.task.status === "running" || g.task.status === "paused",
  );
}
export function nodeKey(kind: string, id: string): string {
  return `${kind}:${id}`;
}
export function splitNodeKey(key: string): [string, string] {
  const split = key.indexOf(":");
  return [key.slice(0, split), key.slice(split + 1)];
}

export const getColonyContext = (agent: string, signal?: AbortSignal) =>
  apiFetch<components["schemas"]["ColonyContextResponse"]>(
    `/api/colonies/context?agent=${encodeURIComponent(agent)}`,
    { signal: signal ?? new AbortController().signal },
  );
export const addColonyAgent = (
  id: string,
  body: components["schemas"]["ColonyAgentCreateRequest"],
) => apiFetch<ColonyDetail>(`${path(id)}/agents`, json("POST", body));
export const approveColonyCall = (
  id: string,
  goal: string,
  approval: string,
  body: components["schemas"]["ColonyApprovalRequest"],
) =>
  apiFetch<ColonyGoal>(
    `${path(id)}/goals/${encodeURIComponent(goal)}/approvals/${encodeURIComponent(approval)}`,
    json("POST", body),
  );
