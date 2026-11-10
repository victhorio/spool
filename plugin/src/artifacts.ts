import { open } from "node:fs/promises"
import { homedir } from "node:os"
import path from "node:path"
import type { Plugin } from "@opencode/plugin"
import type { Config } from "./config"
import { bundledSkill } from "./skills"

type Archive = Pick<Config, "url" | "token">
type Input = Record<string, unknown>

const maximumSize = 32 * 1024 * 1024
const maximumResponse = 256 * 1024
const types = {
  markdown: "text/markdown", html: "text/html", text: "text/plain", svg: "image/svg+xml",
  png: "image/png", jpeg: "image/jpeg", gif: "image/gif", webp: "image/webp",
} as const
type Type = keyof typeof types
const textTypes: readonly Type[] = ["markdown", "html", "text", "svg"]
const extensions: Record<string, Type> = {
  ".md": "markdown", ".markdown": "markdown", ".html": "html", ".htm": "html", ".txt": "text",
  ".svg": "svg", ".png": "png", ".jpg": "jpeg", ".jpeg": "jpeg", ".gif": "gif", ".webp": "webp",
}
const passed = ["malformed_request", "not_found", "revision_conflict", "payload_too_large", "artifact_quota_exceeded",
  "unsupported_content_type", "invalid_content", "not_text", "edit_not_found", "edit_not_unique", "unauthorized",
  // Retry shortly: another large upload is in progress, or an ID collided.
  "busy", "try_again"]

const string = { type: "string", minLength: 1 }
const id = { ...string, pattern: "^art_[a-z0-9]{12}$", description: "Artifact ID (art_…)." }
const revision = { ...string, pattern: "^[0-9]{1,19}$", description: "The revision the change is based on, from the last result for this artifact." }
const title = { ...string, maxLength: 256, description: "Short human-readable title." }
const type = { type: "string", enum: Object.keys(types), description: "Content type. Required with content; with path, inferred from the extension (.md, .html, .txt, .svg, .png, .jpg, .gif, .webp) unless given." }
const content = { type: "string", description: "Inline content for markdown, html, text or svg. Give content or path, not both." }
const file = { ...string, description: "Absolute path (or ~/…) of a local file to publish instead of inline content." }

export const definitions = {
  create: {
    name: "create_artifact",
    description: "Publish a document (Markdown, self-contained HTML, text or an image) to Spool for the user to read, and return its link. Records the current session. Give the user the returned url.",
    properties: { title, type, content, path: file },
    required: ["title"],
  },
  update: {
    name: "update_artifact",
    description: "Replace an artifact's content and/or title, keeping its ID and link. Fails with revision_conflict if it changed since the given revision.",
    properties: { id, revision, title, type, content, path: file },
    required: ["id", "revision"],
  },
  edit: {
    name: "edit_artifact",
    description: "Apply exact find-and-replace edits to a text artifact on the server, in order, all or nothing. Each old string must occur exactly once at that point; include enough context to make it unique.",
    properties: {
      id, revision,
      edits: { type: "array", minItems: 1, maxItems: 100, items: { type: "object", additionalProperties: false, required: ["old", "new"], properties: { old: string, new: { type: "string" } } } },
    },
    required: ["id", "revision", "edits"],
  },
  find: {
    name: "find_artifacts",
    description: "List published artifacts, most recently updated first. Query is optional: case-insensitive literal whitespace terms, all of which must occur in the title or text content.",
    properties: { query: { ...string, maxLength: 512 }, limit: { type: "integer", minimum: 1, maximum: 50, default: 20 }, cursor: { ...string, maxLength: 2048, description: "next_cursor from a previous result, alone." } },
    required: [],
  },
  read: {
    name: "read_artifact",
    description: "Read an artifact's metadata and, for text types, up to 8 KiB of content from a character offset. Follow next_offset for the rest; null means the end.",
    properties: { id, offset: { type: "integer", minimum: 0, maximum: maximumSize } },
    required: ["id"],
  },
} as const
export type Operation = keyof typeof definitions

const errorResult = (code: string, extra: Input = {}) => JSON.stringify({ error: { code, ...extra } })

