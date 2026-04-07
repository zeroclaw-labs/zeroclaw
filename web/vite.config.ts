import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import path from "path";

const gatewayPort = process.env.ZEROCLAW_GATEWAY_PORT ?? "42617";
const gatewayTarget = `http://127.0.0.1:${gatewayPort}`;

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
  },
  server: {
    proxy: {
      "/api":            { target: gatewayTarget, changeOrigin: true },
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
