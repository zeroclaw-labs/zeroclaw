"use client";

import { useState, useCallback, useEffect } from "react";
import DomainNav from "@/components/DomainNav";
import AgentSidebar from "@/components/AgentSidebar";
import AgentInfoPanel from "@/components/AgentInfoPanel";
import ChatWidget from "@/components/ChatWidget";
import {
  getDomainById,
  getSubAgentById,
  getLLMById,
  getToolById,
  getChannelById,
} from "@/lib/domains";

const STORAGE_KEY_WORKSPACE = "zeroclaw_workspace_state";

interface WorkspaceState {
  selectedDomain: string | null;
  selectedSubAgent: string | null;
  selectedLLM: string | null;
  selectedTool: string | null;
  selectedChannel: string | null;
  sidebarCollapsed: boolean;
}

function loadWorkspaceState(): WorkspaceState {
  if (typeof window === "undefined") {
    return {
      selectedDomain: null,
      selectedSubAgent: null,
      selectedLLM: null,
      selectedTool: null,
      selectedChannel: "web",
      sidebarCollapsed: false,
    };
  }
  try {
    const stored = localStorage.getItem(STORAGE_KEY_WORKSPACE);
    if (stored) return JSON.parse(stored);
  } catch {
    // ignore
  }
  return {
    selectedDomain: null,
    selectedSubAgent: null,
    selectedLLM: null,
    selectedTool: null,
    selectedChannel: "web",
    sidebarCollapsed: false,
  };
}

function saveWorkspaceState(state: WorkspaceState): void {
  if (typeof window === "undefined") return;
  try {
    localStorage.setItem(STORAGE_KEY_WORKSPACE, JSON.stringify(state));
  } catch {
    // ignore
  }
}