function check(operation: Operation, input: unknown): Input | undefined {
  if (!input || typeof input !== "object" || Array.isArray(input)) return
  const value = input as Input
  const definition = definitions[operation]
  const keys = Object.keys(value)
  if (keys.some((key) => !Object.hasOwn(definition.properties, key)) || definition.required.some((key) => !Object.hasOwn(value, key))) return
  const text = (item: unknown) => typeof item === "string" && item.length > 0
  if ("id" in value && !(typeof value.id === "string" && /^art_[a-z0-9]{12}$/.test(value.id))) return
  if ("revision" in value && !(typeof value.revision === "string" && /^[0-9]{1,19}$/.test(value.revision))) return
  if ("title" in value && !text(value.title)) return
  if ("type" in value && !(typeof value.type === "string" && Object.hasOwn(types, value.type))) return
  if ("content" in value && typeof value.content !== "string") return
  if ("path" in value && !text(value.path)) return
  if ("content" in value && "path" in value) return
  if ("type" in value && !("content" in value || "path" in value)) return
  if (operation === "update" && !("title" in value || "content" in value || "path" in value)) return
  if (operation === "edit" && !(Array.isArray(value.edits) && value.edits.length >= 1 && value.edits.length <= 100 &&
      value.edits.every((edit) => edit && typeof edit === "object" && Object.keys(edit).sort().join() === "new,old" && text(edit.old) && typeof edit.new === "string"))) return
  if (operation === "find") {
    if ("cursor" in value && keys.length !== 1) return
    if ("cursor" in value && !text(value.cursor)) return
    if ("query" in value && !(text(value.query) && String(value.query).trim())) return
    if ("limit" in value && !(Number.isSafeInteger(value.limit) && Number(value.limit) >= 1 && Number(value.limit) <= 50)) return
  }
  if ("offset" in value && !(Number.isSafeInteger(value.offset) && Number(value.offset) >= 0 && Number(value.offset) <= maximumSize)) return
  return value
}

class Refusal extends Error {
  constructor(readonly code: string, readonly detail: Input = {}) { super(code) }
}

/** Content and media type from inline content or a local file. */
async function body(input: Input): Promise<{ bytes: Uint8Array<ArrayBuffer> | string; mediaType: string }> {
  if (typeof input.content === "string") {
    const kind = input.type as Type | undefined
    if (!kind || !textTypes.includes(kind)) throw new Refusal("malformed_request", { reason: "inline content needs type markdown, html, text or svg" })
    return { bytes: input.content, mediaType: types[kind] }
  }
  const raw = input.path as string
  const location = raw.startsWith("~/") ? path.join(homedir(), raw.slice(2)) : raw
  if (!path.isAbsolute(location)) throw new Refusal("malformed_request", { reason: "path must be absolute or start with ~/" })
  const kind = (input.type as Type | undefined) ?? extensions[path.extname(location).toLowerCase()]
  if (!kind) throw new Refusal("malformed_request", { reason: "unknown file extension; pass type" })
  const handle = await open(location, "r").catch(() => { throw new Refusal("file_unreadable") })
  try {
    const info = await handle.stat()
    if (!info.isFile()) throw new Refusal("file_unreadable")
    if (info.size > maximumSize) throw new Refusal("payload_too_large")
    // readFile returns a Buffer over its own ArrayBuffer; no copy is needed.
    return { bytes: await handle.readFile() as Uint8Array<ArrayBuffer>, mediaType: types[kind] }
  } finally { await handle.close() }
}

function present(archive: Archive, artifact: Input): Input {
  const { id, title, type, revision, size, source, session, created_at, updated_at } = artifact
  return { id, title, type, revision, size, source, session, created_at, updated_at, url: `${archive.url}/artifacts/${id}` }
}

