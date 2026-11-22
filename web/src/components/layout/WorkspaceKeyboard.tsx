import { createContext, useCallback, useContext, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { useLocation, useNavigate } from "react-router-dom";
import { basePath } from "@/lib/basePath";
import { pathAccess, type NavAccess } from "@/lib/navAccess";
import { t } from "@/lib/i18n";
import { adjacentItem, assignHints, workspaceForPath, workspaces, type WorkspaceId } from "@/lib/workspaceKeys";

interface WorkspaceItems {
  ids: readonly string[];
  selected?: string;
  select: (id: string) => void;
}
type RegisterItems = (workspace: WorkspaceId, items: WorkspaceItems) => () => void;
type FocusMode = "type" | "transcript" | "workspace";
const Context = createContext<RegisterItems | undefined>(undefined);
const KeyboardMode = createContext<ReactNode>(null);

export function WorkspaceKeyboardToggle() {
  return useContext(KeyboardMode);
}

export function useWorkspaceItems(workspace: WorkspaceId, enabled: boolean, items: WorkspaceItems) {
  const register = useContext(Context);
  const latest = useRef(items);
  latest.current = items;
  useLayoutEffect(() => {
    if (!enabled) return;
    return register?.(workspace, {
      get ids() { return latest.current.ids; },
      get selected() { return latest.current.selected; },
      select: (id) => latest.current.select(id),
    });
  }, [enabled, register, workspace]);
}

const clickable = 'button, a[href], input:not([type="hidden"]), select, textarea, summary, [role="button"][tabindex]';
const editable = 'input:not([type="checkbox"], [type="radio"], [type="button"]), textarea';
const transcript = "[data-workspace-transcript]";
const composer = "[data-workspace-composer]";

function isEditing(target: HTMLElement | undefined) {
  return target?.isContentEditable || target?.matches(editable);
}

function visibleBox(target: HTMLElement) {
  if (!target.isConnected || target.closest("[hidden], [inert], [aria-hidden=true]")) return;
  const box = target.getBoundingClientRect();
  if (getComputedStyle(target).visibility !== "visible" || !box.width || !box.height) return;
  const bounds = { left: 0, top: 0, right: window.innerWidth, bottom: window.innerHeight };
  for (let parent = target.parentElement; parent; parent = parent.parentElement) {
    const style = getComputedStyle(parent);
    if (style.position === "fixed") break;
    const clipX = /auto|scroll|hidden|clip/.test(style.overflowX);
    const clipY = /auto|scroll|hidden|clip/.test(style.overflowY);
    if (!clipX && !clipY) continue;
    const rect = parent.getBoundingClientRect();
    if (clipX) {
      bounds.left = Math.max(bounds.left, rect.left);
      bounds.right = Math.min(bounds.right, rect.right);
    }
    if (clipY) {
      bounds.top = Math.max(bounds.top, rect.top);
      bounds.bottom = Math.min(bounds.bottom, rect.bottom);
    }
  }
  if (box.right <= bounds.left || box.bottom <= bounds.top || box.left >= bounds.right || box.top >= bounds.bottom) return;
  return { left: Math.max(box.left, bounds.left), top: Math.max(box.top, bounds.top) };
}

function activeDialog() {
  const native = document.querySelectorAll<HTMLElement>("dialog:modal");
  if (native.length) return native[native.length - 1];
  const dialogs = [...document.querySelectorAll<HTMLElement>('[role="dialog"][aria-modal="true"]')]
    .filter((element) => visibleBox(element));
  return dialogs[dialogs.length - 1];
}

function isEnabled(target: HTMLElement) {
  return !target.matches(":disabled") && !target.closest("[aria-disabled=true]");
}

function visibleControl(scope: ParentNode | null | undefined, selector: string) {
  return [...scope?.querySelectorAll<HTMLElement>(selector) ?? []]
    .find((element) => isEnabled(element) && visibleBox(element));
}

function activeWorkspace() {
  return workspaceForPath(window.location.pathname.slice(basePath.length));
}

function consume(event: KeyboardEvent) {
  event.preventDefault();
  event.stopImmediatePropagation();
}

interface Hint {
  key: string;
  target: HTMLElement;
}
interface HintLayer {
  items: Hint[];
  scope: HTMLElement;
}

export default function WorkspaceKeyboard({ access, children }: { access: NavAccess; children: ReactNode }) {
  const navigate = useNavigate();
  const location = useLocation();
  const workspace = workspaceForPath(location.pathname);
  const [mode, setModeState] = useState<FocusMode>("type");
  const modeRef = useRef<FocusMode>("type");
  const setMode = useCallback((value: FocusMode) => {
    modeRef.current = value;
    setModeState(value);
  }, []);
  const [hints, setHints] = useState<HintLayer | null>(null);
  const [prefix, setPrefix] = useState("");
  const [revision, redraw] = useState(0);
  const hintRef = useRef<HintLayer | null>(null);
  const prefixRef = useRef("");
  const hintTimer = useRef<number | undefined>(undefined);
  const remembered = useRef<Partial<Record<WorkspaceId, { path: string; conversation?: HTMLElement }>>>({});
  const registered = useRef(new Map<WorkspaceId, WorkspaceItems>());
  const pendingItem = useRef<{ list: WorkspaceItems; item: string; url: string } | undefined>(undefined);
  const previousWorkspace = useRef(workspace);
  const root = useRef<HTMLDivElement>(null);
  const register = useCallback<RegisterItems>((id, list) => {
    registered.current.set(id, list);
    return () => {
      if (registered.current.get(id) === list) registered.current.delete(id);
    };
  }, []);

  const cancelHintTimer = useCallback(() => {
    window.clearTimeout(hintTimer.current);
    hintTimer.current = undefined;
  }, []);
  const closeHints = useCallback(() => {
    cancelHintTimer();
    hintRef.current = null;
    prefixRef.current = "";
    setHints(null);
    setPrefix("");
  }, [cancelHintTimer]);

  const conversationControl = useCallback((id: WorkspaceId, selector: string, origin?: HTMLElement) => {
    const conversation = origin?.closest("[data-workspace-conversation]") ?? remembered.current[id]?.conversation;
    return visibleControl(conversation, selector) ?? visibleControl(root.current, selector);
  }, []);

  const enterWorkspace = useCallback(() => {
    setMode("workspace");
    root.current?.querySelector<HTMLElement>("[data-workspace-window]")?.focus({ preventScroll: true });
  }, [setMode]);

  const enterComposer = useCallback((id: WorkspaceId, origin?: HTMLElement) => {
    const target = conversationControl(id, composer, origin);
    if (!target) return false;
    setMode("type");
    target.focus({ preventScroll: true });
    return true;
  }, [conversationControl, setMode]);

  const expandFocus = useCallback((id: WorkspaceId, origin?: HTMLElement) => {
    const target = modeRef.current === "type" && conversationControl(id, transcript, origin);
    if (target) {
      setMode("transcript");
      target.focus({ preventScroll: true });
    } else {
      enterWorkspace();
    }
  }, [conversationControl, enterWorkspace, setMode]);

  useLayoutEffect(() => {
    const pending = pendingItem.current;
    if (pending && (pending.url !== window.location.href || pending.list.selected === pending.item)) {
      pendingItem.current = undefined;
    }
    if (previousWorkspace.current !== workspace) {
      closeHints();
      if (!workspace) setMode("type");
      else if (modeRef.current === "transcript") enterWorkspace();
      previousWorkspace.current = workspace;
    }
    if (!workspace) return;
    remembered.current[workspace] = {
      ...remembered.current[workspace],
      path: location.pathname + location.search + location.hash,
    };
  }, [closeHints, enterWorkspace, location, setMode, workspace]);

  useEffect(() => {
    if (!hints) return;
    const refresh = () => redraw((value) => value + 1);
    window.addEventListener("scroll", refresh, true);
    window.addEventListener("resize", refresh);
    const observer = new ResizeObserver(refresh);
    for (const { target } of hints.items) observer.observe(target);
    return () => {
      window.removeEventListener("scroll", refresh, true);
      window.removeEventListener("resize", refresh);
      observer.disconnect();
    };
  }, [hints]);

  useEffect(() => {
    const showHints = () => {
      cancelHintTimer();
      const scope = activeDialog() ?? document.body;
      const targets = [...scope.querySelectorAll<HTMLElement>(clickable)].filter((element) =>
        isEnabled(element) && visibleBox(element));
      const keys = assignHints(targets.map((element) => element.dataset.workspaceHint));
      const layer = { scope, items: targets.flatMap((element, index) => (
        keys[index] ? [{ target: element, key: keys[index]! }] : []
      )) };
      hintRef.current = layer;
      setHints(layer);
    };

    const capture = (event: KeyboardEvent) => {
      const id = activeWorkspace();
      if (!id || event.defaultPrevented || event.isComposing || event.keyCode === 229) return;
      const target = event.target instanceof HTMLElement ? event.target : undefined;
      const modified = event.altKey || event.metaKey || event.ctrlKey;
      if (event.key === "Shift") {
        if (!modified && !event.repeat && !hintRef.current) {
          if (isEditing(target)) {
            // A normal shifted keystroke cancels this timer before typing.
            hintTimer.current = window.setTimeout(showHints, 250);
          } else {
            showHints();
          }
        }
        return;
      }
      cancelHintTimer();
      const layer = hintRef.current;
      if (layer) {
        if (event.key === "Escape") {
          closeHints();
          consume(event);
          return;
        }
        if (!modified && event.shiftKey && /^[a-z]$/i.test(event.key)) {
          consume(event);
          if (event.repeat) return;
          const key = prefixRef.current + event.key.toLowerCase();
          const item = layer.items.find((entry) => entry.key === key);
          if (item && layer.scope === (activeDialog() ?? document.body)) {
            closeHints();
            if (visibleBox(item.target) && isEnabled(item.target)) {
              item.target.focus({ preventScroll: true });
              item.target.click();
            }
          } else {
            const next = layer.items.some((entry) => entry.key.startsWith(key)) ? key : "";
            prefixRef.current = next;
            setPrefix(next);
          }
          return;
        }
        closeHints();
      }
      const mode = modeRef.current;
      const key = event.key.toLowerCase();
      if (mode === "type" || modified || event.shiftKey || !/^[hjkli]$/.test(key) || isEditing(target) || activeDialog()) return;
      if (key === "i") {
        if (enterComposer(id, target)) consume(event);
        return;
      }
      if (key === "j" || key === "k") {
        const step = key === "j" ? 1 : -1;
        consume(event);
        if (mode === "transcript") {
          const scroller = conversationControl(id, transcript, target);
          if (scroller) {
            if (!scroller.contains(document.activeElement)) scroller.focus({ preventScroll: true });
            scroller.scrollTop += step * 48;
          }
        } else {
          const list = registered.current.get(id);
          if (list) {
            const pending = pendingItem.current;
            const selected = pending?.list === list && pending.url === window.location.href ? pending.item : list.selected;
            const next = adjacentItem(list.ids, selected, step);
            if (next === undefined) return;
            enterWorkspace();
            list.select(next);
            // History can advance before React renders the newly selected row.
            pendingItem.current = { list, item: next, url: window.location.href };
          }
        }
        return;
      }
      if (mode !== "workspace") return;
      consume(event);
      const allowed = workspaces.filter((item) => pathAccess(item.path, access) === "allowed");
      const index = allowed.findIndex((item) => item.id === id);
      if (index < 0 || allowed.length < 2) return;
      const step = key === "l" ? 1 : -1;
      const next = allowed[(index + step + allowed.length) % allowed.length]!;
      pendingItem.current = undefined;
      remembered.current[id] = {
        ...remembered.current[id],
        path: window.location.pathname.slice(basePath.length) + window.location.search + window.location.hash,
      };
      enterWorkspace();
      void navigate(remembered.current[next.id]?.path ?? next.path);
    };

    // Escape runs after the focused widget has had a chance to dismiss itself.
    const escape = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented || event.isComposing || event.keyCode === 229 ||
          event.repeat || event.shiftKey || event.altKey || event.metaKey || event.ctrlKey) return;
      const id = activeWorkspace();
      const target = event.target instanceof HTMLElement ? event.target : undefined;
      if (!id || activeDialog() || target?.isContentEditable || target?.closest("select, [data-workspace-escape]")) return;
      expandFocus(id, target);
      consume(event);
    };
    const release = (event: KeyboardEvent) => { if (event.key === "Shift") closeHints(); };
    window.addEventListener("keydown", capture, true);
    window.addEventListener("keydown", escape);
    window.addEventListener("keyup", release);
    window.addEventListener("blur", closeHints);
    return () => {
      cancelHintTimer();
      window.removeEventListener("keydown", capture, true);
      window.removeEventListener("keydown", escape);
      window.removeEventListener("keyup", release);
      window.removeEventListener("blur", closeHints);
    };
  }, [access, cancelHintTimer, closeHints, conversationControl, enterComposer, enterWorkspace, expandFocus, navigate]);

  const toggle = workspace && (
    <button type="button"
      title={t("workspace.keyboard_help")}
      onClick={() => {
        if (mode === "workspace") {
          if (!enterComposer(workspace)) setMode("type");
        } else {
          expandFocus(workspace);
        }
      }}
      className="rounded-md border border-pc-border bg-pc-surface px-1.5 py-1 text-[0.6875rem] text-pc-text-secondary focus-visible:ring-2 focus-visible:ring-pc-accent"
    >
      {t(mode === "type" ? "workspace.keyboard_insert" : `workspace.keyboard_${mode}`)}
    </button>
  );
  return (
    <Context.Provider value={register}>
      <KeyboardMode.Provider value={toggle}>
        <div ref={root} className="contents" data-workspace-mode={workspace ? mode : undefined} onFocusCapture={(event) => {
          const target = event.nativeEvent.target;
          if (!workspace || !(target instanceof HTMLElement)) return;
          const current = remembered.current[workspace];
          const conversation = target.closest<HTMLElement>("[data-workspace-conversation]");
          if (current && conversation) current.conversation = conversation;
          if (isEditing(target) && !target.closest('dialog, [role="dialog"][aria-modal="true"]')) setMode("type");
        }}>
          {children}
        </div>
      </KeyboardMode.Provider>
      {hints && createPortal(
        <div key={revision} className="pointer-events-none fixed inset-0 z-[100]" aria-hidden="true">
          {hints.items.map(({ key, target }) => {
            const position = key.startsWith(prefix) && visibleBox(target);
            return position ? (
              <kbd key={key} className="fixed rounded border border-pc-base bg-pc-accent px-1 py-0.5 font-mono text-xs font-bold uppercase text-pc-base shadow-md" style={position}>
                {key}
              </kbd>
            ) : null;
          })}
        </div>, hints.scope)}
    </Context.Provider>
  );
}
