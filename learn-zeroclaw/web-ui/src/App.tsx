import { useState, useEffect } from "react";
import { PairingForm } from "@/components/PairingForm";
import { Chat } from "@/components/Chat";
import { healthCheck } from "@/lib/api";

function App() {
  const [token, setToken] = useState<string | null>(
    () => localStorage.getItem("zeroclaw_token"),
  );
  const [gatewayUp, setGatewayUp] = useState<boolean | null>(null);

  useEffect(() => {
    healthCheck()
      .then(() => setGatewayUp(true))
      .catch(() => setGatewayUp(false));
  }, []);

  const handleLogout = () => {
    localStorage.removeItem("zeroclaw_token");
    setToken(null);
  };

  if (gatewayUp === false) {
    return (
      <div className="flex h-screen items-center justify-center bg-background">
        <div className="text-center space-y-2">
          <p className="text-lg font-semibold text-destructive">Gateway Offline</p>
          <p className="text-sm text-muted-foreground">
            Cannot reach ZeroClaw at{" "}
            <code className="bg-muted px-1 rounded">
              {import.meta.env.VITE_API_URL ?? "http://localhost:3000"}
            </code>
          </p>
          <p className="text-sm text-muted-foreground">
            Make sure <code className="bg-muted px-1 rounded">docker compose up</code> is running.
          </p>
        </div>
      </div>
    );
  }

  if (gatewayUp === null) {
    return (
      <div className="flex h-screen items-center justify-center bg-background">
        <p className="text-muted-foreground animate-pulse">Connecting to ZeroClaw...</p>
      </div>
    );
  }

  if (!token) {
    return <PairingForm onPaired={setToken} />;
  }

  return <Chat token={token} onLogout={handleLogout} />;
}

export default App;
