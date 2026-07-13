import { markdown } from '@codemirror/lang-markdown';
import { oneDark } from '@codemirror/theme-one-dark';
import { githubLight } from '@uiw/codemirror-theme-github';
import CodeMirror from '@uiw/react-codemirror';
import { useTheme } from '@/hooks/useTheme';

export interface MarkdownEditorProps {
  value: string;
  onChange: (next: string) => void;
  height?: string;
  placeholder?: string;
  lineNumbers?: boolean;
  onFocus?: () => void;
  onBlur?: () => void;
  autoFocus?: boolean;
}

/// The shared Markdown editing surface: CodeMirror with the markdown language,
/// themed from the active console theme. Single source for personality files and
/// SOP step bodies so their editing experience never drifts.
export function MarkdownEditor({
  value,
  onChange,
  height = '32rem',
  placeholder,
  lineNumbers = true,
  onFocus,
  onBlur,
  autoFocus,
}: MarkdownEditorProps) {
  // `resolvedTheme` is 'dark' | 'light' | 'oled'; only 'light' is a light scheme.
  const { resolvedTheme } = useTheme();
  const cmTheme = resolvedTheme === 'light' ? githubLight : oneDark;
  return (
    <div
      className="overflow-hidden rounded-md border"
      style={{ borderColor: 'var(--pc-border)' }}
    >
      <CodeMirror
        value={value}
        onChange={onChange}
        extensions={[markdown()]}
        theme={cmTheme}
        height={height}
        autoFocus={autoFocus}
        onFocus={onFocus}
        onBlur={onBlur}
        basicSetup={{
          lineNumbers,
          highlightActiveLine: lineNumbers,
          foldGutter: lineNumbers,
          bracketMatching: true,
        }}
        placeholder={placeholder}
      />
    </div>
  );
}

export default MarkdownEditor;
