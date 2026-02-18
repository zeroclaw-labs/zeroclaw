import { runAgentChat, type ChatCompletionMessage } from "../api/mobileclaw";
import { addActivity } from "../state/activity";
import {
  loadAgentConfig,
  loadDeviceToolsConfig,
  loadIntegrationsConfig,
  loadSecurityConfig,
} from "../state/mobileclaw";
import { executeToolDirective, parseToolDirective } from "./tooling";
import type { AgentTurnResult } from "./types";

function makeSystemInstruction(enabledTools: string[], enabledIntegrations: string[]): string {
  const toolList = enabledTools.length ? enabledTools.join(", ") : "none";
  const integrationList = enabledIntegrations.length ? enabledIntegrations.join(", ") : "none";

  return [
    "You are ZeroClaw mobile runtime orchestrator.",
    "Do not pretend to execute device actions.",
    `Enabled tool ids: ${toolList}.`,
    `Enabled integrations: ${integrationList}.`,
    "If a device/tool action is needed, output ONLY valid JSON with this exact shape:",
    '{"type":"tool_call","tool":"<tool_id>","arguments":{}}',
    "For normal conversation, return plain text.",
  ].join(" ");
}

function integrationList(config: Awaited<ReturnType<typeof loadIntegrationsConfig>>): string[] {
  return [
    config.telegramEnabled ? "telegram" : "",
    config.discordEnabled ? "discord" : "",
    config.slackEnabled ? "slack" : "",
    config.whatsappEnabled ? "whatsapp" : "",
    config.composioEnabled ? "composio" : "",
  ].filter(Boolean);
}

function integrationToolIds(integrations: string[]): string[] {
  const map: Record<string, string[]> = {
    telegram: ["integration.telegram.send_message"],
    discord: ["integration.discord.send_message"],
    slack: ["integration.slack.send_message"],
    whatsapp: ["integration.whatsapp.send_message"],
    composio: ["integration.composio.invoke_action"],
  };

  return integrations.flatMap((name) => map[name] ?? []);
}

export async function runAgentTurn(userPrompt: string): Promise<AgentTurnResult> {
  const [runtime, tools, integrations, security] = await Promise.all([
    loadAgentConfig(),
    loadDeviceToolsConfig(),
    loadIntegrationsConfig(),
    loadSecurityConfig(),
  ]);

  const enabledTools = tools.filter((tool) => tool.enabled).map((tool) => tool.id);
  const enabledIntegrations = integrationList(integrations);
  const system = makeSystemInstruction(
    [...enabledTools, ...integrationToolIds(enabledIntegrations)],
    enabledIntegrations,
  );

  const firstMessages: ChatCompletionMessage[] = [
    { role: "system", content: system },
    { role: "user", content: userPrompt },
  ];

  const firstReply = await runAgentChat(runtime, firstMessages);
  const directive = parseToolDirective(firstReply);
  if (!directive) {
    return {
      assistantText: firstReply || "(empty response)",
      toolEvents: [],
    };
  }

  const toolEvent = await executeToolDirective(directive, { tools, security });
  await addActivity({
    kind: "action",
    source: "chat",
    title: `Tool ${toolEvent.status}: ${toolEvent.tool}`,
    detail: toolEvent.detail,
  });

  if (toolEvent.status !== "executed") {
    return {
      assistantText: `I could not run ${toolEvent.tool}: ${toolEvent.detail}`,
      toolEvents: [toolEvent],
    };
  }

  const summarizeToolResult = JSON.stringify(toolEvent.output ?? null);
  const finalMessages: ChatCompletionMessage[] = [
    { role: "system", content: system },
    { role: "user", content: userPrompt },
    { role: "assistant", content: firstReply },
    {
      role: "user",
      content: `Tool ${toolEvent.tool} executed successfully. Tool result JSON: ${summarizeToolResult}. Reply to the user with what happened and next steps.`,
    },
  ];

  const finalReply = await runAgentChat(runtime, finalMessages);

  return {
    assistantText: finalReply || "Tool executed.",
    toolEvents: [toolEvent],
  };
}

export async function runToolExecutionProbe(rawAssistantReply: string): Promise<AgentTurnResult> {
  const [tools, security] = await Promise.all([loadDeviceToolsConfig(), loadSecurityConfig()]);
  const directive = parseToolDirective(rawAssistantReply);

  if (!directive) {
    return {
      assistantText: "Probe failed: response is not a valid tool_call JSON payload.",
      toolEvents: [],
    };
  }

  const toolEvent = await executeToolDirective(directive, { tools, security });
  return {
    assistantText:
      toolEvent.status === "executed"
        ? `Probe success: ${toolEvent.tool} executed.`
        : `Probe result: ${toolEvent.tool} ${toolEvent.status} (${toolEvent.detail})`,
    toolEvents: [toolEvent],
  };
}
