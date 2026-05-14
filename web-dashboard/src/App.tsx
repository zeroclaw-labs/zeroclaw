import { Navigate, Route, Routes } from "react-router-dom";
import { ControlUiBootstrapProvider } from "@/app/ControlUiBootstrapProvider";
import { ChatPage } from "@/chat/ChatPage";

/**
 * Dashboard root.
 *
 * Structure follows `multi-session-dashboard.md §12` (OpenClaw module
 * translation): `ControlUiBootstrapProvider` fetches the
 * `/api/control-ui/config` snapshot on mount and exposes it as React
 * context to the tree. Theme state attaches to `<html>` via
 * `data-theme` / `data-mode` attributes (see `index.html` FOUC script
 * and `index.css` token definitions).
 *
 * Route shape (M3):
 *   `/`                  → redirect to `/chat`
 *   `/chat`              → ChatPage with no active slot (placeholder pane)
 *   `/chat/:slotId`      → ChatPage with the matching slot streaming
 *
 * Putting the active slot in the URL means reload preserves selection
 * and links between dashboards (or a future Board page deep-link)
 * resolve to the right conversation.
 */
export default function App() {
  return (
    <ControlUiBootstrapProvider>
      <Routes>
        <Route path="/" element={<Navigate to="/chat" replace />} />
        <Route path="/chat" element={<ChatPage />} />
        <Route path="/chat/:slotId" element={<ChatPage />} />
        {/* More pages (Board, System, Memory, …) land in M4b–M5 per §12.1. */}
      </Routes>
    </ControlUiBootstrapProvider>
  );
}
