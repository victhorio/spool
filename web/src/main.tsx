import { StrictMode } from "react"
import { createRoot } from "react-dom/client"
import { App } from "./App"
import { Artifacts } from "./Artifacts"
import { Device } from "./Device"
import { sessionHref } from "./api"
import "./style.css"

// Earlier session links were `/#source={uuid}&session={id}`.
const legacy = new URLSearchParams(window.location.hash.slice(1))
const legacySource = legacy.get("source"),
  legacySession = legacy.get("session")
if (window.location.pathname === "/" && legacySource && legacySession)
  window.history.replaceState(null, "", sessionHref(`${legacySource}/${legacySession}`))

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    {window.location.pathname === "/device" ? (
      <Device />
    ) : window.location.pathname.startsWith("/artifacts") ? (
      <Artifacts />
    ) : (
      <App />
    )}
  </StrictMode>,
)
