import { StreamLanguage } from '@codemirror/language';
import { oneDark } from '@codemirror/theme-one-dark';
import { EditorView } from '@codemirror/view';
import { toml } from '@codemirror/legacy-modes/mode/toml';
import CodeMirror from '@uiw/react-codemirror';

interface Props {
  rawToml: string;
  onChange: (raw: string) => void;
  disabled?: boolean;
}

const tomlLanguage = StreamLanguage.define(toml);

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
      <CodeMirror
        value={rawToml}
        onChange={onChange}
        theme={oneDark}
        readOnly={Boolean(disabled)}
        editable={!disabled}
        height="500px"
        basicSetup={{
          lineNumbers: true,
          foldGutter: false,
          highlightActiveLineGutter: false,
          highlightActiveLine: false,
        }}
        extensions={[tomlLanguage, EditorView.lineWrapping]}
        className="text-sm [&_.cm-scroller]:font-mono [&_.cm-scroller]:leading-6 [&_.cm-content]:py-4 [&_.cm-content]:px-0 [&_.cm-gutters]:border-r [&_.cm-gutters]:border-gray-800 [&_.cm-gutters]:bg-gray-950 [&_.cm-editor]:bg-gray-950 [&_.cm-editor]:focus:outline-none [&_.cm-focused]:ring-2 [&_.cm-focused]:ring-blue-500/70 [&_.cm-focused]:ring-inset"
        aria-label="Raw TOML configuration editor with syntax highlighting"
      />
    </div>
  );
}
