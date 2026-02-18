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

function stripSystemReminder(text: string): string {
  return String(text || "")
    .replace(/<system-reminder>[\s\S]*?<\/system-reminder>/gi, "")
    .replace(/<system-reminder>[\s\S]*$/gi, "")
    .trim();
}

async function renderHumanToolReply(
  runtime: Awaited<ReturnType<typeof loadAgentConfig>>,
  system: string,
  userPrompt: string,
  tool: string,
  output: unknown,
): Promise<string> {
  const messages: ChatCompletionMessage[] = [
    { role: "system", content: system },
    { role: "user", content: userPrompt },
    {
      role: "user",
      content: `Tool ${tool} executed successfully. Result JSON: ${JSON.stringify(output ?? null)}. Reply for a human user only. Do not print raw JSON unless explicitly requested.`,
    },
  ];

  try {
    const reply = await runAgentChat(runtime, messages);
    return stripSystemReminder(reply) || `Done. Executed ${tool}.`;
  } catch {
    return `Done. Executed ${tool}.`;
  }
}

function makeSystemInstruction(enabledTools: string[], enabledIntegrations: string[]): string {
  const toolList = enabledTools.length ? enabledTools.join(", ") : "none";
  const integrationList = enabledIntegrations.length ? enabledIntegrations.join(", ") : "none";

  return [
    "You are ZeroClaw mobile runtime orchestrator.",
    "Do not pretend to execute device actions.",
    `Enabled tool ids: ${toolList}.`,
    `Enabled integrations: ${integrationList}.`,
    "Tool intent mapping rules:",
    "- Place outgoing call -> android_device.calls.start with {\"to\":\"+15551234567\"}",
    "- Read last calls/history -> android_device.userdata.call_log",
    "- Read SMS inbox/messages -> android_device.userdata.sms_inbox",
    "- List files/storage -> android_device.storage.files",
    "Never use call_log tool when user asks to place a call.",
    "If a device/tool action is needed, output ONLY valid JSON with this exact shape:",
    '{"type":"tool_call","tool":"<tool_id>","arguments":{}}',
    "For normal conversation, return plain text.",
  ].join(" ");
}

function extractPhoneNumber(text: string): string | null {
  const match = text.match(/(\+?\d[\d\s\-()]{5,}\d)/);
  if (!match?.[1]) return null;
  const normalized = match[1].replace(/[^\d+]/g, "");
  return normalized.length >= 6 ? normalized : null;
}

function extractSmsTargetAndBody(text: string): { to: string; body: string } | null {
  const match = text.match(/(?:send\s+sms\s+to|sms\s+to)\s*([+\d][\d\s\-()]{5,})\s*[:,-]\s*(.+)$/i);
  if (!match?.[1] || !match?.[2]) return null;
  const to = match[1].replace(/[^\d+]/g, "").trim();
  const body = match[2].trim();
  if (!to || !body) return null;
  return { to, body };
}

function normalizeDirectiveForPrompt(
  userPrompt: string,
  directive: { tool: string; arguments: Record<string, unknown> },
): { tool: string; arguments: Record<string, unknown> } {
  const prompt = userPrompt.toLowerCase();
  const asksToCall = /\b(call|dial|make\s+.*call|test\s+call)\b/.test(prompt);
  if (!asksToCall) return directive;
  if (directive.tool === "android_device.calls.start") return directive;

  const guessedPhone =
    (typeof directive.arguments.to === "string" && directive.arguments.to.trim()) ||
    (typeof directive.arguments.phone === "string" && directive.arguments.phone.trim()) ||
    (typeof directive.arguments.number === "string" && directive.arguments.number.trim()) ||
    extractPhoneNumber(userPrompt) ||
    "";

  if (!guessedPhone) return directive;

  return {
    tool: "android_device.calls.start",
    arguments: { to: guessedPhone },
  };
}

function inferDirectiveFromPrompt(
  userPrompt: string,
  enabledToolIds: string[],
): { tool: string; arguments: Record<string, unknown> } | null {
  const text = userPrompt.toLowerCase();
  const hasTool = (id: string) => enabledToolIds.includes(id);

  const sms = extractSmsTargetAndBody(userPrompt);
  if (sms && hasTool("android_device.sms.send")) {
    return { tool: "android_device.sms.send", arguments: sms };
  }

  const callNumber = extractPhoneNumber(userPrompt);
  if (/\b(call|dial|make\s+.*call|test\s+call)\b/.test(text) && callNumber && hasTool("android_device.calls.start")) {
    return { tool: "android_device.calls.start", arguments: { to: callNumber } };
  }

  if (/\b(photo|picture|camera|selfie)\b/.test(text) && hasTool("android_device.camera.capture")) {
    const front = /\b(front|selfie)\b/.test(text);
    return { tool: "android_device.camera.capture", arguments: { lens: front ? "front" : "rear" } };
  }

  if (/\b(gps|coordinates|location|where am i|current location)\b/.test(text) && hasTool("android_device.location.read")) {
    return { tool: "android_device.location.read", arguments: {} };
  }

  if (/\b(last calls|call history|recent calls|phone calls)\b/.test(text) && hasTool("android_device.userdata.call_log")) {
    return { tool: "android_device.userdata.call_log", arguments: { limit: 20 } };
  }

  if (/\b(sms|text messages|inbox)\b/.test(text) && hasTool("android_device.userdata.sms_inbox")) {
    return { tool: "android_device.userdata.sms_inbox", arguments: { limit: 20 } };
  }

  if (/\b(list files|show files|directory|storage)\b/.test(text) && hasTool("android_device.storage.files")) {
    return { tool: "android_device.storage.files", arguments: { scope: "user", path: "", limit: 200 } };
  }

  return null;
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

  const deterministicDirective = inferDirectiveFromPrompt(userPrompt, enabledTools);
  if (deterministicDirective) {
    const toolEvent = await executeToolDirective(deterministicDirective, { tools, security });
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

    const humanReply = await renderHumanToolReply(
      runtime,
      system,
      userPrompt,
      toolEvent.tool,
      toolEvent.output,
    );

    return {
      assistantText: humanReply,
      toolEvents: [toolEvent],
    };
  }

  let firstReply: string;
  try {
    firstReply = await runAgentChat(runtime, firstMessages);
  } catch (error) {
    return {
      assistantText:
        error instanceof Error
          ? `Agent provider error: ${error.message}. Try again or use Restart Agent.`
          : "Agent provider error. Try again or use Restart Agent.",
      toolEvents: [],
    };
  }
  const parsedDirective = parseToolDirective(firstReply);
  const directive = parsedDirective || inferDirectiveFromPrompt(userPrompt, enabledTools);
  if (!directive) {
    return {
      assistantText: stripSystemReminder(firstReply) || "(empty response)",
      toolEvents: [],
    };
  }

  const normalizedDirective = normalizeDirectiveForPrompt(userPrompt, directive);
  const toolEvent = await executeToolDirective(normalizedDirective, { tools, security });
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
      content: `Tool ${toolEvent.tool} executed successfully. Tool result JSON: ${summarizeToolResult}. Reply for a human user with what happened and next steps. Do not print raw JSON unless explicitly requested.`,
    },
  ];

  const finalReply = await runAgentChat(runtime, finalMessages);

  return {
    assistantText: stripSystemReminder(finalReply) || "Tool executed.",
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
