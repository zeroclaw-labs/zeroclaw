import { Navigate, Route, Routes } from "react-router-dom";
import { ControlUiBootstrapProvider } from "@/app/ControlUiBootstrapProvider";
import { ChatPage } from "@/chat/ChatPage";
import { BoardPage } from "@/board/BoardPage";
import { OverviewPage } from "@/overview/OverviewPage";
import { ToastHost, ToastProvider } from "@/lib/toasts";

// Routes:
//   /              → redirect to /chat
//   /chat          → ChatPage (empty pane)
//   /chat/:slotId  → ChatPage with the matching slot streaming
//   /board         → BoardPage 4-lane Kanban
//   /overview      → OverviewPage memory/cron/integrations/tools cards
export default function App() {
  return (
    <ControlUiBootstrapProvider>
      <ToastProvider>
        <Routes>
          <Route path="/" element={<Navigate to="/chat" replace />} />
          <Route path="/chat" element={<ChatPage />} />
          <Route path="/chat/:slotId" element={<ChatPage />} />
          <Route path="/board" element={<BoardPage />} />
          <Route path="/overview" element={<OverviewPage />} />
        </Routes>
        <ToastHost />
      </ToastProvider>
    </ControlUiBootstrapProvider>
  );
}
