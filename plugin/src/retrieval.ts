import { type Archive, archiveFetch, boundedText } from "./archive"
import { ambiguous, readerLink, validSession } from "./reference"
import { type Tool, toolError } from "./tools"

type Route = "find" | "search" | "read"
const maximumBytes = 24 * 1024
const string = { type: "string", minLength: 1 }
const session = { ...string, maxLength: 2048, description: "Session reference or Spool link, such as `ses_…`, `work-m5/ses_…` or `https://…/sessions/ses_…#12`. A link's `#12` is message 12." }
const revision = { ...string, maxLength: 19, pattern: "^[0-9]+$" }
const query = { ...string, maxLength: 512, description: "Case-insensitive words, all of which must occur in one title or message; at most 16." }
const boolean = { type: "boolean" }
const integer = (maximum?: number, defaultValue?: number) => ({ type: "integer", minimum: 1, ...(maximum ? { maximum } : {}), ...(defaultValue ? { default: defaultValue } : {}) })
const definitions = {
  find: {
    name: "find_sessions",
    description: "Find archived sessions across all machines and repositories. Returns each session's reference, link, title, `status` (`running`, `idle` or `offline`, for its machine) and a matching snippet.",
    properties: { query, repository: { ...string, description: "Limit to one repository, as a normalized host/path such as `github.com/owner/repo`." }, include_tools: boolean, limit: integer(50, 10) },
    required: [],
  },
  search: {
    name: "search_messages",
    description: "Find messages in one session that match `query`. Returns message indexes, links and excerpts. Searches user and assistant text; `include_tools` adds tool calls and output. Reasoning and attachments are never searched.",
    properties: { session, query, revision, include_tools: boolean, limit: integer(100, 20) },
    required: ["session", "query"],
  },
  read: {
    name: "read_messages",
    description: "Read messages `start` through `end` (1-based, inclusive) from one session; `start` defaults to a link's message, or 1, and `end` to `start`. Returns their full text and links, including tool calls and output, with a `next_cursor` while more of the range remains.",
    properties: { session, revision, start: integer(100_000, 1), end: integer(100_000) },
    required: ["session"],
  },
}

function validRevision(value: unknown): value is string {
  return typeof value === "string" && /^[0-9]{1,19}$/.test(value) && BigInt(value) > 0 && BigInt(value) <= 9223372036854775807n
}

export function validInput(route: Route, value: unknown): value is Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false
  const input = value as Record<string, unknown>
  const keys = Object.keys(input)
  const text = (item: unknown, max: number) => typeof item === "string" && !!item && !item.includes("\0") && Buffer.byteLength(item) <= max
  if ("cursor" in input) return keys.length === 1 && text(input.cursor, 8192)
  const definition = definitions[route]
  if (keys.some((key) => !Object.hasOwn(definition.properties, key)) || definition.required.some((key) => !Object.hasOwn(input, key))) return false
  for (const [key, item] of Object.entries(input)) {
    if (key === "query") {
      if (typeof item !== "string" || item.includes("\0") || Buffer.byteLength(item) > 512 || item.trim().split(/\s+/u).filter(Boolean).length > 16 || !item.trim()) return false
    } else if (key === "include_tools") {
      if (typeof item !== "boolean") return false
    } else if (["limit", "start", "end"].includes(key)) {
      if (typeof item !== "number" || !Number.isSafeInteger(item) || item < 1 || item > (key === "limit" ? route === "find" ? 50 : 100 : 100_000)) return false
    } else if (key === "revision") {
      if (!validRevision(item)) return false
    } else if (key === "session") {
      if (!validSession(item)) return false
    } else if (!text(item, 1024)) return false
  }
  return !(route === "read" && typeof input.end === "number" && input.end < (typeof input.start === "number" ? input.start : 1))
}

type Json = Record<string, any>

/** A reader link given as `session` becomes its reference; for reads, its `#index` is the default message. */
function fromLink(archive: Archive, route: Route, input: unknown): unknown {
  const value = input as Json
  const link = readerLink(archive, value?.session)
  if (!link) return input
  return { ...value, session: link.session, ...(route === "read" && link.index && !("start" in value) && !("end" in value) ? { start: link.index } : {}) }
}

export async function requestReader(archive: Archive, route: Route, given: unknown, signal?: AbortSignal): Promise<string> {
  const input = fromLink(archive, route, given)
  if (!validInput(route, input)) return toolError("malformed_request")
  try {
    const response = await archiveFetch(archive, `/v1/agent/${route}`, {
      method: "POST", body: JSON.stringify(input), timeout: 5_000, signal,
      // The reader URL lets results link to the reader within the server's page budget.
      headers: { "content-type": "application/json", accept: "application/json", "spool-reader-url": archive.url },
    })
    const raw = await boundedText(response, maximumBytes)
    if (raw === undefined) return toolError("response_too_large")
    const parsed = JSON.parse(raw)
    const code = parsed?.error?.code
    if (code === "ambiguous_session") return ambiguous(response.status, parsed.error, "reader_unavailable")
    if (!response.ok) {
      if (code === "session_changed" && validRevision(parsed?.error?.current_revision)) return toolError(code, { current_revision: parsed.error.current_revision })
      return toolError(["session_changed", "not_indexed", "malformed_request", "not_found", "unauthorized"].includes(code) ? code : "reader_unavailable")
    }
    return raw
  } catch { return toolError("reader_unavailable") }
}

export function retrievalTools(archive: Archive): Tool[] {
  return (["find", "search", "read"] as const).map((route) => ({
    name: definitions[route].name,
    description: definitions[route].description,
    input: {
      type: "object",
      additionalProperties: false,
      properties: { ...definitions[route].properties, cursor: { ...string, maxLength: 8192, description: "`next_cursor` from a previous result, given without other arguments." } },
    },
    run: (input, context) => requestReader(archive, route, input, context.signal),
  }))
}
