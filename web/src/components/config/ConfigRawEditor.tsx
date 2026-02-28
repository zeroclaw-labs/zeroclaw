interface Props {
  rawToml: string;
  onChange: (raw: string) => void;
  disabled?: boolean;
}

export default function ConfigRawEditor({ rawToml, onChange, disabled }: Props) {
  return (
    <div className="bg-gray-900 rounded-xl border border-gray-800 overflow-hidden">
      <div className="flex items-center justify-between px-4 py-2 border-b border-gray-800 bg-gray-800/50">
        <span className="text-xs text-gray-400 font-medium uppercase tracking-wider">
          TOML Configuration
        </span>
        <span className="text-xs text-gray-500">
          {rawToml.split('\n').length} lines
        </span>
      </div>
      <textarea
        value={rawToml}
        onChange={(e) => onChange(e.target.value)}
        disabled={disabled}
        spellCheck={false}
        aria-label="Raw TOML configuration editor"
        className="w-full min-h-[500px] bg-gray-950 text-gray-200 font-mono text-sm p-4 resize-y focus:outline-none focus:ring-2 focus:ring-blue-500 focus:ring-inset disabled:opacity-50"
        style={{ tabSize: 4 }}
      />
    </div>
  );
}
