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

/** A failed reader API request, with the error code the archive returned. */
export class ArchiveError extends Error {
  constructor(
    readonly status: number,
    readonly code?: string,
    /** Qualified references when a bare session ID is archived by several sources. */
    readonly candidates?: string[],
  ) {
    super(code ?? `HTTP ${status}`)
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

/**
 * Calls the reader's own API. An expired Cloudflare Access session redirects
 * to its login, which only a navigation can follow, so the page reloads.
 */
export async function request(path: string, init: RequestInit = {}): Promise<Response> {
  const response = await fetch(path, { ...init, cache: "no-store", redirect: "manual" })
  if (response.type === "opaqueredirect") {
    window.location.reload()
    return new Promise<Response>(() => {})
  }
  if (!response.ok) {
    const error = (await response.json().catch(() => undefined))?.error
    const code = typeof error?.code === "string" ? error.code : undefined
    const candidates =
      code === "ambiguous_session" && Array.isArray(error.candidates)
        ? (error.candidates as unknown[]).filter((item) => typeof item === "string")
        : undefined
    throw new ArchiveError(response.status, code, candidates)
  }
  return response
}

/** Sends a message to a session on a machine that is online, as a prompt from the owner. */
export async function sendMessage(session: string, text: string): Promise<void> {
  await request("/api/messages", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ session, text }),
  })
}

export async function read<T>(path: string, signal?: AbortSignal): Promise<T> {
  return (await request(path, { signal })).json() as Promise<T>
}

export async function readText(path: string, signal?: AbortSignal): Promise<string> {
  return (await request(path, { signal })).text()
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
