import { useState, useEffect } from "react";
import {
  Share2,
  Search,
  Flame,
  Plus,
  Trash2,
  X,
  Link,
  Database,
  CheckCircle,
  AlertTriangle,
  DollarSign,
} from "lucide-react";
import type {
  GraphStats,
  GraphNode,
  GraphHotNode,
  GraphBudget,
} from "@/types/api";
import {
  getGraphStats,
  getGraphNodes,
  getHotNodes,
  createGraphConcept,
  createGraphRelation,
  deleteGraphNode,
  getGraphBudget,
  searchGraph,
} from "@/lib/api";
import { t } from "@/lib/i18n";

function truncate(text: string, max: number): string {
  if (text.length <= max) return text;
  return text.slice(0, max) + "...";
}

function formatDate(iso: string): string {
  return new Date(iso).toLocaleString();
}

interface GraphTabProps {
  active: boolean;
}

export default function GraphTab({ active }: GraphTabProps) {
  // Core data
  const [stats, setStats] = useState<GraphStats | null>(null);
  const [budget, setBudget] = useState<GraphBudget | null>(null);
  const [nodes, setNodes] = useState<GraphNode[]>([]);
  const [hotNodes, setHotNodes] = useState<GraphHotNode[]>([]);

  // Loading / error
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // Search
  const [searchQuery, setSearchQuery] = useState("");
  const [searchResults, setSearchResults] = useState<GraphNode[] | null>(null);
  const [searchLoading, setSearchLoading] = useState(false);

  // Modals
  const [showConceptModal, setShowConceptModal] = useState(false);
  const [showRelationModal, setShowRelationModal] = useState(false);

  // Concept form
  const [conceptName, setConceptName] = useState("");
  const [conceptDesc, setConceptDesc] = useState("");
  const [conceptCat, setConceptCat] = useState("");
  const [conceptError, setConceptError] = useState<string | null>(null);
  const [conceptSubmitting, setConceptSubmitting] = useState(false);

  // Relation form
  const [relFrom, setRelFrom] = useState("");
  const [relTo, setRelTo] = useState("");
  const [relType, setRelType] = useState("");
  const [relWeight, setRelWeight] = useState("");
  const [relError, setRelError] = useState<string | null>(null);
  const [relSubmitting, setRelSubmitting] = useState(false);

  // Delete confirm
  const [confirmDeleteId, setConfirmDeleteId] = useState<string | null>(null);

  const fetchAll = () => {
    setLoading(true);
    Promise.all([
      getGraphStats(),
      getGraphNodes(),
      getHotNodes(),
      getGraphBudget(),
    ])
      .then(([s, n, h, b]) => {
        setStats(s);
        setNodes(n.nodes);
        setHotNodes(h.nodes);
        setBudget(b);
      })
      .catch((err) => setError(err.message))
      .finally(() => setLoading(false));
  };

  useEffect(() => {
    if (active) fetchAll();
  }, [active]);

  const handleSearch = async () => {
    if (!searchQuery.trim()) return;
    setSearchLoading(true);
    try {
      const result = await searchGraph(searchQuery.trim());
      setSearchResults(result.results);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "Search failed");
    } finally {
      setSearchLoading(false);
    }
  };

  const handleCreateConcept = async () => {
    if (!conceptName.trim() || !conceptDesc.trim()) {
      setConceptError("Name and description are required.");
      return;
    }
    setConceptSubmitting(true);
    setConceptError(null);
    try {
      await createGraphConcept({
        name: conceptName.trim(),
        description: conceptDesc.trim(),
        category: conceptCat.trim() || undefined,
      });
      setShowConceptModal(false);
      setConceptName("");
      setConceptDesc("");
      setConceptCat("");
      fetchAll();
    } catch (err: unknown) {
      setConceptError(
        err instanceof Error ? err.message : "Failed to create concept",
      );
    } finally {
      setConceptSubmitting(false);
    }
  };

  const handleCreateRelation = async () => {
    if (!relFrom.trim() || !relTo.trim()) {
      setRelError("From and To node IDs are required.");
      return;
    }
    const weight = relWeight ? parseFloat(relWeight) : undefined;
    if (weight !== undefined && (isNaN(weight) || weight < 0 || weight > 1)) {
      setRelError("Weight must be a number between 0 and 1.");
      return;
    }
    setRelSubmitting(true);
    setRelError(null);
    try {
      await createGraphRelation({
        from_id: relFrom.trim(),
        to_id: relTo.trim(),
        relation_type: relType.trim() || undefined,
        weight,
      });
      setShowRelationModal(false);
      setRelFrom("");
      setRelTo("");
      setRelType("");
      setRelWeight("");
    } catch (err: unknown) {
      setRelError(
        err instanceof Error ? err.message : "Failed to create relation",
      );
    } finally {
      setRelSubmitting(false);
    }
  };

  const handleDeleteNode = async (id: string) => {
    try {
      await deleteGraphNode(id);
      setNodes((prev) => prev.filter((n) => n.id !== id));
      setHotNodes((prev) => prev.filter((n) => n.id !== id));
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "Failed to delete node");
    } finally {
      setConfirmDeleteId(null);
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center h-32">
        <div className="h-8 w-8 border-2 border-[#0080ff30] border-t-[#0080ff] rounded-full animate-spin" />
      </div>
    );
  }

  if (error && !stats) {
    return (
      <div className="rounded-xl bg-[#ff446615] border border-[#ff446630] p-4 text-[#ff6680] animate-fade-in">
        <div className="flex items-center gap-2">
          <AlertTriangle className="h-4 w-4" />
          <span>
            {t("graph.not_active")}: {error}
          </span>
        </div>
      </div>
    );
  }

  return (
    <div className="space-y-6">
      {/* A: Stats Cards */}
      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-4 stagger-children">
        {[
          {
            icon: Database,
            color: "#0080ff",
            bg: "#0080ff15",
            label: t("graph.stats_nodes"),
            value: stats?.total_nodes.toLocaleString() ?? "0",
          },
          {
            icon: Share2,
            color: "#a855f7",
            bg: "#a855f715",
            label: t("graph.stats_backend"),
            value: stats?.backend ?? "—",
          },
          {
            icon: stats?.healthy ? CheckCircle : AlertTriangle,
            color: stats?.healthy ? "#00e68a" : "#ff4466",
            bg: stats?.healthy ? "#00e68a15" : "#ff446615",
            label: t("graph.stats_healthy"),
            value: stats?.healthy
              ? t("graph.healthy_yes")
              : t("graph.healthy_no"),
          },
          {
            icon: DollarSign,
            color: "#ff8800",
            bg: "#ff880015",
            label: t("graph.stats_daily_cost"),
            value: `$${budget?.daily_cost_usd.toFixed(4) ?? "0.0000"}`,
            sub: `${budget?.total_tokens.toLocaleString() ?? 0} tokens`,
          },
        ].map((card, idx) => (
          <div
            key={idx}
            className="glass-card p-5 animate-slide-in-up"
            style={{ animationDelay: `${idx * 60}ms` }}
          >
            <div className="flex items-center gap-3 mb-3">
              <div className="p-2 rounded-xl" style={{ background: card.bg }}>
                <card.icon className="h-5 w-5" style={{ color: card.color }} />
              </div>
              <span className="text-xs text-[#556080] uppercase tracking-wider">
                {card.label}
              </span>
            </div>
            <p className="text-lg font-semibold text-white">{card.value}</p>
            {"sub" in card && (
              <p className="text-sm text-[#556080] mt-0.5">{card.sub}</p>
            )}
          </div>
        ))}
      </div>

      {/* B: Search */}
      <div className="flex gap-3">
        <div className="relative flex-1">
          <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-[#334060]" />
          <input
            type="text"
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") handleSearch();
            }}
            placeholder={t("graph.search_placeholder")}
            className="input-electric w-full pl-10 pr-4 py-2.5 text-sm"
          />
        </div>
        <button
          onClick={handleSearch}
          disabled={searchLoading || !searchQuery.trim()}
          className="btn-electric px-4 py-2.5 text-sm flex items-center gap-2"
        >
          {searchLoading ? (
            <span className="h-4 w-4 border-2 border-white/30 border-t-white rounded-full animate-spin" />
          ) : (
            <Search className="h-4 w-4" />
          )}
          Search
        </button>
        {searchResults !== null && (
          <button
            onClick={() => {
              setSearchResults(null);
              setSearchQuery("");
            }}
            className="px-3 py-2.5 text-sm text-[#556080] hover:text-white border border-[#1a1a3e] rounded-xl hover:bg-[#0080ff08] transition-all duration-300"
          >
            <X className="h-4 w-4" />
          </button>
        )}
      </div>

      {/* Search Results */}
      {searchResults !== null && (
        <div className="glass-card overflow-x-auto animate-fade-in">
          {searchResults.length === 0 ? (
            <div className="p-8 text-center text-[#556080] text-sm">
              No results found.
            </div>
          ) : (
            <table className="table-electric">
              <thead>
                <tr>
                  <th className="text-left">Key</th>
                  <th className="text-left">Content</th>
                  <th className="text-left">Category</th>
                  <th className="text-right">Score</th>
                </tr>
              </thead>
              <tbody>
                {searchResults.map((node) => (
                  <tr key={node.id}>
                    <td className="px-4 py-3 text-white font-medium font-mono text-xs">
                      {node.key}
                    </td>
                    <td className="px-4 py-3 text-[#8892a8] max-w-[300px] text-sm">
                      <span title={node.content}>
                        {truncate(node.content, 80)}
                      </span>
                    </td>
                    <td className="px-4 py-3">
                      <span
                        className="inline-flex items-center px-2.5 py-0.5 rounded-full text-[10px] font-semibold capitalize border border-[#1a1a3e] text-[#8892a8]"
                        style={{ background: "rgba(0,128,255,0.06)" }}
                      >
                        {node.category}
                      </span>
                    </td>
                    <td className="px-4 py-3 text-right font-mono text-xs text-[#556080]">
                      {node.score !== null ? node.score.toFixed(3) : "—"}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
      )}

      {/* Non-fatal error */}
      {error && stats && (
        <div className="rounded-xl bg-[#ff446615] border border-[#ff446630] p-3 text-sm text-[#ff6680] animate-fade-in">
          {error}
        </div>
      )}

      {/* C: Hot Nodes */}
      <div
        className="glass-card overflow-hidden animate-slide-in-up"
        style={{ animationDelay: "100ms" }}
      >
        <div className="px-5 py-4 border-b border-[#1a1a3e] flex items-center gap-2">
          <Flame className="h-5 w-5 text-[#ff8800]" />
          <h3 className="text-sm font-semibold text-white uppercase tracking-wider">
            {t("graph.hot_nodes")}
          </h3>
          <span className="ml-auto text-xs text-[#556080]">
            {hotNodes.length}
          </span>
        </div>
        {hotNodes.length === 0 ? (
          <div className="p-8 text-center text-[#334060] text-sm">
            {t("graph.empty_hot")}
          </div>
        ) : (
          <div className="p-4 grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-3 stagger-children">
            {hotNodes.map((node) => (
              <div
                key={node.id}
                className="rounded-xl p-3 border border-[#ff880020] transition-all duration-300 hover:border-[#ff880050] hover:translate-y-[-1px]"
                style={{ background: "rgba(255,136,0,0.04)" }}
              >
                <div className="flex items-center justify-between mb-2">
                  <span
                    className="inline-flex items-center px-2 py-0.5 rounded-full text-[10px] font-semibold border border-[#1a1a3e] text-[#8892a8]"
                    style={{ background: "rgba(0,128,255,0.06)" }}
                  >
                    {node.category}
                  </span>
                  <div className="flex items-center gap-1">
                    <Flame className="h-3 w-3 text-[#ff8800]" />
                    <span className="text-xs font-mono text-[#ff8800]">
                      {node.heat.toFixed(2)}
                    </span>
                  </div>
                </div>
                <p className="text-sm font-semibold text-white truncate">
                  {node.key}
                </p>
                <p className="text-xs text-[#556080] mt-1 line-clamp-2">
                  {node.content}
                </p>
              </div>
            ))}
          </div>
        )}
      </div>

      {/* D: Action Bar */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Share2 className="h-5 w-5 text-[#0080ff]" />
          <h3 className="text-sm font-semibold text-white uppercase tracking-wider">
            {t("graph.all_nodes")} ({nodes.length})
          </h3>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={() => setShowRelationModal(true)}
            className="flex items-center gap-2 px-4 py-2 text-sm font-medium text-[#8892a8] hover:text-white border border-[#1a1a3e] rounded-xl hover:bg-[#0080ff08] transition-all duration-300"
          >
            <Link className="h-4 w-4" />
            {t("graph.add_relation")}
          </button>
          <button
            onClick={() => setShowConceptModal(true)}
            className="btn-electric flex items-center gap-2 text-sm px-4 py-2"
          >
            <Plus className="h-4 w-4" />
            {t("graph.add_concept")}
          </button>
        </div>
      </div>

      {/* E: All Nodes Table */}
      {nodes.length === 0 ? (
        <div className="glass-card p-8 text-center">
          <Share2 className="h-10 w-10 text-[#1a1a3e] mx-auto mb-3" />
          <p className="text-[#556080]">{t("graph.empty_nodes")}</p>
        </div>
      ) : (
        <div className="glass-card overflow-x-auto">
          <table className="table-electric">
            <thead>
              <tr>
                <th className="text-left">Key</th>
                <th className="text-left">Content</th>
                <th className="text-left">Category</th>
                <th className="text-right">Score</th>
                <th className="text-left">Timestamp</th>
                <th className="text-right">Actions</th>
              </tr>
            </thead>
            <tbody>
              {nodes.map((node) => (
                <tr key={node.id}>
                  <td className="px-4 py-3 text-white font-medium font-mono text-xs max-w-[140px] truncate">
                    {node.key}
                  </td>
                  <td className="px-4 py-3 text-[#8892a8] max-w-[280px] text-sm">
                    <span title={node.content}>
                      {truncate(node.content, 80)}
                    </span>
                  </td>
                  <td className="px-4 py-3">
                    <span
                      className="inline-flex items-center px-2.5 py-0.5 rounded-full text-[10px] font-semibold capitalize border border-[#1a1a3e] text-[#8892a8]"
                      style={{ background: "rgba(0,128,255,0.06)" }}
                    >
                      {node.category}
                    </span>
                  </td>
                  <td className="px-4 py-3 text-right font-mono text-xs text-[#556080]">
                    {node.score !== null ? node.score.toFixed(3) : "—"}
                  </td>
                  <td className="px-4 py-3 text-[#556080] text-xs whitespace-nowrap">
                    {formatDate(node.timestamp)}
                  </td>
                  <td className="px-4 py-3 text-right">
                    {confirmDeleteId === node.id ? (
                      <div className="flex items-center justify-end gap-2 animate-fade-in">
                        <span className="text-xs text-[#ff4466]">
                          {t("graph.confirm_delete")}
                        </span>
                        <button
                          onClick={() => handleDeleteNode(node.id)}
                          className="text-[#ff4466] hover:text-[#ff6680] text-xs font-medium"
                        >
                          Yes
                        </button>
                        <button
                          onClick={() => setConfirmDeleteId(null)}
                          className="text-[#556080] hover:text-white text-xs font-medium"
                        >
                          No
                        </button>
                      </div>
                    ) : (
                      <button
                        onClick={() => setConfirmDeleteId(node.id)}
                        className="text-[#334060] hover:text-[#ff4466] transition-all duration-300"
                      >
                        <Trash2 className="h-4 w-4" />
                      </button>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {/* F: Add Concept Modal */}
      {showConceptModal && (
        <div className="fixed inset-0 modal-backdrop flex items-center justify-center z-50">
          <div className="glass-card p-6 w-full max-w-md mx-4 animate-fade-in-scale">
            <div className="flex items-center justify-between mb-4">
              <h3 className="text-lg font-semibold text-white">
                {t("graph.add_concept")}
              </h3>
              <button
                onClick={() => {
                  setShowConceptModal(false);
                  setConceptError(null);
                }}
                className="text-[#556080] hover:text-white transition-colors duration-300"
              >
                <X className="h-5 w-5" />
              </button>
            </div>

            {conceptError && (
              <div className="mb-4 rounded-xl bg-[#ff446615] border border-[#ff446630] p-3 text-sm text-[#ff6680] animate-fade-in">
                {conceptError}
              </div>
            )}

            <div className="space-y-4">
              <div>
                <label className="block text-xs font-semibold text-[#8892a8] mb-1.5 uppercase tracking-wider">
                  {t("graph.concept_name")}{" "}
                  <span className="text-[#ff4466]">*</span>
                </label>
                <input
                  type="text"
                  value={conceptName}
                  onChange={(e) => setConceptName(e.target.value)}
                  placeholder="e.g. quantum_entanglement"
                  className="input-electric w-full px-3 py-2.5 text-sm"
                />
              </div>
              <div>
                <label className="block text-xs font-semibold text-[#8892a8] mb-1.5 uppercase tracking-wider">
                  {t("graph.concept_description")}{" "}
                  <span className="text-[#ff4466]">*</span>
                </label>
                <textarea
                  value={conceptDesc}
                  onChange={(e) => setConceptDesc(e.target.value)}
                  placeholder="Describe this concept..."
                  rows={3}
                  className="input-electric w-full px-3 py-2.5 text-sm resize-none"
                />
              </div>
              <div>
                <label className="block text-xs font-semibold text-[#8892a8] mb-1.5 uppercase tracking-wider">
                  {t("graph.concept_category")}
                </label>
                <input
                  type="text"
                  value={conceptCat}
                  onChange={(e) => setConceptCat(e.target.value)}
                  placeholder="e.g. physics, history, code"
                  className="input-electric w-full px-3 py-2.5 text-sm"
                />
              </div>
            </div>

            <div className="flex justify-end gap-3 mt-6">
              <button
                onClick={() => {
                  setShowConceptModal(false);
                  setConceptError(null);
                }}
                className="px-4 py-2 text-sm font-medium text-[#8892a8] hover:text-white border border-[#1a1a3e] rounded-xl hover:bg-[#0080ff08] transition-all duration-300"
              >
                Cancel
              </button>
              <button
                onClick={handleCreateConcept}
                disabled={conceptSubmitting}
                className="btn-electric px-4 py-2 text-sm font-medium"
              >
                {conceptSubmitting ? "Saving..." : "Save"}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* G: Add Relation Modal */}
      {showRelationModal && (
        <div className="fixed inset-0 modal-backdrop flex items-center justify-center z-50">
          <div className="glass-card p-6 w-full max-w-md mx-4 animate-fade-in-scale">
            <div className="flex items-center justify-between mb-4">
              <h3 className="text-lg font-semibold text-white">
                {t("graph.add_relation")}
              </h3>
              <button
                onClick={() => {
                  setShowRelationModal(false);
                  setRelError(null);
                }}
                className="text-[#556080] hover:text-white transition-colors duration-300"
              >
                <X className="h-5 w-5" />
              </button>
            </div>

            {relError && (
              <div className="mb-4 rounded-xl bg-[#ff446615] border border-[#ff446630] p-3 text-sm text-[#ff6680] animate-fade-in">
                {relError}
              </div>
            )}

            <div className="space-y-4">
              <div>
                <label className="block text-xs font-semibold text-[#8892a8] mb-1.5 uppercase tracking-wider">
                  {t("graph.relation_from")}{" "}
                  <span className="text-[#ff4466]">*</span>
                </label>
                <input
                  type="text"
                  value={relFrom}
                  onChange={(e) => setRelFrom(e.target.value)}
                  placeholder="source node ID"
                  className="input-electric w-full px-3 py-2.5 text-sm font-mono"
                />
              </div>
              <div>
                <label className="block text-xs font-semibold text-[#8892a8] mb-1.5 uppercase tracking-wider">
                  {t("graph.relation_to")}{" "}
                  <span className="text-[#ff4466]">*</span>
                </label>
                <input
                  type="text"
                  value={relTo}
                  onChange={(e) => setRelTo(e.target.value)}
                  placeholder="target node ID"
                  className="input-electric w-full px-3 py-2.5 text-sm font-mono"
                />
              </div>
              <div className="grid grid-cols-2 gap-3">
                <div>
                  <label className="block text-xs font-semibold text-[#8892a8] mb-1.5 uppercase tracking-wider">
                    {t("graph.relation_type")}
                  </label>
                  <input
                    type="text"
                    value={relType}
                    onChange={(e) => setRelType(e.target.value)}
                    placeholder="e.g. causes, part_of"
                    className="input-electric w-full px-3 py-2.5 text-sm"
                  />
                </div>
                <div>
                  <label className="block text-xs font-semibold text-[#8892a8] mb-1.5 uppercase tracking-wider">
                    {t("graph.relation_weight")}
                  </label>
                  <input
                    type="number"
                    min="0"
                    max="1"
                    step="0.1"
                    value={relWeight}
                    onChange={(e) => setRelWeight(e.target.value)}
                    placeholder="0.8"
                    className="input-electric w-full px-3 py-2.5 text-sm"
                  />
                </div>
              </div>
            </div>

            <div className="flex justify-end gap-3 mt-6">
              <button
                onClick={() => {
                  setShowRelationModal(false);
                  setRelError(null);
                }}
                className="px-4 py-2 text-sm font-medium text-[#8892a8] hover:text-white border border-[#1a1a3e] rounded-xl hover:bg-[#0080ff08] transition-all duration-300"
              >
                Cancel
              </button>
              <button
                onClick={handleCreateRelation}
                disabled={relSubmitting}
                className="btn-electric px-4 py-2 text-sm font-medium"
              >
                {relSubmitting ? "Saving..." : "Save"}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
