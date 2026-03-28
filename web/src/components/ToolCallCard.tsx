import type { LucideIcon } from 'lucide-react';
import {
  Terminal, FileText, FilePlus, FileEdit, Search, FolderSearch,
  Globe, ExternalLink, Download, Wifi, Database, GitBranch,
  Image, Camera, Calculator, Wrench, CheckCircle2, Loader2,
} from 'lucide-react';

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
    <div className="tool-card">
      <div className="tool-card__header">
        <Icon className="h-4 w-4 flex-shrink-0" style={{ color: 'var(--pc-accent)' }} />
        <span>{toolCall.name}</span>
        {resolved ? (
          <CheckCircle2 className="h-3.5 w-3.5 flex-shrink-0" style={{ color: '#34d399' }} />
        ) : (
          <Loader2 className="h-3.5 w-3.5 flex-shrink-0 animate-spin" style={{ color: 'var(--pc-accent)' }} />
        )}
      </div>

      {argsStr && (
        <details>
          <summary>args</summary>
          <pre>{argsStr}</pre>
        </details>
      )}

      {resolved && (
        isInline ? (
          output && <div className="tool-card__inline">{output}</div>
        ) : (
          <details>
            <summary>{truncate(output, PREVIEW_MAX_CHARS)}</summary>
            <pre>{output}</pre>
          </details>
        )
      )}
    </div>
  );
}
