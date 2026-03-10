import http from 'node:http';
import { WebSocketServer } from 'ws';

const PORT = Number(process.env.PW_API_PORT ?? '4174');
const VALID_TOKEN = 'test-token';
const VALID_CODE = '123456';

let cronJobs = [
  {
    id: 'job-alpha',
    name: 'nightly sync',
    command: 'sync --nightly',
    next_run: '2026-03-10T00:00:00.000Z',
    last_run: '2026-03-09T00:00:00.000Z',
    last_status: 'ok',
    enabled: true,
  },
];

let memoryEntries = [
  {
    id: 'memory-1',
    key: 'workspace_mode',
    content: 'Dashboard E2E workspace is active.',
    category: 'context',
    timestamp: '2026-03-09T12:00:00.000Z',
    session_id: 'session-1',
    score: 0.99,
  },
];

const healthSnapshot = {
  pid: 4242,
  updated_at: '2026-03-09T12:00:00.000Z',
  uptime_seconds: 12345,
  components: {
    gateway: {
      status: 'ok',
      updated_at: '2026-03-09T12:00:00.000Z',
      last_ok: '2026-03-09T12:00:00.000Z',
      last_error: null,
      restart_count: 0,
    },
    scheduler: {
      status: 'ok',
      updated_at: '2026-03-09T12:00:00.000Z',
      last_ok: '2026-03-09T12:00:00.000Z',
      last_error: null,
      restart_count: 1,
    },
  },
};

function sendJson(res, statusCode, body) {
  res.writeHead(statusCode, {
    'Content-Type': 'application/json',
    'Cache-Control': 'no-store',
  });
  res.end(JSON.stringify(body));
}

function unauthorized(res) {
  sendJson(res, 401, { error: 'Unauthorized' });
}

function readBody(req) {
  return new Promise((resolve, reject) => {
    let data = '';
    req.on('data', (chunk) => {
      data += chunk;
    });
    req.on('end', () => resolve(data));
    req.on('error', reject);
  });
}

function isAuthorized(req) {
  const authHeader = req.headers.authorization;
  if (authHeader === `Bearer ${VALID_TOKEN}`) {
    return true;
  }

  const url = new URL(req.url, `http://${req.headers.host}`);
  return url.searchParams.get('token') === VALID_TOKEN;
}

