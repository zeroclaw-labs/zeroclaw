import { useState } from "react";
import { X, Save, Loader2 } from "lucide-react";
import type { McpServerInput } from "@/types/api";
import { addMcpServer } from "@/lib/api";
import StringArrayEditor from "./StringArrayEditor";
import KeyValueEditor from "./KeyValueEditor";

interface McpAddModalProps {
  onClose: () => void;
  onSaved: () => void;
  prefill?: Partial<McpServerInput>;
}

export default function McpAddModal({
  onClose,
  onSaved,
  prefill,
}: McpAddModalProps) {
  const [name, setName] = useState(prefill?.name ?? "");
  const [transport, setTransport] = useState(prefill?.transport ?? "stdio");
  const [command, setCommand] = useState(prefill?.command ?? "");
  const [args, setArgs] = useState<string[]>(prefill?.args ?? []);
  const [url, setUrl] = useState(prefill?.url ?? "");
  const [env, setEnv] = useState<Record<string, string>>(prefill?.env ?? {});
  const [headers, setHeaders] = useState<Record<string, string>>(
    prefill?.headers ?? {},
  );
  const [timeout, setTimeout] = useState<number | "">(
    prefill?.tool_timeout_secs ?? "",
  );
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleSave = async () => {
    if (!name.trim()) {
      setError("Server name is required");
      return;
    }

    setSaving(true);
    setError(null);
    try {
      const server: McpServerInput = {
        name: name.trim(),
        transport,
        command: command.trim(),
        args,
        env,
        headers,
      };
      if (url.trim()) server.url = url.trim();
      if (typeof timeout === "number") server.tool_timeout_secs = timeout;
      await addMcpServer(server);
      onSaved();
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "Failed to add server");
    } finally {
      setSaving(false);
    }
  };

  const isStdio = transport === "stdio";

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center animate-fade-in">
      <div
        className="absolute inset-0 bg-black/60 backdrop-blur-sm"
        onClick={onClose}
      />

      <div className="relative glass-card w-full max-w-lg max-h-[85vh] flex flex-col animate-fade-in-scale mx-4">
        <div
          className="absolute -top-px left-1/4 right-1/4 h-px"
          style={{
            background:
              "linear-gradient(90deg, transparent, #0080ff, transparent)",
          }}
        />

        {/* Header */}
        <div className="flex items-center justify-between p-5 border-b border-[#1a1a3e]/40">
          <h3 className="text-sm font-semibold text-white">Add MCP Server</h3>
          <button
            onClick={onClose}
            className="p-1.5 rounded-lg text-[#556080] hover:text-white hover:bg-[#1a1a3e] transition-colors"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        {/* Body */}
        <div className="flex-1 overflow-y-auto p-5 space-y-4">
          {/* Name */}
          <div>
            <label className="block text-xs font-semibold text-[#8090b0] mb-1.5">
              Name <span className="text-[#ff4466]">*</span>
            </label>
            <input
              type="text"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="my-server"
              className="input-electric w-full px-3 py-2 text-sm"
            />
          </div>

          {/* Transport */}
          <div>
            <label className="block text-xs font-semibold text-[#8090b0] mb-1.5">
              Transport
            </label>
            <select
              value={transport}
              onChange={(e) => setTransport(e.target.value)}
              className="input-electric w-full px-3 py-2 text-sm"
            >
              <option value="stdio">Stdio</option>
              <option value="http">HTTP</option>
              <option value="sse">SSE</option>
            </select>
          </div>

          {/* Command (stdio) */}
          {isStdio && (
            <>
              <div>
                <label className="block text-xs font-semibold text-[#8090b0] mb-1.5">
                  Command
                </label>
                <input
                  type="text"
                  value={command}
                  onChange={(e) => setCommand(e.target.value)}
                  placeholder="npx -y @modelcontextprotocol/server-postgres"
                  className="input-electric w-full px-3 py-2 text-sm"
                />
              </div>
              <div>
                <label className="block text-xs font-semibold text-[#8090b0] mb-1.5">
                  Args
                </label>
                <StringArrayEditor
                  value={args}
                  onChange={setArgs}
                  placeholder="Add argument..."
                />
              </div>
            </>
          )}

          {/* URL (http/sse) */}
          {!isStdio && (
            <>
              <div>
                <label className="block text-xs font-semibold text-[#8090b0] mb-1.5">
                  URL
                </label>
                <input
                  type="text"
                  value={url}
                  onChange={(e) => setUrl(e.target.value)}
                  placeholder="https://mcp-server.example.com"
                  className="input-electric w-full px-3 py-2 text-sm"
                />
              </div>
              <div>
                <label className="block text-xs font-semibold text-[#8090b0] mb-1.5">
                  Headers
                </label>
                <KeyValueEditor
                  value={headers}
                  onChange={setHeaders}
                  keyPlaceholder="Header name"
                  valuePlaceholder="Header value"
                />
              </div>
            </>
          )}

          {/* Env */}
          <div>
            <label className="block text-xs font-semibold text-[#8090b0] mb-1.5">
              Environment Variables
            </label>
            <KeyValueEditor
              value={env}
              onChange={setEnv}
              keyPlaceholder="ENV_VAR"
              valuePlaceholder="value"
            />
          </div>

          {/* Timeout */}
          <div>
            <label className="block text-xs font-semibold text-[#8090b0] mb-1.5">
              Tool Timeout (seconds)
            </label>
            <input
              type="number"
              value={timeout}
              onChange={(e) =>
                setTimeout(e.target.value === "" ? "" : Number(e.target.value))
              }
              placeholder="30"
              className="input-electric w-full px-3 py-2 text-sm"
            />
          </div>

          {error && (
            <div className="rounded-lg bg-[#ff446615] border border-[#ff446630] p-3 text-xs text-[#ff6680] animate-fade-in">
              {error}
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-end gap-2 p-5 border-t border-[#1a1a3e]/40">
          <button
            onClick={onClose}
            className="px-4 py-2 rounded-xl text-xs font-medium text-[#556080] border border-[#1a1a3e] hover:text-white hover:border-[#0080ff40] transition-all"
          >
            Cancel
          </button>
          <button
            onClick={handleSave}
            disabled={saving || !name.trim()}
            className="btn-electric flex items-center gap-1.5 px-4 py-2 rounded-xl text-xs font-semibold"
          >
            {saving ? (
              <Loader2 className="h-3.5 w-3.5 animate-spin" />
            ) : (
              <Save className="h-3.5 w-3.5" />
            )}
            Add Server
          </button>
        </div>
      </div>
    </div>
  );
}
