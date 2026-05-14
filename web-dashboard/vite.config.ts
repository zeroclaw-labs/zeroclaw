import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import path from "path";

const gatewayPort = process.env.ZEROCLAW_GATEWAY_PORT ?? "42617";
const gatewayTarget = `http://127.0.0.1:${gatewayPort}`;

// M3: the dashboard is served at `/dashboard/*` during development and
// initial rollout. Per §12 of multi-session-dashboard.md, a later
// milestone (M5.5) flips this to root-mount once `web-dashboard/`
// reaches feature parity with the existing `web/` app.
export default defineConfig(({ command }) => ({
  base: command === "serve" ? "/" : "/dashboard/",
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  build: {
    outDir: "dist",
  },
  server: {
    proxy: {
      "/api":  { target: gatewayTarget, changeOrigin: true },
      "/ws":   { target: gatewayTarget, changeOrigin: true, ws: true },
    },
  },
}));
