import { useLayoutEffect, useMemo, useRef } from "react";
import { ChevronRight, ListChecks } from "lucide-react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { SopAssistantMessage, SopPromptMessage } from "@/components/SopProposal";
import { t } from "@/lib/i18n";
import type { CodeMessage, CodePlanEntry } from "./CodeConnection";

interface ToolStep {
  name: string;
  input?: unknown;
  output?: string;
}
interface Turn {
  user?: string;
  replies: string[];
  steps: ToolStep[];
}

function groupTurns(messages: CodeMessage[]): Turn[] {
  const turns: Turn[] = [];
  let current: Turn = { replies: [], steps: [] };
  const calls = new Map<string, ToolStep>();
  for (const message of messages) {
    if (message.kind === "tool_call" || message.kind === "tool_result" || message.role === "tool") {
      if (message.kind === "tool_result") {
        const step = message.tool_call_id ? calls.get(message.tool_call_id) : undefined;
        const output = message.tool_output ?? message.content;
        if (step) {
          step.output = output;
          calls.delete(message.tool_call_id!);
        } else {
          current.steps.push({ name: message.tool_name ?? "", output });
        }
      } else {
        const step = { name: message.tool_name ?? "", input: message.tool_input ?? message.content };
        current.steps.push(step);
        if (message.tool_call_id) calls.set(message.tool_call_id, step);
      }
    } else if (message.role === "user") {
      if (current.user !== undefined || current.replies.length || current.steps.length) turns.push(current);
      current = { user: message.content, replies: [], steps: [] };
      calls.clear();
    } else if (message.role === "assistant" && message.content) {
      current.replies.push(message.content);
    }
  }
  if (current.user !== undefined || current.replies.length || current.steps.length) turns.push(current);
  return turns;
}

function outcome(output: string | undefined) {
  if (output === undefined) return "running";
  try {
    const parsed = JSON.parse(output);
    if (parsed?.success === false || (typeof parsed?.exit_code === "number" && parsed.exit_code !== 0)) return "failed";
    if (parsed?.success === true || parsed?.exit_code === 0) return "success";
  } catch { /* Many tool outputs are plain text. */ }
  return "finished";
}

