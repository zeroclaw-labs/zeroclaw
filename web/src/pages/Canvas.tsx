import { useState, useEffect, useRef, useCallback } from 'react';
import { Monitor, Trash2, History, RefreshCw } from 'lucide-react';
import { apiFetch } from '@/lib/api';
import { basePath } from '@/lib/basePath';
import { getToken } from '@/lib/auth';

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
  const iframeRef = useRef<HTMLIFrameElement>(null);

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

  // Render content into the iframe
  useEffect(() => {
    if (!iframeRef.current || !currentFrame) return;
    if (currentFrame.content_type === 'eval') return; // eval frames are special

    const iframe = iframeRef.current;
    const doc = iframe.contentDocument;
    if (!doc) return;

    let html = currentFrame.content;
    const cs = getComputedStyle(document.documentElement);
    const bgBase = cs.getPropertyValue('--pc-bg-base').trim() || '#1e1e24';
    const textPrimary = cs.getPropertyValue('--pc-text-primary').trim() || '#d4d4d8';
    const textSecondary = cs.getPropertyValue('--pc-text-secondary').trim() || '#a1a1aa';
    const fontMono = cs.getPropertyValue('--pc-font-mono').trim() || 'monospace';
    const fontUi = cs.getPropertyValue('--pc-font-ui').trim() || 'system-ui,sans-serif';

    if (currentFrame.content_type === 'svg') {
      html = `<!DOCTYPE html><html><head><style>body{margin:0;display:flex;align-items:center;justify-content:center;min-height:100vh;background:${bgBase};}</style></head><body>${currentFrame.content}</body></html>`;
    } else if (currentFrame.content_type === 'markdown') {
      const escaped = currentFrame.content
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;');
      html = `<!DOCTYPE html><html><head><style>body{margin:1rem;font-family:${fontUi};color:${textSecondary};background:${bgBase};line-height:1.6;}pre{white-space:pre-wrap;word-wrap:break-word;}</style></head><body><pre>${escaped}</pre></body></html>`;
    } else if (currentFrame.content_type === 'text') {
      const escaped = currentFrame.content
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;');
      html = `<!DOCTYPE html><html><head><style>body{margin:1rem;font-family:${fontMono};color:${textPrimary};background:${bgBase};white-space:pre-wrap;}</style></head><body>${escaped}</body></html>`;
    }

    doc.open();
    doc.write(html);
    doc.close();
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
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <Monitor className="h-6 w-6" style={{ color: 'var(--pc-accent)' }} />
          <h1 className="text-xl font-semibold" style={{ color: 'var(--pc-text-primary)' }}>
            Live Canvas
          </h1>
          <span
            className="text-xs px-2 py-0.5 rounded-full font-medium"
            style={{
              background: connected ? 'rgba(34, 197, 94, 0.15)' : 'rgba(239, 68, 68, 0.15)',
              color: connected ? '#22c55e' : '#ef4444',
            }}
          >
            {connected ? 'Connected' : 'Disconnected'}
          </span>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={() => setShowHistory(!showHistory)}
            className="p-2 rounded-lg transition-colors hover:opacity-80"
            style={{ background: 'var(--pc-bg-elevated)', color: 'var(--pc-text-muted)' }}
            title="Toggle history"
          >
            <History className="h-4 w-4" />
          </button>
          <button
            onClick={handleClear}
            className="p-2 rounded-lg transition-colors hover:opacity-80"
            style={{ background: 'var(--pc-bg-elevated)', color: 'var(--pc-text-muted)' }}
            title="Clear canvas"
          >
            <Trash2 className="h-4 w-4" />
          </button>
          <button
            onClick={() => connectWs(canvasId)}
            className="p-2 rounded-lg transition-colors hover:opacity-80"
            style={{ background: 'var(--pc-bg-elevated)', color: 'var(--pc-text-muted)' }}
            title="Reconnect"
          >
            <RefreshCw className="h-4 w-4" />
          </button>
        </div>
      </div>

      {/* Canvas selector */}
      <div className="flex items-center gap-2">
        <input
          type="text"
          value={canvasIdInput}
          onChange={(e) => setCanvasIdInput(e.target.value)}
          onKeyDown={(e) => e.key === 'Enter' && handleSwitchCanvas()}
          placeholder="Canvas ID"
          className="px-3 py-1.5 rounded-lg text-sm border"
          style={{
            background: 'var(--pc-bg-elevated)',
            borderColor: 'var(--pc-border)',
            color: 'var(--pc-text-primary)',
          }}
        />
        <button
          onClick={handleSwitchCanvas}
          className="px-3 py-1.5 rounded-lg text-sm font-medium"
          style={{ background: 'var(--pc-accent)', color: '#fff' }}
        >
          Switch
        </button>
        {canvasList.length > 0 && (
          <div className="flex items-center gap-1 ml-2">
            <span className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>Active:</span>
            {canvasList.map((id) => (
              <button
                key={id}
                onClick={() => {
                  setCanvasIdInput(id);
                  setCanvasId(id);
                  setCurrentFrame(null);
                  setHistory([]);
                }}
                className="px-2 py-0.5 rounded text-xs font-mono transition-colors"
                style={{
                  background: id === canvasId ? 'var(--pc-accent-dim)' : 'var(--pc-bg-elevated)',
                  color: id === canvasId ? 'var(--pc-accent)' : 'var(--pc-text-muted)',
                  borderColor: 'var(--pc-border)',
                }}
              >
                {id}
              </button>
            ))}
          </div>
        )}
      </div>

      {/* Main content area */}
      <div className="flex-1 flex gap-4 min-h-0">
        {/* Canvas viewer */}
        <div
          className="flex-1 rounded-lg border overflow-hidden"
          style={{ borderColor: 'var(--pc-border)', background: 'var(--pc-bg-base)' }}
        >
          {currentFrame ? (
            <iframe
              ref={iframeRef}
              sandbox="allow-scripts allow-same-origin"
              className="w-full h-full border-0"
              title={`Canvas: ${canvasId}`}
              style={{ background: 'var(--pc-bg-base)' }}
            />
          ) : (
            <div className="flex items-center justify-center h-full">
              <div className="text-center">
                <Monitor
                  className="h-12 w-12 mx-auto mb-3 opacity-30"
                  style={{ color: 'var(--pc-text-muted)' }}
                />
                <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
                  Waiting for content on canvas <span className="font-mono">"{canvasId}"</span>
                </p>
                <p className="text-xs mt-1" style={{ color: 'var(--pc-text-muted)', opacity: 0.6 }}>
                  The agent can push content here using the canvas tool
                </p>
              </div>
            </div>
          )}
        </div>

        {/* History panel */}
        {showHistory && (
          <div
            className="w-64 rounded-lg border overflow-y-auto"
            style={{
              borderColor: 'var(--pc-border)',
              background: 'var(--pc-bg-elevated)',
            }}
          >
            <div
              className="px-3 py-2 border-b text-xs font-medium sticky top-0"
              style={{
                borderColor: 'var(--pc-border)',
                background: 'var(--pc-bg-elevated)',
                color: 'var(--pc-text-muted)',
              }}
            >
              Frame History ({history.length})
            </div>
            {history.length === 0 ? (
              <p className="p-3 text-xs" style={{ color: 'var(--pc-text-muted)' }}>
                No frames yet
              </p>
            ) : (
              <div className="space-y-1 p-2">
                {[...history].reverse().map((frame) => (
                  <button
                    key={frame.frame_id}
                    onClick={() => handleSelectHistoryFrame(frame)}
                    className="w-full text-left px-2 py-1.5 rounded text-xs transition-colors"
                    style={{
                      background:
                        currentFrame?.frame_id === frame.frame_id
                          ? 'var(--pc-accent-dim)'
                          : 'transparent',
                      color: 'var(--pc-text-primary)',
                    }}
                  >
                    <div className="flex items-center justify-between">
                      <span className="font-mono truncate" style={{ color: 'var(--pc-accent)' }}>
                        {frame.content_type}
                      </span>
                      <span style={{ color: 'var(--pc-text-muted)' }}>
                        {new Date(frame.timestamp).toLocaleTimeString()}
                      </span>
                    </div>
                    <div
                      className="truncate mt-0.5"
                      style={{ color: 'var(--pc-text-muted)', fontSize: '0.65rem' }}
                    >
                      {frame.content.substring(0, 60)}
                      {frame.content.length > 60 ? '...' : ''}
                    </div>
                  </button>
                ))}
              </div>
            )}
          </div>
        )}
      </div>

      {/* Frame info bar */}
      {currentFrame && (
        <div
          className="flex items-center justify-between px-3 py-1.5 rounded-lg text-xs"
          style={{ background: 'var(--pc-bg-elevated)', color: 'var(--pc-text-muted)' }}
        >
          <span>
            Type: <span className="font-mono">{currentFrame.content_type}</span> | Frame:{' '}
            <span className="font-mono">{currentFrame.frame_id.substring(0, 8)}</span>
          </span>
          <span>{new Date(currentFrame.timestamp).toLocaleString()}</span>
        </div>
      )}
    </div>
  );
}
