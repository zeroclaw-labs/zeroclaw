import type { ToolSpec, CliTool, OptionDomain } from '../types/api';

/** A flattened, group-tagged catalog entry. */
export interface CatalogEntry {
  name: string;
  description: string;
  group: 'agent' | 'cli';
  /** JSON Schema for the tool's args (agent tools only; CLI tools omit it). */
  parameters?: unknown;
  /** Declared structured-output schema, when the tool declares one. */
  output?: unknown;
  /** Parameter name -> runtime option domain, for domain-typed params. */
  param_domains?: Record<string, OptionDomain>;
}

export type CatalogSource = 'agent' | 'cli';

export interface CatalogLoadWarning {
  source: CatalogSource;
  message: string;
}

export interface ToolCatalogLoadResult {
  entries: CatalogEntry[];
  warnings: CatalogLoadWarning[];
}

function reasonMessage(reason: unknown): string {
  return reason instanceof Error ? reason.message : String(reason);
}

function cliDescription(tool: CliTool): string {
  // CliTool has no `description`; synthesize a short one from category/path
  // so the row still says something useful.
  const parts = [tool.category, tool.version ? `v${tool.version}` : null, tool.path]
    .filter(Boolean)
    .join(' · ');
  return parts || tool.path;
}

export function settleToolCatalogResult(
  toolsResult: PromiseSettledResult<ToolSpec[]>,
  cliToolsResult: PromiseSettledResult<CliTool[]>,
): ToolCatalogLoadResult {
  if (toolsResult.status === 'rejected' && cliToolsResult.status === 'rejected') {
    throw new Error(`${reasonMessage(toolsResult.reason)}; ${reasonMessage(cliToolsResult.reason)}`);
  }

  const warnings: CatalogLoadWarning[] = [];
  if (toolsResult.status === 'rejected') {
    warnings.push({ source: 'agent', message: reasonMessage(toolsResult.reason) });
  }
  if (cliToolsResult.status === 'rejected') {
    warnings.push({ source: 'cli', message: reasonMessage(cliToolsResult.reason) });
  }

  const tools = toolsResult.status === 'fulfilled' ? toolsResult.value : [];
  const cliTools = cliToolsResult.status === 'fulfilled' ? cliToolsResult.value : [];
  const agentEntries: CatalogEntry[] = tools.map((tnt: ToolSpec) => ({
    name: tnt.name,
    description: tnt.description,
    group: 'agent' as const,
    parameters: tnt.parameters,
    output: tnt.output,
    param_domains: tnt.param_domains,
  }));
  const cli: CatalogEntry[] = cliTools.map((c: CliTool) => ({
    name: c.name,
    description: cliDescription(c),
    group: 'cli' as const,
  }));
  return { entries: [...agentEntries, ...cli], warnings };
}
