import { useState, useEffect, useCallback } from "react";
import {
  Server,
  Search,
  Trash2,
  Plus,
  Loader2,
  CheckCircle2,
  ExternalLink,
} from "lucide-react";
import type { McpServer, McpServerInput } from "@/types/api";
import { getMcpServers, deleteMcpServer } from "@/lib/api";
import { searchSmitheryServers, type SmitheryServer } from "@/lib/catalogs";
import McpAddModal from "./McpAddModal";

type SubTab = "configured" | "smithery";

export default function McpServersTab() {
  const [subTab, setSubTab] = useState<SubTab>("configured");
  const [servers, setServers] = useState<McpServer[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // Smithery state
  const [smitheryQuery, setSmitheryQuery] = useState("");
  const [smitheryResults, setSmitheryResults] = useState<SmitheryServer[]>([]);
  const [smitheryLoading, setSmitheryLoading] = useState(false);
  const [smitherySearched, setSmitherySearched] = useState(false);
  const [smitheryTotal, setSmitheryTotal] = useState(0);

  // Modal state
  const [showAddModal, setShowAddModal] = useState(false);
  const [addPrefill, setAddPrefill] = useState<Partial<McpServerInput>>({});

  // Delete state
  const [deleting, setDeleting] = useState<string | null>(null);

  const loadServers = useCallback(() => {
    setLoading(true);
    getMcpServers()
      .then(setServers)
      .catch((err) => setError(err.message))
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    loadServers();
  }, [loadServers]);

  // Load popular servers on first visit to Smithery tab
  useEffect(() => {
    if (subTab === "smithery" && !smitherySearched) {
      handleSmitherySearch();
    }
  }, [subTab]);

  const handleSmitherySearch = async () => {
    setSmitheryLoading(true);
    setSmitherySearched(true);
    try {
      const result = await searchSmitheryServers(smitheryQuery, 1, 12);
      setSmitheryResults(result.servers);
      setSmitheryTotal(result.pagination.totalCount);
    } catch {
      setSmitheryResults([]);
    } finally {
      setSmitheryLoading(false);
    }
  };

  const handleSearchKey = (e: React.KeyboardEvent) => {
    if (e.key === "Enter") handleSmitherySearch();
  };

  const handleDelete = async (name: string) => {
    setDeleting(name);
    try {
      await deleteMcpServer(name);
      setServers((prev) => prev.filter((s) => s.name !== name));
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "Delete failed");
    } finally {
      setDeleting(null);
    }
  };

  const handleAddFromSmithery = (server: SmitheryServer) => {
    setAddPrefill({
      name: server.displayName.toLowerCase().replace(/\s+/g, "-"),
      transport: "stdio",
      command: `npx -y @smithery/cli run ${server.qualifiedName}`,
      args: [],
      env: {},
    });
    setShowAddModal(true);
  };

  const handleAddManual = () => {
    setAddPrefill({});
    setShowAddModal(true);
  };

  const handleModalSaved = () => {
    setShowAddModal(false);
    loadServers();
    setSubTab("configured");
  };

  return (
    <div className="space-y-4">
      {/* Sub-tabs + Add button */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          {(["configured", "smithery"] as SubTab[]).map((tab) => (
            <button
              key={tab}
              onClick={() => setSubTab(tab)}
              className={`px-3.5 py-1.5 rounded-xl text-xs font-semibold transition-all duration-300 ${
                subTab === tab
                  ? "text-white shadow-[0_0_15px_rgba(0,128,255,0.2)]"
                  : "text-[#556080] border border-[#1a1a3e] hover:text-white hover:border-[#0080ff40]"
              }`}
              style={
                subTab === tab
                  ? { background: "linear-gradient(135deg, #0080ff, #0066cc)" }
                  : {}
              }
            >
              {tab === "configured"
                ? `Configured (${servers.length})`
                : "Smithery"}
            </button>
          ))}
        </div>
        <button
          onClick={handleAddManual}
          className="btn-electric flex items-center gap-1.5 px-3 py-1.5 rounded-xl text-xs font-semibold"
        >
          <Plus className="h-3.5 w-3.5" />
          Add Manual
        </button>
      </div>

      {error && (
        <div className="rounded-xl bg-[#ff446615] border border-[#ff446630] p-3 text-xs text-[#ff6680] animate-fade-in">
          {error}
          <button
            onClick={() => setError(null)}
            className="ml-2 underline hover:no-underline"
          >
            dismiss
          </button>
        </div>
      )}

      {/* Configured Servers */}
      {subTab === "configured" && (
        <>
          {loading ? (
            <div className="flex items-center justify-center py-12">
              <Loader2 className="h-6 w-6 text-[#0080ff] animate-spin" />
            </div>
          ) : servers.length === 0 ? (
            <div className="glass-card p-8 text-center">
              <Server className="h-10 w-10 text-[#1a1a3e] mx-auto mb-3" />
              <p className="text-[#556080] mb-2">No MCP servers configured</p>
              <button
                onClick={() => setSubTab("smithery")}
                className="text-xs text-[#0080ff] hover:underline"
              >
                Browse Smithery catalog
              </button>
            </div>
          ) : (
            <div className="glass-card overflow-hidden">
              <table className="table-electric w-full">
                <thead>
                  <tr>
                    <th className="text-left">Name</th>
                    <th className="text-left">Transport</th>
                    <th className="text-left">Command / URL</th>
                    <th className="text-left">Timeout</th>
                    <th className="text-right">Actions</th>
                  </tr>
                </thead>
                <tbody>
                  {servers.map((server) => (
                    <tr key={server.name}>
                      <td className="px-4 py-3 text-white font-medium text-sm">
                        {server.name}
                      </td>
                      <td className="px-4 py-3">
                        <span className="inline-flex items-center px-2 py-0.5 rounded-full text-[10px] font-semibold border border-[#1a1a3e] text-[#8892a8] capitalize">
                          {server.transport}
                        </span>
                      </td>
                      <td className="px-4 py-3 text-[#556080] font-mono text-xs truncate max-w-[250px]">
                        {server.url || server.command}
                      </td>
                      <td className="px-4 py-3 text-[#556080] text-xs">
                        {server.tool_timeout_secs
                          ? `${server.tool_timeout_secs}s`
                          : "-"}
                      </td>
                      <td className="px-4 py-3 text-right">
                        <button
                          onClick={() => handleDelete(server.name)}
                          disabled={deleting === server.name}
                          className="p-1.5 rounded-lg text-[#556080] hover:text-[#ff4466] hover:bg-[#ff446615] transition-all"
                        >
                          {deleting === server.name ? (
                            <Loader2 className="h-3.5 w-3.5 animate-spin" />
                          ) : (
                            <Trash2 className="h-3.5 w-3.5" />
                          )}
                        </button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </>
      )}

      {/* Smithery Catalog */}
      {subTab === "smithery" && (
        <>
          <div className="flex gap-2">
            <div className="relative flex-1 max-w-md">
              <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-[#334060]" />
              <input
                type="text"
                value={smitheryQuery}
                onChange={(e) => setSmitheryQuery(e.target.value)}
                onKeyDown={handleSearchKey}
                placeholder="Search MCP servers..."
                className="input-electric w-full pl-10 pr-4 py-2.5 text-sm"
              />
            </div>
            <button
              onClick={handleSmitherySearch}
              disabled={smitheryLoading}
              className="btn-electric px-4 py-2.5 rounded-xl text-xs font-semibold"
            >
              {smitheryLoading ? (
                <Loader2 className="h-4 w-4 animate-spin" />
              ) : (
                "Search"
              )}
            </button>
          </div>

          <p className="text-[10px] text-[#334060]">
            Powered by{" "}
            <a
              href="https://smithery.ai"
              target="_blank"
              rel="noopener noreferrer"
              className="text-[#0080ff] hover:underline"
            >
              Smithery.ai
            </a>{" "}
            — {smitheryTotal.toLocaleString()} MCP servers
          </p>

          {smitheryLoading ? (
            <div className="flex items-center justify-center py-12">
              <Loader2 className="h-6 w-6 text-[#0080ff] animate-spin" />
            </div>
          ) : smitheryResults.length === 0 && smitherySearched ? (
            <div className="glass-card p-8 text-center">
              <Search className="h-8 w-8 text-[#1a1a3e] mx-auto mb-3" />
              <p className="text-[#556080] text-sm">
                No servers found. Try a different search.
              </p>
            </div>
          ) : (
            <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-4 stagger-children">
              {smitheryResults.map((server) => (
                <div
                  key={server.qualifiedName}
                  className="glass-card p-4 animate-slide-in-up"
                >
                  <div className="flex items-start justify-between gap-2">
                    <div className="min-w-0">
                      <div className="flex items-center gap-1.5">
                        <h4 className="text-sm font-semibold text-white truncate">
                          {server.displayName}
                        </h4>
                        {server.verified && (
                          <CheckCircle2 className="h-3.5 w-3.5 text-[#00e68a] flex-shrink-0" />
                        )}
                      </div>
                      <p className="text-xs text-[#556080] mt-1 line-clamp-2">
                        {server.description}
                      </p>
                    </div>
                    {server.homepage && (
                      <a
                        href={server.homepage}
                        target="_blank"
                        rel="noopener noreferrer"
                        className="flex-shrink-0 p-1.5 rounded-lg text-[#334060] hover:text-[#0080ff] transition-colors"
                      >
                        <ExternalLink className="h-3.5 w-3.5" />
                      </a>
                    )}
                  </div>
                  <div className="flex items-center gap-3 mt-2 text-[10px] text-[#334060]">
                    <span>{server.useCount.toLocaleString()} uses</span>
                    <span className="text-[#556080] font-mono truncate">
                      {server.qualifiedName}
                    </span>
                  </div>
                  <button
                    onClick={() => handleAddFromSmithery(server)}
                    className="mt-3 w-full flex items-center justify-center gap-1.5 btn-electric px-3 py-2 rounded-xl text-xs font-semibold"
                  >
                    <Plus className="h-3.5 w-3.5" />
                    Add
                  </button>
                </div>
              ))}
            </div>
          )}
        </>
      )}

      {/* Add Modal */}
      {showAddModal && (
        <McpAddModal
          onClose={() => setShowAddModal(false)}
          onSaved={handleModalSaved}
          prefill={addPrefill}
        />
      )}
    </div>
  );
}
