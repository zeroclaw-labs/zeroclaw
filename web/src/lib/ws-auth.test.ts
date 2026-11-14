import assert from 'node:assert/strict';
import test from 'node:test';

test('chat and ACP keep paired credentials out of URLs while retaining protocol authentication', async (t) => {
  const names = ['window', 'localStorage', 'WebSocket'] as const;
  const originals = names.map((name) => [name, Object.getOwnPropertyDescriptor(globalThis, name)] as const);
  const sockets: Array<{ url: string; protocols: string[] }> = [];
  let token: string | null = 'synthetic-paired-credential';
  class Socket {
    static CONNECTING = 0;
    static OPEN = 1;
    readyState = 0;
    constructor(url: string, protocols: string[]) {
      sockets.push({ url, protocols });
    }
    close() { this.readyState = 3; }
  }
  Object.defineProperty(globalThis, 'window', { configurable: true, value: Object.assign(new EventTarget(), {
    location: { protocol: 'https:', host: 'gateway.example.test' },
    __ZEROCLAW_BASE__: '/proxy',
  }) });
  Object.defineProperty(globalThis, 'localStorage', { configurable: true, value: {
    getItem: () => token,
  } });
  Object.defineProperty(globalThis, 'WebSocket', { configurable: true, value: Socket });
  t.after(() => {
    for (const [name, descriptor] of originals) {
      if (descriptor) Object.defineProperty(globalThis, name, descriptor);
      else Reflect.deleteProperty(globalThis, name);
    }
  });
  const { WebSocketClient } = await import('./ws.ts');
  const { AcpWebSocketClient } = await import('./acp.ts');
  for (const credential of ['synthetic-paired-credential', null]) {
    token = credential;
    const start = sockets.length;
    const chat = new WebSocketClient({ agentAlias: 'example-agent', sessionId: 'example-session', autoReconnect: false });
    const acp = new AcpWebSocketClient();
    chat.connect(); acp.connect();
    const [chatSocket, acpSocket] = sockets.slice(start);
    assert.ok(chatSocket);
    assert.ok(acpSocket);
    const chatUrl = new URL(chatSocket.url), acpUrl = new URL(acpSocket.url);
    assert.equal(chatUrl.pathname, '/proxy/ws/chat');
    assert.equal(chatUrl.searchParams.get('agent'), 'example-agent');
    assert.equal(chatUrl.searchParams.get('session_id'), 'example-session');
    assert.equal(acpUrl.pathname, '/proxy/acp');
    for (const socket of [chatSocket, acpSocket]) {
      assert.equal(new URL(socket.url).searchParams.has('token'), false);
      if (credential) assert.equal(socket.url.includes(credential), false);
    }
    assert.deepEqual(chatSocket.protocols, ['zeroclaw.v1', ...(credential ? [`bearer.${credential}`] : [])]);
    assert.deepEqual(acpSocket.protocols, ['zeroclaw.acp.v1', ...(credential ? [`bearer.${credential}`] : [])]);
    chat.disconnect(); acp.disconnect();
  }
});
