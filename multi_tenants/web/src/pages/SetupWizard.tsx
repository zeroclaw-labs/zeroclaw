import { useState, useEffect, useRef } from 'react';
import { useParams, useNavigate } from 'react-router-dom';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { Zap, CheckCircle2, XCircle, Circle, Loader2 } from 'lucide-react';
import {
  listTenants, updateTenantConfig, deployTenant, getTenantStatus, testProvider, type Tenant,
} from '../api/tenants';
import { createChannel } from '../api/channels';
import Layout from '../components/Layout';
import { useToast } from '../hooks/useToast';
import { PROVIDERS, getModels } from '../config/providerSchemas';
import { CHANNEL_SCHEMAS } from '../config/channelSchemas';

const STEPS = ['Provider', 'Agent', 'Channels', 'Deploy'] as const;
type Step = typeof STEPS[number];

export default function SetupWizard() {
  const { id } = useParams<{ id: string }>();
  const navigate = useNavigate();
  const qc = useQueryClient();
  const toast = useToast();
  const [step, setStep] = useState<Step>('Provider');

  // Provider step state
  const [provider, setProvider] = useState('openai');
  const [model, setModel] = useState('gpt-4o');
  const [customModel, setCustomModel] = useState('');
  const [apiKey, setApiKey] = useState('');
  const [testResult, setTestResult] = useState<{ success: boolean; message: string } | null>(null);
  const [testing, setTesting] = useState(false);

  // Agent step state
  const [systemPrompt, setSystemPrompt] = useState('');
  const [temperature, setTemperature] = useState(0.7);
  const [autonomy, setAutonomy] = useState('supervised');

  // Channel step state
  const [selectedChannel, setSelectedChannel] = useState<string | null>(null);
  const [channelFields, setChannelFields] = useState<Record<string, string>>({});
  const [channelsAdded, setChannelsAdded] = useState<string[]>([]);

  // Deploy step state
  const [deployStatus, setDeployStatus] = useState<'idle' | 'deploying' | 'done' | 'error'>('idle');
  const [deployPhase, setDeployPhase] = useState('');
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  useEffect(() => {
    return () => { if (pollRef.current) clearInterval(pollRef.current); };
  }, []);

  const { data: tenants = [] } = useQuery({
    queryKey: ['tenants'],
    queryFn: listTenants,
  });
  const tenant = tenants.find((t: Tenant) => t.id === id);

  const isCustomModel = model === '__custom__';
  const effectiveModel = isCustomModel ? customModel : model;
  const availableModels = getModels(provider);
  const providerDef = PROVIDERS.find(p => p.id === provider);

  function handleProviderChange(newProvider: string) {
    const models = getModels(newProvider);
    setProvider(newProvider);
    setModel(models.length > 0 ? models[0].id : '__custom__');
    setCustomModel('');
    setTestResult(null);
  }

  async function handleTestConnection() {
    if (!apiKey || !id) return;
    setTesting(true);
    setTestResult(null);
    try {
      const result = await testProvider(id, { provider, api_key: apiKey, model: effectiveModel });
      setTestResult(result);
    } catch {
      setTestResult({ success: false, message: 'Connection test failed' });
    } finally {
      setTesting(false);
    }
  }

  const configMut = useMutation({
    mutationFn: (data: Record<string, unknown>) => updateTenantConfig(id!, data),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['config', id] });
    },
    onError: (err: Error) => toast.error(err.message || 'Failed to save config'),
  });

  async function handleProviderNext() {
    if (!apiKey) { toast.error('API key is required'); return; }
    await configMut.mutateAsync({
      provider,
      model: effectiveModel,
      api_key: apiKey,
    });
    setStep('Agent');
  }

  async function handleAgentNext() {
    const patch: Record<string, unknown> = {
      temperature,
      autonomy_level: autonomy,
    };
    if (systemPrompt) patch.system_prompt = systemPrompt;
    await configMut.mutateAsync(patch);
    setStep('Channels');
  }

  const channelMut = useMutation({
    mutationFn: (data: { kind: string; config: Record<string, unknown> }) => createChannel(id!, data),
    onSuccess: (_data, vars) => {
      setChannelsAdded(prev => [...prev, vars.kind]);
      setSelectedChannel(null);
      setChannelFields({});
      toast.success(`${vars.kind} channel added`);
      qc.invalidateQueries({ queryKey: ['channels', id] });
    },
    onError: (err: Error) => toast.error(err.message || 'Failed to add channel'),
  });

  function handleAddChannel(e: React.FormEvent) {
    e.preventDefault();
    if (!selectedChannel) return;
    const config: Record<string, unknown> = {};
    const schema = CHANNEL_SCHEMAS[selectedChannel] ?? [];
    for (const field of schema) {
      if (channelFields[field.key]) config[field.key] = channelFields[field.key];
    }
    channelMut.mutate({ kind: selectedChannel, config });
  }

  async function handleDeploy() {
    if (!id) return;
    setDeployStatus('deploying');
    setDeployPhase('Initiating deployment...');

    pollRef.current = setInterval(async () => {
      try {
        const s = await getTenantStatus(id);
        const statusMap: Record<string, string> = {
          provisioning: 'Creating filesystem...',
          creating: 'Creating container...',
          starting: 'Starting container...',
          running: 'Running',
        };
        setDeployPhase(statusMap[s.status] || s.status);

        if (s.status === 'running') {
          if (pollRef.current) clearInterval(pollRef.current);
          setDeployStatus('done');
          qc.invalidateQueries({ queryKey: ['tenants'] });
        } else if (s.status === 'error') {
          if (pollRef.current) clearInterval(pollRef.current);
          setDeployStatus('error');
        }
      } catch { /* ignore poll errors */ }
    }, 2000);

    try {
      const result = await deployTenant(id);
      if (result.status === 'running') {
        if (pollRef.current) clearInterval(pollRef.current);
        setDeployStatus('done');
        qc.invalidateQueries({ queryKey: ['tenants'] });
      } else if (result.status === 'error') {
        if (pollRef.current) clearInterval(pollRef.current);
        setDeployStatus('error');
      }
    } catch {
      if (pollRef.current) clearInterval(pollRef.current);
      setDeployStatus('error');
    }
  }

  if (!tenant) return <Layout><div className="flex items-center gap-2 text-text-muted"><Loader2 className="h-5 w-5 animate-spin" /><span>Loading...</span></div></Layout>;

  if (tenant.status !== 'draft') {
    navigate(`/tenants/${id}`, { replace: true });
    return null;
  }

  const stepIndex = STEPS.indexOf(step);

  return (
    <Layout>
      <div className="max-w-2xl mx-auto">
        <h1 className="text-2xl font-bold text-text-primary mb-1">Setup: {tenant.name}</h1>
        <p className="text-sm text-text-muted font-mono mb-6">{tenant.slug}</p>

        {/* Step indicator */}
        <div className="flex items-center mb-8">
          {STEPS.map((s, i) => (
            <div key={s} className="flex items-center flex-1">
              <div className={`w-8 h-8 rounded-full flex items-center justify-center text-sm font-medium transition-colors ${
                i < stepIndex ? 'bg-green-500 text-white'
                : i === stepIndex ? 'bg-accent-blue text-white'
                : 'bg-gray-700 text-text-muted'
              }`}>
                {i < stepIndex ? <CheckCircle2 className="h-4 w-4" /> : i + 1}
              </div>
              <span className={`ml-2 text-sm ${i === stepIndex ? 'font-medium text-text-primary' : i < stepIndex ? 'text-green-400' : 'text-text-muted'}`}>{s}</span>
              {i < STEPS.length - 1 && <div className={`flex-1 h-px mx-3 ${i < stepIndex ? 'bg-green-500' : 'bg-border-default'}`} />}
            </div>
          ))}
        </div>

        {/* Step 1: Provider */}
        {step === 'Provider' && (
          <div className="card p-6">
            <h2 className="text-lg font-semibold text-text-primary mb-4">Configure Provider</h2>
            <div className="space-y-4">
              <div>
                <label className="block text-sm font-medium text-text-secondary mb-1">Provider</label>
                <select value={provider} onChange={e => handleProviderChange(e.target.value)}
                  className="w-full px-3 py-2 bg-bg-input border border-border-default rounded-lg text-sm text-text-primary focus:outline-none focus:ring-2 focus:ring-accent-blue focus:border-transparent transition-colors">
                  {PROVIDERS.map(p => <option key={p.id} value={p.id}>{p.label}</option>)}
                </select>
              </div>
              <div>
                <label className="block text-sm font-medium text-text-secondary mb-1">Model</label>
                <select value={model} onChange={e => { setModel(e.target.value); setCustomModel(''); }}
                  className="w-full px-3 py-2 bg-bg-input border border-border-default rounded-lg text-sm text-text-primary focus:outline-none focus:ring-2 focus:ring-accent-blue focus:border-transparent transition-colors">
                  {availableModels.map(m => (
                    <option key={m.id} value={m.id}>{m.label}{m.context ? ` (${m.context})` : ''}</option>
                  ))}
                  <option value="__custom__">Custom model...</option>
                </select>
                {isCustomModel && (
                  <input value={customModel} onChange={e => setCustomModel(e.target.value)}
                    placeholder="Enter model ID" required
                    className="w-full mt-2 px-3 py-2 bg-bg-input border border-border-default rounded-lg text-sm text-text-primary placeholder-text-muted focus:outline-none focus:ring-2 focus:ring-accent-blue focus:border-transparent transition-colors" />
                )}
              </div>
              <div>
                <label className="block text-sm font-medium text-text-secondary mb-1">API Key</label>
                <input type="password" value={apiKey} onChange={e => { setApiKey(e.target.value); setTestResult(null); }}
                  placeholder={providerDef?.keyPlaceholder ?? 'sk-...'}
                  className="w-full px-3 py-2 bg-bg-input border border-border-default rounded-lg text-sm text-text-primary placeholder-text-muted focus:outline-none focus:ring-2 focus:ring-accent-blue focus:border-transparent transition-colors" />
                {providerDef?.keyHelp && (
                  <p className="text-xs text-text-muted mt-1">
                    Get key: <a href={providerDef.keyHelp} target="_blank" rel="noopener noreferrer" className="text-accent-blue hover:text-accent-blue-hover transition-colors">{providerDef.keyHelp}</a>
                  </p>
                )}
              </div>
              <div className="flex items-center gap-3">
                <button type="button" onClick={handleTestConnection} disabled={testing || !apiKey}
                  className="px-3 py-1.5 text-sm border border-border-default text-text-secondary rounded-lg hover:bg-bg-card-hover disabled:opacity-50 transition-colors flex items-center gap-1.5">
                  {testing ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Zap className="h-3.5 w-3.5" />}
                  {testing ? 'Testing...' : 'Test Connection'}
                </button>
                {testResult && (
                  <span className={`text-sm ${testResult.success ? 'text-green-400' : 'text-red-400'}`}>
                    {testResult.message}
                  </span>
                )}
              </div>
            </div>
            <div className="flex justify-end mt-6">
              <button onClick={handleProviderNext} disabled={!apiKey || configMut.isPending}
                className="px-4 py-2 bg-accent-blue text-white rounded-lg text-sm hover:bg-accent-blue-hover disabled:opacity-50 transition-colors flex items-center gap-2 font-medium">
                {configMut.isPending && <Loader2 className="h-4 w-4 animate-spin" />}
                {configMut.isPending ? 'Saving...' : 'Next'}
              </button>
            </div>
          </div>
        )}

        {/* Step 2: Agent */}
        {step === 'Agent' && (
          <div className="card p-6">
            <h2 className="text-lg font-semibold text-text-primary mb-4">Agent Configuration</h2>
            <div className="space-y-4">
              <div>
                <label className="block text-sm font-medium text-text-secondary mb-1">System Prompt</label>
                <textarea value={systemPrompt} onChange={e => setSystemPrompt(e.target.value)}
                  rows={5}
                  className="w-full px-3 py-2 bg-bg-input border border-border-default rounded-lg text-sm text-text-primary font-mono placeholder-text-muted focus:outline-none focus:ring-2 focus:ring-accent-blue focus:border-transparent transition-colors"
                  placeholder="You are a helpful assistant..." />
                <p className="text-xs text-text-muted mt-1">Optional. Defines the agent's personality and behavior.</p>
              </div>
              <div className="grid grid-cols-2 gap-4">
                <div>
                  <label className="block text-sm font-medium text-text-secondary mb-1">Temperature: {temperature}</label>
                  <input type="range" min="0" max="2" step="0.1"
                    value={temperature} onChange={e => setTemperature(parseFloat(e.target.value))}
                    className="w-full accent-accent-blue" />
                  <div className="flex justify-between text-xs text-text-muted">
                    <span>Precise</span><span>Creative</span>
                  </div>
                </div>
                <div>
                  <label className="block text-sm font-medium text-text-secondary mb-1">Autonomy Level</label>
                  <select value={autonomy} onChange={e => setAutonomy(e.target.value)}
                    className="w-full px-3 py-2 bg-bg-input border border-border-default rounded-lg text-sm text-text-primary focus:outline-none focus:ring-2 focus:ring-accent-blue focus:border-transparent transition-colors">
                    <option value="supervised">Supervised</option>
                    <option value="semi-autonomous">Semi-Autonomous</option>
                    <option value="autonomous">Autonomous</option>
                  </select>
                </div>
              </div>
            </div>
            <div className="flex justify-between mt-6">
              <button onClick={() => setStep('Provider')}
                className="px-4 py-2 text-sm border border-border-default text-text-secondary rounded-lg hover:bg-bg-card-hover transition-colors">Back</button>
              <button onClick={handleAgentNext} disabled={configMut.isPending}
                className="px-4 py-2 bg-accent-blue text-white rounded-lg text-sm hover:bg-accent-blue-hover disabled:opacity-50 transition-colors flex items-center gap-2 font-medium">
                {configMut.isPending && <Loader2 className="h-4 w-4 animate-spin" />}
                {configMut.isPending ? 'Saving...' : 'Next'}
              </button>
            </div>
          </div>
        )}

        {/* Step 3: Channels */}
        {step === 'Channels' && (
          <div className="card p-6">
            <h2 className="text-lg font-semibold text-text-primary mb-4">Connect Channels</h2>
            <p className="text-sm text-text-secondary mb-4">Add messaging channels for your agent. You can skip this and add them later.</p>

            {channelsAdded.length > 0 && (
              <div className="mb-4 p-3 bg-green-900/20 border border-green-700/50 rounded-lg text-sm text-green-400">
                Added: {channelsAdded.join(', ')}
              </div>
            )}

            {!selectedChannel ? (
              <div className="grid grid-cols-2 gap-3 mb-4">
                {Object.keys(CHANNEL_SCHEMAS).map(kind => (
                  <button key={kind} onClick={() => { setSelectedChannel(kind); setChannelFields({}); }}
                    disabled={channelsAdded.includes(kind)}
                    className={`p-4 border rounded-xl text-left transition-colors disabled:opacity-50 disabled:cursor-not-allowed ${
                      channelsAdded.includes(kind)
                        ? 'bg-green-900/20 border-green-700/50'
                        : 'border-border-default hover:border-accent-blue hover:bg-accent-blue/5'
                    }`}>
                    <div className="font-medium text-sm capitalize text-text-primary">{kind}</div>
                    <div className="text-xs text-text-muted mt-1">
                      {CHANNEL_SCHEMAS[kind].filter(f => f.required).length} required field(s)
                    </div>
                  </button>
                ))}
              </div>
            ) : (
              <form onSubmit={handleAddChannel} className="mb-4">
                <div className="flex items-center justify-between mb-3">
                  <h3 className="font-medium text-sm capitalize text-text-primary">{selectedChannel} Configuration</h3>
                  <button type="button" onClick={() => { setSelectedChannel(null); setChannelFields({}); }}
                    className="text-xs text-text-muted hover:text-text-secondary transition-colors">Cancel</button>
                </div>
                {(CHANNEL_SCHEMAS[selectedChannel] ?? []).map(field => (
                  <div key={field.key} className="mb-3">
                    <label className="block text-sm font-medium text-text-secondary mb-1">
                      {field.label}{field.required && ' *'}
                    </label>
                    <input type={field.type}
                      value={channelFields[field.key] ?? ''}
                      onChange={e => setChannelFields(f => ({ ...f, [field.key]: e.target.value }))}
                      required={field.required}
                      placeholder={field.placeholder ?? ''}
                      className="w-full px-3 py-2 bg-bg-input border border-border-default rounded-lg text-sm text-text-primary placeholder-text-muted focus:outline-none focus:ring-2 focus:ring-accent-blue focus:border-transparent transition-colors" />
                    {field.help && <p className="text-xs text-text-muted mt-1">{field.help}</p>}
                  </div>
                ))}
                <button type="submit" disabled={channelMut.isPending}
                  className="px-4 py-2 bg-accent-blue text-white rounded-lg text-sm hover:bg-accent-blue-hover disabled:opacity-50 transition-colors flex items-center gap-2 font-medium">
                  {channelMut.isPending && <Loader2 className="h-4 w-4 animate-spin" />}
                  {channelMut.isPending ? 'Adding...' : 'Add Channel'}
                </button>
              </form>
            )}

            <div className="flex justify-between mt-6 pt-4 border-t border-border-default">
              <button onClick={() => setStep('Agent')}
                className="px-4 py-2 text-sm border border-border-default text-text-secondary rounded-lg hover:bg-bg-card-hover transition-colors">Back</button>
              <button onClick={() => setStep('Deploy')}
                className="px-4 py-2 bg-accent-blue text-white rounded-lg text-sm hover:bg-accent-blue-hover transition-colors font-medium">
                {channelsAdded.length === 0 ? 'Skip & Deploy' : 'Next'}
              </button>
            </div>
          </div>
        )}

        {/* Step 4: Deploy */}
        {step === 'Deploy' && (
          <div className="card p-6">
            <h2 className="text-lg font-semibold text-text-primary mb-4">Deploy Agent</h2>

            <div className="mb-6 p-4 bg-bg-secondary rounded-xl text-sm space-y-2 border border-border-subtle">
              <div className="flex justify-between">
                <span className="text-text-muted">Provider</span>
                <span className="font-medium text-text-primary">{PROVIDERS.find(p => p.id === provider)?.label ?? provider}</span>
              </div>
              <div className="flex justify-between">
                <span className="text-text-muted">Model</span>
                <span className="font-medium text-text-primary">{effectiveModel}</span>
              </div>
              <div className="flex justify-between">
                <span className="text-text-muted">Temperature</span>
                <span className="font-medium text-text-primary">{temperature}</span>
              </div>
              <div className="flex justify-between">
                <span className="text-text-muted">Channels</span>
                <span className="font-medium text-text-primary">{channelsAdded.length > 0 ? channelsAdded.join(', ') : 'None'}</span>
              </div>
            </div>

            {deployStatus === 'idle' && (
              <button onClick={handleDeploy}
                className="w-full px-4 py-3 bg-accent-blue text-white rounded-xl text-sm font-medium hover:bg-accent-blue-hover transition-colors flex items-center justify-center gap-2">
                <Zap className="h-4 w-4" />
                Deploy Agent
              </button>
            )}

            {deployStatus === 'deploying' && (
              <div className="text-center py-8">
                <Loader2 className="h-8 w-8 animate-spin text-accent-blue mx-auto mb-4" />
                <p className="text-sm text-text-secondary">Deploying your agent...</p>
                <p className="text-xs text-text-muted mt-1">{deployPhase}</p>
                <div className="mt-4 space-y-2 text-left max-w-xs mx-auto">
                  {['Creating filesystem...', 'Creating container...', 'Starting container...', 'Running'].map((phase, i) => {
                    const phases = ['Creating filesystem...', 'Creating container...', 'Starting container...', 'Running'];
                    const currentIdx = phases.indexOf(deployPhase);
                    const isDone = i < currentIdx;
                    const isCurrent = i === currentIdx;
                    return (
                      <div key={phase} className={`flex items-center gap-2 text-xs ${isDone ? 'text-green-400' : isCurrent ? 'text-text-primary font-medium' : 'text-text-muted'}`}>
                        {isDone ? <CheckCircle2 className="h-3.5 w-3.5 text-green-400" /> : isCurrent ? <Loader2 className="h-3.5 w-3.5 animate-spin text-accent-blue" /> : <Circle className="h-3.5 w-3.5 text-text-muted" />}
                        <span>{phase === 'Running' ? 'Health check passed' : phase}</span>
                      </div>
                    );
                  })}
                </div>
              </div>
            )}

            {deployStatus === 'done' && (
              <div className="text-center py-8">
                <CheckCircle2 className="h-12 w-12 text-green-400 mx-auto mb-4" />
                <p className="font-medium text-green-400 mb-2">Agent deployed successfully!</p>
                <button onClick={() => navigate(`/tenants/${id}`)}
                  className="px-4 py-2 bg-accent-blue text-white rounded-lg text-sm hover:bg-accent-blue-hover transition-colors font-medium">
                  Go to Dashboard
                </button>
              </div>
            )}

            {deployStatus === 'error' && (
              <div className="text-center py-8">
                <XCircle className="h-12 w-12 text-red-400 mx-auto mb-4" />
                <p className="font-medium text-red-400 mb-2">Deployment failed</p>
                <p className="text-sm text-text-muted mb-4">Check logs for details or try again.</p>
                <div className="flex justify-center gap-2">
                  <button onClick={() => { setDeployStatus('idle'); }}
                    className="px-4 py-2 text-sm border border-border-default text-text-secondary rounded-lg hover:bg-bg-card-hover transition-colors">Retry</button>
                  <button onClick={() => navigate(`/tenants/${id}`)}
                    className="px-4 py-2 text-sm border border-border-default text-text-secondary rounded-lg hover:bg-bg-card-hover transition-colors">View Tenant</button>
                </div>
              </div>
            )}

            {deployStatus === 'idle' && (
              <div className="flex justify-start mt-4">
                <button onClick={() => setStep('Channels')}
                  className="px-4 py-2 text-sm border border-border-default text-text-secondary rounded-lg hover:bg-bg-card-hover transition-colors">Back</button>
              </div>
            )}
          </div>
        )}
      </div>
    </Layout>
  );
}
