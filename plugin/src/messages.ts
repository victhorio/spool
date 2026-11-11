import { setTimeout as delay } from "node:timers/promises"
import type { OpenCodeClient } from "@opencode/client"
import { type Archive, archiveFetch } from "./archive"
import { errorCode, log } from "./log"
import { ambiguous, readerLink, validSession } from "./reference"
import { type Tool, toolError } from "./tools"

interface Delivery { id: string; session: string; text: string; from: { session: string; source: string } | null }

const toolName = "send_message"
const passed = ["malformed_request", "not_found", "session_offline", "unauthorized"]

/** The prompt for a delivered message; one from another agent says where it came from. */
export function prompt(message: Delivery): string {
  if (!message.from) return message.text
  return `<spool>This message was sent to you from an agent in ${message.from.session} in ${message.from.source} through spool_${toolName}.</spool>\n\n${message.text}`
}

/**
 * Collects this machine's messages and prompts their sessions until `signal` aborts.
 * Each poll is the machine's heartbeat, reports its running sessions and acknowledges
 * what the previous poll delivered.
 */
export async function deliver(archive: Archive, connect: () => Promise<OpenCodeClient>, signal: AbortSignal): Promise<void> {
  // Prompted, to acknowledge in the next poll that reaches the archive.
  const delivered = new Set<string>()
  let failures = 0
  while (!signal.aborted) {
    try {
      const client = await connect()
      const running = Object.keys(await client.session.active({ signal: AbortSignal.any([signal, AbortSignal.timeout(10_000)]) }))
      const acknowledged = [...delivered]
      const response = await archiveFetch(archive, "/v1/poll", {
        method: "POST", headers: { "content-type": "application/json" },
        body: JSON.stringify({ running, delivered: acknowledged }), timeout: 60_000, signal,
      })
      if (response.status === 401) { log("delivery_disabled_until_reload"); return }
      if (!response.ok) throw new Error(`http_${response.status}`)
      const { messages } = await response.json() as { messages: Delivery[] }
      for (const id of acknowledged) delivered.delete(id)
      for (const message of messages) {
        try {
          await client.session.prompt({ sessionID: message.session, text: prompt(message) }, { signal: AbortSignal.any([signal, AbortSignal.timeout(30_000)]) })
          log("message_delivered", { session_id: message.session })
        } catch (error) {
          // OpenCode rejected this message, such as for a deleted session or worktree; others still go.
          // Transport failures have no tag and are retried with the whole poll.
          const code = (error as { _tag?: unknown } | null)?._tag
          if (typeof code !== "string" || code === "ServiceUnavailableError") throw error
          log("message_dropped", { session_id: message.session, code })
        }
        delivered.add(message.id)
      }
      failures = 0
    } catch (error) {
      if (signal.aborted) return
      log("delivery_failed", { code: errorCode(error) })
      await delay(Math.min(30_000, 500 * 2 ** Math.min(failures++, 6)), undefined, { signal }).catch(() => {})
    }
  }
}

/** Sends one message for the tool; the result is JSON text for the model. */
export async function send(archive: Archive, input: unknown, from: string, signal?: AbortSignal): Promise<string> {
  const value = input as Record<string, unknown> | undefined
  const session = readerLink(archive, value?.session)?.session ?? value?.session
  const text = value?.text
  if (!value || Object.keys(value).some((key) => key !== "session" && key !== "text") || !validSession(session) ||
      typeof text !== "string" || !text.trim() || text.includes("\0") || Buffer.byteLength(text) > 64 * 1024) {
    return toolError("malformed_request")
  }
  try {
    const response = await archiveFetch(archive, "/v1/messages", {
      method: "POST", headers: { "content-type": "application/json", accept: "application/json" },
      body: JSON.stringify({ session, text, from }), timeout: 10_000, signal,
    })
    const parsed = await response.json() as { status?: string; error?: { code?: string } }
    if (response.ok) return JSON.stringify({ sent: true, status: parsed.status })
    const code = parsed.error?.code
    if (code === "ambiguous_session") return ambiguous(response.status, parsed.error!, "archive_unavailable")
    return toolError(typeof code === "string" && passed.includes(code) ? code : "archive_unavailable")
  } catch { return toolError("archive_unavailable") }
}

export function messageTools(archive: Archive): Tool[] {
  return [{
    name: toolName,
    description: "Send a message to an agent in another session. It arrives as a prompt that says it came from you, steering the agent if it is running. The session's machine must be online: find_sessions reports each session's `status`, `running`, `idle` or `offline`. The result only confirms it was sent; read the session later to see what the agent did.",
    input: {
      type: "object", additionalProperties: false, required: ["session", "text"],
      properties: {
        session: { type: "string", minLength: 1, maxLength: 2048, description: "Session reference or Spool link, such as `ses_…` or `work-m5/ses_…`." },
        text: { type: "string", minLength: 1, description: "The message, at most 64 KiB." },
      },
    },
    run: (input, context) => send(archive, input, context.sessionID, context.signal),
  }]
}
