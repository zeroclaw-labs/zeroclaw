import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  base: "/zeroclaw/",
  plugins: [react()],
  build: {
    outDir: "../gh-pages",
    emptyOutDir: true,
  },
});
