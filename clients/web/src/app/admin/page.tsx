"use client";

import { useState, useEffect, useCallback } from "react";

// ── Types ────────────────────────────────────────────────────────────

interface AdminUser {
  user_id: string;
  username: string;
  email: string | null;
  created_at: number;
  device_count: number;
  online_device_count: number;
  last_active: number | null;
}

interface UsageStat {
  category: string;
  total_events: number;
  total_chars: number;
  unique_users: number;
}

interface UserUsageStat {
  username: string;
  category: string;
  events: number;
  chars: number;
}

interface ActiveSession {
  username: string;
  device_id: string | null;
  device_name: string | null;
  logged_in_at: number;
  expires_at: number;
}

interface Overview {
  summary: {
    total_users: number;
    online_users: number;
    total_devices: number;
    online_devices: number;
    active_sessions: number;
  };
  users: AdminUser[];
  usage_by_category: UsageStat[];
  usage_by_user: UserUsageStat[];
  active_sessions: ActiveSession[];
}

// ── Helpers ──────────────────────────────────────────────────────────

function formatDate(epoch: number): string {
  return new Date(epoch * 1000).toLocaleString("ko-KR", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function timeAgo(epoch: number): string {
  const diff = Date.now() / 1000 - epoch;
  if (diff < 60) return "방금 전";
  if (diff < 3600) return `${Math.floor(diff / 60)}분 전`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}시간 전`;
  return `${Math.floor(diff / 86400)}일 전`;
}

const CATEGORY_LABELS: Record<string, string> = {
  chat: "일반 대화",
  coding: "코딩",
  search: "웹 검색",
  document: "문서 작업",
  image: "이미지",
  music: "음악",
  translation: "번역",
  general: "기타",
};

function categoryLabel(cat: string): string {
  return CATEGORY_LABELS[cat] || cat;
}

// ── API ──────────────────────────────────────────────────────────────

const API_BASE = typeof window !== "undefined"
  ? `${window.location.origin}/api/admin/dashboard`
  : "";

async function adminLogin(username: string, password: string): Promise<string> {
  const res = await fetch(`${API_BASE}/login`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ username, password }),
  });
  const data = await res.json();
  if (!res.ok) throw new Error(data.error || "로그인에 실패했습니다.");
  return data.token;
}

async function fetchOverview(token: string): Promise<Overview> {
  const res = await fetch(`${API_BASE}/overview`, {
    headers: { Authorization: `Bearer ${token}` },
  });
  if (res.status === 401) throw new Error("SESSION_EXPIRED");
  if (!res.ok) throw new Error("데이터를 불러올 수 없습니다.");
  return res.json();
}

// ── Component ────────────────────────────────────────────────────────

export default function AdminDashboard() {
  const [token, setToken] = useState<string | null>(null);
  const [username, setUsername] = useState("admin");
  const [password, setPassword] = useState("");
  const [loginError, setLoginError] = useState("");
  const [data, setData] = useState<Overview | null>(null);
  const [loading, setLoading] = useState(false);
  const [lastRefresh, setLastRefresh] = useState<Date | null>(null);
  const [autoRefresh, setAutoRefresh] = useState(true);

  // Load token from sessionStorage
  useEffect(() => {
    const saved = sessionStorage.getItem("moa_admin_token");
    if (saved) setToken(saved);
  }, []);

  const handleLogin = async () => {
    setLoginError("");
    try {
      const t = await adminLogin(username, password);
      setToken(t);
      sessionStorage.setItem("moa_admin_token", t);
    } catch (e: unknown) {
      setLoginError(e instanceof Error ? e.message : "로그인에 실패했습니다.");
    }
  };

  const handleLogout = () => {
    setToken(null);
    setData(null);
    sessionStorage.removeItem("moa_admin_token");
  };

  const refresh = useCallback(async () => {
    if (!token) return;
    setLoading(true);
    try {
      const d = await fetchOverview(token);
      setData(d);
      setLastRefresh(new Date());
    } catch (e: unknown) {
      if (e instanceof Error && e.message === "SESSION_EXPIRED") {
        handleLogout();
      }
    } finally {
      setLoading(false);
    }
  }, [token]);

  // Initial load + auto-refresh every 10 seconds
  useEffect(() => {
    if (!token) return;
    refresh();
    if (!autoRefresh) return;
    const interval = setInterval(refresh, 10000);
    return () => clearInterval(interval);
  }, [token, autoRefresh, refresh]);

  // ── Login Page ──
  if (!token) {
    return (
      <div style={styles.loginContainer}>
        <div style={styles.loginCard}>
          <h1 style={styles.loginTitle}>MoA 관리자</h1>
          <input
            style={styles.input}
            placeholder="아이디"
            value={username}
            onChange={(e) => setUsername(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && handleLogin()}
          />
          <input
            style={styles.input}
            type="password"
            placeholder="비밀번호"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && handleLogin()}
          />
          <button style={styles.loginBtn} onClick={handleLogin}>
            로그인
          </button>
          {loginError && <p style={styles.error}>{loginError}</p>}
          <p style={styles.hint}>기본 아이디: admin / 비밀번호: admin</p>
        </div>
      </div>
    );
  }

  // ── Loading ──
  if (!data) {
    return (
      <div style={styles.container}>
        <p style={styles.loading}>데이터를 불러오는 중...</p>
      </div>
    );
  }

  const { summary, users, usage_by_category, usage_by_user, active_sessions } = data;

  return (
    <div style={styles.container}>
      {/* Header */}
      <div style={styles.header}>
        <h1 style={styles.title}>MoA 관리자 대시보드</h1>
        <div style={styles.headerRight}>
          <label style={styles.autoRefreshLabel}>
            <input
              type="checkbox"
              checked={autoRefresh}
              onChange={(e) => setAutoRefresh(e.target.checked)}
            />
            자동 새로고침
          </label>
          <button style={styles.refreshBtn} onClick={refresh} disabled={loading}>
            {loading ? "갱신 중..." : "새로고침"}
          </button>
          <button style={styles.logoutBtn} onClick={handleLogout}>
            로그아웃
          </button>
          {lastRefresh && (
            <span style={styles.lastRefresh}>
              마지막 갱신: {lastRefresh.toLocaleTimeString("ko-KR")}
            </span>
          )}
        </div>
      </div>

      {/* Summary Cards */}
      <div style={styles.cardRow}>
        <div style={styles.card}>
          <div style={styles.cardValue}>{summary.total_users}</div>
          <div style={styles.cardLabel}>전체 회원</div>
        </div>
        <div style={{ ...styles.card, borderColor: "#22c55e" }}>
          <div style={{ ...styles.cardValue, color: "#22c55e" }}>
            {summary.online_users}
          </div>
          <div style={styles.cardLabel}>온라인 회원</div>
        </div>
        <div style={styles.card}>
          <div style={styles.cardValue}>{summary.total_devices}</div>
          <div style={styles.cardLabel}>전체 디바이스</div>
        </div>
        <div style={{ ...styles.card, borderColor: "#22c55e" }}>
          <div style={{ ...styles.cardValue, color: "#22c55e" }}>
            {summary.online_devices}
          </div>
          <div style={styles.cardLabel}>온라인 디바이스</div>
        </div>
        <div style={styles.card}>
          <div style={styles.cardValue}>{summary.active_sessions}</div>
          <div style={styles.cardLabel}>활성 세션</div>
        </div>
      </div>

      {/* Usage by Category */}
      <div style={styles.section}>
        <h2 style={styles.sectionTitle}>용도별 사용 현황 (최근 30일)</h2>
        {usage_by_category.length === 0 ? (
          <p style={styles.empty}>아직 사용 데이터가 없습니다.</p>
        ) : (
          <table style={styles.table}>
            <thead>
              <tr>
                <th style={styles.th}>용도</th>
                <th style={styles.th}>사용 횟수</th>
                <th style={styles.th}>총 문자수</th>
                <th style={styles.th}>이용자 수</th>
              </tr>
            </thead>
            <tbody>
              {usage_by_category.map((u) => (
                <tr key={u.category} style={styles.tr}>
                  <td style={styles.td}>{categoryLabel(u.category)}</td>
                  <td style={styles.tdNum}>{u.total_events.toLocaleString()}</td>
                  <td style={styles.tdNum}>{u.total_chars.toLocaleString()}</td>
                  <td style={styles.tdNum}>{u.unique_users}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>

      {/* Users Table */}
      <div style={styles.section}>
        <h2 style={styles.sectionTitle}>회원 목록</h2>
        <table style={styles.table}>
          <thead>
            <tr>
              <th style={styles.th}>아이디</th>
              <th style={styles.th}>이메일</th>
              <th style={styles.th}>가입일</th>
              <th style={styles.th}>디바이스</th>
              <th style={styles.th}>온라인</th>
              <th style={styles.th}>최근 활동</th>
            </tr>
          </thead>
          <tbody>
            {users.map((u) => (
              <tr key={u.user_id} style={styles.tr}>
                <td style={styles.td}>{u.username}</td>
                <td style={styles.td}>{u.email || "-"}</td>
                <td style={styles.td}>{formatDate(u.created_at)}</td>
                <td style={styles.tdNum}>{u.device_count}대</td>
                <td style={styles.tdNum}>
                  {u.online_device_count > 0 ? (
                    <span style={{ color: "#22c55e" }}>
                      {u.online_device_count}대 온라인
                    </span>
                  ) : (
                    <span style={{ color: "#6b7280" }}>오프라인</span>
                  )}
                </td>
                <td style={styles.td}>
                  {u.last_active ? timeAgo(u.last_active) : "-"}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      {/* Per-User Usage */}
      {usage_by_user.length > 0 && (
        <div style={styles.section}>
          <h2 style={styles.sectionTitle}>회원별 사용 현황 (최근 30일)</h2>
          <table style={styles.table}>
            <thead>
              <tr>
                <th style={styles.th}>아이디</th>
                <th style={styles.th}>용도</th>
                <th style={styles.th}>사용 횟수</th>
                <th style={styles.th}>총 문자수</th>
              </tr>
            </thead>
            <tbody>
              {usage_by_user.map((u, i) => (
                <tr key={`${u.username}-${u.category}-${i}`} style={styles.tr}>
                  <td style={styles.td}>{u.username}</td>
                  <td style={styles.td}>{categoryLabel(u.category)}</td>
                  <td style={styles.tdNum}>{u.events.toLocaleString()}</td>
                  <td style={styles.tdNum}>{u.chars.toLocaleString()}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {/* Active Sessions */}
      <div style={styles.section}>
        <h2 style={styles.sectionTitle}>활성 세션</h2>
        {active_sessions.length === 0 ? (
          <p style={styles.empty}>현재 활성 세션이 없습니다.</p>
        ) : (
          <table style={styles.table}>
            <thead>
              <tr>
                <th style={styles.th}>아이디</th>
                <th style={styles.th}>디바이스</th>
                <th style={styles.th}>로그인 시각</th>
                <th style={styles.th}>만료</th>
              </tr>
            </thead>
            <tbody>
              {active_sessions.map((s, i) => (
                <tr key={`${s.username}-${i}`} style={styles.tr}>
                  <td style={styles.td}>{s.username}</td>
                  <td style={styles.td}>{s.device_name || s.device_id || "-"}</td>
                  <td style={styles.td}>{formatDate(s.logged_in_at)}</td>
                  <td style={styles.td}>{formatDate(s.expires_at)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  );
}

// ── Inline Styles ────────────────────────────────────────────────────

const styles: Record<string, React.CSSProperties> = {
  container: {
    maxWidth: 1200,
    margin: "0 auto",
    padding: "24px 16px",
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif",
    color: "#e5e7eb",
    background: "#0f1117",
    minHeight: "100vh",
  },
  header: {
    display: "flex",
    justifyContent: "space-between",
    alignItems: "center",
    marginBottom: 24,
    flexWrap: "wrap" as const,
    gap: 12,
  },
  title: { fontSize: 24, fontWeight: 700, margin: 0 },
  headerRight: {
    display: "flex",
    alignItems: "center",
    gap: 12,
    flexWrap: "wrap" as const,
  },
  autoRefreshLabel: { fontSize: 13, color: "#9ca3af", cursor: "pointer" },
  refreshBtn: {
    padding: "6px 16px",
    fontSize: 13,
    background: "#374151",
    color: "#e5e7eb",
    border: "1px solid #4b5563",
    borderRadius: 6,
    cursor: "pointer",
  },
  logoutBtn: {
    padding: "6px 16px",
    fontSize: 13,
    background: "#7f1d1d",
    color: "#fca5a5",
    border: "1px solid #991b1b",
    borderRadius: 6,
    cursor: "pointer",
  },
  lastRefresh: { fontSize: 12, color: "#6b7280" },
  cardRow: {
    display: "grid",
    gridTemplateColumns: "repeat(auto-fit, minmax(160px, 1fr))",
    gap: 16,
    marginBottom: 32,
  },
  card: {
    background: "#1a1d27",
    borderRadius: 12,
    padding: "20px 16px",
    textAlign: "center" as const,
    border: "1px solid #374151",
  },
  cardValue: { fontSize: 32, fontWeight: 700, color: "#60a5fa" },
  cardLabel: { fontSize: 13, color: "#9ca3af", marginTop: 4 },
  section: { marginBottom: 32 },
  sectionTitle: {
    fontSize: 18,
    fontWeight: 600,
    marginBottom: 12,
    borderBottom: "1px solid #374151",
    paddingBottom: 8,
  },
  table: {
    width: "100%",
    borderCollapse: "collapse" as const,
    fontSize: 14,
  },
  th: {
    textAlign: "left" as const,
    padding: "10px 12px",
    borderBottom: "2px solid #374151",
    color: "#9ca3af",
    fontWeight: 600,
    fontSize: 13,
  },
  tr: { borderBottom: "1px solid #1f2937" },
  td: { padding: "10px 12px", color: "#d1d5db" },
  tdNum: { padding: "10px 12px", color: "#d1d5db", textAlign: "right" as const },
  empty: { color: "#6b7280", fontStyle: "italic" as const, padding: 16 },
  loading: { color: "#9ca3af", textAlign: "center" as const, padding: 48 },
  loginContainer: {
    display: "flex",
    justifyContent: "center",
    alignItems: "center",
    minHeight: "100vh",
    background: "#0f1117",
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif",
  },
  loginCard: {
    background: "#1a1d27",
    borderRadius: 16,
    padding: "40px 32px",
    width: 360,
    border: "1px solid #374151",
    textAlign: "center" as const,
  },
  loginTitle: { fontSize: 24, fontWeight: 700, color: "#e5e7eb", marginBottom: 24 },
  input: {
    width: "100%",
    padding: "12px 14px",
    fontSize: 15,
    background: "#0f1117",
    border: "1px solid #374151",
    borderRadius: 8,
    color: "#e5e7eb",
    marginBottom: 12,
    boxSizing: "border-box" as const,
    outline: "none",
  },
  loginBtn: {
    width: "100%",
    padding: "12px",
    fontSize: 15,
    fontWeight: 600,
    background: "#3b82f6",
    color: "white",
    border: "none",
    borderRadius: 8,
    cursor: "pointer",
    marginTop: 4,
  },
  error: { color: "#f87171", fontSize: 14, marginTop: 8 },
  hint: { color: "#6b7280", fontSize: 12, marginTop: 12 },
};
