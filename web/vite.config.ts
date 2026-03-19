import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import path from "path";

// Build-only config. The web dashboard is served by the Rust gateway
// via rust-embed. Run `npm run build` then `cargo build` to update.
export default defineConfig({
  base: "/_app/",
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
      "/pair": "http://127.0.0.1:42617",
      "/health": "http://127.0.0.1:42617",
      "/api": "http://127.0.0.1:42617",
      "/ws": { target: "ws://127.0.0.1:42617", ws: true },
      "/webhook": "http://127.0.0.1:42617",
      "/metrics": "http://127.0.0.1:42617",
    },
  },
});
