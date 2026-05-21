import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

// Build modes:
//   default — desktop Tauri webview entry (index.html, base "/")
//   mobile  — remote-access SPA served at /v2 by the embedded HTTP server
//             (mobile/index.html, base "/v2/", output dist/mobile/)
//
// `npm run build` runs both back-to-back so a single build pass produces
// everything tauri-build needs to embed.
export default defineConfig(async ({ mode }) => {
  const isMobile = mode === "mobile";

  return {
    plugins: [react(), tailwindcss()],
    root: isMobile ? "mobile" : undefined,
    // Mobile served at `/` (matches the QR code URL) — Vite emits asset
    // hrefs as `/assets/...` which the embedded HTTP server now routes
    // to the rust-embed bundle. Desktop continues at `/` inside the
    // Tauri webview.
    base: "/",
    build: isMobile
      ? {
          outDir: "../dist/mobile",
          emptyOutDir: true,
        }
      : undefined,

    clearScreen: false,
    server: {
      port: 1420,
      strictPort: true,
      host: host || false,
      hmr: host
        ? {
            protocol: "ws",
            host,
            port: 1421,
          }
        : undefined,
      watch: {
        ignored: ["**/src-tauri/**"],
      },
    },
  };
});
