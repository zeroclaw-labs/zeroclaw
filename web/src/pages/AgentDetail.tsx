import { useState, useEffect, useCallback, useRef } from 'react';
import { useParams, useNavigate } from 'react-router-dom';
import {
    ArrowLeft,
    Loader2,
    AlertCircle,
    Save,
    Plus,
    FileText,
    Wrench,
    Brain,
    Sparkles,
    Users,
    Megaphone,
    Zap,
    Bug,
    Search,
    MessageCircle,
    SendHorizonal,
} from 'lucide-react';
import {
    getAgent,
    updateAgentIdentity,
    getAgentSkills,
    getAgentSkill,
    putAgentSkill,
    postAgentMessage,
    type AgentDef,
} from '@/lib/api';

const AVATAR_ICONS: Record<string, typeof Users> = {
    ops: Megaphone,
    design: Zap,
    dev: Sparkles,
    qa: Bug,
    intel: Search,
};

const AVATAR_GRADIENTS: Record<string, string> = {
    ops: 'from-orange-400 to-red-500',
    design: 'from-purple-400 to-pink-500',
    dev: 'from-cyan-400 to-blue-500',
    qa: 'from-green-400 to-emerald-500',
    intel: 'from-yellow-400 to-amber-500',
};

type Tab = 'identity' | 'soul' | 'skills' | 'chat';

interface ChatMessage {
    role: 'user' | 'agent';
    text: string;
}

