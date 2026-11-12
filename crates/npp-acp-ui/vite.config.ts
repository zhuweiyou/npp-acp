import { defineConfig } from "vite-plus"
import react from "@vitejs/plugin-react"

export default defineConfig({
  base: "./",
  plugins: [react()],
  fmt: {
    semi: false,
    singleQuote: false,
    tabWidth: 2,
    printWidth: 120,
    ignorePatterns: ["dist/**", "node_modules/**"],
  },
  server: {
    port: 5173,
    strictPort: true,
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
})
