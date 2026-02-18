export type ToolCallDirective = {
  tool: string;
  arguments: Record<string, unknown>;
};

export type ToolExecutionEvent = {
  tool: string;
  status: "executed" | "blocked" | "failed";
  detail: string;
  output?: unknown;
};

export type AgentTurnResult = {
  assistantText: string;
  toolEvents: ToolExecutionEvent[];
};
