import { useState } from 'react';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { Plus, Pencil, Trash2, Loader2, Radio, AlertTriangle } from 'lucide-react';
import { listChannels, createChannel, getChannel, updateChannel, deleteChannel, type Channel } from '../api/channels';
import { CHANNEL_SCHEMAS, PLAN_LIMITS, type FieldDef } from '../config/channelSchemas';
import Modal from '../components/Modal';
import ConfirmModal from '../components/ConfirmModal';
import { useToast } from '../hooks/useToast';

const CHANNEL_KINDS = Object.keys(CHANNEL_SCHEMAS);

const KIND_LABELS: Record<string, string> = {
  telegram: 'Telegram',
  discord: 'Discord',
  slack: 'Slack',
  webhook: 'Webhook',
  mattermost: 'Mattermost',
  whatsapp: 'WhatsApp',
  email: 'Email',
  irc: 'IRC',
  matrix: 'Matrix',
  signal: 'Signal',
  lark: 'Lark',
  dingtalk: 'DingTalk',
  qq: 'QQ',
};

interface Props {
  tenantId: string;
  plan: string;
}

export default function ChannelsTab({ tenantId, plan }: Props) {
  const qc = useQueryClient();
  const toast = useToast();
  const planInfo = PLAN_LIMITS[plan] ?? PLAN_LIMITS.free;

  const [showAdd, setShowAdd] = useState(false);
  const [editId, setEditId] = useState<string | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<Channel | null>(null);

  const { data: channels = [], isLoading } = useQuery({
    queryKey: ['channels', tenantId],
    queryFn: () => listChannels(tenantId),
  });

  const toggleMut = useMutation({
    mutationFn: ({ id, enabled }: { id: string; enabled: boolean }) =>
      updateChannel(tenantId, id, { enabled }),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['channels', tenantId] });
      toast.success('Channel updated. Agent will restart.');
    },
    onError: (err: Error) => toast.error(err.message || 'Failed to toggle channel'),
  });

  const deleteMut = useMutation({
    mutationFn: (id: string) => deleteChannel(tenantId, id),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['channels', tenantId] });
      setDeleteTarget(null);
      toast.success('Channel deleted. Agent will restart.');
    },
    onError: (err: Error) => toast.error(err.message || 'Failed to delete channel'),
  });

  const atLimit = channels.length >= planInfo.channels;

  return (
    <div>
      <div className="flex items-center justify-between mb-4">
        <div>
          <h2 className="text-lg font-semibold text-text-primary">Channels</h2>
          <p className="text-xs text-text-muted">
            {channels.length} / {planInfo.channels} channels connected
          </p>
        </div>
        <button
          onClick={() => setShowAdd(true)}
          disabled={atLimit}
          className="px-3 py-1.5 text-sm bg-accent-blue text-white rounded-lg hover:bg-accent-blue-hover disabled:opacity-50 disabled:cursor-not-allowed transition-colors flex items-center gap-1.5"
        >
          <Plus className="h-3.5 w-3.5" />
          Add Channel
        </button>
      </div>

      {atLimit && (
        <div className="bg-amber-900/20 border border-amber-700/50 rounded-lg p-3 mb-4 flex items-start gap-2">
          <AlertTriangle className="h-4 w-4 text-amber-400 flex-shrink-0 mt-0.5" />
          <p className="text-xs text-amber-300">
            Channel limit reached ({planInfo.channels} on {plan} plan). Upgrade to add more channels.
          </p>
        </div>
      )}

      {isLoading ? (
        <div className="flex items-center gap-2 text-text-muted">
          <Loader2 className="h-5 w-5 animate-spin" />
          <span>Loading channels...</span>
        </div>
      ) : channels.length === 0 ? (
        <div className="card p-8 text-center">
          <Radio className="h-8 w-8 text-text-muted mx-auto mb-3" />
          <p className="text-text-muted mb-2">No channels connected</p>
          <p className="text-xs text-text-muted mb-4">
            Connect a messaging platform to start receiving messages.
          </p>
          <button
            onClick={() => setShowAdd(true)}
            className="px-4 py-2 text-sm bg-accent-blue text-white rounded-lg hover:bg-accent-blue-hover transition-colors inline-flex items-center gap-1.5"
          >
            <Plus className="h-3.5 w-3.5" />
            Add Your First Channel
          </button>
        </div>
      ) : (
        <div className="space-y-3">
          {channels.map(ch => (
            <div key={ch.id} className="card p-4 flex items-center justify-between">
              <div className="flex items-center gap-3 min-w-0">
                <div className={`w-2 h-2 rounded-full flex-shrink-0 ${ch.enabled ? 'bg-green-500' : 'bg-gray-500'}`} />
                <div className="min-w-0">
                  <p className="text-sm font-medium text-text-primary">
                    {KIND_LABELS[ch.kind] ?? ch.kind}
                  </p>
                  <p className="text-xs text-text-muted">
                    {ch.enabled ? 'Active' : 'Disabled'} &middot; Added {ch.created_at}
                  </p>
                </div>
              </div>
              <div className="flex items-center gap-2 flex-shrink-0">
                <button
                  onClick={() => toggleMut.mutate({ id: ch.id, enabled: !ch.enabled })}
                  className={`px-2.5 py-1 text-xs rounded-lg border transition-colors ${
                    ch.enabled
                      ? 'border-border-default text-text-muted hover:bg-bg-card-hover'
                      : 'border-green-700/50 text-green-400 hover:bg-green-900/20'
                  }`}
                >
                  {ch.enabled ? 'Disable' : 'Enable'}
                </button>
                <button
                  onClick={() => setEditId(ch.id)}
                  className="px-2.5 py-1 text-xs border border-border-default text-text-secondary rounded-lg hover:bg-bg-card-hover transition-colors inline-flex items-center gap-1"
                >
                  <Pencil className="h-3 w-3" />
                  Edit
                </button>
                <button
                  onClick={() => setDeleteTarget(ch)}
                  className="px-2.5 py-1 text-xs border border-red-700/50 text-red-400 rounded-lg hover:bg-red-900/20 transition-colors inline-flex items-center gap-1"
                >
                  <Trash2 className="h-3 w-3" />
                </button>
              </div>
            </div>
          ))}
        </div>
      )}

      <AddChannelModal
        open={showAdd}
        onClose={() => setShowAdd(false)}
        tenantId={tenantId}
      />

      {editId && (
        <EditChannelModal
          open
          onClose={() => setEditId(null)}
          tenantId={tenantId}
          channelId={editId}
        />
      )}

      <ConfirmModal
        open={!!deleteTarget}
        onClose={() => setDeleteTarget(null)}
        onConfirm={() => deleteTarget && deleteMut.mutate(deleteTarget.id)}
        title="Delete Channel"
        message={`Delete ${KIND_LABELS[deleteTarget?.kind ?? ''] ?? deleteTarget?.kind} channel? This will restart the agent container.`}
        confirmLabel="Delete"
        danger
        loading={deleteMut.isPending}
      />
    </div>
  );
}

