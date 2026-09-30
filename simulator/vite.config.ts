import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

export default defineConfig({
    root: __dirname,
    cacheDir: "../node_modules/.vite-simulator",
    build: {
        emptyOutDir: true,
        outDir: "../dist/simulator",
    },
    clearScreen: false,
    optimizeDeps: {
        include: ["react", "react-dom/client"],
    },
    plugins: [react()],
    server: {
        host: "127.0.0.1",
        port: 1421,
        strictPort: true,
    },
    test: {
        environment: "jsdom",
        include: ["src/**/*.test.{ts,tsx}"],
    },
});
