import { useControlUiBootstrap } from "@/app/ControlUiBootstrapProvider";
import { SlotSidebar } from "@/chat/SlotSidebar";

/**
 * Chat page (M3 scaffold).
 *
 * Three-pane layout matching OpenClaw's chat surface (plan §12
 * translation table): sidebar ≤ 280px on the left, chat view centre,
 * optional right panel for side results (M4b+).
 *
 * This scaffold wires the sidebar + a placeholder chat view. Real
 * streaming chat lands in the follow-up M3 sub-commits per §12.1.
 */
export function ChatPage() {
  const bootstrap = useControlUiBootstrap();

  return (
    <div className="flex h-full">
      <aside
        className="flex flex-col border-r"
        style={{ width: "280px", borderColor: "var(--color-border)" }}
      >
        <header
          className="flex items-center justify-between px-4 py-3 border-b"
          style={{ borderColor: "var(--color-border)" }}
        >
          <span className="text-sm font-semibold">{bootstrap.assistant_identity.name}</span>
          <span className="text-xs opacity-50">v{bootstrap.server_version}</span>
        </header>
        <SlotSidebar />
      </aside>
      <main className="flex-1 flex items-center justify-center">
        <p className="opacity-60 text-sm max-w-md text-center">
          Select a slot from the sidebar, or create one to start chatting.
          <br />
          Streaming chat lands in the next M3 sub-commit.
        </p>
      </main>
    </div>
  );
}