function milestone(step: ToolStep) {
  const input = step.input as { command?: string; path?: string; file_path?: string } | undefined;
  const rawCommand = input?.command ?? (typeof step.input === "string" ? step.input : "");
  const test = /(^|[\s"'])(cargo\s+test|npm\s+(run\s+)?test|pnpm\s+(run\s+)?test|pytest|vitest|go\s+test|swift\s+test)\b/.test(rawCommand);
  const state = outcome(step.output);
  if (test) {
    return t(state === "running" ? "code.activity_testing" : state === "success" ? "code.activity_tests_passed"
      : state === "failed" ? "code.activity_tests_failed" : "code.activity_tests_finished");
  }
  if (state === "failed") return t("code.activity_failed");
  if (/read|open_file/i.test(step.name)) return t(state === "running" ? "code.activity_reading" : "code.activity_read");
  if (/search|grep|glob|list/i.test(step.name)) return t(state === "running" ? "code.activity_searching" : "code.activity_search");
  if (/edit|write|patch/i.test(step.name)) {
    return t(state === "running" ? "code.activity_editing" : state === "success" ? "code.activity_edit" : "code.activity_edit_done");
  }
  return t(state === "running" ? "code.activity_working" : "code.activity_step");
}

function TurnActivity({ steps, plan, running }: { steps: ToolStep[]; plan: CodePlanEntry[]; running: boolean }) {
  if (!steps.length && !plan.length) return null;
  const milestones = [...new Set([
    ...plan.filter((entry) => entry.status === "in_progress" || entry.status === "completed")
      .map((entry) => entry.status === "in_progress" ? entry.activeForm || entry.content : entry.content),
    ...steps.slice(-5).map(milestone),
  ])].slice(-3);
  return (
    <div className="my-2 ml-3 border-l border-pc-border pl-3 text-xs text-pc-text-muted">
      {running && (
        <ul aria-label={t("code.progress")} className="mb-1 space-y-1">
          {milestones.map((step) => <li key={step}>{step}</li>)}
        </ul>
      )}
      {!!steps.length && (
        <details className="group">
          <summary className="flex w-fit cursor-pointer list-none items-center gap-1.5 rounded px-1 py-1.5 hover:bg-pc-elevated">
            <ChevronRight className="h-3 w-3 group-open:rotate-90" aria-hidden="true" />
            <ListChecks className="h-3.5 w-3.5" aria-hidden="true" />
            {t("code.activity_count").replace("{count}", String(steps.length))}
          </summary>
          <ol className="space-y-1 py-1 pl-4">
            {steps.map((step, index) => (
              <li key={index}>
                <details>
                  <summary className="cursor-pointer py-1">{milestone(step)}</summary>
                  <p className="mt-1 font-medium text-pc-text-secondary">{step.name}</p>
                  {step.input !== undefined && <pre className="my-1 max-h-48 overflow-auto whitespace-pre-wrap break-words rounded bg-pc-elevated p-2">{typeof step.input === "string" ? step.input : JSON.stringify(step.input, null, 2)}</pre>}
                  {step.output && <pre className="my-1 max-h-48 overflow-auto whitespace-pre-wrap break-words rounded bg-pc-elevated p-2">{step.output}</pre>}
                </details>
              </li>
            ))}
          </ol>
        </details>
      )}
    </div>
  );
}

export interface ReadingPosition {
  top: number;
  follows: boolean;
}

export default function CodeTranscript({
  alias, messages, stream, plan, busy, loaded, visible, position, hasOlder, onOlder, sopAssistant,
}: {
  alias: string;
  messages: CodeMessage[];
  stream: string;
  plan: CodePlanEntry[];
  busy: boolean;
  loaded: boolean;
  visible: boolean;
  position: ReadingPosition;
  hasOlder: boolean;
  onOlder: () => void;
  sopAssistant?: { onApply: (source: string) => Promise<void> };
}) {
  const ref = useRef<HTMLDivElement>(null);
  const restored = useRef<ReadingPosition | null>(null);
  const turns = useMemo(() => groupTurns(messages), [messages]);
  useLayoutEffect(() => {
    const pane = ref.current;
    if (!pane || !visible || !loaded) return;
    if (position.follows) pane.scrollTop = pane.scrollHeight;
    else if (restored.current !== position) pane.scrollTop = position.top;
    restored.current = position;
  }, [loaded, messages, position, stream, plan, visible]);

  const reply = (text: string, key?: number, partial = false) => (
    <div key={key} className="my-4">
      <p className="mb-1 text-xs font-semibold text-pc-accent">{alias}</p>
      <div className="chat-markdown text-sm break-words leading-relaxed">
        {sopAssistant && !partial ? <SopAssistantMessage message={text} onApply={sopAssistant.onApply} />
          : <ReactMarkdown remarkPlugins={[remarkGfm]}>{text}</ReactMarkdown>}
      </div>
    </div>
  );
  return (
    <div ref={ref} onScroll={(event) => {
      if (!visible || !loaded) return;
      const pane = event.currentTarget;
      position.top = pane.scrollTop;
      position.follows = pane.scrollHeight - pane.scrollTop - pane.clientHeight < 120;
    }} className="min-h-0 flex-1 overflow-y-auto px-4 py-5 sm:px-6">
      <div className="mx-auto max-w-3xl">
        {hasOlder && <button type="button" onClick={onOlder} className="btn-secondary mx-auto mb-4 block rounded px-3 py-1 text-xs">{t("code.load_older")}</button>}
        {!turns.length && !stream && loaded && (
          <div className="py-8">
            <h3 className="text-sm font-semibold">{t(sopAssistant ? "sop_workspace.helper_title" : "code.start_title")}</h3>
            <p className="mt-1.5 text-sm text-pc-text-muted">{t(sopAssistant ? "sop_workspace.helper_hint" : "code.start_hint")}</p>
          </div>
        )}
        {turns.map((turn, index) => {
          const active = busy && index === turns.length - 1;
          return (
            <article key={index} className="mb-6 space-y-2">
              {turn.user !== undefined && (
                <div className="rounded-lg border-l-2 border-pc-text-faint bg-pc-elevated/65 px-3 py-2.5">
                  <p className="mb-1 text-xs font-semibold text-pc-text-secondary">{t("code.you")}</p>
                  {sopAssistant ? <SopPromptMessage message={turn.user} /> : <p className="whitespace-pre-wrap break-words text-sm">{turn.user}</p>}
                </div>
              )}
              {active && <TurnActivity steps={turn.steps} plan={plan} running />}
              {turn.replies.map((text, offset) => reply(text, offset))}
              {active && stream && reply(stream, undefined, true)}
              {!active && <TurnActivity steps={turn.steps} plan={[]} running={false} />}
            </article>
          );
        })}
        {!turns.length && stream && reply(stream, undefined, true)}
      </div>
    </div>
  );
}
