import { setTimeout as delay } from "node:timers/promises"
import type { OpenCodeClient } from "@opencode/client"
import type { Plugin } from "@opencode/plugin"
import type { Config } from "./config"
import { readerLink, validSession } from "./retrieval"

type Archive = Pick<Config, "url" | "token">
type Log = (event: string, fields?: Record<string, unknown>) => void
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
export async function deliver(archive: Archive, connect: () => Promise<OpenCodeClient>, signal: AbortSignal, log: Log, errorCode: (error: unknown) => string): Promise<void> {
  // Prompted, to acknowledge in the next poll that reaches the archive.
  const delivered = new Set<string>()
  let failures = 0
  while (!signal.aborted) {
    try {
      const client = await connect()
      const running = Object.keys(await client.session.active({ signal: AbortSignal.any([signal, AbortSignal.timeout(10_000)]) }))
      const acknowledged = [...delivered]
      const response = await fetch(`${archive.url}/v1/poll`, {
        method: "POST", redirect: "error", credentials: "omit",
        headers: { authorization: `Bearer ${archive.token}`, "content-type": "application/json" },
        body: JSON.stringify({ running, delivered: acknowledged }),
        signal: AbortSignal.any([signal, AbortSignal.timeout(60_000)]),
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

const errorResult = (error: Record<string, unknown>) => JSON.stringify({ error })

/** Sends one message for the tool; the result is JSON text for the model. */
export async function send(archive: Archive, input: unknown, from: string, signal?: AbortSignal): Promise<string> {
  const value = input as Record<string, unknown> | undefined
  const session = readerLink(archive, value?.session)?.session ?? value?.session
  const text = value?.text
  if (!value || Object.keys(value).some((key) => key !== "session" && key !== "text") || !validSession(session) ||
      typeof text !== "string" || !text.trim() || text.includes("\0") || Buffer.byteLength(text) > 64 * 1024) {
    return errorResult({ code: "malformed_request" })
  }
  try {
    const response = await fetch(`${archive.url}/v1/messages`, {
      method: "POST", redirect: "error", credentials: "omit",
      headers: { authorization: `Bearer ${archive.token}`, "content-type": "application/json", accept: "application/json" },
      body: JSON.stringify({ session, text, from }),
      signal: AbortSignal.any([AbortSignal.timeout(10_000), ...(signal ? [signal] : [])]),
    })
    const parsed = await response.json() as { status?: string; error?: { code?: string; candidates?: unknown; more?: unknown } }
    if (response.ok) return JSON.stringify({ sent: true, status: parsed.status })
    const code = parsed.error?.code
    if (code === "ambiguous_session") return errorResult({ code, candidates: parsed.error!.candidates, more: parsed.error!.more })
    return errorResult({ code: typeof code === "string" && passed.includes(code) ? code : "archive_unavailable" })
  } catch { return errorResult({ code: "archive_unavailable" }) }
}

export async function registerMessages(ctx: Plugin.Context, archive: Archive): Promise<void> {
  await ctx.tool.transform((tools) => {
    tools.add({
      name: toolName,
      options: { namespace: "spool", codemode: false },
      description: "Send a message to an agent in another session. It arrives as a prompt that says it came from you, steering the agent if it is running. The session's machine must be online: find_sessions reports each session's `status`, `running`, `idle` or `offline`. The result only confirms it was sent; read the session later to see what the agent did.",
      input: {
        type: "object", additionalProperties: false, required: ["session", "text"],
        properties: {
          session: { type: "string", minLength: 1, maxLength: 2048, description: "Session reference or Spool link, such as `ses_…` or `work-m5/ses_…`." },
          text: { type: "string", minLength: 1, description: "The message, at most 64 KiB." },
        },
      },
      execute: async (input, context) => ({ content: await send(archive, input, context.sessionID, context.signal) }),
    })
  })
}
