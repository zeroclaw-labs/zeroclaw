import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import path from "path";

export default defineConfig(() => {
  const apiTarget = process.env.VITE_API_TARGET ?? "http://127.0.0.1:42617";
  const wsTarget = process.env.VITE_WS_TARGET ?? apiTarget.replace(/^http/, "ws");

  return {
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
        "/health": {
          target: apiTarget,
          changeOrigin: true,
        },
        "/pair": {
          target: apiTarget,
          changeOrigin: true,
        },
        "/api": {
          target: apiTarget,
          changeOrigin: true,
        },
        "/ws": {
          target: wsTarget,
          ws: true,
        },
      },
    },
  };
});