export default function AgentDetail() {
    const { id } = useParams<{ id: string }>();
    const navigate = useNavigate();

    const [agent, setAgent] = useState<AgentDef | null>(null);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);
    const [saving, setSaving] = useState(false);
    const [saveMsg, setSaveMsg] = useState<string | null>(null);

    const [tab, setTab] = useState<Tab>('identity');
    const [identity, setIdentity] = useState('');
    const [soul, setSoul] = useState('');

    // Chat state
    const [chatMessages, setChatMessages] = useState<ChatMessage[]>([]);
    const [chatInput, setChatInput] = useState('');
    const [chatLoading, setChatLoading] = useState(false);
    const [chatSessionId, setChatSessionId] = useState<string | undefined>(undefined);
    const chatEndRef = useRef<HTMLDivElement>(null);

    // Skills state
    const [skills, setSkills] = useState<string[]>([]);
    const [selectedSkill, setSelectedSkill] = useState<string | null>(null);
    const [skillContent, setSkillContent] = useState('');
    const [newSkillName, setNewSkillName] = useState('');

    const load = useCallback(async () => {
        if (!id) return;
        try {
            const a = await getAgent(id);
            setAgent(a);
            setIdentity(a.identity ?? '');
            setSoul(a.soul ?? '');
            const sk = await getAgentSkills(id);
            setSkills(sk);
            if (sk.length > 0 && !selectedSkill) {
                const first = sk[0]!;
                setSelectedSkill(first);
                const content = await getAgentSkill(id, first);
                setSkillContent(content);
            }
        } catch (e: any) {
            setError(e.message);
        } finally {
            setLoading(false);
        }
    }, [id, selectedSkill]);

    useEffect(() => {
        load();
    }, [load]);

    const handleSelectSkill = async (name: string) => {
        if (!id) return;
        setSelectedSkill(name);
        try {
            const content = await getAgentSkill(id, name);
            setSkillContent(content);
        } catch {
            setSkillContent('');
        }
    };

    const handleSaveIdentity = async () => {
        if (!id) return;
        setSaving(true);
        setSaveMsg(null);
        try {
            await updateAgentIdentity(id, identity, soul);
            setSaveMsg('已保存');
            setTimeout(() => setSaveMsg(null), 2000);
        } catch (e: any) {
            setSaveMsg(`错误: ${e.message}`);
        } finally {
            setSaving(false);
        }
    };

    const handleSaveSkill = async () => {
        if (!id || !selectedSkill) return;
        setSaving(true);
        setSaveMsg(null);
        try {
            await putAgentSkill(id, selectedSkill, skillContent);
            setSaveMsg('已保存');
            setTimeout(() => setSaveMsg(null), 2000);
        } catch (e: any) {
            setSaveMsg(`错误: ${e.message}`);
        } finally {
            setSaving(false);
        }
    };

    const handleSendMessage = async () => {
        if (!id || !chatInput.trim() || chatLoading) return;
        const text = chatInput.trim();
        setChatInput('');
        setChatMessages((prev) => [...prev, { role: 'user', text }]);
        setChatLoading(true);
        try {
            const res = await postAgentMessage(id, text, chatSessionId);
            if (!chatSessionId) setChatSessionId(res.session_id);
            setChatMessages((prev) => [...prev, { role: 'agent', text: res.reply }]);
            setTimeout(() => chatEndRef.current?.scrollIntoView({ behavior: 'smooth' }), 50);
        } catch (e: any) {
            setChatMessages((prev) => [...prev, { role: 'agent', text: `[错误] ${e.message}` }]);
        } finally {
            setChatLoading(false);
        }
    };

    const handleCreateSkill = async () => {
        if (!id || !newSkillName.trim()) return;
        const name = newSkillName.trim().replace(/\s+/g, '-').toLowerCase();
        setSaving(true);
        try {
            await putAgentSkill(id, name, `# ${name}\n\nDescribe this skill...\n`);
            setSkills((prev) => [...prev, name].sort());
            setSelectedSkill(name);
            setSkillContent(`# ${name}\n\nDescribe this skill...\n`);
            setNewSkillName('');
        } catch (e: any) {
            setSaveMsg(`错误: ${e.message}`);
        } finally {
            setSaving(false);
        }
    };

    if (loading) {
        return (
            <div className="flex items-center justify-center h-64">
                <Loader2 className="w-8 h-8 animate-spin" style={{ color: 'var(--pc-accent)' }} />
            </div>
        );
    }

    if (error || !agent) {
        return (
            <div className="p-6">
                <div className="flex items-center gap-2 text-red-500">
                    <AlertCircle className="w-5 h-5" />
                    <span>{error ?? 'Agent not found'}</span>
                </div>
            </div>
        );
    }

    const avatarKey = agent.avatar || agent.id;
    const AvatarIcon = AVATAR_ICONS[avatarKey] ?? Users;
    const gradient = AVATAR_GRADIENTS[avatarKey] ?? 'from-gray-400 to-slate-500';

    return (
        <div className="p-6 max-w-5xl mx-auto">
            {/* Back + Header */}
            <button
                type="button"
                onClick={() => navigate('/agents')}
                className="flex items-center gap-1 text-sm mb-6 hover:underline"
                style={{ color: 'var(--pc-text-secondary)' }}
            >
                <ArrowLeft className="w-4 h-4" /> 返回团队
            </button>

            <div className="flex items-center gap-5 mb-8">
                <div
                    className={`w-20 h-20 rounded-2xl bg-gradient-to-br ${gradient} flex items-center justify-center shadow-lg`}
                >
                    <AvatarIcon className="w-10 h-10 text-white" />
                </div>
                <div>
                    <h1 className="text-2xl font-bold" style={{ color: 'var(--pc-text-primary)' }}>
                        {agent.display_name}
                    </h1>
                    <p className="text-sm mt-1" style={{ color: 'var(--pc-text-secondary)' }}>
                        {agent.role}
                    </p>
                    <div className="flex gap-2 mt-2">
                        {agent.focus.map((tag) => (
                            <span
                                key={tag}
                                className="text-xs px-2 py-0.5 rounded-md"
                                style={{
                                    background: 'var(--pc-bg-secondary)',
                                    color: 'var(--pc-text-secondary)',
                                }}
                            >
                                {tag}
                            </span>
                        ))}
                    </div>
                </div>
            </div>

            {/* Tabs */}
            <div className="flex gap-1 border-b mb-6" style={{ borderColor: 'var(--pc-border)' }}>
                {([
                    { key: 'identity' as Tab, icon: Brain, label: 'IDENTITY' },
                    { key: 'soul' as Tab, icon: Sparkles, label: 'SOUL' },
                    { key: 'skills' as Tab, icon: Wrench, label: 'SKILLS' },
                    { key: 'chat' as Tab, icon: MessageCircle, label: 'CHAT' },
                ]).map(({ key, icon: Icon, label }) => (
                    <button
                        key={key}
                        type="button"
                        onClick={() => setTab(key)}
                        className={`flex items-center gap-2 px-4 py-2.5 text-sm font-medium border-b-2 transition-colors ${tab === key ? 'border-current' : 'border-transparent'
                            }`}
                        style={{
                            color: tab === key ? 'var(--pc-accent)' : 'var(--pc-text-secondary)',
                        }}
                    >
                        <Icon className="w-4 h-4" /> {label}
                    </button>
                ))}
            </div>

            {/* Tab content */}
            {tab === 'identity' && (
                <div>
                    <div className="flex items-center justify-between mb-3">
                        <div className="flex items-center gap-2">
                            <FileText className="w-4 h-4" style={{ color: 'var(--pc-text-secondary)' }} />
                            <span className="text-sm font-medium" style={{ color: 'var(--pc-text-primary)' }}>
                                IDENTITY.md
                            </span>
                        </div>
                        <button
                            type="button"
                            onClick={handleSaveIdentity}
                            disabled={saving}
                            className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-sm font-medium text-white transition-opacity"
                            style={{ background: 'var(--pc-accent)', opacity: saving ? 0.6 : 1 }}
                        >
                            {saving ? <Loader2 className="w-3.5 h-3.5 animate-spin" /> : <Save className="w-3.5 h-3.5" />}
                            保存
                        </button>
                    </div>
                    <textarea
                        value={identity}
                        onChange={(e) => setIdentity(e.target.value)}
                        rows={18}
                        className="w-full rounded-xl border p-4 font-mono text-sm resize-y focus:outline-none focus:ring-2"
                        style={{
                            background: 'var(--pc-bg-secondary)',
                            borderColor: 'var(--pc-border)',
                            color: 'var(--pc-text-primary)',
                        }}
                    />
                    {saveMsg && (
                        <p className="text-sm mt-2" style={{ color: saveMsg.startsWith('错误') ? '#ef4444' : '#22c55e' }}>
                            {saveMsg}
                        </p>
                    )}
                </div>
            )}

            {tab === 'soul' && (
                <div>
                    <div className="flex items-center justify-between mb-3">
                        <div className="flex items-center gap-2">
                            <Sparkles className="w-4 h-4" style={{ color: 'var(--pc-text-secondary)' }} />
                            <span className="text-sm font-medium" style={{ color: 'var(--pc-text-primary)' }}>
                                SOUL.md
                            </span>
                        </div>
                        <button
                            type="button"
                            onClick={handleSaveIdentity}
                            disabled={saving}
                            className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-sm font-medium text-white transition-opacity"
                            style={{ background: 'var(--pc-accent)', opacity: saving ? 0.6 : 1 }}
                        >
                            {saving ? <Loader2 className="w-3.5 h-3.5 animate-spin" /> : <Save className="w-3.5 h-3.5" />}
                            保存
                        </button>
                    </div>
                    <textarea
                        value={soul}
                        onChange={(e) => setSoul(e.target.value)}
                        rows={18}
                        className="w-full rounded-xl border p-4 font-mono text-sm resize-y focus:outline-none focus:ring-2"
                        style={{
                            background: 'var(--pc-bg-secondary)',
                            borderColor: 'var(--pc-border)',
                            color: 'var(--pc-text-primary)',
                        }}
                    />
                    {saveMsg && (
                        <p className="text-sm mt-2" style={{ color: saveMsg.startsWith('错误') ? '#ef4444' : '#22c55e' }}>
                            {saveMsg}
                        </p>
                    )}
                </div>
            )}

            {tab === 'skills' && (
                <div className="flex gap-5">
                    {/* Skill list sidebar */}
                    <div className="w-48 shrink-0">
                        <div className="flex items-center justify-between mb-3">
                            <span className="text-sm font-medium" style={{ color: 'var(--pc-text-primary)' }}>
                                Skills
                            </span>
                            <span className="text-xs" style={{ color: 'var(--pc-text-secondary)' }}>
                                {skills.length}
                            </span>
                        </div>

                        <div className="space-y-1 mb-4">
                            {skills.map((name) => (
                                <button
                                    key={name}
                                    type="button"
                                    onClick={() => handleSelectSkill(name)}
                                    className={`w-full text-left px-3 py-2 rounded-lg text-sm transition-colors ${selectedSkill === name ? 'font-medium' : ''
                                        }`}
                                    style={{
                                        background: selectedSkill === name ? 'var(--pc-bg-secondary)' : 'transparent',
                                        color:
                                            selectedSkill === name
                                                ? 'var(--pc-accent)'
                                                : 'var(--pc-text-secondary)',
                                    }}
                                >
                                    {name}
                                </button>
                            ))}
                        </div>

                        {/* New skill */}
                        <div className="flex gap-1">
                            <input
                                value={newSkillName}
                                onChange={(e) => setNewSkillName(e.target.value)}
                                placeholder="new-skill"
                                className="flex-1 min-w-0 px-2 py-1.5 rounded-lg border text-xs"
                                style={{
                                    background: 'var(--pc-bg-secondary)',
                                    borderColor: 'var(--pc-border)',
                                    color: 'var(--pc-text-primary)',
                                }}
                                onKeyDown={(e) => e.key === 'Enter' && handleCreateSkill()}
                            />
                            <button
                                type="button"
                                onClick={handleCreateSkill}
                                className="p-1.5 rounded-lg"
                                style={{ background: 'var(--pc-accent)' }}
                            >
                                <Plus className="w-3.5 h-3.5 text-white" />
                            </button>
                        </div>
                    </div>

                    {/* Skill editor */}
                    <div className="flex-1 min-w-0">
                        {selectedSkill ? (
                            <>
                                <div className="flex items-center justify-between mb-3">
                                    <span className="text-sm font-medium" style={{ color: 'var(--pc-text-primary)' }}>
                                        {selectedSkill}/SKILL.md
                                    </span>
                                    <button
                                        type="button"
                                        onClick={handleSaveSkill}
                                        disabled={saving}
                                        className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-sm font-medium text-white transition-opacity"
                                        style={{ background: 'var(--pc-accent)', opacity: saving ? 0.6 : 1 }}
                                    >
                                        {saving ? (
                                            <Loader2 className="w-3.5 h-3.5 animate-spin" />
                                        ) : (
                                            <Save className="w-3.5 h-3.5" />
                                        )}
                                        保存
                                    </button>
                                </div>
                                <textarea
                                    value={skillContent}
                                    onChange={(e) => setSkillContent(e.target.value)}
                                    rows={20}
                                    className="w-full rounded-xl border p-4 font-mono text-sm resize-y focus:outline-none focus:ring-2"
                                    style={{
                                        background: 'var(--pc-bg-secondary)',
                                        borderColor: 'var(--pc-border)',
                                        color: 'var(--pc-text-primary)',
                                    }}
                                />
                                {saveMsg && (
                                    <p
                                        className="text-sm mt-2"
                                        style={{ color: saveMsg.startsWith('错误') ? '#ef4444' : '#22c55e' }}
                                    >
                                        {saveMsg}
                                    </p>
                                )}
                            </>
                        ) : (
                            <div className="flex items-center justify-center h-48 text-sm" style={{ color: 'var(--pc-text-secondary)' }}>
                                选择或创建一个 Skill
                            </div>
                        )}
                    </div>
                </div>
            )}

            {tab === 'chat' && (
                <div className="flex flex-col h-[520px]">
                    {/* Message list */}
                    <div
                        className="flex-1 overflow-y-auto rounded-xl border p-4 space-y-3 mb-3"
                        style={{ background: 'var(--pc-bg-secondary)', borderColor: 'var(--pc-border)' }}
                    >
                        {chatMessages.length === 0 && (
                            <div className="flex items-center justify-center h-full text-sm" style={{ color: 'var(--pc-text-secondary)' }}>
                                向 {agent.display_name} 发送消息吧
                            </div>
                        )}
                        {chatMessages.map((msg, i) => (
                            <div
                                key={i}
                                className={`flex ${msg.role === 'user' ? 'justify-end' : 'justify-start'}`}
                            >
                                <div
                                    className="max-w-[75%] px-4 py-2.5 rounded-2xl text-sm whitespace-pre-wrap leading-relaxed"
                                    style={
                                        msg.role === 'user'
                                            ? { background: 'var(--pc-accent)', color: '#fff' }
                                            : { background: 'var(--pc-bg-primary)', color: 'var(--pc-text-primary)', border: '1px solid var(--pc-border)' }
                                    }
                                >
                                    {msg.text}
                                </div>
                            </div>
                        ))}
                        {chatLoading && (
                            <div className="flex justify-start">
                                <div
                                    className="px-4 py-2.5 rounded-2xl text-sm"
                                    style={{ background: 'var(--pc-bg-primary)', color: 'var(--pc-text-secondary)', border: '1px solid var(--pc-border)' }}
                                >
                                    <Loader2 className="w-4 h-4 animate-spin inline" /> 思考中…
                                </div>
                            </div>
                        )}
                        <div ref={chatEndRef} />
                    </div>

                    {/* Input row */}
                    <div className="flex gap-2">
                        <input
                            value={chatInput}
                            onChange={(e) => setChatInput(e.target.value)}
                            onKeyDown={(e) => e.key === 'Enter' && !e.shiftKey && handleSendMessage()}
                            placeholder="输入消息…"
                            disabled={chatLoading}
                            className="flex-1 px-4 py-2.5 rounded-xl border text-sm focus:outline-none focus:ring-2"
                            style={{
                                background: 'var(--pc-bg-secondary)',
                                borderColor: 'var(--pc-border)',
                                color: 'var(--pc-text-primary)',
                            }}
                        />
                        <button
                            type="button"
                            onClick={handleSendMessage}
                            disabled={chatLoading || !chatInput.trim()}
                            className="px-4 py-2.5 rounded-xl text-white font-medium flex items-center gap-1.5 text-sm transition-opacity"
                            style={{ background: 'var(--pc-accent)', opacity: chatLoading || !chatInput.trim() ? 0.5 : 1 }}
                        >
                            <SendHorizonal className="w-4 h-4" /> 发送
                        </button>
                    </div>
                </div>
            )}
        </div>
    );
}
