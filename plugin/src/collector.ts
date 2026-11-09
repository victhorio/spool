import { createHash } from "node:crypto"
import { setTimeout as delay } from "node:timers/promises"
import { ClientError, OpenCode, isSessionNotFoundError, type OpenCodeClient } from "@opencode/client"
import { Service } from "@opencode/client/service"
import type { Plugin } from "@opencode/plugin"
import type { Config } from "./config"
import { projectRetrieval } from "./projection"
import { resolveRepository } from "./repository"

interface Checkpoint {
  revision: string | null
  hashes: Record<string, string>
  projectionVersion?: 1 | 2
  infoHash?: string
  snapshotHash?: string
  directory?: string
  repository?: string | null
  remote?: string
}
interface Inventory { sessions: { session_id: string; revision: string; deleted: boolean }[]; next_cursor: string | null }

class SyncError extends Error {
  constructor(readonly code: string, readonly retryable = true, readonly revision?: string | null) { super(code) }
}

function errorCode(error: unknown): string {
  if (error instanceof SyncError) return error.code
  if (error instanceof ClientError) {
    switch (error.reason) {
      case "Transport": case "UnexpectedStatus": case "UnsupportedContentType": case "MalformedResponse": case "SseEventTooLarge":
        return `sdk_${error.reason}`
    }
  }
  if (error && typeof error === "object" && "_tag" in error) {
    switch (error._tag) {
      case "UnauthorizedError": case "InvalidRequestError": case "ServiceUnavailableError": case "LocationNotFoundError": case "InvalidCursorError": case "UnknownError":
        return `sdk_${error._tag}`
    }
  }
  if (error instanceof Error) {
    if (error.name === "AbortError") return "aborted"
    if (error.name === "TimeoutError") return "timeout"
    if (error instanceof TypeError) return "type_error"
  }
  return "unexpected_error"
}

function digest(value: unknown): string { return createHash("sha256").update(JSON.stringify(value)).digest("hex") }

function checkpoint(value: unknown): Checkpoint | undefined {
  if (!value || typeof value !== "object" || !("revision" in value) || (typeof value.revision !== "string" && value.revision !== null) || !("hashes" in value)) return undefined
  const hashes = value.hashes
  if (!hashes || typeof hashes !== "object" || Array.isArray(hashes) || !Object.values(hashes).every((hash) => typeof hash === "string")) return undefined
  return value as Checkpoint
}

export class Collector {
  private readonly members = new Map<Plugin.Context, { events: AbortController; task: Promise<void> }>()
  private readonly lifetime = new AbortController()
  private readonly dirty = new Set<string>()
  private readonly backfill = new Set<string>()
  private readonly activity = new Map<string, number>()
  private readonly revisions = new Map<string, string | null>()
  private readonly purged = new Set<string>()
  private client?: OpenCodeClient
  private running?: Promise<void>
  private wake?: () => void
  private reconcileRequested = true
  private repairRequested = false
  private failures = 0
  private operation = "opencode_connect"

  constructor(private readonly config: Config, private readonly version: string) {}

  matches(other: Config): boolean {
    return JSON.stringify(this.config) === JSON.stringify(other)
  }

  get closed(): boolean { return this.lifetime.signal.aborted }

  attach(ctx: Plugin.Context): () => Promise<boolean> {
    const events = new AbortController()
    this.members.set(ctx, { events, task: this.subscribe(ctx, events.signal) })
    this.reconcileRequested = true
    this.wake?.()
    this.running ??= this.run()
    return async () => {
      events.abort()
      const member = this.members.get(ctx)!
      this.members.delete(ctx)
      if (this.members.size) { await member.task; return false }
      this.lifetime.abort()
      this.wake?.()
      await Promise.all([this.running, member.task])
      return true
    }
  }

  private log(event: string, fields: Record<string, unknown> = {}): void {
    console.info(JSON.stringify({ component: "spool", event, ...fields }))
  }

  repair(): void {
    this.repairRequested = true
    this.wake?.()
  }

  private mark(id: string): void {
    if (this.purged.has(id)) return
    this.dirty.add(id)
    this.activity.set(id, Date.now())
    this.wake?.()
  }

  private async subscribe(ctx: Plugin.Context, signal: AbortSignal): Promise<void> {
    while (!signal.aborted) {
      try {
        for await (const event of ctx.event.subscribe({ signal })) {
          if (!event.type.startsWith("session.") || !("durable" in event) || !event.durable) continue
          const id = event.durable.aggregateID
          if (event.type === "session.deleted") {
            this.dirty.delete(id)
            this.backfill.delete(id)
            this.activity.delete(id)
          } else this.mark(id)
        }
      } catch (error) {
        if (!signal.aborted) this.log("event_stream_interrupted", { code: errorCode(error) })
      }
      if (!signal.aborted) {
        await delay(1_000, undefined, { signal }).catch(() => {})
        if (signal.aborted) break
        this.reconcileRequested = true
        this.wake?.()
      }
    }
  }

