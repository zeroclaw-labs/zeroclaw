import { Link, useNavigate, useParams } from "react-router-dom";
import { useControlUiBootstrap } from "@/app/ControlUiBootstrapProvider";
import { SlotSidebar } from "@/chat/SlotSidebar";
import { ChatView } from "@/chat/ChatView";
import { ThemeSwitcher } from "@/theme/ThemeSwitcher";
import { useSlotsQuery } from "@/chat/slotsQuery";

/**
 * Chat page (M3, US-001 + US-002).
 *
 * Three-pane layout matching OpenClaw's chat surface (plan §12
 * translation table): sidebar ≤ 280px on the left, chat view centre,
 * optional right panel for side results (M4b+).
 *
 * Active slot lives in the URL path: `/chat/:slotId` is the active
 * conversation; bare `/chat` is the empty state. Reloading on a slot
 * URL re-enters that slot. Streaming chat (US-003) replaces the
 * placeholder pane once `useSlotStream` lands.
 */
export function ChatPage() {
  const bootstrap = useControlUiBootstrap();
  const { slotId } = useParams<{ slotId: string }>();
  const navigate = useNavigate();

  const handleSelect = (id: string) => {
    navigate(`/chat/${encodeURIComponent(id)}`);
  };
  const handleDeleted = (id: string) => {
    if (id === slotId) navigate("/chat");
  };

  return (
    <div className="flex h-full">
      <aside
        className="flex flex-col border-r"
        style={{ width: "280px", borderColor: "var(--color-border)" }}
      >
        <header
          className="flex items-center justify-between gap-2 px-4 py-3 border-b"
          style={{ borderColor: "var(--color-border)" }}
        >
          <nav className="flex items-center gap-2 text-sm">
            <span className="font-semibold truncate">
              {bootstrap.assistant_identity.name}
            </span>
          </nav>
          <div className="flex items-center gap-2">
            <span className="text-xs opacity-50">
              v{bootstrap.server_version}
            </span>
            <ThemeSwitcher />
          </div>
        </header>
        <nav
          className="flex items-center gap-1 px-3 py-1 border-b text-xs"
          style={{ borderColor: "var(--color-border)" }}
          aria-label="Dashboard sections"
        >
          <Link
            to="/chat"
            className="px-2 py-1 rounded font-medium underline"
            style={{ color: "var(--color-text)" }}
          >
            Chat
          </Link>
          <Link
            to="/board"
            className="px-2 py-1 rounded hover:underline"
            style={{ color: "var(--color-text-muted)" }}
          >
            Board
          </Link>
        </nav>
        <SlotSidebar
          activeSlotId={slotId}
          onSelectSlot={handleSelect}
          onSlotDeleted={handleDeleted}
        />
      </aside>
      <main className="flex-1 flex flex-col min-h-0">
        {slotId ? <ChatPane slotId={slotId} /> : <EmptyChatPane />}
      </main>
    </div>
  );
}

function EmptyChatPane() {
  return (
    <div className="flex-1 flex items-center justify-center">
      <p className="opacity-60 text-sm max-w-md text-center">
        Select a slot from the sidebar, or click <strong>+ New</strong> to
        start a fresh conversation.
      </p>
    </div>
  );
}

/**
 * Resolves the slot from the cached `["slots"]` list and hands the
 * matched row to `ChatView`. The slot-not-found fallback renders a
 * compact notice rather than a full error screen — the user is one
 * click away from another slot in the sidebar.
 *
 * Uses the shared `useSlotsQuery` hook so this component and
 * `SlotSidebar` register a single query observer. React Query keys on
 * the `queryKey` alone, so two `useQuery({queryKey:["slots"]})` calls
 * with different `queryFn`s would silently let the first-mounted
 * observer win — see PR #5 review thread.
 */
function ChatPane({ slotId }: { slotId: string }) {
  const { data, isLoading } = useSlotsQuery();

  if (isLoading) {
    return (
      <div className="flex-1 flex items-center justify-center text-sm opacity-60">
        Loading slot…
      </div>
    );
  }

  const slot = data?.slots.find((s) => s.id === slotId);
  if (!slot) {
    return (
      <div className="flex-1 flex items-center justify-center">
        <div className="text-sm opacity-60 text-center max-w-md">
          Slot <code className="font-mono">{slotId}</code> not found.
          <br />
          It may have been deleted from another tab.
          <br />
          <Link className="underline mt-2 inline-block" to="/chat">
            Back to slot list
          </Link>
        </div>
      </div>
    );
  }

  return <ChatView slotId={slot.id} title={slot.title} />;
}
