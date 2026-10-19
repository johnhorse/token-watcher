import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// 开发流：cd ui && npm run tauri dev（tauri-cli 工作目录即 ui/）
export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server: {
    port: 5175,
    strictPort: true,
  },
  envPrefix: ["VITE_", "TAURI_ENV_"],
  build: {
    // WKWebView / WebView2 基线
    target: "safari15",
    sourcemap: false,
  },
});