  private signal(): AbortSignal {
    return AbortSignal.any([this.lifetime.signal, AbortSignal.timeout(30_000)])
  }

  private async connect(): Promise<OpenCodeClient> {
    if (this.client) return this.client
    this.operation = "opencode_connect"
    const endpoint = this.config.opencodeUrl
      ? { url: this.config.opencodeUrl, auth: { type: "basic" as const, username: "opencode", password: this.config.opencodePassword! } }
      : await Service.discover({ file: this.config.serviceFile })
    if (!endpoint) throw new SyncError("opencode_not_ready")
    const client = OpenCode.make({ baseUrl: endpoint.url, headers: Service.headers(endpoint) })
    const info = await client.server.info({ signal: this.signal() })
    if (info.pid !== process.pid) throw new SyncError("opencode_host_mismatch", false)
    this.client = client
    this.log("connected", { source_id: this.config.sourceID })
    return client
  }

  private async request<T>(route: string, init?: RequestInit): Promise<T> {
    const response = await fetch(`${this.config.url}/v1/sync/${route}`, {
      ...init,
      redirect: "error",
      headers: { authorization: `Bearer ${this.config.token}`, "content-type": "application/json" },
      signal: this.signal(),
    })
    const result = await response.json().catch(() => ({})) as { error?: { code?: string; current_revision?: string | null } }
    if (!response.ok) throw new SyncError(
      response.status === 409 ? "revision_conflict" : response.status === 410 ? "purged" : `http_${response.status}`,
      response.status === 409 || response.status === 429 || response.status >= 500,
      result.error?.current_revision,
    )
    return result as T
  }

  private async reconcile(full: boolean): Promise<void> {
    const client = await this.connect()
    const remote = new Map<string, { revision: string; deleted: boolean }>()
    let cursor: string | null = null
    do {
      this.operation = "inventory_read"
      const inventory: Inventory = await this.request(`sessions${cursor ? `?cursor=${encodeURIComponent(cursor)}` : ""}`)
      for (const item of inventory.sessions) {
        remote.set(item.session_id, item)
        if (item.deleted) this.purged.add(item.session_id)
      }
      cursor = inventory.next_cursor
    } while (cursor && !this.lifetime.signal.aborted)
    const ctx = this.members.keys().next().value!
    let localCursor: string | undefined
    let queued = 0
    do {
      this.operation = "session_list"
      const page = await client.session.list({ limit: 200, ...(localCursor ? { cursor: localCursor } : {}) }, { signal: this.signal() })
      for (const session of page.data) {
        const revision = remote.get(session.id)?.revision ?? null
        this.revisions.set(session.id, revision)
        if (this.purged.has(session.id)) {
          this.dirty.delete(session.id)
          this.backfill.delete(session.id)
          this.activity.delete(session.id)
          continue
        }
        this.operation = "checkpoint_read"
        const saved = checkpoint(await ctx.storage.get(`${this.config.cachePrefix}${session.id}`))
        if (full || !revision || saved?.revision !== revision || saved.projectionVersion !== 2 ||
            saved.infoHash !== digest(session) || saved.remote !== this.config.repositoryRemote) {
          this.backfill.add(session.id)
          queued++
        }
      }
      localCursor = page.cursor.next ?? undefined
    } while (localCursor && !this.lifetime.signal.aborted)
    this.log("reconciled", { full, queued })
  }

