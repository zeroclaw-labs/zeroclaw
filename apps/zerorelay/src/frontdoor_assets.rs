pub(crate) const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>ZeroClaw Relay</title>
  <style>
    :root { color-scheme: light dark; font-family: Inter, ui-sans-serif, system-ui, sans-serif; }
    body { margin: 0; min-height: 100vh; display: grid; place-items: start center; padding: 32px 0; background: #f7f8fa; color: #17191c; }
    main { width: min(760px, calc(100vw - 32px)); display: grid; gap: 18px; }
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
    .chat { display: grid; gap: 12px; }
    .messages { min-height: 180px; max-height: 420px; overflow: auto; display: grid; align-content: start; gap: 10px; border: 1px solid #d9dde4; border-radius: 6px; padding: 12px; background: #fff; }
    .msg { display: grid; gap: 4px; white-space: pre-wrap; overflow-wrap: anywhere; font-size: 14px; line-height: 1.45; }
    .role { font-size: 11px; font-weight: 800; letter-spacing: 0; text-transform: uppercase; color: #606a78; }
    .approval { display: flex; flex-wrap: wrap; gap: 8px; align-items: center; }
    .approval button { height: 34px; padding: 0 12px; }
    output { min-height: 22px; font-size: 13px; color: #4d5663; }
    @media (prefers-color-scheme: dark) {
      body { background: #111316; color: #f3f5f7; }
      input, textarea, .messages { background: #191d22; color: #f3f5f7; border-color: #343b45; }
      .role { color: #b6beca; }
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
    <section id="chat-panel" class="chat" hidden>
      <form id="chat">
        <label>Agent <input id="agent-alias" autocomplete="off" spellcheck="false" value="default"></label>
        <label>Message <textarea id="prompt" autocomplete="off" spellcheck="true" required></textarea></label>
        <button id="send" type="submit">Send</button>
      </form>
      <div id="messages" class="messages" aria-live="polite"></div>
    </section>
    <output id="status">Disconnected</output>
  </main>
  <script src="/app.js" defer></script>
</body>
</html>
"#;

pub(crate) const APP_JS: &str = r#"const form = document.getElementById('pair');
const status = document.getElementById('status');
const button = document.getElementById('connect');
const sasPanel = document.getElementById('sas-panel');
const sasCode = document.getElementById('sas-code');
const sasConfirm = document.getElementById('sas-confirm');
const sasAbort = document.getElementById('sas-abort');
const chatPanel = document.getElementById('chat-panel');
const chatForm = document.getElementById('chat');
const sendButton = document.getElementById('send');
const promptInput = document.getElementById('prompt');
const agentAliasInput = document.getElementById('agent-alias');
const messages = document.getElementById('messages');
const tunnel = new Worker('/tunnel-worker.js');
const encoder = new TextEncoder();
const decoder = new TextDecoder();

let rpcSeq = 1;
let sessionId = null;
let currentAssistant = null;
let turnInFlight = false;

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
    const transfers = [...event.ports];
    if (msg.body instanceof ArrayBuffer) {
      transfers.push(msg.body);
    }
    tunnel.postMessage(msg, transfers);
  });
}

tunnel.addEventListener('message', (event) => {
  const msg = event.data || {};
  if (msg.type === 'connecting') {
    status.textContent = 'Preparing client key';
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
    button.disabled = false;
  } else if (msg.type === 'rpc-ready') {
    status.textContent = 'Secure tunnel ready. Choose an agent and send a message.';
    chatPanel.hidden = false;
    sendButton.disabled = false;
  } else if (msg.type === 'rpc-notification') {
    handleRpcNotification(msg);
  } else if (msg.type === 'rpc-closed') {
    status.textContent = msg.reason || 'Secure tunnel closed.';
    sendButton.disabled = true;
  } else if (msg.type === 'enrollment-aborted') {
    sasPanel.hidden = true;
    status.textContent = msg.reason || 'Enrollment aborted';
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

  button.disabled = true;
  sendButton.disabled = true;
  sasPanel.hidden = true;
  sasConfirm.disabled = true;
  sasAbort.disabled = true;
  chatPanel.hidden = true;
  messages.replaceChildren();
  sessionId = null;
  currentAssistant = null;
  turnInFlight = false;
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

chatForm.addEventListener('submit', async (event) => {
  event.preventDefault();
  const prompt = promptInput.value.trim();
  if (!prompt || turnInFlight) {
    return;
  }
  promptInput.value = '';
  appendMessage('You', prompt);
  currentAssistant = null;
  turnInFlight = true;
  sendButton.disabled = true;
  status.textContent = 'Sending prompt';
  try {
    const session = await ensureSession();
    await rpcCall('session/prompt', {
      session_id: session.session_id,
      prompt,
      attachments: []
    });
    status.textContent = 'Waiting for daemon';
  } catch (error) {
    turnInFlight = false;
    sendButton.disabled = false;
    status.textContent = error?.message || 'Prompt failed';
    appendMessage('System', status.textContent);
  }
});

async function ensureSession() {
  if (sessionId) {
    return { session_id: sessionId };
  }
  const agent = agentAliasInput.value.trim() || 'default';
  const result = await rpcCall('session/new', {
    agent_alias: agent,
    chat_mode: 'chat'
  });
  sessionId = result.session_id;
  agentAliasInput.disabled = true;
  appendMessage('System', `Session ready for agent ${result.agent_alias || agent}.`);
  return result;
}

function rpcCall(method, params = null) {
  return new Promise((resolve, reject) => {
    const id = rpcSeq++;
    const channel = new MessageChannel();
    const timeout = setTimeout(() => {
      channel.port1.close();
      reject(new Error('browser RPC request timed out'));
    }, 60000);
    channel.port1.onmessage = (event) => {
      clearTimeout(timeout);
      channel.port1.close();
      const msg = event.data || {};
      if (!msg.ok) {
        reject(new Error(msg.error || 'browser RPC request failed'));
        return;
      }
      try {
        const body = msg.body ? decoder.decode(toUint8(msg.body)) : '{}';
        const parsed = JSON.parse(body || '{}');
        if (parsed.error) {
          reject(new Error(parsed.error.message || 'RPC error'));
        } else {
          resolve(parsed.result);
        }
      } catch (error) {
        reject(error);
      }
    };
    channel.port1.start();

    const bytes = encoder.encode(JSON.stringify({ jsonrpc: '2.0', id, method, params }));
    const body = bytes.buffer;
    tunnel.postMessage({
      type: 'zeroclaw-rpc-request',
      method: 'POST',
      url: '/__zeroclaw/rpc',
      headers: [['content-type', 'application/json']],
      body
    }, [channel.port2, body]);
  });
}

function handleRpcNotification(msg) {
  if (msg.method !== 'session/update') {
    return;
  }
  const params = msg.params || {};
  if (sessionId && params.session_id && params.session_id !== sessionId) {
    return;
  }
  if (params.type === 'agent_message_chunk') {
    appendAssistantChunk(params.text || '');
  } else if (params.type === 'agent_thought_chunk') {
    appendMessage('Thought', params.text || '');
  } else if (params.type === 'tool_call') {
    appendMessage('Tool', `${params.name || 'tool'} called`);
  } else if (params.type === 'tool_result') {
    appendMessage('Tool', `${params.name || 'tool'} returned`);
  } else if (params.type === 'approval_request') {
    appendApproval(params);
  } else if (params.type === 'turn_complete') {
    if (!currentAssistant && params.content) {
      appendMessage('ZeroClaw', params.content);
    }
    currentAssistant = null;
    turnInFlight = false;
    sendButton.disabled = false;
    status.textContent = params.outcome === 'completed'
      ? 'Ready'
      : `Turn ${params.outcome || 'finished'}`;
  } else if (params.type === 'history_trimmed') {
    appendMessage('System', params.reason || 'Conversation history was trimmed.');
  }
}

function appendAssistantChunk(text) {
  if (!currentAssistant) {
    currentAssistant = appendMessage('ZeroClaw', '');
  }
  const body = currentAssistant.querySelector('.body');
  body.textContent += text;
  scrollMessages();
}

function appendApproval(params) {
  const node = appendMessage(
    'Approval',
    `${params.tool_name || 'tool'}: ${params.arguments_summary || ''}`
  );
  const row = document.createElement('div');
  row.className = 'approval';
  const allow = document.createElement('button');
  allow.type = 'button';
  allow.textContent = 'Allow';
  const deny = document.createElement('button');
  deny.type = 'button';
  deny.className = 'secondary';
  deny.textContent = 'Deny';
  row.append(allow, deny);
  node.append(row);
  const decide = async (decision) => {
    allow.disabled = true;
    deny.disabled = true;
    try {
      await rpcCall('session/approve', {
        session_id: params.session_id,
        request_id: params.request_id,
        decision
      });
      appendMessage('System', `Approval ${decision}.`);
    } catch (error) {
      appendMessage('System', error?.message || 'Approval failed');
    }
  };
  allow.addEventListener('click', () => decide('allow_once'));
  deny.addEventListener('click', () => decide('reject'));
}

function appendMessage(role, text) {
  const node = document.createElement('div');
  node.className = 'msg';
  const label = document.createElement('div');
  label.className = 'role';
  label.textContent = role;
  const body = document.createElement('div');
  body.className = 'body';
  body.textContent = text;
  node.append(label, body);
  messages.append(node);
  scrollMessages();
  return node;
}

function scrollMessages() {
  messages.scrollTop = messages.scrollHeight;
}

function toUint8(value) {
  if (value instanceof Uint8Array) {
    return value;
  }
  if (value instanceof ArrayBuffer) {
    return new Uint8Array(value);
  }
  if (ArrayBuffer.isView(value)) {
    return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
  }
  return new Uint8Array(0);
}
"#;

pub(crate) const SERVICE_WORKER_JS: &str = r#"self.addEventListener('install', (event) => {
  event.waitUntil(self.skipWaiting());
});

const RPC_TIMEOUT_MS = 15000;
const TLS_ENGINE_UNAVAILABLE =
  'Secure browser TLS enrollment is unavailable in this build.';

self.addEventListener('activate', (event) => {
  event.waitUntil(self.clients.claim());
});

self.addEventListener('fetch', (event) => {
  const url = new URL(event.request.url);
  if (url.pathname === '/__zeroclaw/tunnel/state') {
    event.respondWith(new Response(JSON.stringify({
      ready: true,
      enrollmentTls: true,
      mtlsEngine: true
    }), {
      headers: { 'content-type': 'application/json' }
    }));
  } else if (url.pathname === '/__zeroclaw/rpc') {
    event.respondWith(proxyRpc(event));
  }
});

self.addEventListener('message', (event) => {
  event.source?.postMessage({ type: 'zeroclaw-relay-worker-ready' });
});

async function proxyRpc(event) {
  const client = event.clientId ? await self.clients.get(event.clientId) : null;
  if (!client) {
    return unavailable('no controlled browser client is available');
  }

  let body = null;
  try {
    body = await requestBody(event.request);
  } catch (_) {
    return unavailable('could not read request body');
  }

  const url = new URL(event.request.url);
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

  const msg = {
    type: 'zeroclaw-rpc-request',
    method: event.request.method,
    url: `${url.pathname}${url.search}`,
    headers: Array.from(event.request.headers.entries()),
    body
  };
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
    return unavailable('could not dispatch browser tunnel request');
  }

  return responseFromRpcMessage(await reply);
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
  return new Response(JSON.stringify({ error }), {
    status,
    headers: { 'content-type': 'application/json' }
  });
}
"#;

pub(crate) const TUNNEL_WORKER_JS: &str = r#"const DB_NAME = 'zeroclaw-relay-enrollment';
const DB_VERSION = 1;
const MATERIAL_STORE = 'materials';
const MATERIAL_KEY_PREFIX = 'node:';

importScripts('/tls-engine.js');

let ws = null;
let dbPromise = null;
let route = null;
let pendingEnrollment = null;
let activeProfile = null;
let rpcClient = null;
let rpcConnecting = null;

const TLS_ENGINE_UNAVAILABLE =
  'Secure browser TLS enrollment is unavailable in this build.';

self.addEventListener('message', (event) => {
  const msg = event.data || {};
  if (msg.type === 'connectEnrollment') {
    connectEnrollment(msg.relayUrl, msg.nodeId, msg.pairingCode);
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
      beginBrowserEnrollment(route, material);
    } else if (frame.t === 'error') {
      self.postMessage({ type: 'route-error', reason: frame.code || 'relay error' });
      closeExisting();
    } else if (frame.t === 'close') {
      self.postMessage({ type: 'route-closed', reason: frame.reason || 'closed' });
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
  if (!self.ZeroClawEnrollmentTls?.enroll) {
    self.postMessage({
      type: 'tls-engine-missing',
      reason: TLS_ENGINE_UNAVAILABLE
    });
    return;
  }
  try {
    const response = await self.ZeroClawEnrollmentTls.enroll(_transport, {
      pairingCode: _material.pairingCode,
      csrPem: _material.csrPem,
      serverName: '127.0.0.1',
      host: '127.0.0.1'
    });
    const caFingerprint = await firstCertificateFingerprintHex(response.ca_chain_pem);
    const sas = await enrollmentSas(_material.pairingCode, caFingerprint);
    pendingEnrollment = {
      material: _material,
      response,
      sas
    };
    self.postMessage({
      type: 'enrollment-sas',
      sas,
      deviceId: response.device_id
    });
  } catch (error) {
    self.postMessage({
      type: 'route-error',
      reason: error?.message || 'secure enrollment failed'
    });
    closeExisting();
  }
}

async function confirmEnrollment() {
  if (!pendingEnrollment) {
    self.postMessage({ type: 'enrollment-aborted', reason: 'No enrollment is waiting for confirmation' });
    return;
  }
  const { material, response, sas } = pendingEnrollment;
  pendingEnrollment = null;
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
  closeExisting();
  self.postMessage({ type: 'enrollment-aborted', reason });
}

async function firstCertificateFingerprintHex(pem) {
  const der = pemDecodeFirst(pem, 'CERTIFICATE');
  const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', der));
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

function pemDecodeFirst(pem, label) {
  const begin = `-----BEGIN ${label}-----`;
  const end = `-----END ${label}-----`;
  const start = pem.indexOf(begin);
  if (start < 0) {
    throw new Error(`missing ${label} PEM block`);
  }
  const bodyStart = start + begin.length;
  const bodyEnd = pem.indexOf(end, bodyStart);
  if (bodyEnd < 0) {
    throw new Error(`unterminated ${label} PEM block`);
  }
  const b64 = pem.slice(bodyStart, bodyEnd).replace(/\s+/g, '');
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i += 1) {
    out[i] = bin.charCodeAt(i);
  }
  return out;
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
  const relayUrl = profile.relayProfile?.relay_url || profile.relayUrl;
  const nodeId = profile.relayProfile?.node_id || profile.nodeId;
  if (!relayUrl || !nodeId) {
    throw new Error('Enrolled profile is missing relay coordinates');
  }
  const transport = await openRelayDataRoute(relayUrl, nodeId, 'connect');
  rpcClient = await self.ZeroClawEnrollmentTls.connectRpc(transport, {
    clientCertificatePem: profile.certPem,
    clientPrivateKey: profile.privateKey,
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
  self.postMessage({ type: 'rpc-ready' });
  return rpcClient;
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
  if (existing?.privateKey && existing?.csrPem) {
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
    privateKey: keyPair.privateKey,
    csrPem: pemEncode('CERTIFICATE REQUEST', csr),
    createdAt: new Date().toISOString()
  };

  await writeMaterial(db, material);
  return material;
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

async function createCertificationRequest(privateKey, spki, commonName) {
  const cri = derSequence(
    derInteger(new Uint8Array([0])),
    derNameCommonName(commonName),
    spki,
    der(0xa0)
  );
  const rawSignature = new Uint8Array(await crypto.subtle.sign(
    { name: 'ECDSA', hash: 'SHA-256' },
    privateKey,
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
