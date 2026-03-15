import { useEffect, useState } from "react";
import {
  isTauri,
  isGatewayRunning,
  onGatewayStatus,
  type GatewayStatusEvent,
} from "../lib/tauri-bridge";

interface GatewayStatusProps {
  onReady: () => void;
}

/**
 * Overlay shown while the MoA gateway is starting.
 * Listens for `gateway-status` events from the Rust backend
 * and displays progress to the user. Auto-dismisses on "ready"
 * or shows an error message on "failed".
 *
 * In non-Tauri environments (browser), immediately calls onReady.
 */
export function GatewayStatus({ onReady }: GatewayStatusProps) {
  const [status, setStatus] = useState<GatewayStatusEvent["status"]>("starting");
  const [message, setMessage] = useState("Starting backend service...");

  useEffect(() => {
    // In browser mode, skip gateway check entirely
    if (!isTauri()) {
      onReady();
      return;
    }

    let cancelled = false;

    // Check if already running (fast path)
    isGatewayRunning().then((running) => {
      if (cancelled) return;
      if (running) {
        setStatus("ready");
        onReady();
      }
    });

    // Listen for status events from backend
    const unlistenPromise = onGatewayStatus((event) => {
      if (cancelled) return;
      setStatus(event.status);
      setMessage(event.message);
      if (event.status === "ready") {
        onReady();
      }
    });

    return () => {
      cancelled = true;
      unlistenPromise.then((unlisten) => unlisten());
    };
  }, [onReady]);

  if (status === "ready") return null;

  return (
    <div style={overlayStyle}>
      <div style={cardStyle}>
        <div style={logoStyle}>MoA</div>
        {status === "starting" && (
          <>
            <div style={spinnerStyle} />
            <p style={messageStyle}>{message}</p>
          </>
        )}
        {status === "failed" && (
          <>
            <p style={{ ...messageStyle, color: "#e74c3c" }}>{message}</p>
            <p style={hintStyle}>
              Please install MoA or start it manually, then restart the app.
            </p>
          </>
        )}
      </div>
    </div>
  );
}

const overlayStyle: React.CSSProperties = {
  position: "fixed",
  inset: 0,
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  backgroundColor: "rgba(0, 0, 0, 0.85)",
  zIndex: 9999,
};

const cardStyle: React.CSSProperties = {
  textAlign: "center",
  padding: "2rem 3rem",
  borderRadius: "12px",
  backgroundColor: "#1a1a2e",
  color: "#eee",
  maxWidth: "400px",
};

const logoStyle: React.CSSProperties = {
  fontSize: "2rem",
  fontWeight: 700,
  marginBottom: "1.5rem",
  letterSpacing: "0.05em",
};

const spinnerStyle: React.CSSProperties = {
  width: "32px",
  height: "32px",
  margin: "0 auto 1rem",
  border: "3px solid rgba(255,255,255,0.2)",
  borderTopColor: "#6c63ff",
  borderRadius: "50%",
  animation: "spin 0.8s linear infinite",
};

const messageStyle: React.CSSProperties = {
  fontSize: "0.95rem",
  margin: "0.5rem 0",
};

const hintStyle: React.CSSProperties = {
  fontSize: "0.8rem",
  color: "#999",
  marginTop: "1rem",
};
