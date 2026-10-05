import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwind from "@tailwindcss/vite";

export default defineConfig({
  plugins: [react(), tailwind()],
  server: {
    port: 5173,
    watch: {
      // Honour CHOKIDAR_USEPOLLING=true (set in docker-compose web-dev) so
      // HMR works through macOS docker bind mounts. No-op on host runs.
      usePolling: process.env.CHOKIDAR_USEPOLLING === "true",
      interval: Number(process.env.CHOKIDAR_INTERVAL ?? 300),
    },
  },
});
