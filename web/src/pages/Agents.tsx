import { useState, useEffect } from 'react';
import { useNavigate } from 'react-router-dom';
import { Users, Loader2, AlertCircle, Zap, Coffee, Bug, Search, Megaphone } from 'lucide-react';
import { getAgents, type AgentDef } from '@/lib/api';

// Cartoon avatar mapping — SVG inline for each role
const AVATARS: Record<string, { bg: string; emoji: string; Icon: typeof Users }> = {
    ops: { bg: 'from-orange-400 to-red-500', emoji: '📢', Icon: Megaphone },
    design: { bg: 'from-purple-400 to-pink-500', emoji: '🎯', Icon: Zap },
    dev: { bg: 'from-cyan-400 to-blue-500', emoji: '💻', Icon: Zap },
    qa: { bg: 'from-green-400 to-emerald-500', emoji: '🐛', Icon: Bug },
    intel: { bg: 'from-yellow-400 to-amber-500', emoji: '🔍', Icon: Search },
};

function getAvatarStyle(id: string) {
    return AVATARS[id] ?? { bg: 'from-gray-400 to-slate-500', emoji: '🤖', Icon: Users };
}

function StatusBadge({ status }: { status: AgentDef['status'] }) {
    if (status.state === 'idle') {
        return (
            <span className="inline-flex items-center gap-1.5 text-xs font-medium px-2 py-0.5 rounded-full bg-gray-100 text-gray-600 dark:bg-gray-700 dark:text-gray-300">
                <Coffee className="w-3 h-3" /> 空闲
            </span>
        );
    }
    if (status.state === 'working') {
        return (
            <span className="inline-flex items-center gap-1.5 text-xs font-medium px-2 py-0.5 rounded-full bg-blue-100 text-blue-700 dark:bg-blue-900 dark:text-blue-300">
                <Loader2 className="w-3 h-3 animate-spin" /> {status.task}
            </span>
        );
    }
    return (
        <span className="inline-flex items-center gap-1.5 text-xs font-medium px-2 py-0.5 rounded-full bg-red-100 text-red-700 dark:bg-red-900 dark:text-red-300">
            <AlertCircle className="w-3 h-3" /> 错误
        </span>
    );
}

export default function Agents() {
    const [agents, setAgents] = useState<AgentDef[]>([]);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);
    const navigate = useNavigate();

    useEffect(() => {
        getAgents()
            .then(setAgents)
            .catch((e) => setError(e.message))
            .finally(() => setLoading(false));
    }, []);

    if (loading) {
        return (
            <div className="flex items-center justify-center h-64">
                <Loader2 className="w-8 h-8 animate-spin" style={{ color: 'var(--pc-accent)' }} />
            </div>
        );
    }

    if (error) {
        return (
            <div className="p-6">
                <div className="flex items-center gap-2 text-red-500">
                    <AlertCircle className="w-5 h-5" />
                    <span>{error}</span>
                </div>
            </div>
        );
    }

    return (
        <div className="p-6 max-w-6xl mx-auto">
            {/* Header */}
            <div className="flex items-center gap-3 mb-8">
                <Users className="w-7 h-7" style={{ color: 'var(--pc-accent)' }} />
                <h1 className="text-2xl font-bold" style={{ color: 'var(--pc-text-primary)' }}>
                    Agent 团队
                </h1>
                <span
                    className="text-sm px-2 py-0.5 rounded-full"
                    style={{ background: 'var(--pc-bg-secondary)', color: 'var(--pc-text-secondary)' }}
                >
                    {agents.length} 个 Agent
                </span>
            </div>

            {agents.length === 0 ? (
                <div className="text-center py-16" style={{ color: 'var(--pc-text-secondary)' }}>
                    <Users className="w-16 h-16 mx-auto mb-4 opacity-30" />
                    <p className="text-lg font-medium">还没有 Agent</p>
                    <p className="text-sm mt-1">在 workspace/agents/ 目录下创建 Agent 定义</p>
                </div>
            ) : (
                <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-5 gap-5">
                    {agents.map((agent) => {
                        const av = getAvatarStyle(agent.avatar || agent.id);
                        return (
                            <button
                                key={agent.id}
                                type="button"
                                onClick={() => navigate(`/agents/${agent.id}`)}
                                className="group relative rounded-2xl border p-5 text-left transition-all hover:scale-[1.02] hover:shadow-lg focus:outline-none focus:ring-2"
                                style={{
                                    background: 'var(--pc-bg-base)',
                                    borderColor: 'var(--pc-border)',
                                }}
                            >
                                {/* Avatar */}
                                <div
                                    className={`w-20 h-20 mx-auto mb-4 rounded-2xl bg-gradient-to-br ${av.bg} flex items-center justify-center text-3xl shadow-lg group-hover:shadow-xl transition-shadow`}
                                >
                                    {agent.avatar && AVATARS[agent.avatar] ? (
                                        <av.Icon className="w-10 h-10 text-white" />
                                    ) : (
                                        <span>{av.emoji}</span>
                                    )}
                                </div>

                                {/* Name */}
                                <h3
                                    className="text-center font-semibold text-base mb-1"
                                    style={{ color: 'var(--pc-text-primary)' }}
                                >
                                    {agent.display_name}
                                </h3>

                                {/* Role */}
                                <p
                                    className="text-center text-xs mb-3 line-clamp-2"
                                    style={{ color: 'var(--pc-text-secondary)' }}
                                >
                                    {agent.role}
                                </p>

                                {/* Status */}
                                <div className="flex justify-center">
                                    <StatusBadge status={agent.status} />
                                </div>

                                {/* Focus tags */}
                                {agent.focus.length > 0 && (
                                    <div className="flex flex-wrap gap-1 mt-3 justify-center">
                                        {agent.focus.slice(0, 3).map((tag) => (
                                            <span
                                                key={tag}
                                                className="text-[10px] px-1.5 py-0.5 rounded-md"
                                                style={{
                                                    background: 'var(--pc-bg-secondary)',
                                                    color: 'var(--pc-text-secondary)',
                                                }}
                                            >
                                                {tag}
                                            </span>
                                        ))}
                                    </div>
                                )}

                                {/* Pulse indicator for working state */}
                                {agent.status.state === 'working' && (
                                    <div className="absolute top-3 right-3">
                                        <span className="relative flex h-3 w-3">
                                            <span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-blue-400 opacity-75" />
                                            <span className="relative inline-flex rounded-full h-3 w-3 bg-blue-500" />
                                        </span>
                                    </div>
                                )}
                            </button>
                        );
                    })}
                </div>
            )}
        </div>
    );
}
