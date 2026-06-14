import { useState, useEffect, useRef, useCallback, useMemo } from 'react';
import { Monitor, Trash2, History, RefreshCw } from 'lucide-react';
import { apiFetch } from '@/lib/api';
import { basePath } from '@/lib/basePath';
import { getToken } from '@/lib/auth';
import { Badge, Button, Card, PageHeader } from '@/components/ui';

interface CanvasFrame {
  frame_id: string;
  content_type: string;
  content: string;
  timestamp: string;
}

interface WsCanvasMessage {
  type: string;
  canvas_id: string;
  frame?: CanvasFrame;
}

export default function Canvas() {
  const [canvasId, setCanvasId] = useState('default');
  const [canvasIdInput, setCanvasIdInput] = useState('default');
  const [currentFrame, setCurrentFrame] = useState<CanvasFrame | null>(null);
  const [history, setHistory] = useState<CanvasFrame[]>([]);
  const [connected, setConnected] = useState(false);
  const [showHistory, setShowHistory] = useState(false);
  const [canvasList, setCanvasList] = useState<string[]>([]);
  const wsRef = useRef<WebSocket | null>(null);

  // Build WebSocket URL for canvas
  const getWsUrl = useCallback((id: string) => {
    const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    const base = basePath || '';
    return `${proto}//${location.host}${base}/ws/canvas/${encodeURIComponent(id)}`;
  }, []);

  // Connect to canvas WebSocket
  const connectWs = useCallback((id: string) => {
    if (wsRef.current) {
      wsRef.current.close();
    }

    const token = getToken();
    const protocols = token ? ['zeroclaw.v1', `bearer.${token}`] : ['zeroclaw.v1'];
    const ws = new WebSocket(getWsUrl(id), protocols);

    ws.onopen = () => setConnected(true);
    ws.onclose = () => setConnected(false);
    ws.onerror = () => setConnected(false);

    ws.onmessage = (event) => {
      try {
        const msg: WsCanvasMessage = JSON.parse(event.data);
        if (msg.type === 'frame' && msg.frame) {
          if (msg.frame.content_type === 'clear') {
            setCurrentFrame(null);
            setHistory([]);
          } else {
            setCurrentFrame(msg.frame);
            setHistory((prev) => [...prev.slice(-49), msg.frame!]);
          }
        }
      } catch {
        // ignore parse errors
      }
    };

    wsRef.current = ws;
  }, [getWsUrl]);

  // Connect on mount and when canvasId changes
  useEffect(() => {
    connectWs(canvasId);
    return () => {
      wsRef.current?.close();
    };
  }, [canvasId, connectWs]);

  // Fetch canvas list periodically
  useEffect(() => {
    const fetchList = async () => {
      try {
        const data = await apiFetch<{ canvases: string[] }>('/api/canvas');
        setCanvasList(data.canvases || []);
      } catch {
        // ignore
      }
    };
    fetchList();
    const interval = setInterval(fetchList, 5000);
    return () => clearInterval(interval);
  }, []);

  // Build srcdoc HTML for the iframe — avoids needing allow-same-origin to
  // access contentDocument.  Content types that don't need scripts get a
  // restrictive CSP meta tag; only the explicit `html` content type can
  // execute scripts inside the opaque-origin sandbox.  Every other content
  // type — including `eval` and any unrecognised type — renders an inert
  // no-script document, so a previous frame's srcdoc can never remain
  // visible or active across a content_type transition.
  const srcdoc = useMemo(() => {
    if (!currentFrame) return undefined;

    const cs = getComputedStyle(document.documentElement);
    const bgBase = cs.getPropertyValue('--pc-bg-base').trim() || '#1e1e24';
    const textPrimary = cs.getPropertyValue('--pc-text-primary').trim() || '#d4d4d8';
    const textSecondary = cs.getPropertyValue('--pc-text-secondary').trim() || '#a1a1aa';
    const fontMono = cs.getPropertyValue('--pc-font-mono').trim() || 'monospace';
    const fontUi = cs.getPropertyValue('--pc-font-ui').trim() || 'system-ui,sans-serif';

    // CSP that blocks all scripts — used for non-interactive content types
    // and for the inert placeholder.  object-src 'none' is required
    // separately because in the absence of a default-src directive,
    // object-src would otherwise fall back to * and allow <object>,
    // <embed>, and <applet> to load external content from these frames.
    const noScriptCsp =
      '<meta http-equiv="Content-Security-Policy" content="script-src \'none\'; object-src \'none\'">';

    // Inert placeholder document.  Used for `eval` (where iframe rendering
    // is intentionally a no-op and execution happens out of band) and as
    // the deny-by-default fallback for any unrecognised content_type.
    // Replacing the previous srcdoc with this guarantees that stale frame
    // content cannot retain capability across a transition.
    const inertDoc =
      `<!DOCTYPE html><html><head>${noScriptCsp}</head><body style="margin:0;background:${bgBase};"></body></html>`;

    if (currentFrame.content_type === 'eval') {
      return inertDoc;
    }

    if (currentFrame.content_type === 'svg') {
      // Strip <script> tags and event-handler attributes from SVG to prevent XSS.
      // Run the matched-pair strip first; then strip any remaining <script ...>
      // opener so the void-element / unclosed form (`<script src="..."/>` or
      // `<script src="...">` with no closing tag) does not survive. The \b
      // word boundary keeps the patterns from matching tag names that merely
      // start with "script" (e.g. <scriptlet>, should one ever exist).
      const sanitized = currentFrame.content
        .replace(/<script\b[^>]*>[\s\S]*?<\/script\s*>/gi, '')
        .replace(/<script\b[^>]*\/?>/gi, '')
        .replace(/\bon\w+\s*=\s*("[^"]*"|'[^']*'|[^\s>]*)/gi, '');
      return `<!DOCTYPE html><html><head>${noScriptCsp}<style>body{margin:0;display:flex;align-items:center;justify-content:center;min-height:100vh;background:${bgBase};}</style></head><body>${sanitized}</body></html>`;
    }

    if (currentFrame.content_type === 'markdown') {
      const escaped = currentFrame.content
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;');
      return `<!DOCTYPE html><html><head>${noScriptCsp}<style>body{margin:1rem;font-family:${fontUi};color:${textSecondary};background:${bgBase};line-height:1.6;}pre{white-space:pre-wrap;word-wrap:break-word;}</style></head><body><pre>${escaped}</pre></body></html>`;
    }

    if (currentFrame.content_type === 'text') {
      const escaped = currentFrame.content
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;');
      return `<!DOCTYPE html><html><head>${noScriptCsp}<style>body{margin:1rem;font-family:${fontMono};color:${textPrimary};background:${bgBase};white-space:pre-wrap;}</style></head><body>${escaped}</body></html>`;
    }

    if (currentFrame.content_type === 'html') {
      // Scripts allowed but still sandboxed (no same-origin).
      return currentFrame.content;
    }

    // Unrecognised content_type — render inert rather than defaulting to
    // scriptable HTML.  Future content types must be added explicitly above.
    return inertDoc;
  }, [currentFrame]);

  const handleSwitchCanvas = () => {
    if (canvasIdInput.trim()) {
      setCanvasId(canvasIdInput.trim());
      setCurrentFrame(null);
      setHistory([]);
    }
  };

  const handleClear = async () => {
    try {
      await apiFetch(`/api/canvas/${encodeURIComponent(canvasId)}`, {
        method: 'DELETE',
      });
      setCurrentFrame(null);
      setHistory([]);
    } catch {
      // ignore
    }
  };

  const handleSelectHistoryFrame = (frame: CanvasFrame) => {
    setCurrentFrame(frame);
  };

  return (
    <div className="p-6 space-y-4 h-full flex flex-col">
      {/* Header */}
      <PageHeader
        title={
          <span className="inline-flex items-center gap-2.5">
            <Monitor className="h-5 w-5 text-pc-accent" />
            Live Canvas
            <Badge tone={connected ? 'ok' : 'error'}>
              {connected ? 'Connected' : 'Disconnected'}
            </Badge>
          </span>
        }
        actions={
          <>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => setShowHistory(!showHistory)}
              title="Toggle history"
              aria-pressed={showHistory}
            >
              <History className="h-4 w-4" />
            </Button>
            <Button
              variant="ghost"
              size="sm"
              onClick={handleClear}
              title="Clear canvas"
            >
              <Trash2 className="h-4 w-4" />
            </Button>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => connectWs(canvasId)}
              title="Reconnect"
            >
              <RefreshCw className="h-4 w-4" />
            </Button>
          </>
        }
      />

      {/* Canvas selector */}
      <div className="flex items-center gap-2 flex-wrap">
        <input
          type="text"
          value={canvasIdInput}
          onChange={(e) => setCanvasIdInput(e.target.value)}
          onKeyDown={(e) => e.key === 'Enter' && handleSwitchCanvas()}
          placeholder="Canvas ID"
          className="h-9 px-3 rounded-[var(--radius-md)] text-sm border border-pc-border bg-pc-input text-pc-text placeholder:text-pc-text-faint focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-pc-accent/40 focus-visible:border-pc-accent/40"
        />
        <Button size="md" onClick={handleSwitchCanvas}>
          Switch
        </Button>
        {canvasList.length > 0 && (
          <div className="flex items-center gap-1.5 ml-2 flex-wrap">
            <span className="text-xs text-pc-text-muted">Active:</span>
            {canvasList.map((id) => {
              const active = id === canvasId;
              return (
                <button
                  key={id}
                  onClick={() => {
                    setCanvasIdInput(id);
                    setCanvasId(id);
                    setCurrentFrame(null);
                    setHistory([]);
                  }}
                  className={[
                    'px-2 py-1 rounded-[var(--radius-sm)] text-xs font-mono border transition-colors',
                    active
                      ? 'bg-pc-accent/10 text-pc-accent border-pc-accent/30'
                      : 'bg-pc-elevated text-pc-text-muted border-pc-border hover:text-pc-text hover:border-pc-border-strong',
                  ].join(' ')}
                >
                  {id}
                </button>
              );
            })}
          </div>
        )}
      </div>

      {/* Main content area */}
      <div className="flex-1 flex gap-4 min-h-0">
        {/* Canvas viewer */}
        <div className="flex-1 rounded-[var(--radius-lg)] border border-pc-border bg-pc-base overflow-hidden shadow-[var(--pc-shadow-sm)]">
          {currentFrame ? (
            <iframe
              sandbox="allow-scripts"
              srcDoc={srcdoc}
              className="w-full h-full border-0"
              title={`Canvas: ${canvasId}`}
              style={{ background: 'var(--pc-bg-base)' }}
            />
          ) : (
            <div className="flex items-center justify-center h-full">
              <div className="text-center">
                <Monitor className="h-12 w-12 mx-auto mb-3 text-pc-text-faint" />
                <p className="text-sm text-pc-text-muted">
                  Waiting for content on canvas <span className="font-mono text-pc-text-secondary">"{canvasId}"</span>
                </p>
                <p className="text-xs mt-1 text-pc-text-faint">
                  The agent can push content here using the canvas tool
                </p>
              </div>
            </div>
          )}
        </div>

        {/* History panel */}
        {showHistory && (
          <Card padded={false} className="w-64 overflow-y-auto">
            <div className="px-3 py-2 border-b border-pc-border text-[11px] font-medium uppercase tracking-wide text-pc-text-faint sticky top-0 bg-pc-surface">
              Frame History ({history.length})
            </div>
            {history.length === 0 ? (
              <p className="p-3 text-xs text-pc-text-muted">No frames yet</p>
            ) : (
              <div className="space-y-1 p-2">
                {[...history].reverse().map((frame) => {
                  const active = currentFrame?.frame_id === frame.frame_id;
                  return (
                    <button
                      key={frame.frame_id}
                      onClick={() => handleSelectHistoryFrame(frame)}
                      className={[
                        'w-full text-left px-2 py-1.5 rounded-[var(--radius-sm)] text-xs transition-colors border',
                        active
                          ? 'bg-pc-accent/10 border-pc-accent/30'
                          : 'border-transparent hover:bg-[var(--pc-hover)]',
                      ].join(' ')}
                    >
                      <div className="flex items-center justify-between">
                        <span className="font-mono truncate text-pc-accent">
                          {frame.content_type}
                        </span>
                        <span className="text-pc-text-muted">
                          {new Date(frame.timestamp).toLocaleTimeString()}
                        </span>
                      </div>
                      <div className="truncate mt-0.5 text-[0.65rem] text-pc-text-muted">
                        {frame.content.substring(0, 60)}
                        {frame.content.length > 60 ? '...' : ''}
                      </div>
                    </button>
                  );
                })}
              </div>
            )}
          </Card>
        )}
      </div>

      {/* Frame info bar */}
      {currentFrame && (
        <div className="flex items-center justify-between px-3 py-2 rounded-[var(--radius-md)] text-xs bg-pc-elevated border border-pc-border text-pc-text-muted">
          <span>
            Type: <span className="font-mono text-pc-text-secondary">{currentFrame.content_type}</span>
            <span className="mx-2 text-pc-text-faint">|</span>
            Frame: <span className="font-mono text-pc-text-secondary">{currentFrame.frame_id.substring(0, 8)}</span>
          </span>
          <span>{new Date(currentFrame.timestamp).toLocaleString()}</span>
        </div>
      )}
    </div>
  );
}
