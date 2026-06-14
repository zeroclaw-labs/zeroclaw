import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import path from "path";

const gatewayPort = process.env.ZEROCLAW_GATEWAY_PORT ?? "42617";
const gatewayTarget = `http://127.0.0.1:${gatewayPort}`;

// Extra Host header values the dev server will accept, comma-separated, e.g.
// ZEROCLAW_WEB_ALLOWED_HOSTS=my-box.internal,dev.example.com. Unset → Vite default.
const allowedHosts = process.env.ZEROCLAW_WEB_ALLOWED_HOSTS
  ?.split(",")
  .map((h) => h.trim())
  .filter(Boolean);

export default defineConfig(({ command }) => ({
  base: command === "serve" ? "/" : "/_app/",
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  build: {
    outDir: "dist",
    target: ["chrome111", "edge111", "firefox113", "safari16.2"],
  },
  server: {
    allowedHosts,
    proxy: {
      "/api":            { target: gatewayTarget, changeOrigin: true },
      "^/acp(?:\\?.*)?$": { target: gatewayTarget, changeOrigin: true, ws: true },
      "/ws":             { target: gatewayTarget, changeOrigin: true, ws: true },
      "/admin":          { target: gatewayTarget, changeOrigin: true },
      "/health":         { target: gatewayTarget, changeOrigin: true },
      "/metrics":        { target: gatewayTarget, changeOrigin: true },
      "/pair":           { target: gatewayTarget, changeOrigin: true },
      "/webhook":        { target: gatewayTarget, changeOrigin: true },
      "/whatsapp":       { target: gatewayTarget, changeOrigin: true },
      "/linq":           { target: gatewayTarget, changeOrigin: true },
      "/wati":           { target: gatewayTarget, changeOrigin: true },
      "/nextcloud-talk": { target: gatewayTarget, changeOrigin: true },
      "/hooks":          { target: gatewayTarget, changeOrigin: true },
    },
  },
}));