const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, `http://${req.headers.host}`);

  if (req.method === 'GET' && url.pathname === '/health') {
    return sendJson(res, 200, { require_pairing: true, paired: false });
  }

  if (req.method === 'POST' && url.pathname === '/pair') {
    if (req.headers['x-pairing-code'] !== VALID_CODE) {
      return sendJson(res, 400, { error: 'Invalid pairing code' });
    }
    return sendJson(res, 200, { token: VALID_TOKEN });
  }

  if (url.pathname.startsWith('/api') && !isAuthorized(req)) {
    return unauthorized(res);
  }

  if (req.method === 'GET' && url.pathname === '/api/status') {
    return sendJson(res, 200, {
      provider: 'openrouter',
      model: 'anthropic/claude-sonnet-4.6',
      temperature: 0.7,
      uptime_seconds: 12345,
      gateway_port: PORT,
      locale: 'en',
      memory_backend: 'sqlite',
      paired: true,
      channels: {
        discord: true,
        telegram: false,
        slack: false,
      },
      health: healthSnapshot,
    });
  }

  if (req.method === 'GET' && url.pathname === '/api/health') {
    return sendJson(res, 200, { health: healthSnapshot });
  }

  if (req.method === 'GET' && url.pathname === '/api/config') {
    return sendJson(res, 200, {
      format: 'toml',
      content: [
        'default_provider = "openrouter"',
        'default_model = "anthropic/claude-sonnet-4.6"',
        '',
        '[gateway]',
        `port = ${PORT}`,
      ].join('\n'),
    });
  }

  if (req.method === 'PUT' && url.pathname === '/api/config') {
    await readBody(req);
    return sendJson(res, 200, { status: 'ok' });
  }

  if (req.method === 'GET' && url.pathname === '/api/tools') {
    return sendJson(res, 200, {
      tools: [
        {
          name: 'shell',
          description: 'Execute shell commands in the active workspace.',
          parameters: { type: 'object', properties: { command: { type: 'string' } } },
        },
        {
          name: 'memory_store',
          description: 'Persist a memory entry.',
          parameters: { type: 'object', properties: { key: { type: 'string' } } },
        },
      ],
    });
  }

  if (req.method === 'GET' && url.pathname === '/api/cli-tools') {
    return sendJson(res, 200, {
      cli_tools: [
        { name: 'git', path: '/usr/bin/git', version: '2.39.0', category: 'vcs' },
        { name: 'cargo', path: '/usr/bin/cargo', version: '1.86.0', category: 'rust' },
      ],
    });
  }

  if (req.method === 'GET' && url.pathname === '/api/cron') {
    return sendJson(res, 200, { jobs: cronJobs });
  }

  if (req.method === 'POST' && url.pathname === '/api/cron') {
    const raw = await readBody(req);
    const body = JSON.parse(raw || '{}');
    const job = {
      id: `job-${Date.now()}`,
      name: body.name ?? null,
      command: body.command,
      next_run: '2026-03-11T00:00:00.000Z',
      last_run: null,
      last_status: null,
      enabled: body.enabled ?? true,
    };
    cronJobs = [...cronJobs, job];
    return sendJson(res, 200, { status: 'ok', job });
  }

  if (req.method === 'DELETE' && url.pathname.startsWith('/api/cron/')) {
    const id = decodeURIComponent(url.pathname.split('/').pop());
    cronJobs = cronJobs.filter((job) => job.id !== id);
    res.writeHead(204);
    return res.end();
  }

  if (req.method === 'GET' && url.pathname === '/api/integrations') {
    return sendJson(res, 200, {
      integrations: [
        {
          name: 'Discord',
          description: 'Send notifications and respond in channels.',
          category: 'chat',
          status: 'Active',
        },
        {
          name: 'GitHub',
          description: 'Track pull requests and issues.',
          category: 'devops',
          status: 'Available',
        },
        {
          name: 'Linear',
          description: 'Sync roadmap items and tasks.',
          category: 'project',
          status: 'ComingSoon',
        },
      ],
    });
  }

  if (req.method === 'POST' && url.pathname === '/api/doctor') {
    return sendJson(res, 200, {
      results: [
        { severity: 'ok', category: 'config', message: 'Configuration looks healthy.' },
        { severity: 'warn', category: 'network', message: 'Webhook endpoint is not configured.' },
      ],
    });
  }

  if (req.method === 'GET' && url.pathname === '/api/memory') {
    const query = url.searchParams.get('query')?.toLowerCase() ?? '';
    const category = url.searchParams.get('category') ?? '';
    const entries = memoryEntries.filter((entry) => {
      const matchesQuery = !query
        || entry.key.toLowerCase().includes(query)
        || entry.content.toLowerCase().includes(query);
      const matchesCategory = !category || entry.category === category;
      return matchesQuery && matchesCategory;
    });
    return sendJson(res, 200, { entries });
  }

  if (req.method === 'POST' && url.pathname === '/api/memory') {
    const raw = await readBody(req);
    const body = JSON.parse(raw || '{}');
    const entry = {
      id: `memory-${Date.now()}`,
      key: body.key,
      content: body.content,
      category: body.category || 'notes',
      timestamp: new Date().toISOString(),
      session_id: 'session-e2e',
      score: 1,
    };
    memoryEntries = [entry, ...memoryEntries];
    return sendJson(res, 200, { status: 'ok' });
  }

  if (req.method === 'DELETE' && url.pathname.startsWith('/api/memory/')) {
    const key = decodeURIComponent(url.pathname.split('/').pop());
    memoryEntries = memoryEntries.filter((entry) => entry.key !== key);
    res.writeHead(204);
    return res.end();
  }

  if (req.method === 'GET' && url.pathname === '/api/cost') {
    return sendJson(res, 200, {
      cost: {
        session_cost_usd: 0.0132,
        daily_cost_usd: 0.1024,
        monthly_cost_usd: 2.3811,
        total_tokens: 48231,
        request_count: 128,
        by_model: {
          'anthropic/claude-sonnet-4.6': {
            model: 'anthropic/claude-sonnet-4.6',
            cost_usd: 1.8123,
            total_tokens: 35123,
            request_count: 84,
          },
          'openai/gpt-4o-mini': {
            model: 'openai/gpt-4o-mini',
            cost_usd: 0.5688,
            total_tokens: 13108,
            request_count: 44,
          },
        },
      },
    });
  }

  if (req.method === 'GET' && url.pathname === '/api/events') {
    res.writeHead(200, {
      'Content-Type': 'text/event-stream',
      'Cache-Control': 'no-cache',
      Connection: 'keep-alive',
    });

    const push = (payload) => {
      res.write(`event: ${payload.type}\n`);
      res.write(`data: ${JSON.stringify(payload)}\n\n`);
    };

    push({ type: 'status', timestamp: new Date().toISOString(), message: 'Event stream connected.' });
    const interval = setInterval(() => {
      push({ type: 'health', timestamp: new Date().toISOString(), message: 'Scheduler heartbeat ok.' });
    }, 750);

    req.on('close', () => {
      clearInterval(interval);
      res.end();
    });
    return;
  }

  sendJson(res, 404, { error: 'Not found' });
});

const wsServer = new WebSocketServer({ noServer: true });

wsServer.on('connection', (socket) => {
  socket.send(JSON.stringify({ type: 'message', content: 'Connected to mock ZeroClaw runtime.' }));

  socket.on('message', (raw) => {
    const message = JSON.parse(String(raw));
    if (message.type !== 'message') {
      return;
    }

    socket.send(JSON.stringify({ type: 'chunk', content: 'Echo: ' }));
    socket.send(JSON.stringify({ type: 'done', content: `Echo: ${message.content}` }));
  });
});

server.on('upgrade', (req, socket, head) => {
  const url = new URL(req.url, `http://${req.headers.host}`);
  if (url.pathname !== '/ws/chat' || !isAuthorized(req)) {
    socket.write('HTTP/1.1 401 Unauthorized\r\n\r\n');
    socket.destroy();
    return;
  }

  wsServer.handleUpgrade(req, socket, head, (client) => {
    wsServer.emit('connection', client, req);
  });
});

server.listen(PORT, '127.0.0.1', () => {
  console.log(`Mock backend listening on http://127.0.0.1:${PORT}`);
});
