import { StrictMode } from "react"
import { createRoot } from "react-dom/client"
import { App } from "./App"
import { Device } from "./Device"
import "./style.css"

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    {window.location.pathname === "/device" ? <Device /> : <App />}
  </StrictMode>,
)
