import { StrictMode } from "react"
import { createRoot } from "react-dom/client"
import { BrowserRouter } from "react-router"
import App from "./App"
import "./index.css"
import { loadRuntimeConfig } from "./lib/api"
import { applyStoredTheme } from "./lib/theme"

applyStoredTheme()

const root = createRoot(document.getElementById("root")!)

try {
  await loadRuntimeConfig()
  root.render(<StrictMode><BrowserRouter><App /></BrowserRouter></StrictMode>)
} catch (reason) {
  const message = reason instanceof Error ? reason.message : "Unable to start Treer"
  root.render(<main className="grid min-h-dvh place-items-center bg-sidebar p-6 text-sm text-red-700">{message}</main>)
}
