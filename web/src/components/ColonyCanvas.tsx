import { useEffect, useId, useRef, useState, type PointerEvent } from "react";
import {
  Bot,
  Crown,
  MessageSquare,
  Radio,
  Type,
  Minus,
  Plus,
  Maximize,
  Move,
} from "lucide-react";
import { t } from "@/lib/i18n";

/** Display projection only. Node identity, membership and coordinates are
 * resolved from the gateway's canonical colony definition on each render. */
export interface ColonyCanvasNode {
  id: string;
  kind: "agent" | "queen" | "text" | "room" | "channel";
  label: string;
  purpose: string;
  colonyId?: string;
  x: number;
  y: number;
  state?: string;
  selectable?: boolean;
}
export interface ColonyCanvasWire {
  id: string;
  from: string;
  to: string;
  label: string;
}
export interface ColonyCanvasBox {
  id: string;
  name: string;
  status: string;
}
interface XY {
  x: number;
  y: number;
}
const WIDTH = 208;
const HEIGHT = 120;
const icons = {
  agent: Bot,
  queen: Crown,
  text: Type,
  room: MessageSquare,
  channel: Radio,
};

export default function ColonyCanvas({
  nodes,
  wires,
  boxes,
  selection,
  onSelection,
  onInspect,
  onMove,
  onConnect,
  disabled = false,
  visible = true,
}: {
  nodes: ColonyCanvasNode[];
  wires: ColonyCanvasWire[];
  boxes: ColonyCanvasBox[];
  selection: ReadonlySet<string>;
  onSelection: (ids: string[]) => void;
  onInspect: (id: string) => void;
  onMove: (id: string, x: number, y: number) => void;
  onConnect: (from: string, to: string) => void;
  disabled?: boolean;
  visible?: boolean;
}) {
  const viewport = useRef<HTMLDivElement>(null);
  const [camera, setCamera] = useState({ x: 36, y: 62, zoom: 0.9 });
  const [gesture, setGesture] = useState<{
    kind: "select" | "pan" | "node";
    start: XY;
    point: XY;
    id?: string;
    origin?: XY;
  } | null>(null);
  const gestureRef = useRef(gesture);
  const [source, setSource] = useState<string | null>(null);
  const fitPending = useRef(true);
  const previousIdentity = useRef("");
  const previousSize = useRef({ width: 0, height: 0 });
  const marker = useId().replace(/:/g, "");
  const world = (e: { clientX: number; clientY: number }): XY => {
    const rect = viewport.current?.getBoundingClientRect();
    return {
      x: (e.clientX - (rect?.left ?? 0) - camera.x) / camera.zoom,
      y: (e.clientY - (rect?.top ?? 0) - camera.y) / camera.zoom,
    };
  };
  const begin = (e: PointerEvent<HTMLDivElement>) => {
    if (e.target !== e.currentTarget || disabled) return;
    e.currentTarget.setPointerCapture(e.pointerId);
    const point = world(e);
    const value = {
      kind: e.altKey || e.button === 1 ? ("pan" as const) : ("select" as const),
      start: point,
      point,
      origin: { x: camera.x, y: camera.y },
    };
    gestureRef.current = value;
    setGesture(value);
  };
  const move = (e: PointerEvent<HTMLDivElement>) => {
    const value = gestureRef.current;
    if (!value) return;
    const point = world(e);
    if (value.kind === "pan") {
      setCamera((c) => ({
        ...c,
        x: c.x + (point.x - value.start.x) * c.zoom,
        y: c.y + (point.y - value.start.y) * c.zoom,
      }));
      return;
    }
    const next = { ...value, point };
    gestureRef.current = next;
    setGesture(next);
  };
  const end = () => {
    const value = gestureRef.current;
    if (!value) return;
    if (value.kind === "select") {
      const left = Math.min(value.start.x, value.point.x),
        right = Math.max(value.start.x, value.point.x);
      const top = Math.min(value.start.y, value.point.y),
        bottom = Math.max(value.start.y, value.point.y);
      onSelection(
        nodes
          .filter(
            (n) =>
              n.kind === "agent" &&
              n.selectable !== false &&
              !n.colonyId &&
              n.x + WIDTH >= left &&
              n.x <= right &&
              n.y + HEIGHT >= top &&
              n.y <= bottom,
          )
          .map((n) => n.id),
      );
    } else if (value.kind === "node" && value.id && value.origin) {
      const travel =
        Math.abs(value.start.x - value.point.x) +
        Math.abs(value.start.y - value.point.y);
      if (travel > 4)
        onMove(
          value.id,
          Math.max(0, value.origin.x + value.point.x - value.start.x),
          Math.max(0, value.origin.y + value.point.y - value.start.y),
        );
    }
    gestureRef.current = null;
    setGesture(null);
  };
  const fit = () => {
    const rect = viewport.current?.getBoundingClientRect();
    if (!rect || rect.width <= 0 || rect.height <= 0) return;
    if (!nodes.length) {
      setCamera({ x: 36, y: 62, zoom: 0.9 });
      return;
    }
    const minX = Math.min(...nodes.map((n) => n.x)) - 32,
      minY = Math.min(...nodes.map((n) => n.y)) - 56;
    const width = Math.max(...nodes.map((n) => n.x)) + WIDTH + 32 - minX;
    const height = Math.max(...nodes.map((n) => n.y)) + HEIGHT + 32 - minY;
    const zoom = Math.min(
      1,
      Math.max(
        0.2,
        Math.min((rect.width - 40) / width, (rect.height - 80) / height),
      ),
    );
    setCamera({
      x: (rect.width - width * zoom) / 2 - minX * zoom,
      y: 55 - minY * zoom,
      zoom,
    });
  };
  const nodeIdentity = nodes.map((node) => node.id).join("\n");
  const fitRef = useRef(fit);
  fitRef.current = fit;
  useEffect(() => {
    if (!visible || !viewport.current) return;
    const observer = new ResizeObserver(([entry]) => {
      if (!entry) return;
      const { width, height } = entry.contentRect;
      if (
        width <= 0 ||
        height <= 0 ||
        (width === previousSize.current.width &&
          height === previousSize.current.height)
      )
        return;
      previousSize.current = { width, height };
      fitRef.current();
    });
    observer.observe(viewport.current);
    return () => observer.disconnect();
  }, [visible]);
  // Fit when membership changes; polling or dragging never resets the camera.
  useEffect(() => {
    if (previousIdentity.current !== nodeIdentity) {
      fitPending.current = true;
      previousIdentity.current = nodeIdentity;
    }
    if (visible && fitPending.current) {
      fit();
      fitPending.current = false;
    }
  }, [nodeIdentity, visible]);
  const positions = new Map(
    nodes.map((node) => {
      const draft =
        gesture?.kind === "node" && gesture.id === node.id && gesture.origin
          ? {
              x: Math.max(
                0,
                gesture.origin.x + gesture.point.x - gesture.start.x,
              ),
              y: Math.max(
                0,
                gesture.origin.y + gesture.point.y - gesture.start.y,
              ),
            }
          : node;
      return [node.id, draft];
    }),
  );
  return (
    <div className="relative min-h-80 min-w-0 flex-1 overflow-hidden rounded-xl border border-pc-border bg-pc-base">
      <div className="absolute left-3 top-3 z-10 flex items-center gap-1 rounded-lg border border-pc-border bg-pc-surface/95 p-1 shadow-sm">
        <button
          type="button"
          aria-label={t("colony.zoom_out")}
          onClick={() =>
            setCamera((c) => ({ ...c, zoom: Math.max(0.2, c.zoom - 0.1) }))
          }
          className="rounded p-2 hover:bg-pc-elevated"
        >
          <Minus className="h-3.5 w-3.5" />
        </button>
        <span className="min-w-10 text-center text-xs tabular-nums">
          {Math.round(camera.zoom * 100)}%
        </span>
        <button
          type="button"
          aria-label={t("colony.zoom_in")}
          onClick={() =>
            setCamera((c) => ({ ...c, zoom: Math.min(1.8, c.zoom + 0.1) }))
          }
          className="rounded p-2 hover:bg-pc-elevated"
        >
          <Plus className="h-3.5 w-3.5" />
        </button>
        <button
          type="button"
          aria-label={t("colony.fit")}
          onClick={fit}
          className="rounded p-2 hover:bg-pc-elevated"
        >
          <Maximize className="h-3.5 w-3.5" />
        </button>
      </div>
      <div
        ref={viewport}
        data-testid="colony-canvas"
        role="group"
        aria-label={t("colony.canvas")}
        onPointerDown={begin}
        onPointerMove={move}
        onPointerUp={end}
        onPointerCancel={() => {
          gestureRef.current = null;
          setGesture(null);
        }}
        className="absolute inset-0 touch-none select-none"
        style={{
          backgroundImage:
            "radial-gradient(var(--pc-border-strong) 1px, transparent 1px)",
          backgroundSize: `${24 * camera.zoom}px ${24 * camera.zoom}px`,
          backgroundPosition: `${camera.x}px ${camera.y}px`,
        }}
      >
        <div
          className="pointer-events-none absolute left-0 top-0"
          style={{
            transform: `translate(${camera.x}px, ${camera.y}px) scale(${camera.zoom})`,
            transformOrigin: "0 0",
          }}
        >
          {boxes.map((box) => {
            const members = nodes.filter((n) => n.colonyId === box.id);
            if (!members.length) return null;
            const left =
              Math.min(...members.map((n) => positions.get(n.id)?.x ?? n.x)) -
              28;
            const top =
              Math.min(...members.map((n) => positions.get(n.id)?.y ?? n.y)) -
              50;
            const width =
              Math.max(...members.map((n) => positions.get(n.id)?.x ?? n.x)) +
              WIDTH +
              28 -
              left;
            const height =
              Math.max(...members.map((n) => positions.get(n.id)?.y ?? n.y)) +
              HEIGHT +
              28 -
              top;
            return (
              <div
                key={box.id}
                className="absolute rounded-2xl border-2 border-pc-accent/30 bg-pc-accent/[0.035]"
                style={{ left, top, width, height }}
              >
                <button
                  type="button"
                  onClick={() => onInspect(`colony:${box.id}`)}
                  className="pointer-events-auto absolute left-4 top-2 flex max-w-[90%] items-center gap-2 truncate rounded px-2 py-1 text-xs font-medium text-pc-accent hover:bg-pc-accent/10"
                >
                  <Crown className="h-3.5 w-3.5 shrink-0" />
                  {box.name}
                  <span className="rounded bg-pc-accent/10 px-2 py-0.5 text-[10px]">
                    {box.status}
                  </span>
                </button>
              </div>
            );
          })}
          <svg
            width="5000"
            height="5000"
            className="pointer-events-none absolute overflow-visible"
            aria-hidden="true"
          >
            <defs>
              <marker
                id={marker}
                viewBox="0 0 10 10"
                refX="9"
                refY="5"
                markerWidth="7"
                markerHeight="7"
                orient="auto-start-reverse"
              >
                <path d="M 0 0 L 10 5 L 0 10 z" fill="var(--pc-accent)" />
              </marker>
            </defs>
            {wires.map((wire, index) => {
              const from = positions.get(wire.from),
                to = positions.get(wire.to);
              if (!from || !to) return null;
              const x1 = from.x + WIDTH,
                y1 = from.y + HEIGHT / 2,
                x2 = to.x,
                y2 = to.y + HEIGHT / 2;
              const bend = Math.max(60, Math.abs(x2 - x1) * 0.45);
              const around =
                x2 < x1 ||
                (Math.abs(y2 - y1) < 10 && Math.abs(x2 - x1) > WIDTH);
              const lane = Math.min(from.y, to.y) - 24 - (index % 4) * 14;
              const path = around
                ? `M${x1},${y1} C${x1 + 32},${y1} ${x1 + 32},${lane} ${x1 + 32},${lane} L${x2 - 28},${lane} C${x2 - 28},${lane} ${x2 - 28},${y2} ${x2},${y2}`
                : `M${x1},${y1} C${x1 + bend},${y1} ${x2 - bend},${y2} ${x2},${y2}`;
              return (
                <path
                  key={wire.id}
                  d={path}
                  stroke="var(--pc-accent)"
                  strokeWidth="1.75"
                  fill="none"
                  opacity="0.65"
                  markerEnd={`url(#${marker})`}
                >
                  <title>{wire.label}</title>
                </path>
              );
            })}
          </svg>
          {nodes.map((node) => {
            const position = positions.get(node.id) ?? node;
            const Icon = icons[node.kind];
            return (
              <div
                key={node.id}
                data-testid={`colony-node-${node.id}`}
                className={`pointer-events-auto absolute flex flex-col rounded-xl border bg-pc-surface shadow-lg ${selection.has(node.id) ? "border-pc-accent ring-2 ring-pc-accent/25" : node.kind === "queen" ? "border-pc-accent/60" : "border-pc-border-strong"}`}
                style={{
                  left: position.x,
                  top: position.y,
                  width: WIDTH,
                  height: HEIGHT,
                }}
              >
                <div className="flex items-center gap-2 border-b border-pc-border px-3 py-2">
                  <Icon
                    className={`h-4 w-4 shrink-0 ${node.kind === "queen" ? "text-pc-accent" : "text-pc-text-muted"}`}
                  />
                  <button
                    type="button"
                    onClick={() => onInspect(node.id)}
                    className="min-w-0 flex-1 truncate text-left text-xs font-semibold"
                    title={node.label}
                  >
                    {node.label}
                  </button>
                  {node.kind === "agent" && !node.colonyId && (
                    <input
                      type="checkbox"
                      disabled={node.selectable === false}
                      aria-label={`${t("colony.select_agent")} ${node.label}`}
                      checked={selection.has(node.id)}
                      onChange={(e) =>
                        onSelection(
                          e.target.checked
                            ? [...selection, node.id]
                            : [...selection].filter((id) => id !== node.id),
                        )
                      }
                    />
                  )}
                  <button
                    type="button"
                    disabled={disabled}
                    aria-label={`${t("colony.move_node")} ${node.label}`}
                    onPointerDown={(e) => {
                      e.preventDefault();
                      e.stopPropagation();
                      viewport.current?.setPointerCapture(e.pointerId);
                      const point = world(e),
                        value = {
                          kind: "node" as const,
                          id: node.id,
                          start: point,
                          point,
                          origin: { x: node.x, y: node.y },
                        };
                      gestureRef.current = value;
                      setGesture(value);
                    }}
                    onKeyDown={(e) => {
                      const d = e.shiftKey ? 40 : 10;
                      const delta: Record<string, XY> = {
                        ArrowLeft: { x: -d, y: 0 },
                        ArrowRight: { x: d, y: 0 },
                        ArrowUp: { x: 0, y: -d },
                        ArrowDown: { x: 0, y: d },
                      };
                      const step = delta[e.key];
                      if (step) {
                        e.preventDefault();
                        onMove(
                          node.id,
                          Math.max(0, node.x + step.x),
                          Math.max(0, node.y + step.y),
                        );
                      }
                    }}
                    className="rounded p-1 text-pc-text-faint hover:text-pc-text disabled:opacity-40"
                  >
                    <Move className="h-3 w-3" />
                  </button>
                </div>
                <button
                  type="button"
                  onClick={() => onInspect(node.id)}
                  className="flex min-h-0 flex-1 flex-col items-start px-3 py-2 text-left"
                >
                  <span className="line-clamp-2 text-[11px] leading-relaxed text-pc-text-muted">
                    {node.purpose || t(`colony.node_${node.kind}`)}
                  </span>
                  <span className="mt-auto text-[10px] text-pc-accent">
                    {node.state || t(`colony.node_${node.kind}`)}
                  </span>
                </button>
                {node.colonyId && (
                  <>
                    <button
                      type="button"
                      disabled={disabled || !source || source === node.id}
                      aria-label={`${t("colony.connect_to")} ${node.label}`}
                      onClick={() => {
                        if (source) {
                          onConnect(source, node.id);
                          setSource(null);
                        }
                      }}
                      className={`absolute -left-2 top-[53px] h-4 w-4 rounded-full border-2 border-pc-surface ${source && source !== node.id ? "bg-pc-accent ring-2 ring-pc-accent/30" : "bg-pc-text-faint"} disabled:opacity-60`}
                    />
                    <button
                      type="button"
                      disabled={disabled}
                      aria-label={`${t("colony.connect_from")} ${node.label}`}
                      aria-pressed={source === node.id}
                      onClick={() =>
                        setSource(source === node.id ? null : node.id)
                      }
                      className={`absolute -right-2 top-[53px] h-4 w-4 rounded-full border-2 border-pc-surface ${source === node.id ? "bg-pc-accent ring-2 ring-pc-accent/30" : "bg-pc-accent/70"}`}
                    />
                  </>
                )}
              </div>
            );
          })}
          {gesture?.kind === "select" && (
            <div
              className="absolute border border-pc-accent bg-pc-accent/10"
              style={{
                left: Math.min(gesture.start.x, gesture.point.x),
                top: Math.min(gesture.start.y, gesture.point.y),
                width: Math.abs(gesture.start.x - gesture.point.x),
                height: Math.abs(gesture.start.y - gesture.point.y),
              }}
            />
          )}
        </div>
      </div>
      {source && (
        <button
          type="button"
          onClick={() => setSource(null)}
          className="absolute bottom-10 left-3 max-w-[calc(100%-1.5rem)] rounded-lg border border-pc-accent/30 bg-pc-surface px-3 py-2 text-xs text-pc-accent"
        >
          {t("colony.wire_hint")} · {t("common.cancel")}
        </button>
      )}
      <p className="pointer-events-none absolute bottom-3 left-3 right-3 text-[10px] text-pc-text-faint">
        {t("colony.canvas_hint")}
      </p>
    </div>
  );
}
