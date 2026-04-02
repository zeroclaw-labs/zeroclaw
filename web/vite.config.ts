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
});