async function call(archive: Archive, route: string, init: RequestInit, timeout: number, signal?: AbortSignal): Promise<Input> {
  let response: Response
  try {
    response = await fetch(`${archive.url}/v1/artifacts${route}`, {
      ...init, redirect: "error", credentials: "omit",
      headers: { accept: "application/json", authorization: `Bearer ${archive.token}`, ...init.headers },
      signal: AbortSignal.any([AbortSignal.timeout(timeout), ...(signal ? [signal] : [])]),
    })
  } catch { throw new Refusal("archive_unavailable") }
  const length = Number(response.headers.get("content-length") ?? 0)
  const raw = length > maximumResponse ? undefined : await response.text().catch(() => undefined)
  let parsed: Input | undefined
  try { parsed = raw && raw.length <= maximumResponse ? JSON.parse(raw) : undefined } catch {}
  if (!parsed || typeof parsed !== "object") throw new Refusal("archive_unavailable")
  if (!response.ok) {
    const error = parsed.error as Input | undefined
    const code = typeof error?.code === "string" && passed.includes(error.code) ? error.code : "archive_unavailable"
    const detail: Input = {}
    if (code === "revision_conflict" && typeof error?.current_revision === "string") detail.current_revision = error.current_revision
    if (code.startsWith("edit_") && typeof error?.edit === "number") detail.edit = error.edit
    throw new Refusal(code, detail)
  }
  return parsed
}

/** Runs one artifact tool; the result is JSON text for the model. */
export async function runArtifact(archive: Archive, operation: Operation, raw: unknown, sessionID?: string, signal?: AbortSignal): Promise<string> {
  const input = check(operation, raw)
  if (!input) return errorResult("malformed_request")
  try {
    const id = input.id as string
    let result: Input
    if (operation === "create" || (operation === "update" && ("content" in input || "path" in input))) {
      if (!("content" in input || "path" in input)) return errorResult("malformed_request", { reason: "give content or path" })
      const { bytes, mediaType } = await body(input)
      const query = new URLSearchParams()
      if (operation === "create") {
        query.set("title", input.title as string)
        if (sessionID) query.set("session", sessionID)
      } else {
        query.set("revision", input.revision as string)
        if (typeof input.title === "string") query.set("title", input.title)
      }
      result = await call(archive, `${operation === "create" ? "" : `/${id}`}?${query}`, {
        method: operation === "create" ? "POST" : "PUT", body: bytes,
        headers: { "content-type": mediaType.startsWith("text/") || mediaType === "image/svg+xml" ? `${mediaType}; charset=utf-8` : mediaType },
      }, 120_000, signal)
    } else if (operation === "update" || operation === "edit") {
      const change: Input = { revision: input.revision }
      if (typeof input.title === "string") change.title = input.title
      if (Array.isArray(input.edits)) change.edits = input.edits
      result = await call(archive, `/${id}`, { method: "PATCH", body: JSON.stringify(change), headers: { "content-type": "application/json" } }, 30_000, signal)
    } else if (operation === "find") {
      const query = new URLSearchParams(Object.entries(input).map(([key, value]) => [key, String(value)]))
      result = await call(archive, `?${query}`, {}, 15_000, signal)
      const artifacts = Array.isArray(result.artifacts) ? result.artifacts as Input[] : []
      return JSON.stringify({ artifacts: artifacts.map((artifact) => present(archive, artifact)), next_cursor: result.next_cursor ?? null })
    } else {
      const query = typeof input.offset === "number" ? `?offset=${input.offset}` : ""
      result = await call(archive, `/${id}${query}`, {}, 15_000, signal)
      return JSON.stringify({ artifact: present(archive, result.artifact as Input), text: result.text ?? null, offset: result.offset, next_offset: result.next_offset ?? null })
    }
    return JSON.stringify({ artifact: present(archive, result.artifact as Input) })
  } catch (error) {
    if (error instanceof Refusal) return errorResult(error.code, error.detail)
    return errorResult("archive_unavailable")
  }
}

export async function registerArtifacts(ctx: Plugin.Context, archive: Archive): Promise<void> {
  const skill = await bundledSkill("publishing-artifacts")
  await ctx.tool.transform((tools) => {
    for (const operation of Object.keys(definitions) as Operation[]) {
      const definition = definitions[operation]
      tools.add({
        name: definition.name,
        options: { namespace: "spool", codemode: false },
        description: definition.description,
        input: { type: "object", additionalProperties: false, properties: definition.properties, required: [...definition.required] },
        execute: async (input, context) => ({ content: await runArtifact(archive, operation, input, context.sessionID, context.signal) }),
      })
    }
  })
  await ctx.skill.transform((skills) => { skills.add(skill) })
}
