pub(crate) const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>ZeroClaw Relay</title>
  <style>
    :root { color-scheme: light dark; font-family: Inter, ui-sans-serif, system-ui, sans-serif; }
    body { margin: 0; min-height: 100vh; display: grid; place-items: start center; padding: 32px 0; background: #f7f8fa; color: #17191c; }
    [hidden] { display: none !important; }
    main { width: min(760px, calc(100vw - 32px)); display: grid; gap: 18px; }
    main.webui-active { width: min(1180px, calc(100vw - 32px)); }
    body.webui-active { display: block; padding: 0; background: #05070c; }
    body.webui-active main { width: 100%; min-height: 100vh; gap: 0; }
    body.webui-active h1, body.webui-active output, body.webui-active .webui-head { display: none; }
    body.webui-active .webui { gap: 0; }
    body.webui-active .webui iframe { min-height: 100vh; height: 100vh; border: 0; border-radius: 0; }
    h1 { margin: 0; font-size: 26px; font-weight: 700; letter-spacing: 0; }
    form { display: grid; gap: 12px; }
    label { display: grid; gap: 6px; font-size: 13px; font-weight: 600; }
    input { box-sizing: border-box; width: 100%; height: 42px; border: 1px solid #c9cdd4; border-radius: 6px; padding: 0 12px; font: inherit; background: #fff; color: #17191c; }
    textarea { box-sizing: border-box; width: 100%; min-height: 88px; resize: vertical; border: 1px solid #c9cdd4; border-radius: 6px; padding: 10px 12px; font: inherit; background: #fff; color: #17191c; }
    button { height: 42px; border: 0; border-radius: 6px; background: #1463ff; color: #fff; font: inherit; font-weight: 700; cursor: pointer; }
    .secondary { background: #4d5663; }
    button:disabled { opacity: .55; cursor: default; }
    section[hidden] { display: none; }
    .sas { display: grid; gap: 10px; }
    .sas code { font: 700 24px ui-monospace, SFMono-Regular, Menlo, monospace; letter-spacing: 0; }
    .actions { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; }
    .webui { display: grid; gap: 10px; }
    .webui-head { display: flex; align-items: center; justify-content: space-between; color: #4d5663; font-size: 13px; font-weight: 700; }
    .webui iframe { width: 100%; min-height: min(760px, calc(100vh - 170px)); border: 1px solid #d9dde4; border-radius: 6px; background: #fff; }
    output { min-height: 22px; font-size: 13px; color: #4d5663; }
    @media (prefers-color-scheme: dark) {
      body { background: #111316; color: #f3f5f7; }
      input, textarea, .webui iframe { background: #191d22; color: #f3f5f7; border-color: #343b45; }
      .webui-head { color: #b6beca; }
      output { color: #b6beca; }
    }
  </style>
</head>
<body>
  <main>
    <h1>ZeroClaw Relay</h1>
    <form id="pair">
      <label>Server ID <input id="server-id" autocomplete="off" spellcheck="false" required></label>
      <label>Pairing Code <input id="pairing-code" autocomplete="one-time-code" inputmode="numeric" required></label>
      <button id="connect" type="submit">Pair</button>
    </form>
    <section id="sas-panel" class="sas" hidden>
      <label>Daemon SAS <code id="sas-code">----</code></label>
      <div class="actions">
        <button id="sas-confirm" type="button">Confirm</button>
        <button id="sas-abort" class="secondary" type="button">Abort</button>
      </div>
    </section>
    <section id="webui-panel" class="webui" hidden>
      <div class="webui-head">
        <span>Remote WebUI</span>
        <span id="webui-state">Starting</span>
      </div>
      <iframe id="webui-frame" title="ZeroClaw WebUI" src="about:blank"></iframe>
    </section>
    <output id="status">Disconnected</output>
  </main>
  <script src="/app.js" defer></script>
</body>
</html>
"#;

pub(crate) const APP_JS: &str = r#"const form = document.getElementById('pair');
const main = document.querySelector('main');
const status = document.getElementById('status');
const button = document.getElementById('connect');
const sasPanel = document.getElementById('sas-panel');
const sasCode = document.getElementById('sas-code');
const sasConfirm = document.getElementById('sas-confirm');
const sasAbort = document.getElementById('sas-abort');
const webuiPanel = document.getElementById('webui-panel');
const webuiFrame = document.getElementById('webui-frame');
const webuiState = document.getElementById('webui-state');
const tunnel = new Worker('/tunnel-worker.js');
const SAVED_CONNECTION_KEY = 'zeroclaw_relay_last_connection';
let activeNodeId = '';
let openingWebUi = null;
let attemptedAutoResume = false;

if ('serviceWorker' in navigator) {
  navigator.serviceWorker.register('/sw.js', { scope: '/' }).then(async () => {
    await navigator.serviceWorker.ready;
    if (!navigator.serviceWorker.controller && sessionStorage.getItem('zeroclaw-sw-reload') !== '1') {
      sessionStorage.setItem('zeroclaw-sw-reload', '1');
      location.reload();
      return;
    }
    if (navigator.serviceWorker.controller) {
      sessionStorage.removeItem('zeroclaw-sw-reload');
    }
  }).catch(() => {});
  navigator.serviceWorker.addEventListener('controllerchange', () => {
    sessionStorage.removeItem('zeroclaw-sw-reload');
  });
  navigator.serviceWorker.addEventListener('message', (event) => {
    const msg = event.data || {};
    if (msg.type !== 'zeroclaw-rpc-request' || !event.ports?.length) {
      return;
    }
    forwardToTunnel(msg, event.ports);
  });
}

window.addEventListener('message', (event) => {
  if (event.origin !== location.origin) {
    return;
  }
  const msg = event.data || {};
  if (msg.type !== 'zeroclaw-rpc-request' || !event.ports?.length) {
    return;
  }
  forwardToTunnel(msg, event.ports);
});

tunnel.addEventListener('message', (event) => {
  const msg = event.data || {};
  if (msg.type === 'connecting') {
    status.textContent = 'Preparing client key';
  } else if (msg.type === 'resuming') {
    status.textContent = `Restoring saved connection to ${msg.nodeId || 'the daemon'}.`;
  } else if (msg.type === 'enrollment-material-ready') {
    status.textContent = 'Client key and CSR ready. Opening enrollment route.';
  } else if (msg.type === 'route-open') {
    status.textContent = 'Enrollment route open. Starting secure enrollment.';
  } else if (msg.type === 'enrollment-sas') {
    sasCode.textContent = msg.sas || '----';
    sasPanel.hidden = false;
    status.textContent = 'Confirm the daemon SAS to finish enrollment.';
    sasConfirm.disabled = false;
    sasAbort.disabled = false;
  } else if (msg.type === 'enrollment-complete') {
    sasPanel.hidden = true;
    status.textContent = `Enrolled as ${msg.deviceId || 'this browser'}.`;
    rememberConnection(activeNodeId);
    button.disabled = false;
  } else if (msg.type === 'rpc-ready') {
    if (msg.nodeId) {
      activeNodeId = msg.nodeId;
      rememberConnection(msg.nodeId);
    }
    status.textContent = 'Secure tunnel ready. Opening WebUI.';
    showWebUi();
  } else if (msg.type === 'rpc-notification') {
    webuiFrame?.contentWindow?.postMessage({
      type: 'zeroclaw-rpc-notification',
      method: msg.method,
      params: msg.params || null
    }, location.origin);
  } else if (msg.type === 'rpc-closed') {
    status.textContent = msg.reason || 'Secure tunnel closed.';
    if (webuiState) webuiState.textContent = 'Disconnected';
  } else if (msg.type === 'enrollment-aborted') {
    sasPanel.hidden = true;
    status.textContent = msg.reason || 'Enrollment aborted';
    button.disabled = false;
  } else if (msg.type === 'resume-missing') {
    status.textContent = msg.reason || 'No saved browser certificate found. Pair once to connect.';
    button.disabled = false;
  } else if (msg.type === 'tls-engine-missing') {
    status.textContent = msg.reason || 'Secure browser enrollment is unavailable in this build.';
    button.disabled = false;
  } else if (msg.type === 'route-error') {
    status.textContent = msg.reason || 'Relay error';
    button.disabled = false;
  } else if (msg.type === 'route-closed') {
    status.textContent = msg.reason || 'Closed';
    button.disabled = false;
  }
});

form.addEventListener('submit', (event) => {
  event.preventDefault();
  const nodeId = document.getElementById('server-id').value.trim();
  const pairingCode = document.getElementById('pairing-code').value.trim();
  if (!nodeId || !pairingCode) return;

  activeNodeId = nodeId;
  button.disabled = true;
  sasPanel.hidden = true;
  sasConfirm.disabled = true;
  sasAbort.disabled = true;
  resetWebUi();
  status.textContent = 'Preparing client key';

  tunnel.postMessage({
    type: 'connectEnrollment',
    nodeId,
    pairingCode,
    relayUrl: `${location.protocol === 'https:' ? 'wss:' : 'ws:'}//${location.host}/relay`
  });

  navigator.serviceWorker.controller?.postMessage({
    type: 'zeroclaw-pairing-held',
    nodeId,
    pairingCodeLength: pairingCode.length
  });
});

sasConfirm.addEventListener('click', () => {
  sasConfirm.disabled = true;
  sasAbort.disabled = true;
  tunnel.postMessage({ type: 'confirmEnrollment' });
});

sasAbort.addEventListener('click', () => {
  sasConfirm.disabled = true;
  sasAbort.disabled = true;
  tunnel.postMessage({ type: 'abortEnrollment' });
});

window.__ZEROCLAW_RELAY_APP_READY = true;
restoreSavedConnection();

function showWebUi() {
  if (!openingWebUi) {
    openingWebUi = openWebUi().finally(() => {
      openingWebUi = null;
    });
  }
}

async function openWebUi() {
  seedRelayWebUiAuth();
  status.textContent = 'Secure tunnel ready. Opening WebUI.';
  form.hidden = true;
  sasPanel.hidden = true;
  webuiPanel.hidden = false;
  document.body.classList.add('webui-active');
  main?.classList.add('webui-active');
  if (webuiState) webuiState.textContent = 'Connected';
  if (!webuiFrame.src || webuiFrame.src === 'about:blank') {
    webuiFrame.src = '/webui/';
  }
}

function resetWebUi() {
  webuiPanel.hidden = true;
  document.body.classList.remove('webui-active');
  main?.classList.remove('webui-active');
  if (webuiState) webuiState.textContent = 'Starting';
  if (webuiFrame.src && webuiFrame.src !== 'about:blank') {
    webuiFrame.src = 'about:blank';
  }
}

function seedRelayWebUiAuth() {
  try {
    localStorage.setItem('zeroclaw_token', relayWebUiToken(activeNodeId || 'browser'));
  } catch (_) {}
}

function restoreSavedConnection() {
  if (attemptedAutoResume) {
    return;
  }
  attemptedAutoResume = true;
  const saved = readSavedConnection();
  if (!saved?.nodeId) {
    return;
  }
  activeNodeId = saved.nodeId;
  const serverInput = document.getElementById('server-id');
  if (serverInput && !serverInput.value) {
    serverInput.value = saved.nodeId;
  }
  button.disabled = true;
  sasPanel.hidden = true;
  resetWebUi();
  status.textContent = `Restoring saved connection to ${saved.nodeId}.`;
  tunnel.postMessage({
    type: 'resumeConnection',
    nodeId: saved.nodeId,
    relayUrl: `${location.protocol === 'https:' ? 'wss:' : 'ws:'}//${location.host}/relay`
  });
}

function rememberConnection(nodeId) {
  const cleanNodeId = String(nodeId || '').trim();
  if (!cleanNodeId) {
    return;
  }
  try {
    localStorage.setItem(SAVED_CONNECTION_KEY, JSON.stringify({
      nodeId: cleanNodeId,
      savedAt: new Date().toISOString()
    }));
  } catch (_) {}
}

function readSavedConnection() {
  try {
    const raw = localStorage.getItem(SAVED_CONNECTION_KEY);
    if (!raw) {
      return null;
    }
    const parsed = JSON.parse(raw);
    const nodeId = String(parsed?.nodeId || '').trim();
    return nodeId ? { nodeId } : null;
  } catch (_) {
    return null;
  }
}

function relayWebUiToken(nodeId) {
  const clean = String(nodeId || 'browser')
    .replace(/[^A-Za-z0-9!#$%&'*+\-.^_`|~]/g, '.')
    .replace(/\.+/g, '.')
    .replace(/^\.+|\.+$/g, '');
  return `relay-mtls.${clean || 'browser'}`;
}

function forwardToTunnel(msg, ports = []) {
  const transfers = [...ports];
  if (msg.body instanceof ArrayBuffer) {
    transfers.push(msg.body);
  }
  tunnel.postMessage(msg, transfers);
}
"#;

pub(crate) const SERVICE_WORKER_JS: &str = r#"self.addEventListener('install', (event) => {
  event.waitUntil(self.skipWaiting());
});

const RPC_TIMEOUT_MS = 15000;
const TLS_ENGINE_UNAVAILABLE =
  'Secure browser TLS enrollment is unavailable in this build.';
const ZERO_COST = {
  session_cost_usd: 0,
  daily_cost_usd: 0,
  monthly_cost_usd: 0,
  total_tokens: 0,
  request_count: 0,
  by_model: {},
  by_agent: {}
};

let rpcSeq = 1;
let initializePromise = null;

self.addEventListener('activate', (event) => {
  event.waitUntil(self.clients.claim());
});

self.addEventListener('fetch', (event) => {
  const url = new URL(event.request.url);
  const apiPath = normalizedApiPath(url.pathname);
  if (url.pathname === '/__zeroclaw/tunnel/state') {
    event.respondWith(jsonResponse({
      ready: true,
      enrollmentTls: true,
      mtlsEngine: true
    }));
  } else if (url.pathname === '/__zeroclaw/rpc') {
    event.respondWith(proxyRpc(event));
  } else if (apiPath || url.pathname === '/health' || url.pathname === '/webui/health') {
    event.respondWith(proxyGatewayApi(event, apiPath || '/health'));
  }
});

self.addEventListener('message', (event) => {
  event.source?.postMessage({ type: 'zeroclaw-relay-worker-ready' });
});

function normalizedApiPath(pathname) {
  if (pathname === '/api' || pathname.startsWith('/api/')) {
    return pathname;
  }
  if (pathname === '/webui/api' || pathname.startsWith('/webui/api/')) {
    return pathname.slice('/webui'.length);
  }
  return null;
}

async function proxyGatewayApi(event, apiPath) {
  if (event.request.method !== 'GET') {
    return jsonResponse({ error: 'relay webui bridge currently supports read-only dashboard requests' }, 405);
  }
  const url = new URL(event.request.url);
  try {
    await ensureInitialized();
    switch (apiPath) {
      case '/health':
        return jsonResponse(await publicHealth());
      case '/api/health':
        return jsonResponse(await rpcCall('health'));
      case '/api/status':
        return jsonResponse(await dashboardStatus(url));
      case '/api/events':
        return emptyEventStream();
      case '/api/tuis':
        return jsonResponse(await rpcOr('tui/list', null, { tuis: [] }));
      case '/api/cost':
        return jsonResponse(await rpcOr('cost/query', costParams(url), ZERO_COST));
      case '/api/sessions':
        return jsonResponse(await rpcOr('session/list', {}, { sessions: [] }));
      case '/api/channels':
        return jsonResponse({ channels: [] });
      case '/api/memory':
        return jsonResponse(await memoryResponse(url));
      case '/api/logs':
        return jsonResponse(await rpcOr('logs/query', logsParams(url), { events: [], at_end: true, next_cursor: null, next_cursor_line_offset: null }));
      case '/api/config/status':
        return jsonResponse(await rpcOr('config/status', null, {
          needs_quickstart: false,
          reason: '',
          has_partial_state: false,
          missing: []
        }));
      case '/api/config/reload-status':
        return jsonResponse(await configReloadStatus());
      case '/api/config/drift':
        return jsonResponse(await configDrift());
      case '/api/config/sections':
        return jsonResponse(await rpcOr('config/sections', null, { sections: [] }));
      case '/api/config/map-keys':
        return jsonResponse(await rpcCall('config/map-keys', { path: url.searchParams.get('path') || '' }));
      case '/api/config/list':
        return jsonResponse(await rpcCall('config/list', { prefix: url.searchParams.get('prefix') || null }));
      case '/api/quickstart/state':
        return jsonResponse(await rpcOr('quickstart/state', null, {
          quickstart_completed: false,
          agents: [],
          risk_profiles: [],
          runtime_profiles: [],
          model_providers: [],
          channels: [],
          unassigned_channels: [],
          storage: [],
          model_provider_types: [],
          channel_types: [],
          risk_presets: [],
          runtime_presets: [],
          memory_kinds: [],
          personality_files: []
        }));
      case '/api/cron':
        return jsonResponse(await rpcOr('cron/list', null, { jobs: [] }));
      case '/api/cli-tools':
        return jsonResponse({ cli_tools: [] });
      case '/api/tools':
        return jsonResponse({ tools: [] });
      default:
        return jsonResponse({ error: 'relay webui bridge has no mapping for this route', path: apiPath }, 404);
    }
  } catch (error) {
    return jsonResponse({ error: error?.message || TLS_ENGINE_UNAVAILABLE }, 503);
  }
}

async function dashboardStatus(url) {
  const [status, health] = await Promise.all([
    rpcCall('status'),
    rpcOr('health', null, {})
  ]);
  const port = Number(url.port || (url.protocol === 'https:' ? 443 : 80));
  return {
    version: status.server_version,
    model_provider: null,
    model: '',
    temperature: 0,
    uptime_seconds: Number(health.uptime_seconds || 0),
    daemon_started_at: health.updated_at || new Date().toISOString(),
    gateway_port: port,
    locale: 'en',
    memory_backend: '',
    paired: true,
    channels: {},
    health,
    process: health.process || {
      rss_bytes: 0,
      system_ram_total_bytes: 0,
      cpu_percent: null,
      num_cpus: 0
    }
  };
}

async function publicHealth() {
  const health = await rpcOr('health', null, {});
  return {
    ...health,
    require_pairing: false,
    paired: true
  };
}

async function configReloadStatus() {
  const status = await rpcOr('config/reload-status', null, { pending_reload: false });
  return {
    pending_reload: Boolean(status?.pending_reload)
  };
}

async function configDrift() {
  const drift = await rpcOr('config/drift', null, { drifted: [] });
  if (Array.isArray(drift?.drifted)) {
    return { drifted: drift.drifted };
  }
  return { drifted: [] };
}

function costParams(url) {
  return {
    agent: url.searchParams.get('agent') || null,
    from: url.searchParams.get('from') || null,
    to: url.searchParams.get('to') || null
  };
}

function logsParams(url) {
  const limit = Number(url.searchParams.get('limit') || '50');
  return {
    since_ts: url.searchParams.get('since_ts') || null,
    until_ts: url.searchParams.get('until_ts') || null,
    until_id: url.searchParams.get('until_id') || null,
    until_line_offset: optionalNumber(url.searchParams.get('until_line_offset')),
    severity_min: optionalNumber(url.searchParams.get('severity_min')),
    q: url.searchParams.get('q') || null,
    category: url.searchParams.get('category') || null,
    action: url.searchParams.get('action') || null,
    outcome: url.searchParams.get('outcome') || null,
    trace_id: url.searchParams.get('trace_id') || null,
    hide_internal: url.searchParams.get('hide_internal') === 'true',
    limit: Number.isFinite(limit) ? limit : 50
  };
}

async function memoryResponse(url) {
  const query = url.searchParams.get('query');
  if (query) {
    return rpcOr('memory/search', {
      query,
      limit: 100,
      agent: url.searchParams.get('agent') || null
    }, { entries: [], count: 0 });
  }
  return rpcOr('memory/list', {
    category: url.searchParams.get('category') || null,
    agent: url.searchParams.get('agent') || null
  }, { entries: [], count: 0 });
}

function optionalNumber(value) {
  if (value === null || value === '') {
    return null;
  }
  const number = Number(value);
  return Number.isFinite(number) ? number : null;
}

async function ensureInitialized() {
  if (!initializePromise) {
    initializePromise = rpcCall('initialize', {
      protocol_version: 1,
      clientCapabilities: { elicitation: {} }
    }).catch((error) => {
      initializePromise = null;
      throw error;
    });
  }
  return initializePromise;
}

async function rpcOr(method, params, fallback) {
  try {
    return await rpcCall(method, params);
  } catch (_) {
    return fallback;
  }
}

async function rpcCall(method, params = null) {
  const id = rpcSeq++;
  const bytes = new TextEncoder().encode(JSON.stringify({ jsonrpc: '2.0', id, method, params }));
  const body = bytes.buffer;
  const msg = await dispatchToRelayShell({
    type: 'zeroclaw-rpc-request',
    method: 'POST',
    url: '/__zeroclaw/rpc',
    headers: [['content-type', 'application/json']],
    body
  }, body);
  if (!msg?.ok) {
    throw new Error(msg?.error || 'browser tunnel request failed');
  }
  const text = msg.body ? new TextDecoder().decode(toUint8(msg.body)) : '{}';
  const parsed = JSON.parse(text || '{}');
  if (parsed.error) {
    throw new Error(parsed.error.message || 'RPC error');
  }
  return parsed.result;
}

async function proxyRpc(event) {
  let body = null;
  try {
    body = await requestBody(event.request);
  } catch (_) {
    return unavailable('could not read request body');
  }

  const url = new URL(event.request.url);
  const msg = await dispatchToRelayShell({
    type: 'zeroclaw-rpc-request',
    method: event.request.method,
    url: `${url.pathname}${url.search}`,
    headers: Array.from(event.request.headers.entries()),
    body
  }, body);
  return responseFromRpcMessage(msg);
}

async function dispatchToRelayShell(msg, body) {
  const client = await relayShellClient();
  if (!client) {
    throw new Error('relay pairing shell is not available');
  }

  const channel = new MessageChannel();
  let timeout = null;
  const reply = new Promise((resolve) => {
    timeout = setTimeout(() => {
      timeout = null;
      channel.port1.close();
      resolve({ ok: false, status: 504, error: 'browser tunnel request timed out' });
    }, RPC_TIMEOUT_MS);
    channel.port1.onmessage = (messageEvent) => {
      if (timeout) {
        clearTimeout(timeout);
        timeout = null;
      }
      channel.port1.close();
      resolve(messageEvent.data || {});
    };
    channel.port1.start();
  });

  const transfers = [channel.port2];
  if (body) {
    transfers.push(body);
  }
  try {
    client.postMessage(msg, transfers);
  } catch (_) {
    if (timeout) {
      clearTimeout(timeout);
    }
    channel.port1.close();
    throw new Error('could not dispatch browser tunnel request');
  }
  return reply;
}

async function relayShellClient() {
  const clients = await self.clients.matchAll({ type: 'window', includeUncontrolled: true });
  return clients.find((client) => {
    const path = new URL(client.url).pathname;
    return path === '/' || path === '/index.html';
  }) || null;
}

function requestBody(request) {
  if (request.method === 'GET' || request.method === 'HEAD') {
    return Promise.resolve(null);
  }
  return request.arrayBuffer();
}

function responseFromRpcMessage(msg) {
  if (msg?.ok) {
    return new Response(msg.body || null, {
      status: msg.status || 200,
      headers: msg.headers || {}
    });
  }
  return unavailable(msg?.error || TLS_ENGINE_UNAVAILABLE, msg?.status || 503);
}

function unavailable(error, status = 503) {
  return jsonResponse({ error }, status);
}

function jsonResponse(value, status = 200) {
  return new Response(JSON.stringify(value), {
    status,
    headers: { 'content-type': 'application/json' }
  });
}

function emptyEventStream() {
  return new Response(': relay bridge event stream ready\n\n', {
    status: 200,
    headers: {
      'content-type': 'text/event-stream',
      'cache-control': 'no-store',
      'connection': 'keep-alive'
    }
  });
}

function toUint8(value) {
  if (value instanceof Uint8Array) return value;
  if (value instanceof ArrayBuffer) return new Uint8Array(value);
  if (ArrayBuffer.isView(value)) return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
  return new Uint8Array(0);
}
"#;

pub(crate) const WEBUI_FETCH_BRIDGE_JS: &str = r#"(() => {
  if (window.__ZEROCLAW_RELAY_FETCH_BRIDGE__) {
    return;
  }
  window.__ZEROCLAW_RELAY_FETCH_BRIDGE__ = true;

  const RPC_TIMEOUT_MS = 15000;
  const ZERO_COST = {
    session_cost_usd: 0,
    daily_cost_usd: 0,
    monthly_cost_usd: 0,
    total_tokens: 0,
    request_count: 0,
    by_model: {},
    by_agent: {}
  };

  const originalFetch = window.fetch.bind(window);
  const NativeWebSocket = window.WebSocket;
  const WS_CONNECTING = NativeWebSocket?.CONNECTING ?? 0;
  const WS_OPEN = NativeWebSocket?.OPEN ?? 1;
  const WS_CLOSING = NativeWebSocket?.CLOSING ?? 2;
  const WS_CLOSED = NativeWebSocket?.CLOSED ?? 3;
  const relayChatSockets = new Set();
  let rpcSeq = 1;
  let initializePromise = null;

  window.fetch = async (input, init) => {
    const request = new Request(input, init);
    const url = new URL(request.url, location.href);
    const apiPath = normalizedApiPath(url.pathname);
    if (!apiPath && url.pathname !== '/health' && url.pathname !== '/webui/health') {
      return originalFetch(input, init);
    }
    if (request.method !== 'GET') {
      return jsonResponse({ error: 'relay webui bridge currently supports read-only dashboard requests' }, 405);
    }
    try {
      await ensureInitialized();
      return await bridgeGatewayApi(apiPath || '/health', url);
    } catch (error) {
      return jsonResponse({ error: error?.message || 'Secure browser TLS enrollment is unavailable in this build.' }, 503);
    }
  };

  if (NativeWebSocket) {
    window.WebSocket = RelayWebSocket;
    window.WebSocket.CONNECTING = WS_CONNECTING;
    window.WebSocket.OPEN = WS_OPEN;
    window.WebSocket.CLOSING = WS_CLOSING;
    window.WebSocket.CLOSED = WS_CLOSED;
  }

  window.addEventListener('message', (event) => {
    if (event.origin !== location.origin) {
      return;
    }
    const msg = event.data || {};
    if (msg.type !== 'zeroclaw-rpc-notification') {
      return;
    }
    for (const socket of Array.from(relayChatSockets)) {
      socket.handleNotification(msg.method, msg.params || null);
    }
  });

  function RelayWebSocket(url, protocols) {
    const parsed = new URL(url, location.href);
    if (isRelayChatPath(parsed.pathname)) {
      return new RelayChatSocket(parsed.toString(), protocols);
    }
    return new NativeWebSocket(url, protocols);
  }

  function isRelayChatPath(pathname) {
    const base = String(window.__ZEROCLAW_BASE__ || '').replace(/\/$/, '');
    return pathname === '/ws/chat'
      || pathname === '/webui/ws/chat'
      || Boolean(base && pathname === `${base}/ws/chat`);
  }

  class RelayChatSocket {
    constructor(url, protocols) {
      this.url = url;
      this.protocol = selectProtocol(protocols);
      this.extensions = '';
      this.binaryType = 'blob';
      this.bufferedAmount = 0;
      this.readyState = WS_CONNECTING;
      this.onopen = null;
      this.onmessage = null;
      this.onclose = null;
      this.onerror = null;
      this.events = new EventTarget();
      this.closed = false;
      this.sessionId = '';
      this.agentAlias = 'default';
      this.lastInputTokens = null;
      this.maxContextTokens = null;
      relayChatSockets.add(this);
      queueMicrotask(() => this.connect());
    }

    addEventListener(type, listener, options) {
      this.events.addEventListener(type, listener, options);
    }

    removeEventListener(type, listener, options) {
      this.events.removeEventListener(type, listener, options);
    }

    dispatchEvent(event) {
      return this.events.dispatchEvent(event);
    }

    send(data) {
      if (this.readyState !== WS_OPEN) {
        throw new Error('WebSocket is not connected');
      }
      let parsed;
      try {
        parsed = JSON.parse(String(data));
      } catch (error) {
        this.emitJson({
          type: 'error',
          code: 'INVALID_JSON',
          message: error?.message || 'Invalid JSON'
        });
        return;
      }

      if (parsed.type === 'message') {
        const prompt = String(parsed.content || '');
        rpcCall('session/prompt', {
          session_id: this.sessionId,
          prompt
        }).catch((error) => {
          this.emitJson({
            type: 'error',
            code: 'PROMPT_FAILED',
            message: error?.message || 'Prompt failed'
          });
        });
      } else if (parsed.type === 'approval_response') {
        rpcCall('session/approve', {
          session_id: this.sessionId,
          request_id: String(parsed.request_id || ''),
          decision: normalizeApprovalDecision(parsed.decision),
          replacement: parsed.replacement || null
        }).catch((error) => {
          this.emitJson({
            type: 'error',
            code: 'APPROVAL_FAILED',
            message: error?.message || 'Approval failed'
          });
        });
      } else if (parsed.type === 'connect') {
        this.emitJson({ type: 'connected', message: 'Connection established' });
      } else {
        this.emitJson({
          type: 'error',
          code: 'UNKNOWN_MESSAGE_TYPE',
          message: `Unknown message type: ${parsed.type || 'unknown'}`
        });
      }
    }

    close(code = 1000, reason = '') {
      if (this.readyState === WS_CLOSED || this.readyState === WS_CLOSING) {
        return;
      }
      this.closed = true;
      this.readyState = WS_CLOSING;
      relayChatSockets.delete(this);
      const sessionId = this.sessionId;
      const finish = () => {
        this.readyState = WS_CLOSED;
        this.emit(closeEvent(code, reason, true));
      };
      if (sessionId) {
        rpcCall('session/close', { session_id: sessionId }).catch(() => {}).finally(finish);
      } else {
        finish();
      }
    }

    async connect() {
      const url = new URL(this.url, location.href);
      this.sessionId = url.searchParams.get('session_id') || randomSessionId();
      this.agentAlias = url.searchParams.get('agent') || 'default';
      try {
        const result = await rpcCall('session/new', {
          agent_alias: this.agentAlias,
          session_id: this.sessionId
        });
        if (this.closed) {
          await rpcCall('session/close', { session_id: result?.session_id || this.sessionId }).catch(() => {});
          return;
        }
        if (result?.session_id) {
          this.sessionId = result.session_id;
        }
        this.readyState = WS_OPEN;
        this.emit(new Event('open'));
        this.emitJson({
          type: 'session_start',
          session_id: this.sessionId,
          resumed: Number(result?.message_count || 0) > 0,
          message_count: Number(result?.message_count || 0)
        });
        this.emitJson({ type: 'connected', message: 'Connection established' });
      } catch (error) {
        this.fail(error, 'CHAT_CONNECT_FAILED');
      }
    }

    handleNotification(method, params) {
      if (method !== 'session/update' || !params || params.session_id !== this.sessionId) {
        return;
      }
      switch (params.type) {
        case 'agent_message_chunk':
          this.emitJson({ type: 'chunk', content: params.text || '' });
          break;
        case 'agent_thought_chunk':
          this.emitJson({ type: 'thinking', content: params.text || '' });
          break;
        case 'tool_call':
          this.emitJson({
            type: 'tool_call',
            id: params.tool_call_id || '',
            name: params.name || '',
            args: params.raw_input
          });
          break;
        case 'tool_result':
          this.emitJson({
            type: 'tool_result',
            id: params.tool_call_id || '',
            name: params.name || '',
            output: params.raw_output || ''
          });
          break;
        case 'approval_request':
          this.emitJson({
            type: 'approval_request',
            request_id: params.request_id || '',
            tool: params.tool_name || '',
            arguments_summary: params.arguments_summary || '',
            timeout_secs: params.timeout_secs || 120
          });
          break;
        case 'context_usage':
          this.lastInputTokens = optionalNumber(params.input_tokens);
          this.maxContextTokens = optionalNumber(params.max_context_tokens);
          break;
        case 'plan':
          this.emitJson({ type: 'plan', entries: params.entries || [] });
          break;
        case 'history_trimmed':
          this.emitJson({
            type: 'history_trimmed',
            dropped_messages: params.dropped_messages || 0,
            kept_turns: params.kept_turns || 0,
            reason: params.reason || ''
          });
          break;
        case 'turn_complete':
          this.handleTurnComplete(params);
          break;
        default:
          break;
      }
    }

    handleTurnComplete(params) {
      const outcome = params.outcome || 'completed';
      const content = params.content || '';
      if (outcome === 'completed') {
        this.emitJson({
          type: 'done',
          full_response: content,
          input_tokens: this.lastInputTokens,
          last_input_tokens: this.lastInputTokens,
          max_context_tokens: this.maxContextTokens
        });
      } else if (outcome === 'cancelled') {
        if (content.trim()) {
          this.emitJson({
            type: 'done',
            full_response: content,
            input_tokens: this.lastInputTokens,
            last_input_tokens: this.lastInputTokens,
            max_context_tokens: this.maxContextTokens
          });
        }
        this.emitJson({ type: 'aborted' });
      } else {
        this.emitJson({
          type: 'error',
          code: 'TURN_FAILED',
          message: content || 'Turn failed'
        });
      }
    }

    emitJson(value) {
      if (this.readyState !== WS_OPEN) {
        return;
      }
      this.emit(new MessageEvent('message', { data: JSON.stringify(value) }));
    }

    emit(event) {
      const handler = this[`on${event.type}`];
      if (typeof handler === 'function') {
        try {
          handler.call(this, event);
        } catch (error) {
          setTimeout(() => { throw error; }, 0);
        }
      }
      this.events.dispatchEvent(event);
    }

    fail(error, code) {
      if (this.readyState === WS_CLOSED) {
        return;
      }
      relayChatSockets.delete(this);
      this.emit(errorEvent(error));
      if (this.readyState === WS_OPEN) {
        this.emitJson({
          type: 'error',
          code,
          message: error?.message || 'Relay chat bridge failed'
        });
      }
      this.readyState = WS_CLOSED;
      this.emit(closeEvent(1011, error?.message || 'Relay chat bridge failed', false));
    }
  }

  function selectProtocol(protocols) {
    if (Array.isArray(protocols)) {
      return protocols[0] || '';
    }
    return protocols || '';
  }

  function randomSessionId() {
    if (crypto.randomUUID) {
      return crypto.randomUUID();
    }
    return `relay-${Date.now()}-${Math.random().toString(16).slice(2)}`;
  }

  function normalizeApprovalDecision(decision) {
    switch (decision) {
      case 'approve':
        return 'allow_once';
      case 'always':
        return 'allow_always';
      case 'deny':
        return 'reject';
      default:
        return String(decision || '');
    }
  }

  function closeEvent(code, reason, wasClean) {
    if (typeof CloseEvent === 'function') {
      return new CloseEvent('close', { code, reason, wasClean });
    }
    return new Event('close');
  }

  function errorEvent(error) {
    if (typeof ErrorEvent === 'function') {
      return new ErrorEvent('error', {
        message: error?.message || 'Relay chat bridge failed',
        error
      });
    }
    return new Event('error');
  }

  function normalizedApiPath(pathname) {
    if (pathname === '/api' || pathname.startsWith('/api/')) {
      return pathname;
    }
    if (pathname === '/webui/api' || pathname.startsWith('/webui/api/')) {
      return pathname.slice('/webui'.length);
    }
    return null;
  }

  async function bridgeGatewayApi(apiPath, url) {
    switch (apiPath) {
      case '/health':
        return jsonResponse(await publicHealth());
      case '/api/health':
        return jsonResponse(await rpcCall('health'));
      case '/api/status':
        return jsonResponse(await dashboardStatus(url));
      case '/api/events':
        return emptyEventStream();
      case '/api/tuis':
        return jsonResponse(await rpcOr('tui/list', null, { tuis: [] }));
      case '/api/cost':
        return jsonResponse(await rpcOr('cost/query', costParams(url), ZERO_COST));
      case '/api/sessions':
        return jsonResponse(await rpcOr('session/list', {}, { sessions: [] }));
      case '/api/channels':
        return jsonResponse({ channels: [] });
      case '/api/memory':
        return jsonResponse(await memoryResponse(url));
      case '/api/logs':
        return jsonResponse(await rpcOr('logs/query', logsParams(url), {
          events: [],
          at_end: true,
          next_cursor: null,
          next_cursor_line_offset: null
        }));
      case '/api/config/status':
        return jsonResponse(await rpcOr('config/status', null, {
          needs_quickstart: false,
          reason: '',
          has_partial_state: false,
          missing: []
        }));
      case '/api/config/reload-status':
        return jsonResponse(await configReloadStatus());
      case '/api/config/drift':
        return jsonResponse(await configDrift());
      case '/api/config/sections':
        return jsonResponse(await rpcOr('config/sections', null, { sections: [] }));
      case '/api/config/map-keys':
        return jsonResponse(await rpcCall('config/map-keys', { path: url.searchParams.get('path') || '' }));
      case '/api/config/list':
        return jsonResponse(await rpcCall('config/list', { prefix: url.searchParams.get('prefix') || null }));
      case '/api/quickstart/state':
        return jsonResponse(await rpcOr('quickstart/state', null, {
          quickstart_completed: false,
          agents: [],
          risk_profiles: [],
          runtime_profiles: [],
          model_providers: [],
          channels: [],
          unassigned_channels: [],
          storage: [],
          model_provider_types: [],
          channel_types: [],
          risk_presets: [],
          runtime_presets: [],
          memory_kinds: [],
          personality_files: []
        }));
      case '/api/cron':
        return jsonResponse(await rpcOr('cron/list', null, { jobs: [] }));
      case '/api/cli-tools':
        return jsonResponse({ cli_tools: [] });
      case '/api/tools':
        return jsonResponse({ tools: [] });
      default:
        return jsonResponse({ error: 'relay webui bridge has no mapping for this route', path: apiPath }, 404);
    }
  }

  async function dashboardStatus(url) {
    const [status, health] = await Promise.all([
      rpcCall('status'),
      rpcOr('health', null, {})
    ]);
    const port = Number(url.port || (url.protocol === 'https:' ? 443 : 80));
    return {
      version: status.server_version,
      model_provider: null,
      model: '',
      temperature: 0,
      uptime_seconds: Number(health.uptime_seconds || 0),
      daemon_started_at: health.updated_at || new Date().toISOString(),
      gateway_port: port,
      locale: 'en',
      memory_backend: '',
      paired: true,
      channels: {},
      health,
      process: health.process || {
        rss_bytes: 0,
        system_ram_total_bytes: 0,
        cpu_percent: null,
        num_cpus: 0
      }
    };
  }

  async function publicHealth() {
    const health = await rpcOr('health', null, {});
    return {
      ...health,
      require_pairing: false,
      paired: true
    };
  }

  async function configReloadStatus() {
    const status = await rpcOr('config/reload-status', null, { pending_reload: false });
    return {
      pending_reload: Boolean(status?.pending_reload)
    };
  }

  async function configDrift() {
    const drift = await rpcOr('config/drift', null, { drifted: [] });
    if (Array.isArray(drift?.drifted)) {
      return { drifted: drift.drifted };
    }
    return { drifted: [] };
  }

  function costParams(url) {
    return {
      agent: url.searchParams.get('agent') || null,
      from: url.searchParams.get('from') || null,
      to: url.searchParams.get('to') || null
    };
  }

  function logsParams(url) {
    const limit = Number(url.searchParams.get('limit') || '50');
    return {
      since_ts: url.searchParams.get('since_ts') || null,
      until_ts: url.searchParams.get('until_ts') || null,
      until_id: url.searchParams.get('until_id') || null,
      until_line_offset: optionalNumber(url.searchParams.get('until_line_offset')),
      severity_min: optionalNumber(url.searchParams.get('severity_min')),
      q: url.searchParams.get('q') || null,
      category: url.searchParams.get('category') || null,
      action: url.searchParams.get('action') || null,
      outcome: url.searchParams.get('outcome') || null,
      trace_id: url.searchParams.get('trace_id') || null,
      hide_internal: url.searchParams.get('hide_internal') === 'true',
      limit: Number.isFinite(limit) ? limit : 50
    };
  }

  async function memoryResponse(url) {
    const query = url.searchParams.get('query');
    if (query) {
      return rpcOr('memory/search', {
        query,
        limit: 100,
        agent: url.searchParams.get('agent') || null
      }, { entries: [], count: 0 });
    }
    return rpcOr('memory/list', {
      category: url.searchParams.get('category') || null,
      agent: url.searchParams.get('agent') || null
    }, { entries: [], count: 0 });
  }

  function optionalNumber(value) {
    if (value === null || value === '') {
      return null;
    }
    const number = Number(value);
    return Number.isFinite(number) ? number : null;
  }

  async function ensureInitialized() {
    if (!initializePromise) {
      initializePromise = rpcCallRaw('initialize', {
        protocol_version: 1,
        clientCapabilities: { elicitation: {} }
      }).catch((error) => {
        initializePromise = null;
        throw error;
      });
    }
    return initializePromise;
  }

  async function rpcOr(method, params, fallback) {
    try {
      return await rpcCall(method, params);
    } catch (_) {
      return fallback;
    }
  }

  async function rpcCall(method, params = null) {
    await ensureInitialized();
    return rpcCallRaw(method, params);
  }

  async function rpcCallRaw(method, params = null) {
    const id = rpcSeq++;
    const encoded = new TextEncoder().encode(JSON.stringify({ jsonrpc: '2.0', id, method, params }));
    const body = encoded.buffer.slice(encoded.byteOffset, encoded.byteOffset + encoded.byteLength);
    const msg = await dispatchToRelayShell({
      type: 'zeroclaw-rpc-request',
      method: 'POST',
      url: '/__zeroclaw/rpc',
      headers: [['content-type', 'application/json']],
      body
    }, body);
    if (!msg?.ok) {
      throw new Error(msg?.error || 'browser tunnel request failed');
    }
    const text = msg.body ? new TextDecoder().decode(toUint8(msg.body)) : '{}';
    const parsed = JSON.parse(text || '{}');
    if (parsed.error) {
      throw new Error(parsed.error.message || 'RPC error');
    }
    return parsed.result;
  }

  async function dispatchToRelayShell(msg, body) {
    if (!window.parent || window.parent === window) {
      throw new Error('relay pairing shell is not available');
    }

    const channel = new MessageChannel();
    let timeout = null;
    const reply = new Promise((resolve) => {
      timeout = setTimeout(() => {
        timeout = null;
        channel.port1.close();
        resolve({ ok: false, status: 504, error: 'browser tunnel request timed out' });
      }, RPC_TIMEOUT_MS);
      channel.port1.onmessage = (messageEvent) => {
        if (timeout) {
          clearTimeout(timeout);
          timeout = null;
        }
        channel.port1.close();
        resolve(messageEvent.data || {});
      };
      channel.port1.start();
    });

    const transfers = [channel.port2];
    if (body) {
      transfers.push(body);
    }
    try {
      window.parent.postMessage(msg, location.origin, transfers);
    } catch (_) {
      if (timeout) {
        clearTimeout(timeout);
      }
      channel.port1.close();
      throw new Error('could not dispatch browser tunnel request');
    }
    return reply;
  }

  function jsonResponse(value, status = 200) {
    return new Response(JSON.stringify(value), {
      status,
      headers: { 'content-type': 'application/json' }
    });
  }

  function emptyEventStream() {
    return new Response(': relay bridge event stream ready\n\n', {
      status: 200,
      headers: {
        'content-type': 'text/event-stream',
        'cache-control': 'no-store'
      }
    });
  }

  function toUint8(value) {
    if (value instanceof Uint8Array) return value;
    if (value instanceof ArrayBuffer) return new Uint8Array(value);
    if (ArrayBuffer.isView(value)) return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
    return new Uint8Array(0);
  }
})();"#;

pub(crate) const TUNNEL_WORKER_JS: &str = r#"const DB_NAME = 'zeroclaw-relay-enrollment';
const DB_VERSION = 1;
const MATERIAL_STORE = 'materials';
const MATERIAL_KEY_PREFIX = 'node:';

importScripts('/tls-engine.js');

let ws = null;
let dbPromise = null;
let route = null;
let pendingEnrollment = null;
let pendingEnrollmentPost = null;
let activeProfile = null;
let rpcClient = null;
let rpcConnecting = null;

const TLS_ENGINE_UNAVAILABLE =
  'Secure browser TLS enrollment is unavailable in this build.';

self.addEventListener('message', (event) => {
  const msg = event.data || {};
  if (msg.type === 'connectEnrollment') {
    connectEnrollment(msg.relayUrl, msg.nodeId, msg.pairingCode);
  } else if (msg.type === 'resumeConnection') {
    resumeConnection(msg.relayUrl, msg.nodeId);
  } else if (msg.type === 'confirmEnrollment') {
    confirmEnrollment();
  } else if (msg.type === 'abortEnrollment') {
    abortEnrollment('Enrollment aborted');
  } else if (msg.type === 'zeroclaw-rpc-request') {
    handleRpcRequest(msg, event.ports?.[0]);
  }
});

async function connectEnrollment(relayUrl, nodeId, pairingCode) {
  if (!relayUrl || !nodeId || !pairingCode) {
    self.postMessage({ type: 'route-error', reason: 'missing relay URL, server ID, or pairing code' });
    return;
  }

  closeExisting();
  pendingEnrollment = null;
  pendingEnrollmentPost = null;
  activeProfile = null;
  self.postMessage({ type: 'connecting' });

  let material;
  try {
    material = await ensureEnrollmentMaterial(nodeId);
  } catch (error) {
    self.postMessage({
      type: 'route-error',
      reason: error?.message || 'failed to prepare client key'
    });
    return;
  }
  material = { ...material, relayUrl, nodeId, pairingCode };

  self.postMessage({
    type: 'enrollment-material-ready',
    csrBytes: material.csrPem.length
  });

  openEnrollmentRoute(material);
}

async function resumeConnection(relayUrl, nodeId) {
  if (!nodeId) {
    self.postMessage({ type: 'resume-missing', reason: 'No saved relay connection is selected' });
    return;
  }

  closeExisting();
  pendingEnrollment = null;
  pendingEnrollmentPost = null;
  activeProfile = null;
  self.postMessage({ type: 'resuming', nodeId });

  let profile;
  try {
    profile = await loadCompletedEnrollment(nodeId);
  } catch (error) {
    self.postMessage({
      type: 'resume-missing',
      reason: error?.message || 'Could not read the saved browser certificate'
    });
    return;
  }
  if (!profile) {
    self.postMessage({
      type: 'resume-missing',
      reason: `No saved browser certificate found for ${nodeId}. Pair once to connect.`
    });
    return;
  }

  activeProfile = {
    ...profile,
    relayUrl: relayUrl || profile.relayUrl,
    nodeId: profile.nodeId || nodeId
  };
  connectRpcTunnel(activeProfile).catch((error) => {
    self.postMessage({
      type: 'route-error',
      reason: error?.message || 'failed to restore saved relay connection'
    });
  });
}

function openEnrollmentRoute(material) {
  const { relayUrl, nodeId } = material;
  const socket = new WebSocket(relayUrl, 'zeroclaw.relay.v1');
  ws = socket;
  socket.binaryType = 'arraybuffer';

  socket.addEventListener('open', () => {
    if (ws !== socket) {
      return;
    }
    socket.send(JSON.stringify({ t: 'enroll', node_id: nodeId }));
  });
  socket.addEventListener('message', (event) => {
    if (ws !== socket) {
      return;
    }
    if (typeof event.data !== 'string') {
      let frame;
      try {
        frame = decodeDataFrame(event.data);
      } catch (error) {
        self.postMessage({
          type: 'route-error',
          reason: error?.message || 'malformed relay data frame'
        });
        closeExisting();
        return;
      }
      if (route && frame.connId === route.connId) {
        route.receive(frame.payload);
      }
      self.postMessage({
        type: 'route-data',
        connId: frame.connId,
        bytes: frame.payload.byteLength
      });
      return;
    }
    let frame;
    try {
      frame = JSON.parse(event.data);
    } catch (_) {
      self.postMessage({ type: 'route-error', reason: 'malformed relay control frame' });
      return;
    }
    if (frame.t === 'opened') {
      try {
        route = new RelayDataTransport(socket, frame.conn_id);
      } catch (error) {
        self.postMessage({
          type: 'route-error',
          reason: error?.message || 'invalid relay connection id'
        });
        closeExisting();
        return;
      }
      self.postMessage({ type: 'route-open', connId: route.connId });
      if (pendingEnrollmentPost) {
        const pending = pendingEnrollmentPost;
        pendingEnrollmentPost = null;
        finishBrowserEnrollment(route, pending);
      } else {
        beginBrowserEnrollment(route, material);
      }
    } else if (frame.t === 'error') {
      self.postMessage({ type: 'route-error', reason: frame.code || 'relay error' });
      closeExisting();
    } else if (frame.t === 'close') {
      let closedConnId = null;
      try {
        closedConnId =
          frame.conn_id !== undefined && frame.conn_id !== null ? normalizeConnId(frame.conn_id) : null;
      } catch (error) {
        self.postMessage({
          type: 'route-error',
          reason: error?.message || 'invalid relay connection id'
        });
        closeExisting();
        return;
      }
      if (closedConnId !== null && route && closedConnId !== route.connId) {
        return;
      }
      if (closedConnId !== null && !route && pendingEnrollmentPost) {
        return;
      }
      self.postMessage({ type: 'route-closed', reason: frame.reason || 'closed' });
      if (pendingEnrollment && !pendingEnrollmentPost) {
        if (route) {
          route.close(frame.reason || 'closed');
          route = null;
        }
        return;
      }
      closeExisting();
    }
  });
  socket.addEventListener('close', () => {
    if (ws !== socket) {
      return;
    }
    if (route) {
      route.close('relay websocket closed');
      route = null;
    }
    self.postMessage({ type: 'route-closed', reason: 'relay websocket closed' });
    ws = null;
  });
  socket.addEventListener('error', () => {
    if (ws !== socket) {
      return;
    }
    self.postMessage({ type: 'route-error', reason: 'relay websocket failed' });
  });
}

async function beginBrowserEnrollment(_transport, _material) {
  if (!self.ZeroClawEnrollmentTls?.fetchEnrollmentTrust || !self.ZeroClawEnrollmentTls?.enroll) {
    self.postMessage({
      type: 'tls-engine-missing',
      reason: TLS_ENGINE_UNAVAILABLE
    });
    return;
  }
  try {
    const trust = await self.ZeroClawEnrollmentTls.fetchEnrollmentTrust(_transport, {
      serverName: '127.0.0.1',
      host: '127.0.0.1'
    });
    const caFingerprint = await singleCertificateFingerprintHex(trust.ca_chain_pem);
    const sas = await enrollmentSas(_material.pairingCode, caFingerprint);
    pendingEnrollment = {
      material: _material,
      trust,
      sas
    };
    self.postMessage({
      type: 'enrollment-sas',
      sas
    });
  } catch (error) {
    self.postMessage({
      type: 'route-error',
      reason: error?.message || 'secure enrollment trust fetch failed'
    });
    closeExisting();
  }
}

async function confirmEnrollment() {
  if (!pendingEnrollment) {
    self.postMessage({ type: 'enrollment-aborted', reason: 'No enrollment is waiting for confirmation' });
    return;
  }
  const pending = pendingEnrollment;
  try {
    pendingEnrollment = null;
    pendingEnrollmentPost = pending;
    if (route) {
      route.close('enrollment SAS confirmed');
      route = null;
    }
    if (ws) {
      const old = ws;
      ws = null;
      try {
        old.close();
      } catch (_) {
        // The provisional control socket may already be gone after /enroll/ca.
      }
    }
    openEnrollmentRoute(pending.material);
    self.postMessage({ type: 'route-opening-confirmed-enrollment' });
  } catch (error) {
    pendingEnrollmentPost = null;
    self.postMessage({
      type: 'route-error',
      reason: error?.message || 'failed to open confirmed enrollment route'
    });
    closeExisting();
  }
}

async function finishBrowserEnrollment(_transport, pending) {
  const { material, trust, sas } = pending;
  try {
    const response = await self.ZeroClawEnrollmentTls.enroll(_transport, {
      pairingCode: material.pairingCode,
      csrPem: material.csrPem,
      caChainPem: trust.ca_chain_pem,
      serverName: '127.0.0.1',
      host: '127.0.0.1'
    });
    const confirmedFingerprint = await singleCertificateFingerprintHex(trust.ca_chain_pem);
    const responseFingerprint = await singleCertificateFingerprintHex(response.ca_chain_pem);
    if (responseFingerprint !== confirmedFingerprint) {
      throw new Error('enrollment response CA does not match the confirmed daemon CA');
    }
    await storeCompletedEnrollment(material, response, sas);
  } catch (error) {
    self.postMessage({
      type: 'route-error',
      reason: error?.message || 'secure enrollment failed'
    });
    closeExisting();
  }
}

async function storeCompletedEnrollment(material, response, sas) {
  const db = await openEnrollmentDb();
  const stored = {
    ...material,
    pairingCode: undefined,
    certPem: response.cert_pem,
    caChainPem: response.ca_chain_pem,
    deviceId: response.device_id,
    notAfter: response.not_after,
    relayProfile: response.relay_profile || {},
    relayUrl: material.relayUrl,
    nodeId: material.nodeId,
    sas,
    enrolledAt: new Date().toISOString()
  };
  delete stored.pairingCode;
  await writeMaterial(db, stored);
  self.postMessage({
    type: 'enrollment-complete',
    deviceId: response.device_id,
    notAfter: response.not_after
  });
  activeProfile = stored;
  connectRpcTunnel(stored).catch((error) => {
    self.postMessage({
      type: 'route-error',
      reason: error?.message || 'failed to open RPC tunnel'
    });
  });
}

function abortEnrollment(reason) {
  pendingEnrollment = null;
  pendingEnrollmentPost = null;
  closeExisting();
  self.postMessage({ type: 'enrollment-aborted', reason });
}

async function singleCertificateFingerprintHex(pem) {
  const certs = pemDecodeAll(pem, 'CERTIFICATE');
  if (certs.length !== 1) {
    throw new Error(`enrollment response must contain exactly one daemon CA certificate, got ${certs.length}`);
  }
  const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', certs[0]));
  return hexEncode(digest);
}

async function enrollmentSas(pairingCode, caFingerprintHex) {
  const input = concatBytes(
    new TextEncoder().encode('zeroclaw-enroll-sas-v1\0'),
    new TextEncoder().encode(pairingCode.trim()),
    new Uint8Array([0]),
    new TextEncoder().encode(caFingerprintHex.trim().toLowerCase())
  );
  const digest = hexEncode(new Uint8Array(await crypto.subtle.digest('SHA-256', input)))
    .slice(0, 8)
    .toUpperCase();
  return `${digest.slice(0, 4)}-${digest.slice(4, 8)}`;
}

function pemDecodeAll(pem, label) {
  const begin = `-----BEGIN ${label}-----`;
  const end = `-----END ${label}-----`;
  const certs = [];
  let offset = 0;
  while (true) {
    const start = pem.indexOf(begin, offset);
    if (start < 0) {
      break;
    }
    const bodyStart = start + begin.length;
    const bodyEnd = pem.indexOf(end, bodyStart);
    if (bodyEnd < 0) {
      throw new Error(`unterminated ${label} PEM block`);
    }
    const b64 = pem.slice(bodyStart, bodyEnd).replace(/\s+/g, '');
    const bin = atob(b64);
    const der = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i += 1) {
      der[i] = bin.charCodeAt(i);
    }
    certs.push(der);
    offset = bodyEnd + end.length;
  }
  if (certs.length === 0) {
    throw new Error(`missing ${label} PEM block`);
  }
  return certs;
}

function hexEncode(bytes) {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('');
}

async function handleRpcRequest(msg, port) {
  if (!port) {
    return;
  }
  try {
    const rpc = await ensureRpcClient();
    const request = parseRpcFetchBody(msg.body);
    const result = await rpc.call(request.method, request.params ?? null);
    replyRpc(port, {
      ok: true,
      status: 200,
      body: {
        jsonrpc: '2.0',
        id: request.id ?? null,
        result
      }
    });
  } catch (error) {
    replyRpc(port, {
      ok: false,
      status: 503,
      error: error?.message || TLS_ENGINE_UNAVAILABLE
    });
  }
}

async function ensureRpcClient() {
  if (rpcClient && !rpcClient.closed) {
    return rpcClient;
  }
  if (!activeProfile) {
    throw new Error('Browser enrollment is not complete');
  }
  if (!rpcConnecting) {
    rpcConnecting = connectRpcTunnel(activeProfile).finally(() => {
      rpcConnecting = null;
    });
  }
  return rpcConnecting;
}

async function connectRpcTunnel(profile) {
  if (!self.ZeroClawEnrollmentTls?.connectRpc) {
    throw new Error('Secure browser RPC tunnel engine is unavailable in this build');
  }
  const relayUrl = resolveRelayUrl(profile);
  const nodeId = resolveRelayNodeId(profile);
  if (!relayUrl || !nodeId) {
    throw new Error('Enrolled profile is missing relay coordinates');
  }
  const transport = await openRelayDataRoute(relayUrl, nodeId, 'connect');
  rpcClient = await self.ZeroClawEnrollmentTls.connectRpc(transport, {
    clientCertificatePem: profile.certPem,
    clientSigningKey: profile.signingKey,
    caChainPem: profile.caChainPem,
    serverName: '127.0.0.1',
    host: '127.0.0.1'
  });
  rpcClient.onNotification((message) => {
    self.postMessage({
      type: 'rpc-notification',
      method: message.method,
      params: message.params || null
    });
  });
  const connectedClient = rpcClient;
  connectedClient.readTask?.finally(() => {
    if (rpcClient === connectedClient) {
      self.postMessage({ type: 'rpc-closed', reason: 'Secure RPC tunnel closed' });
    }
  });
  self.postMessage({ type: 'rpc-ready', nodeId });
  return rpcClient;
}

function resolveRelayUrl(profile) {
  const relayUrl = profile.relayUrl || profile.relayProfile?.relay_url;
  if (!relayUrl) {
    return '';
  }
  return normalizeRelayWebSocketUrl(relayUrl);
}

function resolveRelayNodeId(profile) {
  return profile.nodeId || profile.relayProfile?.node_id || '';
}

function normalizeRelayWebSocketUrl(relayUrl) {
  if (/^wss?:\/\//i.test(relayUrl)) {
    return relayUrl;
  }
  const protocol = self.location.protocol === 'https:' ? 'wss:' : 'ws:';
  if (/^[^/]+:\d+$/.test(relayUrl)) {
    return `${protocol}//${relayUrl}/relay`;
  }
  const url = new URL(relayUrl, self.location.origin);
  if (url.protocol === 'http:') {
    url.protocol = 'ws:';
  } else if (url.protocol === 'https:') {
    url.protocol = 'wss:';
  }
  return url.toString();
}

function parseRpcFetchBody(body) {
  if (!body) {
    throw new Error('RPC request body is required');
  }
  const text = new TextDecoder().decode(body);
  const request = JSON.parse(text);
  if (!request.method) {
    throw new Error('RPC request is missing method');
  }
  return request;
}

function replyRpc(port, msg) {
  const reply = { ...msg };
  if (reply.body && !(reply.body instanceof ArrayBuffer)) {
    const encoded = new TextEncoder().encode(JSON.stringify(reply.body));
    reply.headers = { 'content-type': 'application/json' };
    reply.body = encoded.buffer;
    port.postMessage(reply, [reply.body]);
  } else {
    port.postMessage(reply);
  }
  port.close?.();
}

function openRelayDataRoute(relayUrl, nodeId, controlType) {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(relayUrl, 'zeroclaw.relay.v1');
    socket.binaryType = 'arraybuffer';
    let opened = false;
    let dataRoute = null;
    socket.addEventListener('open', () => {
      socket.send(JSON.stringify({ t: controlType, node_id: nodeId }));
    });
    socket.addEventListener('message', (event) => {
      if (typeof event.data !== 'string') {
        if (!dataRoute) {
          return;
        }
        try {
          const frame = decodeDataFrame(event.data);
          if (frame.connId === dataRoute.connId) {
            dataRoute.receive(frame.payload);
          }
        } catch (error) {
          dataRoute.close(error?.message || 'malformed relay data frame');
          socket.close();
        }
        return;
      }
      let frame;
      try {
        frame = JSON.parse(event.data);
      } catch (_) {
        reject(new Error('malformed relay control frame'));
        socket.close();
        return;
      }
      if (frame.t === 'opened') {
        opened = true;
        dataRoute = new RelayDataTransport(socket, frame.conn_id);
        resolve(dataRoute);
      } else if (frame.t === 'error') {
        reject(new Error(frame.msg || frame.code || 'relay error'));
        socket.close();
      } else if (frame.t === 'close' && dataRoute) {
        dataRoute.close(frame.reason || 'closed');
      }
    });
    socket.addEventListener('close', () => {
      if (dataRoute) {
        dataRoute.close('relay websocket closed');
      } else if (!opened) {
        reject(new Error('relay websocket closed before route opened'));
      }
    });
    socket.addEventListener('error', () => {
      if (!opened) {
        reject(new Error('relay websocket failed'));
      }
    });
  });
}

class RelayDataTransport {
  constructor(socket, connId) {
    this.socket = socket;
    this.connId = normalizeConnId(connId);
    this.queue = [];
    this.waiters = [];
    this.closed = false;
  }

  write(payload) {
    if (this.closed) {
      throw new Error('relay route is closed');
    }
    this.socket.send(encodeDataFrame(this.connId, payload));
  }

  receive(payload) {
    if (this.closed) {
      return;
    }
    const waiter = this.waiters.shift();
    if (waiter) {
      waiter(payload);
    } else {
      this.queue.push(payload);
    }
  }

  read() {
    if (this.queue.length > 0) {
      return Promise.resolve(this.queue.shift());
    }
    if (this.closed) {
      return Promise.resolve(null);
    }
    return new Promise((resolve) => {
      this.waiters.push(resolve);
    });
  }

  close(_reason) {
    if (this.closed) {
      return;
    }
    this.closed = true;
    for (const waiter of this.waiters.splice(0)) {
      waiter(null);
    }
  }
}

function encodeDataFrame(connId, payload) {
  const bytes = toUint8Array(payload);
  const out = new Uint8Array(8 + bytes.byteLength);
  writeUint64(out, normalizeConnId(connId));
  out.set(bytes, 8);
  return out;
}

function decodeDataFrame(data) {
  const bytes = toUint8Array(data);
  if (bytes.byteLength < 8) {
    throw new Error('relay data frame is too short');
  }
  return {
    connId: readUint64(bytes),
    payload: bytes.slice(8)
  };
}

function toUint8Array(value) {
  if (value instanceof Uint8Array) {
    return value;
  }
  if (value instanceof ArrayBuffer) {
    return new Uint8Array(value);
  }
  if (ArrayBuffer.isView(value)) {
    return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
  }
  throw new Error('expected binary payload');
}

function normalizeConnId(connId) {
  if (!Number.isSafeInteger(connId) || connId < 0) {
    throw new Error('relay connection id is out of range');
  }
  return connId;
}

function writeUint64(out, value) {
  let n = BigInt(value);
  for (let i = 7; i >= 0; i -= 1) {
    out[i] = Number(n & 0xffn);
    n >>= 8n;
  }
}

function readUint64(bytes) {
  let n = 0n;
  for (let i = 0; i < 8; i += 1) {
    n = (n << 8n) | BigInt(bytes[i]);
  }
  if (n > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new Error('relay connection id is out of range');
  }
  return Number(n);
}

async function ensureEnrollmentMaterial(nodeId) {
  if (!self.crypto?.subtle) {
    throw new Error('WebCrypto is not available');
  }
  if (!self.indexedDB) {
    throw new Error('IndexedDB is not available');
  }

  const db = await openEnrollmentDb();
  const id = `${MATERIAL_KEY_PREFIX}${nodeId}`;
  const existing = await readMaterial(db, id);
  if (existing?.signingKey && existing?.csrPem) {
    return existing;
  }

  const keyPair = await crypto.subtle.generateKey(
    { name: 'ECDSA', namedCurve: 'P-256' },
    false,
    ['sign']
  );
  const spki = new Uint8Array(await crypto.subtle.exportKey('spki', keyPair.publicKey));
  const csr = await createCertificationRequest(
    keyPair.privateKey,
    spki,
    enrollmentCommonName(nodeId)
  );
  const material = {
    id,
    nodeId,
    signingKey: keyPair.privateKey,
    csrPem: pemEncode('CERTIFICATE REQUEST', csr),
    createdAt: new Date().toISOString()
  };

  await writeMaterial(db, material);
  return material;
}

async function loadCompletedEnrollment(nodeId) {
  if (!self.indexedDB) {
    throw new Error('IndexedDB is not available');
  }
  const db = await openEnrollmentDb();
  const stored = await readMaterial(db, `${MATERIAL_KEY_PREFIX}${nodeId}`);
  if (!isCompletedEnrollment(stored)) {
    return null;
  }
  return {
    ...stored,
    nodeId: stored.nodeId || nodeId
  };
}

function isCompletedEnrollment(profile) {
  return Boolean(
    profile?.signingKey &&
    profile?.certPem &&
    profile?.caChainPem &&
    (profile?.nodeId || profile?.relayProfile?.node_id)
  );
}

function openEnrollmentDb() {
  if (dbPromise) {
    return dbPromise;
  }
  dbPromise = new Promise((resolve, reject) => {
    const req = indexedDB.open(DB_NAME, DB_VERSION);
    req.onupgradeneeded = () => {
      const db = req.result;
      if (!db.objectStoreNames.contains(MATERIAL_STORE)) {
        db.createObjectStore(MATERIAL_STORE, { keyPath: 'id' });
      }
    };
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error || new Error('failed to open enrollment database'));
    req.onblocked = () => reject(new Error('enrollment database upgrade blocked'));
  });
  return dbPromise;
}

function readMaterial(db, id) {
  return new Promise((resolve, reject) => {
    const tx = db.transaction(MATERIAL_STORE, 'readonly');
    const req = tx.objectStore(MATERIAL_STORE).get(id);
    req.onsuccess = () => resolve(req.result || null);
    req.onerror = () => reject(req.error || new Error('failed to read enrollment material'));
  });
}

function writeMaterial(db, material) {
  return new Promise((resolve, reject) => {
    const tx = db.transaction(MATERIAL_STORE, 'readwrite');
    tx.objectStore(MATERIAL_STORE).put(material);
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error || new Error('failed to store enrollment material'));
    tx.onabort = () => reject(tx.error || new Error('enrollment material store aborted'));
  });
}

async function createCertificationRequest(signingKey, spki, commonName) {
  const cri = derSequence(
    derInteger(new Uint8Array([0])),
    derNameCommonName(commonName),
    spki,
    der(0xa0)
  );
  const rawSignature = new Uint8Array(await crypto.subtle.sign(
    { name: 'ECDSA', hash: 'SHA-256' },
    signingKey,
    cri
  ));
  return derSequence(
    cri,
    derAlgorithmIdentifierEcdsaSha256(),
    derBitString(ecdsaRawSignatureToDer(rawSignature))
  );
}

function enrollmentCommonName(nodeId) {
  const normalized = nodeId.replace(/[^A-Za-z0-9_.:-]/g, '-').slice(0, 64) || 'client';
  return `zeroclaw-browser-${normalized}`;
}

function derNameCommonName(commonName) {
  return derSequence(
    der(0x31, derSequence(
      derObjectIdentifier([0x55, 0x04, 0x03]),
      derUtf8String(commonName)
    ))
  );
}

function derAlgorithmIdentifierEcdsaSha256() {
  return derSequence(derObjectIdentifier([
    0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02
  ]));
}

function ecdsaRawSignatureToDer(rawSignature) {
  if (rawSignature.length % 2 !== 0) {
    throw new Error('invalid ECDSA signature length');
  }
  const width = rawSignature.length / 2;
  return derSequence(
    derInteger(rawSignature.slice(0, width)),
    derInteger(rawSignature.slice(width))
  );
}

function derSequence(...parts) {
  return der(0x30, ...parts);
}

function derInteger(bytes) {
  let value = stripLeadingZeroes(bytes);
  if (value.length === 0) {
    value = new Uint8Array([0]);
  }
  if (value[0] & 0x80) {
    value = concatBytes(new Uint8Array([0]), value);
  }
  return der(0x02, value);
}

function derObjectIdentifier(encoded) {
  return der(0x06, new Uint8Array(encoded));
}

function derUtf8String(text) {
  return der(0x0c, new TextEncoder().encode(text));
}

function derBitString(bytes) {
  return der(0x03, concatBytes(new Uint8Array([0]), bytes));
}

function der(tag, ...parts) {
  const body = concatBytes(...parts);
  return concatBytes(new Uint8Array([tag]), derLength(body.length), body);
}

function derLength(length) {
  if (length < 0x80) {
    return new Uint8Array([length]);
  }
  const bytes = [];
  let value = length;
  while (value > 0) {
    bytes.unshift(value & 0xff);
    value >>= 8;
  }
  return new Uint8Array([0x80 | bytes.length, ...bytes]);
}

function stripLeadingZeroes(bytes) {
  let offset = 0;
  while (offset < bytes.length - 1 && bytes[offset] === 0) {
    offset += 1;
  }
  return bytes.slice(offset);
}

function concatBytes(...parts) {
  const total = parts.reduce((sum, part) => sum + part.length, 0);
  const out = new Uint8Array(total);
  let offset = 0;
  for (const part of parts) {
    out.set(part, offset);
    offset += part.length;
  }
  return out;
}

function pemEncode(label, bytes) {
  let binary = '';
  for (let i = 0; i < bytes.length; i += 1) {
    binary += String.fromCharCode(bytes[i]);
  }
  const base64 = btoa(binary);
  const lines = [];
  for (let i = 0; i < base64.length; i += 64) {
    lines.push(base64.slice(i, i + 64));
  }
  return `-----BEGIN ${label}-----\n${lines.join('\n')}\n-----END ${label}-----\n`;
}

function closeExisting() {
  pendingEnrollmentPost = null;
  rpcConnecting = null;
  if (rpcClient) {
    const old = rpcClient;
    rpcClient = null;
    try {
      old.close?.();
    } catch (_) {}
  }
  if (route) {
    route.close('replaced');
    route = null;
  }
  if (ws) {
    const old = ws;
    ws = null;
    old.close();
  }
}
"#;
