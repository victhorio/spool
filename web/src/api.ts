export type Document = Record<string, unknown>

export function object(value: unknown): value is Document {
  return value !== null && typeof value === "object" && !Array.isArray(value)
}

export interface Session {
  source_id: string
  source_label: string
  session_id: string
  revision: string
  info: Document
  received_at: string
}

export interface SessionPage {
  sessions: Session[]
  sources: { source_id: string; source_label: string }[]
  next_cursor: string | null
}

export interface Transcript extends Session {
  messages: (Document & { id: string })[]
  next_cursor: string | null
}

export class ArchiveError extends Error {
  constructor(readonly status: number) {
    super(
      status === 409
        ? "This session changed. Reload it to read the latest version."
        : status === 404
          ? "This session is no longer in the archive."
          : "Couldn’t load the archive. Please try again.",
    )
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
  if (!response.ok) throw new ArchiveError(response.status)
  return response
}

export async function read<T>(path: string, signal?: AbortSignal): Promise<T> {
  return (await get(path, signal)).json() as Promise<T>
}

export async function readText(path: string, signal?: AbortSignal): Promise<string> {
  return (await get(path, signal)).text()
}

export function sessionPath(session: Pick<Session, "source_id" | "session_id">): string {
  return `/api/sources/${encodeURIComponent(session.source_id)}/sessions/${encodeURIComponent(session.session_id)}`
}

export function sessionKey(session: Pick<Session, "source_id" | "session_id">): string {
  return `${session.source_id}/${session.session_id}`
}