export default function WorkspacePage() {
  const [state, setState] = useState<WorkspaceState>(loadWorkspaceState);
  const [showInfoPanel, setShowInfoPanel] = useState(false);
  const [showMobileSidebar, setShowMobileSidebar] = useState(false);

  useEffect(() => {
    saveWorkspaceState(state);
  }, [state]);

  const handleSelectSubAgent = useCallback((domainId: string, subAgentId: string) => {
    const subAgent = getSubAgentById(domainId, subAgentId);
    setState((prev: WorkspaceState) => ({
      ...prev,
      selectedDomain: domainId,
      selectedSubAgent: subAgentId,
      selectedLLM: subAgent?.recommendedLLMs[0] || prev.selectedLLM,
      selectedTool: subAgent?.recommendedTools[0] || prev.selectedTool,
    }));
    setShowMobileSidebar(false);
  }, []);

  const handleSelectLLM = useCallback((llmId: string) => {
    setState((prev: WorkspaceState) => ({ ...prev, selectedLLM: llmId }));
  }, []);

  const handleSelectTool = useCallback((toolId: string) => {
    setState((prev: WorkspaceState) => ({ ...prev, selectedTool: toolId }));
  }, []);

  const handleSelectChannel = useCallback((channelId: string) => {
    setState((prev: WorkspaceState) => ({ ...prev, selectedChannel: channelId }));
  }, []);

  const handleToggleCollapse = useCallback(() => {
    setState((prev: WorkspaceState) => ({ ...prev, sidebarCollapsed: !prev.sidebarCollapsed }));
  }, []);

  // Build context info for display
  const domain = state.selectedDomain ? getDomainById(state.selectedDomain) : null;
  const subAgent = state.selectedDomain && state.selectedSubAgent
    ? getSubAgentById(state.selectedDomain, state.selectedSubAgent)
    : null;
  const llm = state.selectedLLM ? getLLMById(state.selectedLLM) : null;
  const tool = state.selectedTool ? getToolById(state.selectedTool) : null;
  const channel = state.selectedChannel ? getChannelById(state.selectedChannel) : null;

  return (
    <div className="flex h-screen flex-col pt-[65px]">
      {/* Top: Domain Navigation */}
      <DomainNav
        selectedDomain={state.selectedDomain}
        selectedSubAgent={state.selectedSubAgent}
        onSelectSubAgent={handleSelectSubAgent}
      />

      {/* Main area: Sidebar + Chat + InfoPanel */}
      <div className="flex flex-1 overflow-hidden">
        {/* Mobile sidebar toggle */}
        <button
          onClick={() => setShowMobileSidebar(true)}
          className="fixed bottom-4 left-4 z-40 flex h-12 w-12 items-center justify-center rounded-full bg-primary-500 text-white shadow-lg md:hidden active:scale-95 transition-all"
          aria-label="Open sidebar"
        >
          <svg className="h-5 w-5" fill="none" viewBox="0 0 24 24" strokeWidth={2} stroke="currentColor">
            <path strokeLinecap="round" strokeLinejoin="round" d="M10.5 6h9.75M10.5 6a1.5 1.5 0 11-3 0m3 0a1.5 1.5 0 10-3 0M3.75 6H7.5m3 12h9.75m-9.75 0a1.5 1.5 0 01-3 0m3 0a1.5 1.5 0 00-3 0m-3.75 0H7.5m9-6h3.75m-3.75 0a1.5 1.5 0 01-3 0m3 0a1.5 1.5 0 00-3 0m-9.75 0h9.75" />
          </svg>
        </button>

        {/* Mobile sidebar overlay */}
        {showMobileSidebar && (
          <div className="fixed inset-0 z-50 md:hidden">
            <div
              className="absolute inset-0 bg-dark-950/80 backdrop-blur-sm"
              onClick={() => setShowMobileSidebar(false)}
            />
            <div className="relative h-full w-72 animate-slide-in-left">
              <AgentSidebar
                selectedDomain={state.selectedDomain}
                selectedSubAgent={state.selectedSubAgent}
                selectedLLM={state.selectedLLM}
                selectedTool={state.selectedTool}
                selectedChannel={state.selectedChannel}
                onSelectLLM={handleSelectLLM}
                onSelectTool={handleSelectTool}
                onSelectChannel={handleSelectChannel}
                collapsed={false}
                onToggleCollapse={() => setShowMobileSidebar(false)}
              />
            </div>
          </div>
        )}

        {/* Desktop: Left Sidebar */}
        <div className="hidden md:block">
          <AgentSidebar
            selectedDomain={state.selectedDomain}
            selectedSubAgent={state.selectedSubAgent}
            selectedLLM={state.selectedLLM}
            selectedTool={state.selectedTool}
            selectedChannel={state.selectedChannel}
            onSelectLLM={handleSelectLLM}
            onSelectTool={handleSelectTool}
            onSelectChannel={handleSelectChannel}
            collapsed={state.sidebarCollapsed}
            onToggleCollapse={handleToggleCollapse}
          />
        </div>

        {/* Center: Chat area */}
        <div className="flex flex-1 flex-col overflow-hidden">
          {/* Active config bar */}
          <div className="flex items-center justify-between border-b border-dark-800/50 px-4 py-2 bg-dark-950/50">
            <div className="flex items-center gap-2 overflow-x-auto flex-1 min-w-0">
              {subAgent ? (
                <>
                  <span className="text-[10px] text-dark-500 uppercase tracking-wider flex-shrink-0">Active:</span>
                  <div className="flex items-center gap-1.5 flex-shrink-0">
                    <span className="text-xs font-medium text-dark-200">{subAgent.nameKo}</span>
                  </div>
                  <span className="text-dark-700 flex-shrink-0">|</span>
                  {llm && (
                    <div className="flex items-center gap-1 flex-shrink-0">
                      <svg className="h-3 w-3 text-primary-400" fill="none" viewBox="0 0 24 24" strokeWidth={1.5} stroke="currentColor">
                        <path strokeLinecap="round" strokeLinejoin="round" d="M9.813 15.904L9 18.75l-.813-2.846a4.5 4.5 0 00-3.09-3.09L2.25 12l2.846-.813a4.5 4.5 0 003.09-3.09L9 5.25l.813 2.846a4.5 4.5 0 003.09 3.09L15.75 12l-2.846.813a4.5 4.5 0 00-3.09 3.09z" />
                      </svg>
                      <span className="text-xs text-primary-300">{llm.name}</span>
                    </div>
                  )}
                  {tool && (
                    <>
                      <span className="text-dark-700 flex-shrink-0">|</span>
                      <div className="flex items-center gap-1 flex-shrink-0">
                        <svg className="h-3 w-3 text-accent-400" fill="none" viewBox="0 0 24 24" strokeWidth={1.5} stroke="currentColor">
                          <path strokeLinecap="round" strokeLinejoin="round" d="M11.42 15.17l-5.1 5.1a2.121 2.121 0 11-3-3l5.1-5.1m0 0L15.17 4.93a2.121 2.121 0 013 3l-7.75 7.24z" />
                        </svg>
                        <span className="text-xs text-accent-300">{tool.name}</span>
                      </div>
                    </>
                  )}
                  {channel && (
                    <>
                      <span className="text-dark-700 flex-shrink-0">|</span>
                      <div className="flex items-center gap-1 flex-shrink-0">
                        <svg className="h-3 w-3 text-secondary-400" fill="none" viewBox="0 0 24 24" strokeWidth={1.5} stroke="currentColor">
                          <path strokeLinecap="round" strokeLinejoin="round" d="M8.625 12a.375.375 0 11-.75 0 .375.375 0 01.75 0zm0 0H8.25m4.125 0a.375.375 0 11-.75 0 .375.375 0 01.75 0zm0 0H12m4.125 0a.375.375 0 11-.75 0 .375.375 0 01.75 0zm0 0h-.375M21 12c0 4.556-4.03 8.25-9 8.25a9.764 9.764 0 01-2.555-.337A5.972 5.972 0 015.41 20.97a5.969 5.969 0 01-.474-.065 4.48 4.48 0 00.978-2.025c.09-.457-.133-.901-.467-1.226C3.93 16.178 3 14.189 3 12c0-4.556 4.03-8.25 9-8.25s9 3.694 9 8.25z" />
                        </svg>
                        <span className="text-xs text-secondary-300">{channel.name}</span>
                      </div>
                    </>
                  )}
                </>
              ) : (
                <div className="flex items-center gap-2">
                  <svg className="h-3.5 w-3.5 text-dark-500" fill="none" viewBox="0 0 24 24" strokeWidth={1.5} stroke="currentColor">
                    <path strokeLinecap="round" strokeLinejoin="round" d="M11.25 11.25l.041-.02a.75.75 0 011.063.852l-.708 2.836a.75.75 0 001.063.853l.041-.021M21 12a9 9 0 11-18 0 9 9 0 0118 0zm-9-3.75h.008v.008H12V8.25z" />
                  </svg>
                  <span className="text-xs text-dark-500">
                    상단 메뉴에서 도메인과 에이전트를 선택하세요
                  </span>
                </div>
              )}
            </div>

            {/* Info panel toggle */}
            {subAgent && (
              <button
                onClick={() => setShowInfoPanel(!showInfoPanel)}
                className={`flex-shrink-0 ml-2 flex h-7 w-7 items-center justify-center rounded-md transition-all ${
                  showInfoPanel
                    ? "bg-primary-500/10 text-primary-400"
                    : "text-dark-500 hover:bg-dark-800 hover:text-dark-300"
                }`}
                title="에이전트 상세 정보"
              >
                <svg className="h-4 w-4" fill="none" viewBox="0 0 24 24" strokeWidth={1.5} stroke="currentColor">
                  <path strokeLinecap="round" strokeLinejoin="round" d="M11.25 11.25l.041-.02a.75.75 0 011.063.852l-.708 2.836a.75.75 0 001.063.853l.041-.021M21 12a9 9 0 11-18 0 9 9 0 0118 0zm-9-3.75h.008v.008H12V8.25z" />
                </svg>
              </button>
            )}
          </div>

          {/* Chat Widget or Welcome */}
          {subAgent ? (
            <ChatWidget className="flex-1 relative" />
          ) : (
            <WelcomeScreen />
          )}
        </div>

        {/* Right: Agent Info Panel */}
        {showInfoPanel && domain && subAgent && (
          <div className="hidden lg:block">
            <AgentInfoPanel
              domain={domain}
              subAgent={subAgent}
              selectedLLM={state.selectedLLM}
              selectedTool={state.selectedTool}
              onSelectLLM={handleSelectLLM}
              onSelectTool={handleSelectTool}
              onClose={() => setShowInfoPanel(false)}
            />
          </div>
        )}
      </div>
    </div>
  );
}

