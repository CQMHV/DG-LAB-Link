import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

export default defineConfig({
    build: {
        outDir: "dist/client",
    },
    clearScreen: false,
    optimizeDeps: {
        include: ["react", "react-dom/client"],
    },
    plugins: [react()],
    server: {
        host: "127.0.0.1",
        port: 1420,
        strictPort: true,
        watch: {
            ignored: ["**/src-tauri/**"],
        },
        warmup: {
            clientFiles: ["./src/main.tsx"],
        },
    },
    test: {
        include: ["src/**/*.test.{ts,tsx}"],
    },
});
