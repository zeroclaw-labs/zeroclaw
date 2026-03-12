"use client";

import { useState } from "react";
import Link from "next/link";

export default function MyPage() {
  const [activeTab, setActiveTab] = useState<"profile" | "usage" | "api-keys" | "preferences">("profile");

  return (
    <div className="min-h-screen pt-[65px]">
      <div className="mx-auto max-w-4xl px-4 py-12 sm:px-6 lg:px-8">
        {/* Back link */}
        <Link
          href="/workspace"
          className="inline-flex items-center gap-1.5 text-sm text-dark-400 hover:text-dark-200 transition-all mb-8"
        >
          <svg className="h-4 w-4" fill="none" viewBox="0 0 24 24" strokeWidth={2} stroke="currentColor">
            <path strokeLinecap="round" strokeLinejoin="round" d="M10.5 19.5L3 12m0 0l7.5-7.5M3 12h18" />
          </svg>
          워크스페이스로 돌아가기
        </Link>

        {/* Header */}
        <div className="flex items-center gap-4 mb-8">
          <div className="flex h-16 w-16 items-center justify-center rounded-2xl bg-primary-500/10 border border-primary-500/20">
            <svg className="h-8 w-8 text-primary-400" fill="none" viewBox="0 0 24 24" strokeWidth={1.5} stroke="currentColor">
              <path strokeLinecap="round" strokeLinejoin="round" d="M15.75 6a3.75 3.75 0 11-7.5 0 3.75 3.75 0 017.5 0zM4.501 20.118a7.5 7.5 0 0114.998 0A17.933 17.933 0 0112 21.75c-2.676 0-5.216-.584-7.499-1.632z" />
            </svg>
          </div>
          <div>
            <h1 className="text-2xl font-bold text-dark-50">마이페이지</h1>
            <p className="text-sm text-dark-400">My Page</p>
          </div>
        </div>

        {/* Tabs */}
        <div className="flex border-b border-dark-800/50 mb-6 overflow-x-auto">
          {[
            { id: "profile" as const, label: "프로필", labelEn: "Profile" },
            { id: "usage" as const, label: "사용 내역", labelEn: "Usage" },
            { id: "api-keys" as const, label: "API 키", labelEn: "API Keys" },
            { id: "preferences" as const, label: "설정", labelEn: "Settings" },
          ].map((tab) => (
            <button
              key={tab.id}
              onClick={() => setActiveTab(tab.id)}
              className={`flex-shrink-0 px-4 py-2.5 text-sm font-medium transition-all border-b-2 ${
                activeTab === tab.id
                  ? "text-primary-400 border-primary-400"
                  : "text-dark-500 border-transparent hover:text-dark-300"
              }`}
            >
              {tab.label} <span className="text-[10px] text-dark-600 ml-1">{tab.labelEn}</span>
            </button>
          ))}
        </div>

        {/* Profile Tab */}
        {activeTab === "profile" && (
          <div className="space-y-6">
            <div className="glass-card rounded-xl p-6">
              <h3 className="text-sm font-semibold text-dark-200 mb-4">기본 정보 Basic Info</h3>
              <div className="space-y-4">
                <div>
                  <label className="block text-xs text-dark-400 mb-1.5">이름 Name</label>
                  <input
                    type="text"
                    placeholder="Enter your name"
                    className="w-full rounded-lg border border-dark-700 bg-dark-800/50 px-4 py-2.5 text-sm text-dark-100 placeholder-dark-500 outline-none focus:border-primary-500 focus:ring-1 focus:ring-primary-500 transition-all"
                  />
                </div>
                <div>
                  <label className="block text-xs text-dark-400 mb-1.5">이메일 Email</label>
                  <input
                    type="email"
                    placeholder="Enter your email"
                    className="w-full rounded-lg border border-dark-700 bg-dark-800/50 px-4 py-2.5 text-sm text-dark-100 placeholder-dark-500 outline-none focus:border-primary-500 focus:ring-1 focus:ring-primary-500 transition-all"
                  />
                </div>
                <div>
                  <label className="block text-xs text-dark-400 mb-1.5">현재 플랜 Current Plan</label>
                  <div className="flex items-center gap-3">
                    <span className="text-sm font-medium text-dark-200">Free</span>
                    <Link href="/workspace/payment" className="text-xs text-primary-400 hover:text-primary-300">
                      업그레이드 Upgrade
                    </Link>
                  </div>
                </div>
              </div>
              <button className="mt-6 btn-primary text-xs px-4 py-2">
                저장 Save
              </button>
            </div>

            {/* Connected Services */}
            <div className="glass-card rounded-xl p-6">
              <h3 className="text-sm font-semibold text-dark-200 mb-4">연결된 서비스 Connected Services</h3>
              <div className="space-y-3">
                {[
                  { name: "MoA Server", status: "connected", detail: "localhost:3000" },
                  { name: "Google Calendar", status: "disconnected", detail: "" },
                  { name: "Notion", status: "disconnected", detail: "" },
                  { name: "Figma", status: "disconnected", detail: "" },
                ].map((svc) => (
                  <div key={svc.name} className="flex items-center justify-between py-2 border-b border-dark-800/30 last:border-0">
                    <div className="flex items-center gap-2">
                      <div className={`h-2 w-2 rounded-full ${svc.status === "connected" ? "bg-green-400" : "bg-dark-600"}`} />
                      <span className="text-xs text-dark-200">{svc.name}</span>
                      {svc.detail && <span className="text-[10px] text-dark-500">{svc.detail}</span>}
                    </div>
                    <button className={`text-[10px] px-3 py-1 rounded-md transition-all ${
                      svc.status === "connected"
                        ? "border border-red-500/30 text-red-400 hover:bg-red-500/10"
                        : "border border-dark-600 text-dark-400 hover:bg-dark-800"
                    }`}>
                      {svc.status === "connected" ? "해제" : "연결"}
                    </button>
                  </div>
                ))}
              </div>
            </div>
          </div>
        )}

        {/* Usage Tab */}
        {activeTab === "usage" && (
          <div className="space-y-6">
            {/* Usage Summary */}
            <div className="grid grid-cols-2 sm:grid-cols-4 gap-4">
              {[
                { label: "총 대화", value: "1,247", icon: "chat" },
                { label: "LLM 호출", value: "3,891", icon: "sparkle" },
                { label: "도구 실행", value: "562", icon: "tool" },
                { label: "이미지 생성", value: "87", icon: "image" },
              ].map((stat) => (
                <div key={stat.label} className="glass-card rounded-xl p-4 text-center">
                  <div className="text-xl font-bold text-dark-100">{stat.value}</div>
                  <div className="text-[10px] text-dark-500 mt-1">{stat.label}</div>
                </div>
              ))}
            </div>

            {/* Recent Activity */}
            <div className="glass-card rounded-xl p-6">
              <h3 className="text-sm font-semibold text-dark-200 mb-4">최근 활동 Recent Activity</h3>
              <div className="space-y-3">
                {[
                  { time: "2분 전", agent: "셀프 코딩 에이전트", action: "Claude Opus 4.6으로 코드 생성", domain: "코딩/개발" },
                  { time: "15분 전", agent: "개인 쇼핑 플래너", action: "Playwright로 가격 비교 수행", domain: "웹/쇼핑" },
                  { time: "1시간 전", agent: "문서 생성", action: "Google Docs API로 보고서 작성", domain: "문서작업" },
                  { time: "3시간 전", agent: "유튜브 숏폼 스튜디오", action: "Seedance 2.0으로 비디오 생성", domain: "비디오" },
                  { time: "어제", agent: "음악/가사/편곡 에이전트", action: "Suno API로 데모 곡 생성", domain: "음악" },
                ].map((activity, i) => (
                  <div key={i} className="flex items-start gap-3 py-2 border-b border-dark-800/30 last:border-0">
                    <span className="text-[10px] text-dark-600 w-16 flex-shrink-0 pt-0.5">{activity.time}</span>
                    <div>
                      <div className="flex items-center gap-2">
                        <span className="text-xs font-medium text-dark-200">{activity.agent}</span>
                        <span className="text-[9px] px-1.5 py-0.5 rounded-full bg-dark-800 text-dark-400">{activity.domain}</span>
                      </div>
                      <p className="text-[11px] text-dark-500 mt-0.5">{activity.action}</p>
                    </div>
                  </div>
                ))}
              </div>
            </div>
          </div>
        )}

        {/* API Keys Tab */}
        {activeTab === "api-keys" && (
          <div className="space-y-6">
            <div className="glass-card rounded-xl p-6">
              <div className="flex items-center justify-between mb-4">
                <h3 className="text-sm font-semibold text-dark-200">외부 API 키 External API Keys</h3>
                <p className="text-[10px] text-dark-500">각 서비스의 API 키를 등록하면 해당 도구를 사용할 수 있습니다.</p>
              </div>
              <div className="space-y-4">
                {[
                  { provider: "OpenAI", key: "sk-...xxxx", status: "active" },
                  { provider: "Anthropic (Claude)", key: "", status: "empty" },
                  { provider: "Google (Gemini)", key: "", status: "empty" },
                  { provider: "Suno", key: "", status: "empty" },
                  { provider: "Seedance", key: "", status: "empty" },
                  { provider: "Freepik", key: "", status: "empty" },
                  { provider: "Figma", key: "", status: "empty" },
                  { provider: "Shopify", key: "", status: "empty" },
                ].map((api) => (
                  <div key={api.provider} className="flex items-center justify-between py-2 border-b border-dark-800/30 last:border-0">
                    <div className="flex items-center gap-3">
                      <div className={`h-2 w-2 rounded-full ${api.status === "active" ? "bg-green-400" : "bg-dark-600"}`} />
                      <span className="text-xs text-dark-200 w-32">{api.provider}</span>
                      {api.key ? (
                        <span className="text-[10px] font-mono text-dark-500">{api.key}</span>
                      ) : (
                        <span className="text-[10px] text-dark-600">미등록</span>
                      )}
                    </div>
                    <button className="text-[10px] px-3 py-1 rounded-md border border-dark-600 text-dark-400 hover:bg-dark-800 transition-all">
                      {api.key ? "변경" : "등록"}
                    </button>
                  </div>
                ))}
              </div>
            </div>
          </div>
        )}

        {/* Preferences Tab */}
        {activeTab === "preferences" && (
          <div className="space-y-6">
            <div className="glass-card rounded-xl p-6">
              <h3 className="text-sm font-semibold text-dark-200 mb-4">기본 설정 Default Settings</h3>
              <div className="space-y-5">
                <div>
                  <label className="block text-xs text-dark-400 mb-1.5">기본 LLM Default LLM</label>
                  <select className="w-full rounded-lg border border-dark-700 bg-dark-800/50 px-4 py-2.5 text-sm text-dark-100 outline-none focus:border-primary-500 transition-all">
                    <option>Claude Opus 4.6</option>
                    <option>Gemini 2.5 Pro</option>
                    <option>GPT-4.1</option>
                    <option>Claude Sonnet 4</option>
                    <option>Gemini 2.5 Flash</option>
                  </select>
                </div>
                <div>
                  <label className="block text-xs text-dark-400 mb-1.5">기본 채널 Default Channel</label>
                  <select className="w-full rounded-lg border border-dark-700 bg-dark-800/50 px-4 py-2.5 text-sm text-dark-100 outline-none focus:border-primary-500 transition-all">
                    <option>Web Chat</option>
                    <option>KakaoTalk</option>
                    <option>Telegram</option>
                    <option>Discord</option>
                    <option>Slack</option>
                  </select>
                </div>
                <div>
                  <label className="block text-xs text-dark-400 mb-1.5">언어 Language</label>
                  <select className="w-full rounded-lg border border-dark-700 bg-dark-800/50 px-4 py-2.5 text-sm text-dark-100 outline-none focus:border-primary-500 transition-all">
                    <option>한국어</option>
                    <option>English</option>
                    <option>日本語</option>
                    <option>中文</option>
                  </select>
                </div>
                <div className="flex items-center justify-between">
                  <div>
                    <span className="text-xs text-dark-200">자동 에이전트 추천</span>
                    <p className="text-[10px] text-dark-500">질문에 따라 적절한 서브 에이전트를 자동 추천</p>
                  </div>
                  <button className="relative h-6 w-11 rounded-full bg-primary-500 transition-all">
                    <span className="absolute right-0.5 top-0.5 h-5 w-5 rounded-full bg-white transition-all" />
                  </button>
                </div>
                <div className="flex items-center justify-between">
                  <div>
                    <span className="text-xs text-dark-200">비용 절약 모드</span>
                    <p className="text-[10px] text-dark-500">가능한 경우 저비용 LLM을 자동 선택</p>
                  </div>
                  <button className="relative h-6 w-11 rounded-full bg-dark-700 transition-all">
                    <span className="absolute left-0.5 top-0.5 h-5 w-5 rounded-full bg-dark-400 transition-all" />
                  </button>
                </div>
              </div>
              <button className="mt-6 btn-primary text-xs px-4 py-2">
                저장 Save
              </button>
            </div>

            {/* Danger Zone */}
            <div className="glass-card rounded-xl p-6 border-red-500/20">
              <h3 className="text-sm font-semibold text-red-400 mb-4">위험 영역 Danger Zone</h3>
              <div className="space-y-3">
                <div className="flex items-center justify-between">
                  <div>
                    <span className="text-xs text-dark-200">대화 기록 전체 삭제</span>
                    <p className="text-[10px] text-dark-500">모든 채팅 기록과 메모리를 삭제합니다</p>
                  </div>
                  <button className="text-[10px] px-3 py-1.5 rounded-md border border-red-500/30 text-red-400 hover:bg-red-500/10 transition-all">
                    삭제 Delete
                  </button>
                </div>
                <div className="flex items-center justify-between">
                  <div>
                    <span className="text-xs text-dark-200">계정 삭제</span>
                    <p className="text-[10px] text-dark-500">계정과 모든 데이터를 영구 삭제합니다</p>
                  </div>
                  <button className="text-[10px] px-3 py-1.5 rounded-md border border-red-500/30 text-red-400 hover:bg-red-500/10 transition-all">
                    계정 삭제
                  </button>
                </div>
              </div>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
