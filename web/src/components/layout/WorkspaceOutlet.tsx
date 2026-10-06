import { createContext, useContext, useRef, type ReactNode } from "react";
import { useLocation, useOutlet } from "react-router-dom";
import { ErrorBoundary } from "@/App";

export const VisibleWorkspace = createContext(true);
export const useWorkspaceVisible = () => useContext(VisibleWorkspace);

/** Live work belongs to the app session, not the currently selected page.
 * Keep the routed elements (and their own connections) mounted until logout.
 * This stores presentation identity only; runtime state stays in each owner.
 */
export default function WorkspaceOutlet() {
  const { pathname } = useLocation();
  const outlet = useOutlet();
  const active = /^\/agent\/[^/]+$/.test(pathname)
    ? "chat"
    : pathname === "/code"
      ? "code"
      : pathname === "/sops" || pathname.startsWith("/sops/") ? "sop" : null;
  const mounted = useRef<Partial<Record<"chat" | "code" | "sop", ReactNode>>>({});
  if (active) mounted.current[active] = outlet;
  return (
    <>
      {(["chat", "code", "sop"] as const).map((key) => (
        <div key={key} hidden={active !== key} className="h-full">
          <VisibleWorkspace.Provider value={active === key}>
            <ErrorBoundary>{mounted.current[key]}</ErrorBoundary>
          </VisibleWorkspace.Provider>
        </div>
      ))}
      {!active && (
        <ErrorBoundary key={pathname.split("/")[1] ?? ""}>
          {outlet}
        </ErrorBoundary>
      )}
    </>
  );
}