/* ── Channel Config Form ─────────────────────────────────────── */

function ChannelConfigForm({ kind, values, onChange }: {
  kind: string;
  values: Record<string, string>;
  onChange: (values: Record<string, string>) => void;
}) {
  const fields = CHANNEL_SCHEMAS[kind] ?? [];

  function setField(key: string, val: string) {
    onChange({ ...values, [key]: val });
  }

  return (
    <div className="space-y-3">
      {fields.map((f: FieldDef) => (
        <div key={f.key}>
          <label className="block text-sm font-medium text-text-secondary mb-1">
            {f.label}
            {f.required && <span className="text-red-400 ml-0.5">*</span>}
          </label>
          <input
            type={f.type === 'number' ? 'number' : f.type === 'password' ? 'password' : f.type === 'url' ? 'url' : 'text'}
            value={values[f.key] ?? ''}
            onChange={e => setField(f.key, e.target.value)}
            placeholder={f.placeholder}
            required={f.required}
            className="w-full px-3 py-2 bg-bg-input border border-border-default rounded-lg text-sm text-text-primary placeholder-text-muted focus:outline-none focus:ring-2 focus:ring-accent-blue focus:border-transparent transition-colors"
          />
          {f.help && (
            <p className="text-xs text-text-muted mt-1">{f.help}</p>
          )}
        </div>
      ))}
    </div>
  );
}

/* ── Add Channel Modal ───────────────────────────────────────── */

