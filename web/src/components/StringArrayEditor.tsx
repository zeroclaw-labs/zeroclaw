import { useState, useRef } from "react";
import { X, Plus } from "lucide-react";

interface StringArrayEditorProps {
  value: string[];
  onChange: (value: string[]) => void;
  placeholder?: string;
  disabled?: boolean;
}

export default function StringArrayEditor({
  value,
  onChange,
  placeholder,
  disabled,
}: StringArrayEditorProps) {
  const [input, setInput] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);

  const addItem = () => {
    const trimmed = input.trim();
    if (!trimmed || value.includes(trimmed)) return;
    onChange([...value, trimmed]);
    setInput("");
    inputRef.current?.focus();
  };

  const removeItem = (index: number) => {
    onChange(value.filter((_, i) => i !== index));
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter") {
      e.preventDefault();
      addItem();
    }
  };

  return (
    <div className="space-y-2">
      {value.length > 0 && (
        <div className="flex flex-wrap gap-1.5">
          {value.map((item, i) => (
            <span
              key={i}
              className="inline-flex items-center gap-1 px-2.5 py-1 rounded-lg text-xs font-medium text-[#e8edf5] border border-[#1a1a3e] bg-[#0a0a18]"
            >
              {item}
              {!disabled && (
                <button
                  type="button"
                  onClick={() => removeItem(i)}
                  className="text-[#556080] hover:text-[#ff4466] transition-colors"
                >
                  <X className="h-3 w-3" />
                </button>
              )}
            </span>
          ))}
        </div>
      )}
      <div className="flex gap-2">
        <input
          ref={inputRef}
          type="text"
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder={placeholder ?? "Add item..."}
          disabled={disabled}
          className="input-electric flex-1 px-3 py-2 text-sm"
        />
        <button
          type="button"
          onClick={addItem}
          disabled={disabled || !input.trim()}
          className="btn-electric px-3 py-2 rounded-xl disabled:opacity-40"
        >
          <Plus className="h-4 w-4" />
        </button>
      </div>
    </div>
  );
}
