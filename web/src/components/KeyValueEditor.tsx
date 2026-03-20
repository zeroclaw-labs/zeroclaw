import { useState, useRef } from "react";
import { X, Plus } from "lucide-react";

interface KeyValueEditorProps {
  value: Record<string, string>;
  onChange: (value: Record<string, string>) => void;
  keyPlaceholder?: string;
  valuePlaceholder?: string;
  disabled?: boolean;
}

export default function KeyValueEditor({
  value,
  onChange,
  keyPlaceholder = "Key",
  valuePlaceholder = "Value",
  disabled,
}: KeyValueEditorProps) {
  const [newKey, setNewKey] = useState("");
  const [newValue, setNewValue] = useState("");
  const keyRef = useRef<HTMLInputElement>(null);

  const entries = Object.entries(value);

  const addEntry = () => {
    const k = newKey.trim();
    const v = newValue.trim();
    if (!k) return;
    onChange({ ...value, [k]: v });
    setNewKey("");
    setNewValue("");
    keyRef.current?.focus();
  };

  const removeEntry = (key: string) => {
    const next = { ...value };
    delete next[key];
    onChange(next);
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter") {
      e.preventDefault();
      addEntry();
    }
  };

  return (
    <div className="space-y-2">
      {entries.length > 0 && (
        <div className="space-y-1">
          {entries.map(([k, v]) => (
            <div
              key={k}
              className="flex items-center gap-2 px-3 py-1.5 rounded-lg text-xs border border-[#1a1a3e] bg-[#0a0a18]"
            >
              <span className="font-medium text-[#0080ff] min-w-0 truncate">
                {k}
              </span>
              <span className="text-[#334060]">=</span>
              <span className="text-[#e8edf5] min-w-0 truncate flex-1">
                {v}
              </span>
              {!disabled && (
                <button
                  type="button"
                  onClick={() => removeEntry(k)}
                  className="flex-shrink-0 text-[#556080] hover:text-[#ff4466] transition-colors"
                >
                  <X className="h-3 w-3" />
                </button>
              )}
            </div>
          ))}
        </div>
      )}
      <div className="flex gap-2">
        <input
          ref={keyRef}
          type="text"
          value={newKey}
          onChange={(e) => setNewKey(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder={keyPlaceholder}
          disabled={disabled}
          className="input-electric flex-1 px-3 py-2 text-sm"
        />
        <input
          type="text"
          value={newValue}
          onChange={(e) => setNewValue(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder={valuePlaceholder}
          disabled={disabled}
          className="input-electric flex-1 px-3 py-2 text-sm"
        />
        <button
          type="button"
          onClick={addEntry}
          disabled={disabled || !newKey.trim()}
          className="btn-electric px-3 py-2 rounded-xl disabled:opacity-40"
        >
          <Plus className="h-4 w-4" />
        </button>
      </div>
    </div>
  );
}
