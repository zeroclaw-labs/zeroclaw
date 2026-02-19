import { runAgentChat, type ChatCompletionMessage } from "../api/mobileclaw";
import { addActivity } from "../state/activity";
import { sanitizeAssistantArtifacts } from "../state/chat";
import {
  loadAgentConfig,
  loadDeviceToolsConfig,
  loadIntegrationsConfig,
  loadSecurityConfig,
} from "../state/mobileclaw";
import { getRuntimeSupervisorState } from "./supervisor";
import { executeToolDirective, parseToolDirective } from "./tooling";
import type { AgentTurnResult } from "./types";

function stripSystemReminder(text: string): string {
  return sanitizeAssistantArtifacts(text);
}

function extractFirstUrl(text: string): string | null {
  const match = String(text || "").match(/https?:\/\/[^\s)]+/i);
  if (!match?.[0]) return null;
  return match[0].replace(/[.,!?;:]+$/, "");
}

function isUrlReadingIntent(text: string): boolean {
  const lower = String(text || "").toLowerCase();
  return /\b(read|summarize|analyze|check|what is|what's in|follow)\b/.test(lower);
}

function htmlToPlainText(html: string): string {
  return html
    .replace(/<script[\s\S]*?<\/script>/gi, " ")
    .replace(/<style[\s\S]*?<\/style>/gi, " ")
    .replace(/<[^>]+>/g, " ")
    .replace(/\s+/g, " ")
    .trim();
}

async function runStandardWebReadTool(url: string): Promise<string> {
  const res = await fetch(url, { method: "GET" });
  if (!res.ok) throw new Error(`Web fetch failed: HTTP ${res.status}`);
  const contentType = String(res.headers.get("content-type") || "").toLowerCase();
  const raw = await res.text();
  const normalized = contentType.includes("text/html") ? htmlToPlainText(raw) : raw;
  return normalized.slice(0, 24000);
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
    "You are MobileClaw: a lightweight autonomous AI agent with Mobile UX designed to run on Android devices.",
    "Do not pretend to execute device actions.",
    `Enabled tool ids: ${toolList}.`,
    `Enabled integrations: ${integrationList}.`,
    "Tool intent mapping rules:",
    "- Place outgoing call -> android_device.calls.start with {\"to\":\"+15551234567\"}",
    "- Read last calls/history -> android_device.userdata.call_log",
    "- Read SMS inbox/messages -> android_device.userdata.sms_inbox",
    "- List files/storage -> android_device.storage.files",
    "- Read URL content -> standard web read tool (never android_device.browser.*)",
    "Never use call_log tool when user asks to place a call.",
    "If a device/tool action is needed, output ONLY valid JSON with this exact shape:",
    '{"type":"tool_call","tool":"<tool_id>","arguments":{}}',
    "For normal conversation, return plain text.",
  ].join(" ");
}

function makeIntegrationHints(config: Awaited<ReturnType<typeof loadIntegrationsConfig>>): string {
  const hints: string[] = [];

  if (config.telegramEnabled && config.telegramBotToken.trim() && config.telegramChatId.trim()) {
    hints.push(
      "Telegram integration is configured with bot token and chat ID.",
      "When user asks to send/write Telegram message, call integration.telegram.send_message with arguments containing only message text.",
      "Never ask user for Telegram chat ID.",
    );
  } else if (config.telegramEnabled) {
    hints.push(
      "Telegram integration is enabled but incomplete.",
      "If user asks for Telegram sending, ask them to detect chat ID in Integrations screen first.",
    );
  }

  if (config.discordEnabled) {
    hints.push("Discord integration requires backend runtime channel path for inbound/outbound automation.");
  }
  if (config.slackEnabled) {
    hints.push("Slack integration requires backend runtime channel path for inbound/outbound automation.");
  }
  if (config.whatsappEnabled) {
    hints.push("WhatsApp integration requires backend runtime channel path for inbound/outbound automation.");
  }
  if (config.composioEnabled) {
    hints.push("Composio integration actions are executed by backend runtime when available.");
  }

  return hints.join(" ");
}

