import { expect, spyOn, test } from "bun:test"
import { createHash } from "node:crypto"
import { execFileSync } from "node:child_process"
import { mkdtemp, mkdir, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import path from "node:path"
import type { SessionTransferData } from "@opencode/client"
import type { Plugin } from "@opencode/plugin"
import { Collector } from "./collector"
import type { Config } from "./config"
import type { Retrieval } from "./projection"

interface Upload {
  base_revision: string | null
  info: unknown
  upsert_messages: { id: string; text?: string }[]
  retrieval: Retrieval
}

/** Answers the delivery loop: nothing is running, and polls wait until the collector stops. */
function delivery(request: Request): Promise<Response> | undefined {
  const pathname = new URL(request.url).pathname
  if (pathname === "/api/session/active") return Promise.resolve(Response.json({ data: {} }))
  if (pathname === "/v1/poll") return new Promise((resolve) => {
    request.signal.addEventListener("abort", () => resolve(Response.json({ messages: [] })), { once: true })
  })
}

test.each([undefined, 1, 2])("collector upgrades checkpoint %s, captures on demand and retains deleted-worktree identity", async (projectionVersion) => {
  const root = await mkdtemp(path.join(tmpdir(), "spool-capture-"))
  const directory = path.join(root, "historical-session")
  const git = (...args: string[]) => execFileSync("git", args, { cwd: directory, timeout: 5_000, stdio: "pipe" })
  await mkdir(directory)
  git("init", "-q")
  git("remote", "add", "origin", "git@github.com:Captured/Repository.git")
  const transcript = {
    info: { id: "ses_synthetic", title: "Fixture", time: { updated: 1700000000000 }, location: { directory } },
    messages: [{ id: "msg_synthetic", type: "user", text: "fixture\0text" }],
  } as unknown as SessionTransferData
  // The current checkpoint format also starts under the legacy key, which must migrate.
  const checkpoints = new Map<string, unknown>([[projectionVersion === 2 ? "legacy/ses_synthetic" : "fixture/ses_synthetic", {
    revision: "1", hashes: { msg_synthetic: createHash("sha256").update(JSON.stringify(transcript.messages[0])).digest("hex") },
    ...(projectionVersion ? { projectionVersion } : {}),
  }]])
  const uploads: Upload[] = []
  let revision: string | null = "1"
  let conflict = false
  let conflicted = false
  let loseAcknowledgement = false
  let exports = 0
  let lists = 0
  let registrations = 0
  let onList: (() => Promise<void>) | undefined
  const server = Bun.serve({ hostname: "127.0.0.1", port: 0, async fetch(request) {
    const pathname = new URL(request.url).pathname
    const delivered = delivery(request)
    if (delivered) return delivered
    if (pathname === "/v1/sync/source") {
      registrations++
      expect(request.method).toBe("PUT")
      expect(request.headers.get("authorization")).toBe("Bearer synthetic")
      expect(await request.json()).toEqual({ alias: "laptop" })
      return Response.json({ alias: "laptop" })
    }
    if (pathname === "/api/info") return Response.json({ pid: process.pid })
    if (pathname === "/api/session") { lists++; await onList?.(); return Response.json({ data: [transcript.info], cursor: {} }) }
    if (pathname === "/api/experimental/session/ses_synthetic/export") { exports++; return Response.json({ data: transcript }) }
    if (request.method === "GET") return Response.json({ sessions: revision ? [{ session_id: "ses_synthetic", revision, deleted: false }] : [], next_cursor: null })
    const body = await request.json() as Upload
    uploads.push(body)
    if (conflict) {
      conflict = false
      conflicted = true
      revision = null
      return Response.json({ error: { current_revision: null } }, { status: 409 })
    }
    revision = String(Number(revision ?? 0) + 1)
    if (loseAcknowledgement && body.retrieval.repository === "new.example/Actual/Origin") {
      loseAcknowledgement = false
      return Response.json({ error: { code: "synthetic_lost_acknowledgement" } }, { status: 503 })
    }
    return Response.json({ revision, changed: true })
  } })
  const config: Config = {
    url: server.url.origin, token: "synthetic", sourceID: "synthetic", cachePrefix: "fixture/", legacyCachePrefix: "legacy/",
    opencodeUrl: server.url.origin, repositoryRemote: "origin",
  }
  const events: string[] = []
  let wake: (() => void) | undefined
  const emit = (type = "session.metadata.updated") => { events.push(type); wake?.() }
  const ctx = {
    // Deliberately different: capture must not probe the active plugin's Location.
    location: { directory: root },
    storage: {
      get: async (key: string) => checkpoints.get(key),
      set: async (key: string, value: unknown) => { checkpoints.set(key, value) },
      remove: async (key: string) => { checkpoints.delete(key) },
    },
    event: { subscribe: async function* ({ signal }: { signal: AbortSignal }) {
      const abort = () => wake?.()
      signal.addEventListener("abort", abort, { once: true })
      try {
        while (!signal.aborted) {
          const type = events.shift()
          if (!type) { await new Promise<void>((resolve) => { wake = resolve }); continue }
          if (type === "disconnect") return
          yield { type, durable: { aggregateID: "ses_synthetic" } }
        }
      } finally { signal.removeEventListener("abort", abort) }
    } },
  } as unknown as Plugin.Context
  let collector = new Collector(config, "2.2.5")
  let remove: (() => Promise<boolean>) | undefined = collector.attach(ctx)
  async function next(predicate: (upload: Upload) => boolean, after = 0): Promise<Upload> {
    const deadline = Date.now() + 3_000
    while (Date.now() < deadline) {
      const found = uploads.slice(after).find(predicate)
      if (found) return found
      await Bun.sleep(10)
    }
    throw new Error("Synthetic capture did not converge")
  }
  try {
    const first = await next(() => true)
    expect(registrations).toBe(0)
    expect(first.upsert_messages).toEqual(projectionVersion === 2 ? [] : transcript.messages)
    expect(first.retrieval.messages.map((message) => message.id)).toEqual(projectionVersion === 2 ? [] : ["msg_synthetic"])
    expect(first.info).toEqual(transcript.info)
    expect(first.retrieval.repository).toBe("github.com/captured/repository")
    if (projectionVersion !== 2) {
      expect(first.retrieval.messages[0]!.conversation).toBe("fixture\\u0000text")
      expect(first.retrieval.messages[0]!.conversation_spans).toHaveLength(1)
      expect(first.retrieval.messages[0]!.tool_spans).toEqual([])
    }
    await Bun.sleep(100)
    expect((checkpoints.get("fixture/ses_synthetic") as { projectionVersion: number }).projectionVersion).toBe(2)
    expect([...checkpoints.keys()]).toEqual(["fixture/ses_synthetic"])
    const idle = { exports, lists, uploads: uploads.length, checkpoint: checkpoints.get("fixture/ses_synthetic") }
    await Bun.sleep(400)
    expect({ exports, lists, uploads: uploads.length, checkpoint: checkpoints.get("fixture/ses_synthetic") }).toEqual(idle)
    emit()
    await Bun.sleep(400)
    expect(exports).toBe(idle.exports + 1)
    expect(uploads).toHaveLength(idle.uploads)
    expect(checkpoints.get("fixture/ses_synthetic")).toBe(idle.checkpoint)
    transcript.info.title = "Metadata-only change"
    emit()
    const metadata = await next((upload) => (upload.info as { title: string }).title === "Metadata-only change")
    expect(metadata.upsert_messages).toHaveLength(0)
    expect(metadata.retrieval.messages).toHaveLength(0)
    const message = transcript.messages[0] as { text: string }
    message.text = "Content event with unchanged metadata"
    emit("session.text.ended")
    const eventDelta = await next((upload) => upload.upsert_messages.some((item) => item.text === message.text))
    expect(eventDelta.retrieval.messages[0]!.conversation).toBe("Content event with unchanged metadata")
    const changed = uploads.length
    message.text = "Changed evidence"
    await Bun.sleep(400)
    expect(uploads).toHaveLength(changed)
    collector.repair()
    const delta = await next((upload) => upload.upsert_messages.some((item) => item.text === "Changed evidence"), changed)
    expect(delta.retrieval.messages).toHaveLength(1)
    expect(delta.retrieval.messages[0]!.conversation).toBe("Changed evidence")
    await Bun.sleep(100)
    await remove()
    const previous = checkpoints.get("fixture/ses_synthetic") as { remote?: string }
    const { remote: _remote, ...withoutOverride } = previous
    delete config.repositoryRemote
    const switched = { exports, uploads: uploads.length }
    collector = new Collector(config, "2.2.5")
    remove = collector.attach(ctx)
    await Bun.sleep(400)
    expect(exports).toBe(switched.exports + 1)
    expect(uploads).toHaveLength(switched.uploads)
    expect(checkpoints.get("fixture/ses_synthetic")).toEqual(withoutOverride)
    await remove()
    config.sourceAlias = "laptop"
    collector = new Collector(config, "2.2.5")
    expect(collector.matches({ ...config })).toBe(true)
    expect(collector.matches({ ...config, sourceAlias: "desktop" })).toBe(false)
    remove = collector.attach(ctx)
    await Bun.sleep(400)
    expect(registrations).toBe(1)
    expect(exports).toBe(switched.exports + 1)
    expect(uploads).toHaveLength(switched.uploads)
    expect(checkpoints.get("fixture/ses_synthetic")).toEqual(withoutOverride)
    for (const remotes of [[], ["first", "second"], ["origin"]]) {
      git("remote", "remove", "origin")
      for (const name of remotes) git("remote", "add", name, "../unusable-local-remote")
      const unknown = { exports, uploads: uploads.length, checkpoint: checkpoints.get("fixture/ses_synthetic") }
      collector.repair()
      await Bun.sleep(400)
      expect(exports).toBe(unknown.exports + 1)
      expect(uploads).toHaveLength(unknown.uploads)
      expect(checkpoints.get("fixture/ses_synthetic")).toBe(unknown.checkpoint)
      for (const name of remotes) git("remote", "remove", name)
      git("remote", "add", "origin", "git@github.com:Captured/Repository.git")
    }
    expect(registrations).toBe(1)
    loseAcknowledgement = true
    git("remote", "set-url", "origin", "https://new.example/Actual/Origin.git")
    collector.repair()
    await next((upload) => upload.retrieval.repository === "new.example/Actual/Origin")
    expect((checkpoints.get("fixture/ses_synthetic") as { repository: string }).repository).toBe("new.example/Actual/Origin")
    const stop = remove
    remove = undefined
    await stop()
    await rm(directory, { recursive: true, force: true })
    config.repositoryRemote = "unavailable-override"
    const deleted = uploads.length
    remove = new Collector(config, "2.2.5").attach(ctx)
    expect((await next(() => true, deleted)).retrieval.repository).toBe("new.example/Actual/Origin")
    conflict = true
    transcript.info.title = "Trigger conflict"
    emit()
    await next((upload) => conflicted && upload.base_revision === null, uploads.length)
    const retry = uploads.findLast((upload) => upload.base_revision === null)!
    expect(retry.upsert_messages).toHaveLength(1)
    expect(retry.retrieval.repository).toBe("new.example/Actual/Origin")
    transcript.info.location.directory = path.join(root, "different-missing-worktree")
    emit()
    expect((await next((upload) => (upload.info as SessionTransferData["info"]).location.directory === transcript.info.location.directory)).retrieval.repository).toBe("new.example/Actual/Origin")
    await Bun.sleep(100)
    await remove()
    const restarted = { exports, uploads: uploads.length, registrations: registrations + 1 }
    remove = new Collector(config, "2.2.5").attach(ctx)
    await Bun.sleep(400)
    expect({ exports, uploads: uploads.length, registrations }).toEqual(restarted)
    emit("disconnect")
    await Bun.sleep(1_400)
    expect({ exports, uploads: uploads.length, registrations }).toEqual(restarted)
    emit("disconnect")
    await Bun.sleep(400)
    transcript.info.title = "Changed during disconnected event stream"
    const recovered = await next((upload) => (upload.info as { title: string }).title === transcript.info.title)
    expect(recovered.upsert_messages).toHaveLength(0)
    await Bun.sleep(100)
    await remove()
    message.text = "Silent edit repaired during startup reconciliation"
    const entered = Promise.withResolvers<void>()
    const release = Promise.withResolvers<void>()
    onList = async () => { onList = undefined; entered.resolve(); await release.promise }
    const pending = new Collector(config, "2.2.5")
    remove = pending.attach(ctx)
    await entered.promise
    pending.repair()
    release.resolve()
    const pendingRepair = await next((upload) => upload.upsert_messages.some((item) => item.text === message.text))
    expect(pendingRepair.retrieval.messages[0]!.conversation).toBe("Silent edit repaired during startup reconciliation")
  } finally {
    await remove?.()
    await server.stop(true)
    await rm(root, { recursive: true, force: true })
  }
}, 10_000)

test.each([429, 503])("source registration retries HTTP %s once and stays registered across later sync failures and attachments", async (status) => {
  let registrations = 0
  let inventories = 0
  let connections = 0
  let lists = 0
  const server = Bun.serve({ hostname: "127.0.0.1", port: 0, async fetch(request) {
    const delivered = delivery(request)
    if (delivered) return delivered
    switch (new URL(request.url).pathname) {
      case "/v1/sync/source":
        registrations++
        expect(request.method).toBe("PUT")
        expect(request.headers.get("authorization")).toBe("Bearer synthetic")
        expect(await request.json()).toEqual({ alias: "laptop" })
        return registrations === 1 ? Response.json({ error: { code: "transient" } }, { status }) : Response.json({ alias: "laptop" })
      case "/api/info": connections++; return Response.json({ pid: process.pid })
      case "/v1/sync/sessions":
        inventories++
        return inventories === 1 ? new Response(null, { status: 503 }) : Response.json({ sessions: [], next_cursor: null })
      case "/api/session": lists++; return Response.json({ data: [], cursor: {} })
      default: return new Response(null, { status: 404 })
    }
  } })
  const config: Config = { url: server.url.origin, token: "synthetic", sourceID: "synthetic", cachePrefix: "fixture/", legacyCachePrefix: "legacy/", sourceAlias: "laptop", opencodeUrl: server.url.origin }
  const ctx = {
    event: { subscribe: async function* ({ signal }: { signal: AbortSignal }) {
      await new Promise<void>((resolve) => { signal.addEventListener("abort", () => resolve(), { once: true }) })
    } },
  } as unknown as Plugin.Context
  const collector = new Collector(config, "2.2.5")
  const remove = collector.attach(ctx)
  let detach: (() => Promise<boolean>) | undefined
  try {
    const deadline = Date.now() + 3_000
    while (!lists && Date.now() < deadline) await Bun.sleep(10)
    expect(lists).toBe(1)
    expect(registrations).toBe(2)
    // Delivery connects at once; each capture failure drops the connection.
    expect(connections).toBe(3)
    detach = collector.attach({ ...ctx })
    collector.repair()
    await Bun.sleep(100)
    expect(lists).toBeGreaterThan(1)
    expect(registrations).toBe(2)
    expect(await detach()).toBe(false)
    detach = undefined
  } finally {
    await detach?.()
    await remove()
    await server.stop(true)
  }
})

test.each(["source_alias_conflict", "source_alias_mismatch", "unknown_conflict", null])("source assignment %s disables capture until reload without a revision-conflict retry", async (code) => {
  let calls = 0
  const server = Bun.serve({ hostname: "127.0.0.1", port: 0, async fetch(request) {
    const delivered = delivery(request)
    if (delivered) return delivered
    if (new URL(request.url).pathname === "/api/info") return Response.json({ pid: process.pid })
    calls++
    expect(new URL(request.url).pathname).toBe("/v1/sync/source")
    return Response.json(code === null ? null : { error: { code, detail: "SECRET_ERROR_BODY" } }, { status: 409 })
  } })
  const logs = spyOn(console, "info").mockImplementation(() => {})
  const config: Config = { url: server.url.origin, token: "synthetic", sourceID: "synthetic", cachePrefix: "fixture/", legacyCachePrefix: "legacy/", sourceAlias: "laptop", opencodeUrl: server.url.origin }
  const ctx = {
    event: { subscribe: async function* ({ signal }: { signal: AbortSignal }) {
      await new Promise<void>((resolve) => { signal.addEventListener("abort", () => resolve(), { once: true }) })
    } },
  } as unknown as Plugin.Context
  const collector = new Collector(config, "2.2.5")
  const remove = collector.attach(ctx)
  try {
    const deadline = Date.now() + 1_000
    while (!logs.mock.calls.length && Date.now() < deadline) await Bun.sleep(10)
    collector.repair()
    await Bun.sleep(600)
    expect(calls).toBe(1)
    expect(logs.mock.calls.map(([line]) => JSON.parse(String(line))).filter((line) => line.event !== "connected")).toEqual([
      { component: "spool", event: "sync_failed", operation: "source_register", code: code === "source_alias_conflict" || code === "source_alias_mismatch" ? code : "http_409" },
      { component: "spool", event: "disabled_until_reload" },
    ])
  } finally {
    await remove()
    logs.mockRestore()
    await server.stop(true)
  }
})
