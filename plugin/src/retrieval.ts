import { readFile } from "node:fs/promises"
import { fileURLToPath } from "node:url"
import { Skill, type Plugin } from "@opencode/plugin"
import { type Config, validSourceAlias } from "./config"

type Route = "find" | "search" | "read"
const maximumBytes = 24 * 1024
const errorResult = (code: string, current_revision?: string) => JSON.stringify({ error: { code, ...(current_revision ? { current_revision } : {}) } })
const string = { type: "string", minLength: 1 }
const session = { ...string, maxLength: 549, description: "Native OpenCode ID (when globally unique), alias/ID, or legacy source-UUID/ID. On ambiguous_session, select a returned qualified candidate." }
const revision = { ...string, maxLength: 19, pattern: "^[0-9]+$" }
const query = { ...string, maxLength: 512, description: "Case-insensitive literal whitespace terms, AND within one title/message; at most 512 UTF-8 bytes and 16 terms. No query DSL." }
const boolean = { type: "boolean" }
const integer = (maximum?: number, defaultValue?: number) => ({ type: "integer", minimum: 1, ...(maximum ? { maximum } : {}), ...(defaultValue ? { default: defaultValue } : {}) })
const definitions = {
  find: {
    name: "find_sessions",
    description: "Find archived sessions across sources. Query and exact repository key are optional; no implicit current-repository filter. Returns source-qualified session references, one snippet each, revisions, capture times and archive coverage.",
    properties: { query, repository: { ...string, description: "Exact normalized host/path key (not a folder name or URL)." }, include_tools: boolean, limit: integer(50, 10) },
    required: [],
  },
  search: {
    name: "search_messages",
    description: "Find indexed messages in one archived session; initial arguments require session and query. Returns message indexes, excerpts and read cursors. Ordinary user/assistant text only unless include_tools is true; reasoning and binary are not searched.",
    properties: { session, query, revision, include_tools: boolean, limit: integer(100, 20) },
    required: ["session", "query"],
  },
  read: {
    name: "read_messages",
    description: "Read a 1-based inclusive message range; initial arguments require session. Start defaults to 1, end to start. Returns full textual evidence in bounded byte chunks, message indexes/IDs/roles and a continuation cursor.",
    properties: { session, revision, start: integer(100_000, 1), end: integer(100_000) },
    required: ["session"],
  },
}

function validRevision(value: unknown): value is string {
  return typeof value === "string" && /^[0-9]{1,19}$/.test(value) && BigInt(value) > 0 && BigInt(value) <= 9223372036854775807n
}

function validSession(value: unknown, qualifiedOnly = false): value is string {
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

export async function requestReader(archive: Archive, route: Route, input: unknown, signal?: AbortSignal): Promise<string> {
  if (!validInput(route, input)) return errorResult("malformed_request")
  try {
    const response = await fetch(`${archive.url}/v1/agent/${route}`, {
      method: "POST", body: JSON.stringify(input), redirect: "error", credentials: "omit",
      headers: { "content-type": "application/json", accept: "application/json", authorization: `Bearer ${archive.token}` },
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
  const location = new URL("../skills/searching-sessions/SKILL.md", import.meta.url)
  const source = await readFile(location, "utf8")
  const frontmatter = /^---\nname: ([^\n]+)\ndescription: ([^\n]+)\n---\n([\s\S]*)$/.exec(source)
  if (!frontmatter) throw new Error("Invalid bundled searching-sessions skill")
  const skill = {
    id: Skill.ID.make("spool.searching-sessions"), name: Skill.Name.make(frontmatter[1]!),
    description: frontmatter[2]!, path: fileURLToPath(location) as Skill.Info["path"],
    content: frontmatter[3]!.trim(),
  }
  await ctx.tool.transform((tools) => {
    tools.namespace({ name: "spool", description: "Read-only archived coding-agent session retrieval." })
    for (const route of ["find", "search", "read"] as const) {
      const definition = definitions[route]
      tools.add({
        name: definition.name,
        options: { namespace: "spool", codemode: false },
        description: `${definition.description} Supply initial arguments or a cursor alone, never both.`,
        input: {
          type: "object",
          additionalProperties: false,
          properties: { ...definition.properties, cursor: { ...string, maxLength: 8192 } },
        },
        execute: async (input, context) => ({ content: await requestReader(archive, route, input, context.signal) }),
      })
    }
  })
  await ctx.skill.transform((skills) => { skills.add(skill) })
}
