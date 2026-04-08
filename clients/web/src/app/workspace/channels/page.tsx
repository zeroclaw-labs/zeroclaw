"use client";

import { useState, useEffect } from "react";
import { getToken } from "@/lib/auth";
import Link from "next/link";

interface ChannelLink {
  channel: string;
  platform_uid: string;
  device_id: string | null;
  autonomy_mode: string;
  linked_at: number;
}

const CHANNEL_LABELS: Record<string, string> = {
  kakao: "카카오톡",
  telegram: "텔레그램",
  discord: "디스코드",
  slack: "슬랙",
  line: "라인",
};

export default function ChannelsPage() {
  const [channels, setChannels] = useState<ChannelLink[]>([]);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    const token = getToken();
    if (!token) return;
    fetch("/api/user/channels", {
      headers: { Authorization: `Bearer ${token}` },
    })
      .then((r) => (r.ok ? r.json() : []))
      .then((data) => setChannels(Array.isArray(data) ? data : []))
      .catch(() => setChannels([]))
      .finally(() => setLoading(false));
  }, []);

  return (
    <div className="max-w-3xl mx-auto py-8 px-4 text-dark-100">
      <Link href="/workspace" className="text-sm text-dark-400 hover:text-dark-200 mb-4 inline-block">
        ← 워크스페이스
      </Link>
      <h1 className="text-2xl font-bold mb-6">채널 관리</h1>

      <p className="text-dark-400 text-sm mb-6">
        카카오톡, 텔레그램 등 메신저 채널과 MoA를 연결하여 대화할 수 있습니다.
        <br />
        각 채널의 봇을 만들고, MoA 앱 설정에서 토큰을 입력하면 자동으로 연결됩니다.
      </p>

      {loading ? (
        <p className="text-dark-500">불러오는 중...</p>
      ) : channels.length === 0 ? (
        <div className="bg-dark-800 rounded-xl border border-dark-700 p-8 text-center">
          <p className="text-dark-400 mb-4">연결된 채널이 없습니다.</p>
          <p className="text-dark-500 text-sm">
            MoA 데스크톱 앱의 설정 → 채널 연결에서 봇 토큰을 입력하세요.
          </p>
        </div>
      ) : (
        <div className="space-y-3">
          {channels.map((ch) => (
            <div
              key={`${ch.channel}-${ch.platform_uid}`}
              className="bg-dark-800 rounded-xl border border-dark-700 p-4 flex items-center justify-between"
            >
              <div>
                <span className="font-medium">
                  {CHANNEL_LABELS[ch.channel] || ch.channel}
                </span>
                <span className="text-dark-500 text-sm ml-2">
                  {ch.platform_uid}
                </span>
                <div className="text-xs text-dark-500 mt-1">
                  모드: {ch.autonomy_mode === "full" ? "🔓 전체" : "🔒 안전"}
                  {ch.device_id && ` · 디바이스: ${ch.device_id.slice(0, 8)}...`}
                </div>
              </div>
              <span className="text-xs text-dark-500">
                {new Date(ch.linked_at * 1000).toLocaleDateString("ko-KR")}
              </span>
            </div>
          ))}
        </div>
      )}

      <div className="mt-8 bg-dark-800/50 rounded-xl border border-dark-700 p-6">
        <h2 className="font-semibold mb-3">채널 연결 방법</h2>
        <ol className="text-sm text-dark-400 space-y-2 list-decimal list-inside">
          <li>각 메신저에서 봇을 만듭니다 (텔레그램: @BotFather, 카카오: developers.kakao.com)</li>
          <li>MoA 데스크톱 앱 → 설정 → 채널 연결에서 봇 토큰을 입력합니다</li>
          <li>MoA를 재시작하면 해당 메신저에서 MoA와 대화할 수 있습니다</li>
        </ol>
      </div>
    </div>
  );
}