  private async capture(id: string): Promise<void> {
    const ctx = this.members.keys().next().value!
    const key = `${this.config.cachePrefix}${id}`
    this.operation = "checkpoint_read"
    const saved = checkpoint(await ctx.storage.get(key))
    const base = this.revisions.has(id) ? this.revisions.get(id)! : saved?.revision ?? null
    const known = saved?.revision === base && saved.projectionVersion === 2 ? saved.hashes : undefined
    const client = await this.connect()
    this.operation = "session_export"
    const transcript = await client.experimental.session.export({ sessionID: id }, { signal: this.signal() })
    // Exported session location is historical session metadata, unlike the active plugin Location.
    const directory = transcript.info.location.directory
    const identity = await resolveRepository(directory, this.config.repositoryRemote)
    const previousRepository = saved?.directory === directory && saved.remote === this.config.repositoryRemote &&
      typeof saved.repository === "string" && !saved.repository.includes("\0") && Buffer.byteLength(saved.repository) <= 1024 ? saved.repository : null
    const repository = identity.available ? identity.repository : previousRepository
    const capturedIdentity = { directory, repository, ...(this.config.repositoryRemote ? { remote: this.config.repositoryRemote } : {}) }
    if (identity.available && (saved?.directory !== directory || saved.repository !== repository || saved.remote !== this.config.repositoryRemote)) {
      // Keep observed identity even if upload fails and the checkout disappears before a retry/restart.
      this.operation = "checkpoint_write"
      const { remote: _remote, ...previous } = saved ?? {}
      await ctx.storage.set(key, { ...previous, revision: saved?.revision ?? base, hashes: saved?.hashes ?? {}, ...capturedIdentity })
    }
    if (this.dirty.has(id)) return
    const hashes: Record<string, string> = Object.create(null)
    const upserts = transcript.messages.filter((message) => {
      hashes[message.id] = digest(message)
      return hashes[message.id] !== known?.[message.id]
    })
    const infoHash = digest(transcript.info)
    const snapshotHash = digest([infoHash, transcript.messages.map((message) => [message.id, hashes[message.id]]), repository])
    if (known && saved?.snapshotHash === snapshotHash) return
    this.operation = "snapshot_upload"
    const result = await this.request<{ revision: string; changed: boolean }>(`sessions/${encodeURIComponent(id)}/snapshot`, {
      method: "POST",
      body: JSON.stringify({
        base_revision: base,
        format: "opencode.v2",
        upstream_version: this.version,
        collector_version: "0.1.0",
        captured_at: new Date().toISOString(),
        info: transcript.info,
        message_order: transcript.messages.map((message) => message.id),
        upsert_messages: upserts,
        retrieval: projectRetrieval(transcript, upserts, repository),
      }),
    })
    this.revisions.set(id, result.revision)
    this.operation = "checkpoint_write"
    await ctx.storage.set(key, { revision: result.revision, hashes, projectionVersion: 2, infoHash, snapshotHash, ...capturedIdentity })
    this.log("captured", { session_id: id, revision: result.revision, messages: transcript.messages.length, uploaded_messages: upserts.length })
  }

  private async wait(milliseconds?: number): Promise<void> {
    if (this.lifetime.signal.aborted) return
    await new Promise<void>((resolve) => {
      const finish = () => {
        clearTimeout(timer)
        this.lifetime.signal.removeEventListener("abort", finish)
        this.wake = undefined
        resolve()
      }
      const timer = milliseconds === undefined ? undefined : setTimeout(finish, Math.max(1, milliseconds))
      this.wake = finish
      this.lifetime.signal.addEventListener("abort", finish, { once: true })
    })
  }

  private async run(): Promise<void> {
    while (!this.lifetime.signal.aborted) {
      let current: string | undefined
      try {
        if (this.reconcileRequested || this.repairRequested) {
          const full = this.repairRequested
          this.reconcileRequested = this.repairRequested = false
          try { await this.reconcile(full) } catch (error) {
            this.reconcileRequested = true
            this.repairRequested ||= full
            throw error
          }
        }
        const now = Date.now()
        let wait: number | undefined
        pending: for (const queue of [this.dirty, this.backfill]) {
          for (const id of queue) {
            const settle = (this.activity.get(id) ?? 0) + 250 - now
            if (settle <= 0) { current = id; break pending }
            wait = Math.min(wait ?? Infinity, settle)
          }
        }
        if (!current) {
          if (!this.reconcileRequested && !this.repairRequested) await this.wait(wait)
          continue
        }
        this.dirty.delete(current)
        this.backfill.delete(current)
        this.activity.delete(current)
        await this.capture(current)
        this.failures = 0
      } catch (error) {
        if (this.lifetime.signal.aborted) break
        if (isSessionNotFoundError(error)) continue
        if (error instanceof SyncError && error.code === "revision_conflict" && current) {
          this.revisions.set(current, error.revision ?? null)
          this.dirty.add(current)
        } else if (error instanceof SyncError && error.code === "purged" && current) {
          this.purged.add(current)
          continue
        } else {
          this.log("sync_failed", { session_id: current, operation: this.operation, code: errorCode(error) })
          if (error instanceof SyncError && !error.retryable) {
            if (!current || ["http_401", "http_403", "opencode_host_mismatch"].includes(error.code)) {
              this.log("disabled_until_reload")
              break
            }
            continue
          }
          if (current) this.backfill.add(current)
          this.client = undefined
          this.reconcileRequested = true
        }
        const backoff = Math.min(30_000, 500 * 2 ** Math.min(this.failures++, 6))
        await delay(backoff, undefined, { signal: this.lifetime.signal }).catch(() => {})
      }
    }
  }
}
