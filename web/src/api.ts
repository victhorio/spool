export type Document = Record<string, unknown>

export function object(value: unknown): value is Document {
  return value !== null && typeof value === "object" && !Array.isArray(value)
}

export interface Session {
  source_id: string
  source_label: string
  session_id: string
  /** The session's reference, as the agent tools use it: its ID, qualified only when needed. */
  session: string
  revision: string
  info: Document
  received_at: string
  /** Whether its machine is online, and if so whether the session is running there. */
  status: "running" | "idle" | "offline"
}

export interface SessionPage {
  sessions: Session[]
  sources: { source_id: string; source_label: string }[]
  next_cursor: string | null
}

export interface Transcript extends Session {
  /** The latest messages loaded so far, starting at message `first_index`. */
  messages: (Document & { id: string })[]
  first_index: number
  earlier_cursor: string | null
}

export class ArchiveError extends Error {
  /** Qualified references when a bare session ID is archived by several sources. */
  readonly candidates?: string[]

  constructor(
    readonly status: number,
    error?: { code?: unknown; candidates?: unknown },
  ) {
    const ambiguous = error?.code === "ambiguous_session" && Array.isArray(error.candidates)
    super(
      ambiguous
        ? "More than one source archived a session with this ID. Choose one:"
        : status === 409
          ? "This session changed. Reload it to read the latest version."
          : status === 404
            ? "This session is no longer in the archive."
            : "Couldn’t load the archive. Please try again.",
    )
    if (ambiguous) this.candidates = (error.candidates as unknown[]).filter((item) => typeof item === "string")
  }
}

export interface Artifact {
  id: string
  title: string
  type: string
  revision: string
  size: number
  source: string
  source_id: string
  source_label: string
  session: string | null
  session_id: string | null
  created_at: string
  updated_at: string
}

export interface ArtifactPage {
  artifacts: Artifact[]
  next_cursor: string | null
}

async function get(path: string, signal?: AbortSignal): Promise<Response> {
  const response = await fetch(path, { signal, cache: "no-store", redirect: "manual" })
  // An expired Cloudflare Access session redirects to its login; only a navigation can follow it.
  if (response.type === "opaqueredirect") {
    window.location.reload()
    return new Promise<Response>(() => {})
  }
  if (!response.ok) {
    const body = response.status === 409 ? await response.json().catch(() => undefined) : undefined
    throw new ArchiveError(response.status, body?.error)
  }
  return response
}

/** Sends a message to a session on a machine that is online, as a prompt from the owner. */
export async function sendMessage(session: string, text: string): Promise<void> {
  const response = await fetch("/api/messages", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ session, text }),
    cache: "no-store",
    redirect: "manual",
  })
  if (response.type === "opaqueredirect") {
    window.location.reload()
    return new Promise<void>(() => {})
  }
  if (!response.ok) {
    const body = await response.json().catch(() => undefined)
    throw new Error(
      body?.error?.code === "session_offline"
        ? "This session’s machine is offline."
        : "Couldn’t send the message. Please try again.",
    )
  }
}

export async function read<T>(path: string, signal?: AbortSignal): Promise<T> {
  return (await get(path, signal)).json() as Promise<T>
}

export async function readText(path: string, signal?: AbortSignal): Promise<string> {
  return (await get(path, signal)).text()
}

const encodeReference = (reference: string) =>
  reference.split("/").map(encodeURIComponent).join("/")

/** The reader link to a session, or to its one-based message `index`. */
export function sessionHref(reference: string, index?: number): string {
  return `/sessions/${encodeReference(reference)}${index ? `#${index}` : ""}`
}

/** The session reference in the current reader URL, if any. */
export function linkedSession(): string | null {
  const encoded = /^\/sessions\/(.+)$/.exec(window.location.pathname)?.[1]
  try {
    return encoded ? encoded.split("/").map(decodeURIComponent).join("/") : null
  } catch {
    return null
  }
}

export function sessionPath(reference: string): string {
  return `/api/sessions/${encodeReference(reference)}`
}

export function sessionKey(session: Pick<Session, "source_id" | "session_id">): string {
  return `${session.source_id}/${session.session_id}`
}
