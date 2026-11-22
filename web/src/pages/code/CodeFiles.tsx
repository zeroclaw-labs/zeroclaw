import { useEffect, useState } from "react";
import { ArrowUp, FileText, Folder, RefreshCw } from "lucide-react";
import CodeMirror from "@uiw/react-codemirror";
import { githubLight } from "@uiw/codemirror-theme-github";
import { oneDark } from "@codemirror/theme-one-dark";
import { listAgentWorkspace, readAgentWorkspaceFile, type BrowseEntry } from "@/lib/api";
import { t } from "@/lib/i18n";
import { useTheme } from "@/hooks/useTheme";

export default function CodeFiles({ alias, refresh }: { alias: string; refresh: number }) {
  const { theme } = useTheme();
  const [path, setPath] = useState("");
  const [entries, setEntries] = useState<BrowseEntry[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [text, setText] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [tick, setTick] = useState(0);
  useEffect(() => {
    let cancelled = false;
    listAgentWorkspace(alias, path)
      .then((result) => {
        if (!cancelled) {
          setEntries(result.entries);
          setError(null);
        }
      })
      .catch((error: Error) => {
        if (!cancelled) setError(error.message);
      });
    return () => {
      cancelled = true;
    };
  }, [alias, path, refresh, tick]);
  useEffect(() => {
    if (!selected) return;
    let cancelled = false;
    setText("");
    readAgentWorkspaceFile(alias, selected)
      .then((file) => {
        if (!cancelled) {
          setText(file.is_text ? file.content : t("code.binary"));
          setError(null);
        }
      })
      .catch((error: Error) => {
        if (!cancelled) setError(error.message);
      });
    return () => {
      cancelled = true;
    };
  }, [alias, selected, refresh, tick]);
  return (
    <section
      className="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden bg-pc-surface"
      aria-label={t("code.files")}
    >
      <div className="flex items-center gap-2 border-b border-pc-border p-3">
        <span className="ml-auto text-xs text-pc-text-muted">
          {t("code.read_only")}
        </span>
        <button
          type="button"
          onClick={() => setTick((value) => value + 1)}
          aria-label={t("common.refresh")}
          className="p-2"
        >
          <RefreshCw className="h-4 w-4" />
        </button>
      </div>
      {error && (
        <p role="alert" className="p-3 text-sm text-status-error">
          {error}
        </p>
      )}
      <div className="grid min-h-0 flex-1 grid-rows-[minmax(6rem,.4fr)_minmax(0,1fr)]">
        <nav
          aria-label={t("code.files")}
          className="border-b sm:border-b-0 sm:border-r border-pc-border p-2 max-h-60 overflow-auto"
        >
          {path && (
            <button
              type="button"
              className="flex items-center gap-2 p-2 text-xs"
              onClick={() => setPath(path.split("/").slice(0, -1).join("/"))}
            >
              <ArrowUp className="h-4 w-4" />
              {t("code.parent")}
            </button>
          )}
          {entries.map((entry) => {
            const full = path ? `${path}/${entry.name}` : entry.name;
            return (
              <button
                type="button"
                key={entry.name}
                onClick={() =>
                  entry.kind === "dir" ? setPath(full) : setSelected(full)
                }
                className={`flex w-full items-center gap-2 rounded p-2 text-left text-xs hover:bg-pc-elevated ${selected === full ? "bg-pc-elevated text-pc-accent" : ""}`}
                title={entry.name}
              >
                {entry.kind === "dir" ? (
                  <Folder className="h-4 w-4 shrink-0" />
                ) : (
                  <FileText className="h-4 w-4 shrink-0" />
                )}
                <span className="truncate">{entry.name}</span>
              </button>
            );
          })}
        </nav>
        <div className="min-w-0">
          {selected ? (
            <>
              <p
                className="truncate border-b border-pc-border p-3 text-xs font-mono"
                title={selected}
              >
                {selected}
              </p>
              <CodeMirror
                value={text}
                readOnly
                editable={false}
                theme={theme === "light" ? githubLight : oneDark}
                height="100%"
                aria-label={selected}
                basicSetup={{
                  lineNumbers: true,
                  highlightActiveLine: false,
                  foldGutter: false,
                }}
              />
            </>
          ) : (
            <p className="p-6 text-sm text-pc-text-muted">
              {t("code.select_file")}
            </p>
          )}
        </div>
      </div>
    </section>
  );
}
