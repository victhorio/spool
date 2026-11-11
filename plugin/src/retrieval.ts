import type { Plugin } from "@opencode/plugin"
import { type Config, validSourceAlias } from "./config"
import { bundledSkill } from "./skills"

type Route = "find" | "search" | "read"
const maximumBytes = 24 * 1024
const errorResult = (code: string, current_revision?: string) => JSON.stringify({ error: { code, ...(current_revision ? { current_revision } : {}) } })
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

export function validSession(value: unknown, qualifiedOnly = false): value is string {
  if (typeof value !== "string" || !value || value.includes("\0")) return false
  const slash = value.indexOf("/")
  if (slash < 0) return !qualifiedOnly && Buffer.byteLength(value) <= 512
  const source = value.slice(0, slash)
  const id = value.slice(slash + 1)
  return (validSourceAlias(source) || /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.exec(source)?.[0] === source) &&
    !!id && Buffer.byteLength(id) <= 512
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

type Archive = Pick<Config, "url" | "token">
type Json = Record<string, any>

/** The session reference and message index in a reader link, if `value` is one. */
export function readerLink(archive: Archive, value: unknown): { session: string; index?: number } | undefined {
  if (typeof value !== "string" || !value.startsWith(`${archive.url}/sessions/`)) return
  try {
    const url = new URL(value)
    const index = /^#([1-9][0-9]*)$/.exec(url.hash)?.[1]
    return { session: url.pathname.slice("/sessions/".length).split("/").map(decodeURIComponent).join("/"), ...(index ? { index: Number(index) } : {}) }
  } catch { return }
}

/** A reader link given as `session` becomes its reference; for reads, its `#index` is the default message. */
function fromLink(archive: Archive, route: Route, input: unknown): unknown {
  const value = input as Json
  const link = readerLink(archive, value?.session)
  if (!link) return input
  return { ...value, session: link.session, ...(route === "read" && link.index && !("start" in value) && !("end" in value) ? { start: link.index } : {}) }
}

export async function requestReader(archive: Archive, route: Route, given: unknown, signal?: AbortSignal): Promise<string> {
  const input = fromLink(archive, route, given)
  if (!validInput(route, input)) return errorResult("malformed_request")
  try {
    const response = await fetch(`${archive.url}/v1/agent/${route}`, {
      method: "POST", body: JSON.stringify(input), redirect: "error", credentials: "omit",
      // The reader URL lets results link to the reader within the server's page budget.
      headers: { "content-type": "application/json", accept: "application/json", authorization: `Bearer ${archive.token}`, "spool-reader-url": archive.url },
      signal: AbortSignal.any([AbortSignal.timeout(5_000), ...(signal ? [signal] : [])]),
    })
    if (!response.body) return errorResult("reader_unavailable")
    const reader = response.body.getReader()
    const chunks: Uint8Array[] = []
    let length = 0
    try {
      for (;;) {
        const { done, value } = await reader.read()
        if (done) break
        length += value.byteLength
        if (length > maximumBytes) return errorResult("response_too_large")
        chunks.push(value)
      }
    } finally { await reader.cancel().catch(() => {}) }
    const raw = new TextDecoder("utf-8", { fatal: true }).decode(Buffer.concat(chunks))
    const parsed = JSON.parse(raw)
    const code = parsed?.error?.code
    if (code === "ambiguous_session") {
      const { candidates, more } = parsed.error
      if (response.status !== 409 || !Array.isArray(candidates) || candidates.length < 1 || candidates.length > 10 ||
          !candidates.every((candidate: unknown) => validSession(candidate, true)) || typeof more !== "boolean") return errorResult("reader_unavailable")
      return JSON.stringify({ error: { code, candidates, more } })
    }
    if (!response.ok) {
      if (code === "session_changed" && validRevision(parsed?.error?.current_revision)) return errorResult(code, parsed.error.current_revision)
      return errorResult(["session_changed", "not_indexed", "malformed_request", "not_found", "unauthorized"].includes(code) ? code : "reader_unavailable")
    }
    return raw
  } catch { return errorResult("reader_unavailable") }
}

export async function registerRetrieval(ctx: Plugin.Context, archive: Archive): Promise<void> {
  const skill = await bundledSkill("searching-sessions")
  await ctx.tool.transform((tools) => {
    tools.namespace({ name: "spool", description: "Search coding-agent sessions, message sessions on machines that are online, and publish documents for the user to read." })
    for (const route of ["find", "search", "read"] as const) {
      const definition = definitions[route]
      tools.add({
        name: definition.name,
        options: { namespace: "spool", codemode: false },
        description: definition.description,
        input: {
          type: "object",
          additionalProperties: false,
          properties: { ...definition.properties, cursor: { ...string, maxLength: 8192, description: "`next_cursor` from a previous result, given without other arguments." } },
        },
        execute: async (input, context) => ({ content: await requestReader(archive, route, input, context.signal) }),
      })
    }
  })
  await ctx.skill.transform((skills) => { skills.add(skill) })
}
