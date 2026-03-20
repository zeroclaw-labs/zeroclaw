import { useState, useEffect, useCallback } from "react";
import {
  Sparkles,
  Search,
  Trash2,
  Download,
  Loader2,
  ExternalLink,
  Tag,
  User,
} from "lucide-react";
import type { Skill } from "@/types/api";
import { getSkills, installSkill, deleteSkill } from "@/lib/api";
import { searchClawHubSkills, type ClawHubSkill } from "@/lib/catalogs";

type SubTab = "installed" | "clawhub";

export default function SkillsTab() {
  const [subTab, setSubTab] = useState<SubTab>("installed");
  const [skills, setSkills] = useState<Skill[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // ClawHub state
  const [hubQuery, setHubQuery] = useState("");
  const [hubResults, setHubResults] = useState<ClawHubSkill[]>([]);
  const [hubLoading, setHubLoading] = useState(false);
  const [hubSearched, setHubSearched] = useState(false);

  // Action state
  const [installing, setInstalling] = useState<string | null>(null);
  const [deleting, setDeleting] = useState<string | null>(null);

  const loadSkills = useCallback(() => {
    setLoading(true);
    getSkills()
      .then(setSkills)
      .catch((err) => setError(err.message))
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    loadSkills();
  }, [loadSkills]);

  const handleSearch = async () => {
    setHubLoading(true);
    setHubSearched(true);
    try {
      const result = await searchClawHubSkills(hubQuery);
      setHubResults(result.skills);
    } catch {
      setHubResults([]);
    } finally {
      setHubLoading(false);
    }
  };

  const handleSearchKey = (e: React.KeyboardEvent) => {
    if (e.key === "Enter") handleSearch();
  };

  const handleInstall = async (source: string, label: string) => {
    setInstalling(label);
    try {
      await installSkill(source);
      loadSkills();
      setSubTab("installed");
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "Install failed");
    } finally {
      setInstalling(null);
    }
  };

  const handleDelete = async (name: string) => {
    setDeleting(name);
    try {
      await deleteSkill(name);
      setSkills((prev) => prev.filter((s) => s.name !== name));
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "Delete failed");
    } finally {
      setDeleting(null);
    }
  };

  return (
    <div className="space-y-4">
      {/* Sub-tabs */}
      <div className="flex items-center gap-2">
        {(["installed", "clawhub"] as SubTab[]).map((tab) => (
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
            {tab === "installed" ? `Installed (${skills.length})` : "ClawHub"}
          </button>
        ))}
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

      {/* Installed Skills */}
      {subTab === "installed" && (
        <>
          {loading ? (
            <div className="flex items-center justify-center py-12">
              <Loader2 className="h-6 w-6 text-[#0080ff] animate-spin" />
            </div>
          ) : skills.length === 0 ? (
            <div className="glass-card p-8 text-center">
              <Sparkles className="h-10 w-10 text-[#1a1a3e] mx-auto mb-3" />
              <p className="text-[#556080] mb-2">No skills installed</p>
              <button
                onClick={() => setSubTab("clawhub")}
                className="text-xs text-[#0080ff] hover:underline"
              >
                Browse ClawHub catalog
              </button>
            </div>
          ) : (
            <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-4 stagger-children">
              {skills.map((skill) => (
                <div
                  key={skill.name}
                  className="glass-card p-4 animate-slide-in-up"
                >
                  <div className="flex items-start justify-between gap-2">
                    <div className="min-w-0">
                      <h4 className="text-sm font-semibold text-white truncate">
                        {skill.name}
                      </h4>
                      <p className="text-xs text-[#556080] mt-1 line-clamp-2">
                        {skill.description}
                      </p>
                    </div>
                    <button
                      onClick={() => handleDelete(skill.name)}
                      disabled={deleting === skill.name}
                      className="flex-shrink-0 p-1.5 rounded-lg text-[#556080] hover:text-[#ff4466] hover:bg-[#ff446615] transition-all"
                    >
                      {deleting === skill.name ? (
                        <Loader2 className="h-3.5 w-3.5 animate-spin" />
                      ) : (
                        <Trash2 className="h-3.5 w-3.5" />
                      )}
                    </button>
                  </div>
                  <div className="flex items-center gap-3 mt-3 text-[10px] text-[#334060]">
                    {skill.version && <span>v{skill.version}</span>}
                    {skill.author && (
                      <span className="flex items-center gap-0.5">
                        <User className="h-2.5 w-2.5" />
                        {skill.author}
                      </span>
                    )}
                  </div>
                  {skill.tags.length > 0 && (
                    <div className="flex flex-wrap gap-1 mt-2">
                      {skill.tags.slice(0, 4).map((tag) => (
                        <span
                          key={tag}
                          className="inline-flex items-center gap-0.5 px-1.5 py-0.5 rounded text-[9px] font-medium text-[#8892a8] border border-[#1a1a3e]"
                        >
                          <Tag className="h-2 w-2" />
                          {tag}
                        </span>
                      ))}
                    </div>
                  )}
                </div>
              ))}
            </div>
          )}
        </>
      )}

      {/* ClawHub Catalog */}
      {subTab === "clawhub" && (
        <>
          <div className="flex gap-2">
            <div className="relative flex-1 max-w-md">
              <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-[#334060]" />
              <input
                type="text"
                value={hubQuery}
                onChange={(e) => setHubQuery(e.target.value)}
                onKeyDown={handleSearchKey}
                placeholder="Search ClawHub skills..."
                className="input-electric w-full pl-10 pr-4 py-2.5 text-sm"
              />
            </div>
            <button
              onClick={handleSearch}
              disabled={hubLoading}
              className="btn-electric px-4 py-2.5 rounded-xl text-xs font-semibold"
            >
              {hubLoading ? (
                <Loader2 className="h-4 w-4 animate-spin" />
              ) : (
                "Search"
              )}
            </button>
          </div>

          <p className="text-[10px] text-[#334060]">
            Powered by{" "}
            <a
              href="https://clawhub.ai"
              target="_blank"
              rel="noopener noreferrer"
              className="text-[#0080ff] hover:underline"
            >
              ClawHub.ai
            </a>{" "}
            — 3,200+ agent skills
          </p>

          {hubLoading ? (
            <div className="flex items-center justify-center py-12">
              <Loader2 className="h-6 w-6 text-[#0080ff] animate-spin" />
            </div>
          ) : hubResults.length === 0 && hubSearched ? (
            <div className="glass-card p-8 text-center">
              <Search className="h-8 w-8 text-[#1a1a3e] mx-auto mb-3" />
              <p className="text-[#556080] text-sm">
                No skills found. Try a different search.
              </p>
            </div>
          ) : (
            <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-4 stagger-children">
              {hubResults.map((skill) => (
                <div
                  key={skill.slug}
                  className="glass-card p-4 animate-slide-in-up"
                >
                  <div className="flex items-start justify-between gap-2">
                    <div className="min-w-0">
                      <h4 className="text-sm font-semibold text-white truncate">
                        {skill.name || skill.slug}
                      </h4>
                      <p className="text-xs text-[#556080] mt-1 line-clamp-2">
                        {skill.description}
                      </p>
                    </div>
                    {skill.sourceUrl && (
                      <a
                        href={skill.sourceUrl}
                        target="_blank"
                        rel="noopener noreferrer"
                        className="flex-shrink-0 p-1.5 rounded-lg text-[#334060] hover:text-[#0080ff] transition-colors"
                      >
                        <ExternalLink className="h-3.5 w-3.5" />
                      </a>
                    )}
                  </div>
                  <div className="flex items-center gap-3 mt-2 text-[10px] text-[#334060]">
                    {skill.author && <span>by {skill.author}</span>}
                    {skill.installs != null && (
                      <span>{skill.installs} installs</span>
                    )}
                  </div>
                  <button
                    onClick={() =>
                      handleInstall(
                        skill.sourceUrl ||
                          `https://clawhub.ai/skills/${skill.slug}`,
                        skill.slug,
                      )
                    }
                    disabled={installing === skill.slug}
                    className="mt-3 w-full flex items-center justify-center gap-1.5 btn-electric px-3 py-2 rounded-xl text-xs font-semibold"
                  >
                    {installing === skill.slug ? (
                      <Loader2 className="h-3.5 w-3.5 animate-spin" />
                    ) : (
                      <Download className="h-3.5 w-3.5" />
                    )}
                    Install
                  </button>
                </div>
              ))}
            </div>
          )}
        </>
      )}
    </div>
  );
}
