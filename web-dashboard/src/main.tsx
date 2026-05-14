import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { BrowserRouter } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import App from "./App";
import "./index.css";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      // REST responses that the dashboard polls (slot list, status,
      // cost) are cheap to refetch and the staleness tolerance is
      // user-visible, so keep these short. Per-feature hooks can bump
      // their own staleTime when data is genuinely infrequent.
      staleTime: 5_000,
      refetchOnWindowFocus: false,
      retry: 1,
    },
  },
});

// M3: the dashboard is served from `/dashboard/` in production (see
// vite.config.ts `base`). `BrowserRouter` needs the matching basename
// so `<Link to="/slots">` resolves to `/dashboard/slots`. In dev the
// Vite server serves at `/` so we special-case that via BASE_URL.
const routerBasename = import.meta.env.BASE_URL.replace(/\/$/, "") || "/";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <BrowserRouter basename={routerBasename}>
        <App />
      </BrowserRouter>
    </QueryClientProvider>
  </StrictMode>,
);

// Production service worker registration (M3). Mirrors OpenClaw's
// pattern at ui/src/main.ts:3-18 — dev builds skip SW entirely to
// avoid caching the Vite dev bundle.
if (import.meta.env.PROD && "serviceWorker" in navigator) {
  window.addEventListener("load", () => {
    void navigator.serviceWorker.register(`${routerBasename}/sw.js`, { scope: routerBasename })
      .catch((err) => {
        // Non-fatal — the app works without a SW; log for diagnosis.
        console.warn("[zeroclaw-dashboard] SW registration failed:", err);
      });
  });
}