function isInboundIntegrationIntent(prompt: string): boolean {
  const text = prompt.toLowerCase();
  return /(incoming|new message|listen|watch|monitor|reply).*(telegram|discord|slack|whatsapp)/.test(text) ||
    /(telegram|discord|slack|whatsapp).*(incoming|new message|listen|watch|monitor|reply)/.test(text);
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

function extractTelegramMessageBody(text: string): string {
  const trimmed = text.trim();
  const withSeparator = trimmed.match(/telegram[^:]*[:\-]\s*(.+)$/i);
  if (withSeparator?.[1]) return withSeparator[1].trim();

  const quoted = trimmed.match(/telegram[^"']*["']([\s\S]+)["']/i);
  if (quoted?.[1]) return quoted[1].trim();

  return "";
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
  integrations: Awaited<ReturnType<typeof loadIntegrationsConfig>>,
): { tool: string; arguments: Record<string, unknown> } | null {
  const text = userPrompt.toLowerCase();
  const hasTool = (id: string) => enabledToolIds.includes(id);

  const asksTelegramMessage = /\b(telegram)\b/.test(text) && /\b(send|write|message|reply|text)\b/.test(text);
  if (asksTelegramMessage && integrations.telegramEnabled && integrations.telegramBotToken.trim() && integrations.telegramChatId.trim()) {
    const body = extractTelegramMessageBody(userPrompt);
    if (body) {
      return { tool: "integration.telegram.send_message", arguments: { text: body } };
    }
  }

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
  const [runtime, tools, integrations, security, supervisor] = await Promise.all([
    loadAgentConfig(),
    loadDeviceToolsConfig(),
    loadIntegrationsConfig(),
    loadSecurityConfig(),
    getRuntimeSupervisorState(),
  ]);

  if (supervisor.status === "degraded" && isInboundIntegrationIntent(userPrompt)) {
    return {
      assistantText:
        "ZeroClaw runtime is degraded, so inbound channel events may not reach the agent right now. Check Activity status, ensure backend is reachable, then retry.",
      toolEvents: [],
    };
  }

  const lowerPrompt = userPrompt.toLowerCase();
  const asksTelegramMessage = /\btelegram\b/.test(lowerPrompt) && /\b(send|write|message|reply|text)\b/.test(lowerPrompt);
  const telegramConfigured = integrations.telegramEnabled && integrations.telegramBotToken.trim() && integrations.telegramChatId.trim();
  if (asksTelegramMessage && telegramConfigured && !extractTelegramMessageBody(userPrompt)) {
    return {
      assistantText: "Sure. What exact text should I send to your Telegram chat?",
      toolEvents: [],
    };
  }

  const enabledTools = tools.filter((tool) => tool.enabled).map((tool) => tool.id);
  const enabledIntegrations = integrationList(integrations);
  const systemBase = makeSystemInstruction(
    [...enabledTools, ...integrationToolIds(enabledIntegrations)],
    enabledIntegrations,
  );
  const system = [systemBase, makeIntegrationHints(integrations)].filter(Boolean).join(" ");

  const firstMessages: ChatCompletionMessage[] = [
    { role: "system", content: system },
    { role: "user", content: userPrompt },
  ];

  const requestedUrl = extractFirstUrl(userPrompt);
  if (requestedUrl && isUrlReadingIntent(userPrompt) && security.preferStandardWebTool) {
    try {
      const pageText = await runStandardWebReadTool(requestedUrl);
      await addActivity({
        kind: "action",
        source: "chat",
        title: "Tool executed: standard.web_read",
        detail: requestedUrl,
      });

      const reply = await runAgentChat(runtime, [
        { role: "system", content: system },
        { role: "user", content: userPrompt },
        {
          role: "user",
          content:
            `Web tool result for ${requestedUrl}:\n${pageText}\n\nRespond for a human user and do not include raw JSON.`,
        },
      ]);

      return {
        assistantText: stripSystemReminder(reply) || "I read the page and prepared the result.",
        toolEvents: [
          {
            tool: "standard.web_read",
            status: "executed",
            detail: "Standard web read tool executed.",
          },
        ],
      };
    } catch (error) {
      return {
        assistantText:
          error instanceof Error
            ? `I could not read ${requestedUrl} with the standard web tool: ${error.message}`
            : `I could not read ${requestedUrl} with the standard web tool.`,
        toolEvents: [
          {
            tool: "standard.web_read",
            status: "failed",
            detail: error instanceof Error ? error.message : "Web read failed.",
          },
        ],
      };
    }
  }

  const deterministicDirective = inferDirectiveFromPrompt(userPrompt, enabledTools, integrations);
  if (deterministicDirective) {
    const toolEvent = await executeToolDirective(deterministicDirective, { tools, integrations, security });
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
  const directive = parsedDirective || inferDirectiveFromPrompt(userPrompt, enabledTools, integrations);
  if (!directive) {
    return {
      assistantText: stripSystemReminder(firstReply) || "(empty response)",
      toolEvents: [],
    };
  }

  const normalizedDirective = normalizeDirectiveForPrompt(userPrompt, directive);
  const toolEvent = await executeToolDirective(normalizedDirective, { tools, integrations, security });
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
  const [tools, integrations, security] = await Promise.all([
    loadDeviceToolsConfig(),
    loadIntegrationsConfig(),
    loadSecurityConfig(),
  ]);
  const directive = parseToolDirective(rawAssistantReply);

  if (!directive) {
    return {
      assistantText: "Probe failed: response is not a valid tool_call JSON payload.",
      toolEvents: [],
    };
  }

  const toolEvent = await executeToolDirective(directive, { tools, integrations, security });
  return {
    assistantText:
      toolEvent.status === "executed"
        ? `Probe success: ${toolEvent.tool} executed.`
        : `Probe result: ${toolEvent.tool} ${toolEvent.status} (${toolEvent.detail})`,
    toolEvents: [toolEvent],
  };
}
