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
const editable = 'input:not([type="checkbox"]):not([type="radio"]):not([type="button"]), textarea, [contenteditable="true"]';

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

function focusComposer() {
  const target = [...document.querySelectorAll<HTMLElement>("[data-workspace-composer]")]
    .find((element) => isEnabled(element) && visibleBox(element));
  if (!target) return false;
  target.focus({ preventScroll: true });
  return true;
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
  const [navigation, setNavigationState] = useState(false);
  const navigationRef = useRef(false);
  const setNavigation = useCallback((value: boolean) => {
    navigationRef.current = value;
    setNavigationState(value);
  }, []);
  const [hints, setHints] = useState<HintLayer | null>(null);
  const [prefix, setPrefix] = useState("");
  const [revision, redraw] = useState(0);
  const hintRef = useRef<HintLayer | null>(null);
  const prefixRef = useRef("");
  const remembered = useRef<Partial<Record<WorkspaceId, { path: string; focus?: HTMLElement }>>>({});
  const registered = useRef(new Map<WorkspaceId, WorkspaceItems>());
  const pendingFocus = useRef<WorkspaceId | null>(null);
  const root = useRef<HTMLDivElement>(null);
  const keyboardFocus = useRef<HTMLButtonElement>(null);
  const register = useCallback<RegisterItems>((id, list) => {
    registered.current.set(id, list);
    return () => {
      if (registered.current.get(id) === list) registered.current.delete(id);
    };
  }, []);

  const closeHints = useCallback(() => {
    hintRef.current = null;
    prefixRef.current = "";
    setHints(null);
    setPrefix("");
  }, []);
  const enterNavigation = useCallback(() => {
    setNavigation(true);
    keyboardFocus.current?.focus({ preventScroll: true });
  }, [setNavigation]);

  useLayoutEffect(() => {
    if (!workspace) return;
    remembered.current[workspace] = {
      ...remembered.current[workspace],
      path: location.pathname + location.search + location.hash,
    };
  }, [location, workspace]);

  useEffect(() => {
    if (!workspace || pendingFocus.current !== workspace || !root.current) return;
    const focus = () => {
      if (workspace === "code") {
        if (!focusComposer()) return false;
        setNavigation(false);
      } else {
        const target = remembered.current[workspace]?.focus;
        if (target && isEnabled(target) && visibleBox(target)) target.focus({ preventScroll: true });
        else if (workspace !== "agent" || !focusComposer()) keyboardFocus.current?.focus({ preventScroll: true });
      }
      pendingFocus.current = null;
      return true;
    };
    const observer = new MutationObserver(() => { if (focus()) observer.disconnect(); });
    const frame = requestAnimationFrame(() => {
      if (!focus() && root.current) observer.observe(root.current, { childList: true, subtree: true, attributes: true, attributeFilter: ["disabled", "hidden"] });
    });
    return () => { cancelAnimationFrame(frame); observer.disconnect(); };
  }, [location.key, setNavigation, workspace]);

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
    const onKey = (event: KeyboardEvent) => {
      // The URL can change before a lazy route or its effects have committed.
      // Read the destination synchronously when two keys arrive back to back.
      const path = window.location.pathname.slice(basePath.length);
      const workspace = workspaceForPath(path);
      if (!workspace || event.defaultPrevented || event.isComposing || event.keyCode === 229) return;
      if (event.key === "Escape" && hintRef.current) {
        closeHints();
        event.preventDefault();
        event.stopImmediatePropagation();
        return;
      }
      const dialog = activeDialog();
      const target = event.target instanceof HTMLElement ? event.target : undefined;
      const modified = event.altKey || event.metaKey || event.ctrlKey;
      if (modified) return;
      if (event.key === "Shift" && navigationRef.current && !event.repeat && !target?.matches(editable)) {
        const scope = dialog ?? document.body;
        const targets = [...scope.querySelectorAll<HTMLElement>(clickable)].filter((element) =>
          isEnabled(element) && visibleBox(element));
        const keys = assignHints(targets.map((element) => element.dataset.workspaceHint));
        const layer = { scope, items: targets.flatMap((element, index) => (
          keys[index] ? [{ target: element, key: keys[index]! }] : []
        )) };
        hintRef.current = layer;
        setHints(layer);
        return;
      }
      if (hintRef.current && event.shiftKey && /^[a-z]$/i.test(event.key) && !event.repeat) {
        event.preventDefault();
        event.stopImmediatePropagation();
        const key = prefixRef.current + event.key.toLowerCase();
        const item = hintRef.current.items.find((entry) => entry.key === key);
        if (item && hintRef.current.scope === (dialog ?? document.body)) {
          closeHints();
          if (visibleBox(item.target) && isEnabled(item.target)) {
            item.target.focus({ preventScroll: true });
            item.target.click();
          }
        } else {
          const next = hintRef.current.items.some((entry) => entry.key.startsWith(key)) ? key : "";
          prefixRef.current = next;
          setPrefix(next);
        }
        return;
      }
      if (dialog || event.shiftKey) return;
      if (event.key === "Escape") {
        if (target?.matches("select, [contenteditable=true]")) return;
        if (target?.closest("[data-workspace-escape]")) return;
        pendingFocus.current = null;
        enterNavigation();
        event.preventDefault();
        return;
      }
      if (!navigationRef.current || target?.matches(editable)) return;
      if (event.key === "i") {
        if (focusComposer()) {
          setNavigation(false);
          event.preventDefault();
        }
        return;
      }
      if (event.key === "h" || event.key === "l") {
        const allowed = workspaces.filter((item) => pathAccess(item.path, access) === "allowed");
        const index = allowed.findIndex((item) => item.id === workspace);
        if (index < 0 || allowed.length < 2) return;
        const step = event.key === "l" ? 1 : -1;
        const next = allowed[(index + step + allowed.length) % allowed.length]!;
        remembered.current[workspace] = {
          ...remembered.current[workspace],
          path: path + window.location.search + window.location.hash,
        };
        pendingFocus.current = next.id;
        void navigate(remembered.current[next.id]?.path ?? next.path);
        event.preventDefault();
      } else if (event.key === "j" || event.key === "k") {
        const list = registered.current.get(workspace);
        const id = list && adjacentItem(list.ids, list.selected, event.key === "j" ? 1 : -1);
        if (id !== undefined) {
          list?.select(id);
          event.preventDefault();
        }
      }
    };
    // Process hint keys before dialog shortcuts and browser defaults. Other
    // keys run after the focused control has handled them.
    const capture = (event: KeyboardEvent) => {
      if (event.key === "Shift" || (hintRef.current && (event.shiftKey || event.key === "Escape"))) onKey(event);
    };
    const bubble = (event: KeyboardEvent) => { if (event.key !== "Shift") onKey(event); };
    const release = (event: KeyboardEvent) => { if (event.key === "Shift") closeHints(); };
    window.addEventListener("keydown", capture, true);
    window.addEventListener("keydown", bubble);
    window.addEventListener("keyup", release);
    window.addEventListener("blur", closeHints);
    return () => {
      window.removeEventListener("keydown", capture, true);
      window.removeEventListener("keydown", bubble);
      window.removeEventListener("keyup", release);
      window.removeEventListener("blur", closeHints);
    };
  }, [access, closeHints, enterNavigation, navigate, setNavigation]);

  const toggle = workspace && (
    <button type="button" ref={keyboardFocus}
      title={t("workspace.keyboard_help")}
      aria-pressed={navigation}
      onClick={() => navigation && focusComposer() ? setNavigation(false) : enterNavigation()}
      className="rounded-md border border-pc-border bg-pc-surface px-1.5 py-1 text-[0.6875rem] text-pc-text-secondary focus-visible:ring-2 focus-visible:ring-pc-accent"
    >
      {t(navigation ? "workspace.keyboard_navigation" : "workspace.keyboard_insert")}
    </button>
  );
  return (
    <Context.Provider value={register}>
      <KeyboardMode.Provider value={toggle}>
        <div ref={root} className="contents" onFocusCapture={(event) => {
          const target = event.nativeEvent.target;
          if (!workspace || !(target instanceof HTMLElement)) return;
          const current = remembered.current[workspace];
          if (current && target !== keyboardFocus.current) current.focus = target;
          if (target.closest("[data-workspace-composer]")) {
            setNavigation(false);
            pendingFocus.current = null;
          }
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