function AddChannelModal({ open, onClose, tenantId }: {
  open: boolean; onClose: () => void; tenantId: string;
}) {
  const [step, setStep] = useState<'pick' | 'config'>('pick');
  const [selectedKind, setSelectedKind] = useState('');
  const [config, setConfig] = useState<Record<string, string>>({});
  const qc = useQueryClient();
  const toast = useToast();

  const createMut = useMutation({
    mutationFn: (data: { kind: string; config: Record<string, unknown> }) =>
      createChannel(tenantId, data),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['channels', tenantId] });
      toast.success('Channel added. Agent will restart.');
      handleClose();
    },
    onError: (err: Error) => toast.error(err.message || 'Failed to create channel'),
  });

  function handleClose() {
    setStep('pick');
    setSelectedKind('');
    setConfig({});
    onClose();
  }

  function pickKind(kind: string) {
    setSelectedKind(kind);
    setConfig({});
    setStep('config');
  }

  function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    const cleanConfig: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(config)) {
      if (v.trim()) cleanConfig[k] = v.trim();
    }
    createMut.mutate({ kind: selectedKind, config: cleanConfig });
  }

  return (
    <Modal open={open} onClose={handleClose} title={step === 'pick' ? 'Choose Channel Type' : `Add ${KIND_LABELS[selectedKind] ?? selectedKind}`}>
      {step === 'pick' ? (
        <div className="grid grid-cols-3 gap-2">
          {CHANNEL_KINDS.map(kind => (
            <button
              key={kind}
              onClick={() => pickKind(kind)}
              className="p-3 border border-border-default rounded-lg hover:bg-bg-card-hover hover:border-accent-blue/50 transition-colors text-center"
            >
              <p className="text-sm font-medium text-text-primary">{KIND_LABELS[kind] ?? kind}</p>
              <p className="text-xs text-text-muted mt-0.5">
                {CHANNEL_SCHEMAS[kind]?.filter(f => f.required).length ?? 0} required fields
              </p>
            </button>
          ))}
        </div>
      ) : (
        <form onSubmit={handleSubmit}>
          <ChannelConfigForm kind={selectedKind} values={config} onChange={setConfig} />

          <div className="flex items-center gap-2 mt-4 pt-4 border-t border-border-default">
            <p className="text-xs text-amber-400 flex-1 flex items-center gap-1">
              <AlertTriangle className="h-3 w-3" />
              This will restart the agent container
            </p>
            <button
              type="button"
              onClick={() => { setStep('pick'); setConfig({}); }}
              className="px-3 py-2 text-sm border border-border-default text-text-secondary rounded-lg hover:bg-bg-card-hover transition-colors"
            >
              Back
            </button>
            <button
              type="submit"
              disabled={createMut.isPending}
              className="px-4 py-2 text-sm bg-accent-blue text-white rounded-lg hover:bg-accent-blue-hover disabled:opacity-50 transition-colors flex items-center gap-2"
            >
              {createMut.isPending && <Loader2 className="h-4 w-4 animate-spin" />}
              {createMut.isPending ? 'Adding...' : 'Add Channel'}
            </button>
          </div>
        </form>
      )}
    </Modal>
  );
}

/* ── Edit Channel Modal ──────────────────────────────────────── */

function EditChannelModal({ open, onClose, tenantId, channelId }: {
  open: boolean; onClose: () => void; tenantId: string; channelId: string;
}) {
  const [config, setConfig] = useState<Record<string, string>>({});
  const [loaded, setLoaded] = useState(false);
  const qc = useQueryClient();
  const toast = useToast();

  const { data: detail, isLoading } = useQuery({
    queryKey: ['channel-detail', tenantId, channelId],
    queryFn: () => getChannel(tenantId, channelId),
    enabled: open,
  });

  // Populate form when detail loads
  if (detail && !loaded) {
    const vals: Record<string, string> = {};
    for (const [k, v] of Object.entries(detail.config)) {
      vals[k] = String(v ?? '');
    }
    setConfig(vals);
    setLoaded(true);
  }

  const saveMut = useMutation({
    mutationFn: (data: Record<string, unknown>) =>
      updateChannel(tenantId, channelId, { config: data }),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['channels', tenantId] });
      toast.success('Channel updated. Agent will restart.');
      handleClose();
    },
    onError: (err: Error) => toast.error(err.message || 'Failed to update channel'),
  });

  function handleClose() {
    setLoaded(false);
    setConfig({});
    onClose();
  }

  function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    const cleanConfig: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(config)) {
      // Skip masked values (backend returns "****" for secrets)
      if (v.trim() && v !== '****') cleanConfig[k] = v.trim();
    }
    saveMut.mutate(cleanConfig);
  }

  const kind = detail?.kind ?? '';

  return (
    <Modal open={open} onClose={handleClose} title={`Edit ${KIND_LABELS[kind] ?? kind} Channel`}>
      {isLoading || !detail ? (
        <div className="flex items-center gap-2 text-text-muted py-4">
          <Loader2 className="h-5 w-5 animate-spin" />
          <span>Loading channel config...</span>
        </div>
      ) : (
        <form onSubmit={handleSubmit}>
          <ChannelConfigForm kind={kind} values={config} onChange={setConfig} />

          <p className="text-xs text-text-muted mt-3">
            Masked fields (****) will keep their current value unless you enter a new one.
          </p>

          <div className="flex items-center gap-2 mt-4 pt-4 border-t border-border-default">
            <p className="text-xs text-amber-400 flex-1 flex items-center gap-1">
              <AlertTriangle className="h-3 w-3" />
              This will restart the agent container
            </p>
            <button
              type="button"
              onClick={handleClose}
              className="px-3 py-2 text-sm border border-border-default text-text-secondary rounded-lg hover:bg-bg-card-hover transition-colors"
            >
              Cancel
            </button>
            <button
              type="submit"
              disabled={saveMut.isPending}
              className="px-4 py-2 text-sm bg-accent-blue text-white rounded-lg hover:bg-accent-blue-hover disabled:opacity-50 transition-colors flex items-center gap-2"
            >
              {saveMut.isPending && <Loader2 className="h-4 w-4 animate-spin" />}
              {saveMut.isPending ? 'Saving...' : 'Save Changes'}
            </button>
          </div>
        </form>
      )}
    </Modal>
  );
}
