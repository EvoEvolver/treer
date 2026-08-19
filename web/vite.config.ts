import path from "node:path"
import { fileURLToPath } from "node:url"
import { defineConfig } from "vite"
import react from "@vitejs/plugin-react"
import { viteSingleFile } from "vite-plugin-singlefile"

const root = path.dirname(fileURLToPath(import.meta.url))

export default defineConfig({
  plugins: [react(), viteSingleFile()],
  resolve: { alias: { "@": path.resolve(root, "./src") } },
  server: {
    proxy: {
      "/api": { target: "http://127.0.0.1:8787", ws: true },
    },
  },
  build: { target: "es2022", assetsInlineLimit: 100_000_000, cssCodeSplit: false },
})
