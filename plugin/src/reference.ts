import type { Archive } from "./archive"
import { validSourceAlias } from "./config"
import { toolError } from "./tools"

/** A session reference: a native ID, or one qualified by a source alias or UUID; `qualifiedOnly` requires the qualifier. */
export function validSession(value: unknown, qualifiedOnly = false): value is string {
  if (typeof value !== "string" || !value || value.includes("\0")) return false
  const slash = value.indexOf("/")
  if (slash < 0) return !qualifiedOnly && Buffer.byteLength(value) <= 512
  const source = value.slice(0, slash)
  const id = value.slice(slash + 1)
  return (validSourceAlias(source) || /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.exec(source)?.[0] === source) &&
    !!id && Buffer.byteLength(id) <= 512
}

/** The session reference and message index in a reader link, if `value` is one. */
export function readerLink(archive: Archive, value: unknown): { session: string; index?: number } | undefined {
  if (typeof value !== "string" || !value.startsWith(`${archive.url}/sessions/`)) return
  try {
    const url = new URL(value)
    const index = /^#([1-9][0-9]*)$/.exec(url.hash)?.[1]
    return { session: url.pathname.slice("/sessions/".length).split("/").map(decodeURIComponent).join("/"), ...(index ? { index: Number(index) } : {}) }
  } catch { return }
}

/** The archive's ambiguous-session error, passed on only with the bounded qualified references it promises. */
export function ambiguous(status: number, error: Record<string, unknown>, unavailable: string): string {
  const { candidates, more } = error
  if (status !== 409 || !Array.isArray(candidates) || candidates.length < 1 || candidates.length > 10 ||
      !candidates.every((candidate: unknown) => validSession(candidate, true)) || typeof more !== "boolean") return toolError(unavailable)
  return toolError("ambiguous_session", { candidates, more })
}
