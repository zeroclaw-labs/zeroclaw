// ---------------------------------------------------------------------------
// Smithery Registry — MCP Servers catalog
// ---------------------------------------------------------------------------

export interface SmitheryServer {
  qualifiedName: string;
  displayName: string;
  description: string;
  useCount: number;
  verified: boolean;
  isDeployed: boolean;
  homepage?: string;
  iconUrl?: string;
}

export interface SmitherySearchResult {
  servers: SmitheryServer[];
  pagination: {
    currentPage: number;
    pageSize: number;
    totalPages: number;
    totalCount: number;
  };
}

const SMITHERY_BASE = "https://registry.smithery.ai";

export async function searchSmitheryServers(
  query: string = "",
  page: number = 1,
  pageSize: number = 12,
): Promise<SmitherySearchResult> {
  const params = new URLSearchParams({
    page: String(page),
    pageSize: String(pageSize),
  });
  if (query.trim()) {
    params.set("q", query.trim());
  }

  const res = await fetch(`${SMITHERY_BASE}/servers?${params}`);
  if (!res.ok) {
    throw new Error(`Smithery API error: ${res.status}`);
  }
  return res.json() as Promise<SmitherySearchResult>;
}

// ---------------------------------------------------------------------------
// ClawHub — Skills catalog
// ---------------------------------------------------------------------------

export interface ClawHubSkill {
  slug: string;
  name: string;
  description: string;
  author?: string;
  installs?: number;
  stars?: number;
  tags?: string[];
  sourceUrl?: string;
}

export interface ClawHubSearchResult {
  skills: ClawHubSkill[];
  total?: number;
}

const CLAWHUB_BASE = "https://clawhub.ai/api/v1";

export async function searchClawHubSkills(
  query: string = "",
): Promise<ClawHubSearchResult> {
  const params = new URLSearchParams();
  if (query.trim()) {
    params.set("q", query.trim());
  }

  try {
    const res = await fetch(`${CLAWHUB_BASE}/skills?${params}`);
    if (!res.ok) {
      throw new Error(`ClawHub API error: ${res.status}`);
    }
    const data = await res.json();

    // Normalize response — ClawHub may return different shapes
    if (Array.isArray(data)) {
      return { skills: data, total: data.length };
    }
    if (data.skills) {
      return data as ClawHubSearchResult;
    }
    if (data.results) {
      return { skills: data.results, total: data.total ?? data.results.length };
    }
    return { skills: [], total: 0 };
  } catch {
    // ClawHub may have CORS restrictions — return empty gracefully
    return { skills: [], total: 0 };
  }
}
