import type { SessionTransferData } from "@opencode/client"

export interface RetrievalMessage {
  id: string
  role: string
  text: string
  conversation: string
  tools: string
  conversation_spans: [number, number][]
  tool_spans: [number, number][]
}

export interface Retrieval {
  version: 1
  title: string
  repository: string | null
  activity: number
  messages: RetrievalMessage[]
}

export function visible(value: string): string { return value.replaceAll("\0", "\\u0000") }

export function bounded(value: string, bytes: number): string {
  return new TextDecoder().decode(Buffer.from(visible(value)).subarray(0, bytes), { stream: true })
}

function record(value: unknown): Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {}
}

function textual(value: unknown): unknown {
  if (typeof value === "string") return visible(value)
  if (Array.isArray(value)) return value.map(textual)
  if (value !== null && typeof value === "object") {
    const object = record(value)
    const attachment = ["file", "image", "audio", "video", "input_audio", "Buffer"].includes(String(object.type)) ||
      typeof object.mime === "string" || typeof object.mediaType === "string" || object.encoding === "base64" || record(object.source).type === "inline"
    return Object.fromEntries(Object.entries(object).sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0).map(([key, item]) => {
      const binary = (/^(base64|b64_json|bytes|binary)$/i.test(key) && (typeof item === "string" || Array.isArray(item))) || (key === "data" && attachment) ||
        (["uri", "url", "image_url"].includes(key) && typeof item === "string" && item.startsWith("data:"))
      return [visible(key), binary ? "[inline binary omitted]" : textual(item)]
    }))
  }
  return value
}

function json(value: unknown): string { return JSON.stringify(value, null, 2) ?? "null" }

// Keep searched strings verbatim in the read text, including quotes, backslashes and control characters.
function evidence(value: unknown): string {
  if (typeof value === "string") return value
  if (Array.isArray(value)) return value.map((item, index) => `[${index + 1}]\n${evidence(item)}`).join("\n")
  if (value !== null && typeof value === "object") return Object.entries(value).map(([key, item]) => `${key}:\n${evidence(item)}`).join("\n")
  return json(value)
}

function remainder(object: Record<string, unknown>, omit: string[]): string {
  return json(Object.fromEntries(Object.entries(object).filter(([key]) => !omit.includes(key))))
}

export function projectMessage(value: unknown): RetrievalMessage {
  const message = record(textual(value))
  const type = typeof message.type === "string" ? message.type : "unknown"
  const chunks: string[] = []
  const conversation: string[] = []
  const tools: string[] = []
  const conversation_spans: [number, number][] = []
  const tool_spans: [number, number][] = []
  let offset = 0
  const append = (chunk: string, search?: "conversation" | "tools") => {
    const start = offset
    chunks.push(chunk)
    offset += Buffer.byteLength(chunk)
    if (!search || !chunk) return
    const spans = search === "conversation" ? conversation_spans : tool_spans
    const parts = search === "conversation" ? conversation : tools
    spans.push([start, offset])
    parts.push(chunk)
  }
  if (type === "assistant" && Array.isArray(message.content)) {
    append("[assistant]\n")
    for (const [index, value] of message.content.entries()) {
      if (index) append("\n")
      const part = record(value)
      if ((part.type === "text" || part.type === "reasoning") && typeof part.text === "string") {
        append(`[${part.type}]\n`)
        append(part.text, part.type === "text" ? "conversation" : undefined)
        append(`\n${remainder(part, ["type", "text"])}`)
      } else if (part.type === "tool") append(`[tool]\n${evidence(part)}`, "tools")
      else append(json(value))
    }
    append(`\n${remainder(message, ["type", "content"])}`)
  } else if (["user", "system", "synthetic", "skill"].includes(type) && typeof message.text === "string") {
    append(`[${type}]\n`)
    append(message.text, type === "user" ? "conversation" : undefined)
    append(`\n${remainder(message, ["type", "text"])}`)
  } else if (type === "shell") {
    append("[shell]\n")
    append(evidence(message), "tools")
  } else if (type === "compaction" && typeof message.summary === "string") {
    append(`[compaction ${String(message.status ?? "unknown")}]\n${message.summary}\n${remainder(message, ["type", "summary"])}`)
  }
  // New public variants retain JSON evidence without guessing whether their fields are conversational text.
  else append(json(message))
  return {
    id: visible(String(message.id ?? "")),
    role: bounded(type, 256),
    text: chunks.join(""),
    conversation: conversation.join("\n"),
    tools: tools.join("\n"),
    conversation_spans,
    tool_spans,
  }
}

export function projectRetrieval(transcript: SessionTransferData, upserts: SessionTransferData["messages"], repository: string | null): Retrieval {
  const activity = transcript.info.time.updated
  return {
    version: 1,
    title: bounded(transcript.info.title ?? "", 4096),
    repository,
    activity: Number.isSafeInteger(activity) && Math.abs(activity) <= 8.64e15 ? activity : 0,
    messages: upserts.map(projectMessage),
  }
}
