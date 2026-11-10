import { useState, type FormEvent } from "react"
import { SpoolMark } from "./App"

interface Login {
  user_code: string
  alias: string
  created_at: string
  expires_at: string
  status: "pending" | "approved" | "denied" | "expired" | "completed"
}

type State =
  | { kind: "entry"; message?: string }
  | { kind: "loading" }
  | { kind: "login"; login: Login; busy?: boolean; message?: string }

const outcomes: Record<Exclude<Login["status"], "pending">, string> = {
  approved: "Approved. The machine finishes logging in within a few seconds. If you approved it by mistake, deny it now.",
  completed: "This machine is logged in. You can close this page.",
  denied: "Denied. That machine got no access.",
  expired: "This code expired. Run the installer on the machine again for a new one.",
}

async function call(path: string, method: "GET" | "POST"): Promise<Login | number> {
  const response = await fetch(path, { method, cache: "no-store", redirect: "manual" })
  // An expired Cloudflare Access session redirects to its login; only a navigation can follow it.
  if (response.type === "opaqueredirect") {
    window.location.reload()
    return new Promise(() => {})
  }
  return response.ok ? ((await response.json()) as Login) : response.status
}

function problem(status: number): string {
  return status === 404 || status === 400
    ? "No login uses this code. Check it against the one in the machine’s terminal."
    : status === 409
      ? "This code was already decided or has expired."
      : "Couldn’t reach Spool. Please try again."
}

function age(created: string): string {
  const minutes = Math.round((Date.now() - Date.parse(created)) / 60_000)
  return minutes < 1 ? "less than a minute ago" : minutes === 1 ? "1 minute ago" : `${minutes} minutes ago`
}

// Codes are typed, never taken from a link: a link from someone else must not
// look like a request from your own machine.
export function Device() {
  const [code, setCode] = useState("")
  const [state, setState] = useState<State>({ kind: "entry" })

  async function look(value: string) {
    setState({ kind: "loading" })
    const result = await call(`/api/device/${encodeURIComponent(value.trim())}`, "GET").catch(() => 0)
    setState(typeof result === "number" ? { kind: "entry", message: problem(result) } : { kind: "login", login: result })
  }

  async function decide(login: Login, decision: "approve" | "deny") {
    setState({ kind: "login", login, busy: true })
    const result = await call(`/api/device/${encodeURIComponent(login.user_code)}/${decision}`, "POST").catch(() => 0)
    if (typeof result !== "number") return setState({ kind: "login", login: result })
    // The code may have expired or been decided elsewhere; show its current state.
    const current = await call(`/api/device/${encodeURIComponent(login.user_code)}`, "GET").catch(() => 0)
    setState(typeof current === "number"
      ? { kind: "login", login, message: problem(result) }
      : { kind: "login", login: current, message: current.status === "pending" ? problem(result) : undefined })
  }

  // Looks up a code from the link once.
  function submit(event: FormEvent) {
    event.preventDefault()
    if (code.trim()) void look(code)
  }

  return (
    <main className="device">
      <a className="brand" href="/">
        <SpoolMark /> Spool
      </a>
      <section className="device-panel" aria-live="polite" aria-busy={state.kind === "loading"}>
        <h1>Log in a machine</h1>
        {state.kind === "loading" && <p role="status">Looking up the code…</p>}
        {state.kind === "entry" && (
          <form onSubmit={submit}>
            <p>Enter the code shown in the machine’s terminal.</p>
            {state.message && <p className="device-problem" role="alert">{state.message}</p>}
            <div className="device-entry">
              <input
                aria-label="Code"
                value={code}
                onChange={(event) => setCode(event.target.value)}
                placeholder="BCDF-GHJK"
                autoComplete="off"
                autoCapitalize="characters"
                spellCheck={false}
                autoFocus
              />
              <button className="subtle-button" type="submit">Continue</button>
            </div>
          </form>
        )}
        {state.kind === "login" && (
          <>
            <p>
              A machine asked {age(state.login.created_at)} to join Spool as <strong>{state.login.alias}</strong>.
              Approve it only if you just ran the installer on a machine you control and its terminal shows this code.
            </p>
            <p className="device-code">{state.login.user_code}</p>
            {state.message && <p className="device-problem" role="alert">{state.message}</p>}
            {state.login.status === "pending" ? (
              <>
                <p className="device-note">
                  It will upload this machine’s sessions and its agents can read the whole archive.
                </p>
                <div className="device-actions">
                  <button className="device-approve" disabled={state.busy} onClick={() => void decide(state.login, "approve")}>
                    Approve
                  </button>
                  <button className="subtle-button" disabled={state.busy} onClick={() => void decide(state.login, "deny")}>
                    Deny
                  </button>
                </div>
              </>
            ) : (
              <>
                <p className="device-outcome" role="status">{outcomes[state.login.status]}</p>
                {state.login.status === "approved" && (
                  <button className="subtle-button" disabled={state.busy} onClick={() => void decide(state.login, "deny")}>
                    Deny
                  </button>
                )}
              </>
            )}
          </>
        )}
      </section>
    </main>
  )
}
