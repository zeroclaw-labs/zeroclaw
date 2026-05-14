import { Route, Routes } from "react-router-dom";
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
 */
export default function App() {
  return (
    <ControlUiBootstrapProvider>
      <Routes>
        <Route path="/" element={<ChatPage />} />
        <Route path="/chat" element={<ChatPage />} />
        {/* More pages land in M4b+ per §12.1. */}
      </Routes>
    </ControlUiBootstrapProvider>
  );
}