function WelcomeScreen() {
  return (
    <div className="flex-1 flex items-center justify-center">
      <div className="text-center max-w-2xl px-8">
        <div className="flex h-20 w-20 mx-auto items-center justify-center rounded-2xl bg-primary-500/10 border border-primary-500/20 mb-6">
          <svg className="h-10 w-10 text-primary-400" fill="none" viewBox="0 0 24 24" strokeWidth={1} stroke="currentColor">
            <path strokeLinecap="round" strokeLinejoin="round" d="M9.813 15.904L9 18.75l-.813-2.846a4.5 4.5 0 00-3.09-3.09L2.25 12l2.846-.813a4.5 4.5 0 003.09-3.09L9 5.25l.813 2.846a4.5 4.5 0 003.09 3.09L15.75 12l-2.846.813a4.5 4.5 0 00-3.09 3.09zM18.259 8.715L18 9.75l-.259-1.035a3.375 3.375 0 00-2.455-2.456L14.25 6l1.036-.259a3.375 3.375 0 002.455-2.456L18 2.25l.259 1.035a3.375 3.375 0 002.455 2.456L21.75 6l-1.036.259a3.375 3.375 0 00-2.455 2.456z" />
          </svg>
        </div>

        <h2 className="text-2xl font-bold text-dark-100 mb-2">
          MoA Agent Workspace
        </h2>
        <p className="text-xs text-dark-500 mb-6">MoA - Domain Specialist Agent Orchestrator</p>
        <p className="text-sm text-dark-400 mb-8 leading-relaxed">
          상단 카테고리에서 도메인을 선택하고, 원하는 서브 에이전트를 클릭하세요.
          <br />
          좌측 사이드바에서 LLM 모델, 도구(API), 채널을 선택할 수 있습니다.
        </p>

        {/* Architecture overview */}
        <div className="glass-card rounded-2xl p-6 mb-6 text-left">
          <div className="text-[10px] text-dark-500 uppercase tracking-wider mb-3 text-center">Architecture</div>

          {/* ZeroClaw Orchestrator */}
          <div className="rounded-lg border border-primary-500/30 bg-primary-500/5 p-3 mb-3">
            <div className="text-center">
              <span className="text-xs font-bold text-primary-300">MoA Orchestrator</span>
              <p className="text-[10px] text-dark-500 mt-0.5">목표 설정 / 리소스 배분 / 우선순위 결정</p>
            </div>
          </div>

          {/* Arrow */}
          <div className="flex justify-center mb-3">
            <svg className="h-4 w-4 text-dark-600" fill="none" viewBox="0 0 24 24" strokeWidth={2} stroke="currentColor">
              <path strokeLinecap="round" strokeLinejoin="round" d="M19.5 13.5L12 21m0 0l-7.5-7.5M12 21V3" />
            </svg>
          </div>

          {/* Domain Sub-Agents */}
          <div className="grid grid-cols-4 gap-2 mb-3">
            {[
              { label: "웹/쇼핑", color: "primary" },
              { label: "일상/비서", color: "secondary" },
              { label: "문서", color: "accent" },
              { label: "코딩", color: "primary" },
              { label: "통역", color: "secondary" },
              { label: "음악", color: "accent" },
              { label: "이미지", color: "primary" },
              { label: "비디오", color: "secondary" },
            ].map((d) => (
              <div
                key={d.label}
                className={`rounded-md border px-2 py-1.5 text-center text-[10px] font-medium ${
                  d.color === "primary" ? "border-primary-500/20 bg-primary-500/5 text-primary-300"
                    : d.color === "secondary" ? "border-secondary-500/20 bg-secondary-500/5 text-secondary-300"
                      : "border-accent-500/20 bg-accent-500/5 text-accent-300"
                }`}
              >
                {d.label}
              </div>
            ))}
          </div>

          {/* Arrow */}
          <div className="flex justify-center mb-3">
            <svg className="h-4 w-4 text-dark-600" fill="none" viewBox="0 0 24 24" strokeWidth={2} stroke="currentColor">
              <path strokeLinecap="round" strokeLinejoin="round" d="M19.5 13.5L12 21m0 0l-7.5-7.5M12 21V3" />
            </svg>
          </div>

          {/* Tools layer */}
          <div className="grid grid-cols-3 gap-2">
            <div className="rounded-md border border-dark-600 bg-dark-800/50 px-2 py-1.5 text-center">
              <div className="text-[9px] text-dark-500">LLM</div>
              <div className="text-[10px] text-dark-300 font-medium">15+ Models</div>
            </div>
            <div className="rounded-md border border-dark-600 bg-dark-800/50 px-2 py-1.5 text-center">
              <div className="text-[9px] text-dark-500">Tool / API</div>
              <div className="text-[10px] text-dark-300 font-medium">40+ APIs</div>
            </div>
            <div className="rounded-md border border-dark-600 bg-dark-800/50 px-2 py-1.5 text-center">
              <div className="text-[9px] text-dark-500">Channel</div>
              <div className="text-[10px] text-dark-300 font-medium">10+ Channels</div>
            </div>
          </div>
        </div>

        <p className="text-[10px] text-dark-600">
          각 도메인의 서브 에이전트는 MoA가 관리하며, 적절한 LLM과 Tool을 자동 또는 수동으로 선택합니다.
        </p>
      </div>
    </div>
  );
}
