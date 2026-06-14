import type { LucideIcon } from 'lucide-react';
import {
  Terminal, FileText, FilePlus, FileEdit, Search, FolderSearch,
  Globe, ExternalLink, Download, Wifi, Database, GitBranch,
  Image, Camera, Calculator, Wrench, CheckCircle2, Loader2,
} from 'lucide-react';
import { Card, Badge } from '@/components/ui';

export interface ToolCallInfo {
  name: string;
  args?: unknown;
  output?: string;       // undefined = executing; string = completed
}

interface ToolCallCardProps {
  toolCall: ToolCallInfo;
}

const TOOL_ICON_MAP: Record<string, LucideIcon> = {
  shell: Terminal,
  file_read: FileText,
  file_write: FilePlus,
  file_edit: FileEdit,
  content_search: Search,
  glob_search: FolderSearch,
  browser: Globe,
  browser_open: ExternalLink,
  text_browser: Globe,
  web_search_tool: Search,
  web_fetch: Download,
  http_request: Wifi,
  memory_store: Database,
  memory_recall: Database,
  git_operations: GitBranch,
  image_gen: Image,
  screenshot: Camera,
  calculator: Calculator,
};

const INLINE_THRESHOLD = 80;
const PREVIEW_MAX_CHARS = 100;

function getIcon(name: string): LucideIcon {
  return TOOL_ICON_MAP[name] ?? Wrench;
}

function truncate(text: string, max: number): string {
  if (text.length <= max) return text;
  return text.slice(0, max) + '...';
}

export default function ToolCallCard({ toolCall }: ToolCallCardProps) {
  const Icon = getIcon(toolCall.name);
  const resolved = toolCall.output !== undefined;

  const argsStr = toolCall.args != null
    ? JSON.stringify(toolCall.args, null, 2)
    : null;

  const output = toolCall.output ?? '';
  const isInline = output.length <= INLINE_THRESHOLD;

  return (
    <Card padded={false} className="bg-pc-elevated p-3 text-xs">
      <div className="flex items-center gap-2">
        <Icon className="h-4 w-4 flex-shrink-0 text-pc-accent" />
        <span className="font-mono text-pc-text truncate">{toolCall.name}</span>
        <span className="ml-auto flex-shrink-0">
          {resolved ? (
            <Badge tone="ok">
              <CheckCircle2 className="h-3 w-3" />
              done
            </Badge>
          ) : (
            <Badge tone="neutral">
              <Loader2 className="h-3 w-3 animate-spin" />
              running
            </Badge>
          )}
        </span>
      </div>

      {argsStr && (
        <details className="mt-2 group">
          <summary className="cursor-pointer select-none text-pc-text-muted hover:text-pc-text-secondary">
            args
          </summary>
          <pre className="mt-1.5 overflow-auto rounded-[var(--radius-sm)] bg-pc-code p-2 font-mono text-[11px] leading-relaxed text-pc-text-secondary">
            {argsStr}
          </pre>
        </details>
      )}

      {resolved && (
        isInline ? (
          output && (
            <div className="mt-2 overflow-auto rounded-[var(--radius-sm)] bg-pc-code p-2 font-mono text-[11px] leading-relaxed text-pc-text-secondary">
              {output}
            </div>
          )
        ) : (
          <details className="mt-2">
            <summary className="cursor-pointer select-none truncate text-pc-text-muted hover:text-pc-text-secondary">
              {truncate(output, PREVIEW_MAX_CHARS)}
            </summary>
            <pre className="mt-1.5 overflow-auto rounded-[var(--radius-sm)] bg-pc-code p-2 font-mono text-[11px] leading-relaxed text-pc-text-secondary">
              {output}
            </pre>
          </details>
        )
      )}
    </Card>
  );
}
