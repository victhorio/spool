import { expect, test } from "bun:test"
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

test.each([undefined, 1])("collector upgrades projection checkpoint %s, sends sparse deltas and retains deleted-session repository", async (projectionVersion) => {
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
  const checkpoints = new Map<string, unknown>([["fixture/ses_synthetic", {
    revision: "1", hashes: { msg_synthetic: createHash("sha256").update(JSON.stringify(transcript.messages[0])).digest("hex") },
    ...(projectionVersion ? { projectionVersion } : {}),
  }]])
  const uploads: Upload[] = []
  let revision: string | null = "1"
  let conflict = false
  let conflicted = false
  let loseAcknowledgement = false
  const server = Bun.serve({ hostname: "127.0.0.1", port: 0, async fetch(request) {
    const pathname = new URL(request.url).pathname
    if (pathname === "/api/info") return Response.json({ pid: process.pid })
    if (pathname === "/api/session") return Response.json({ data: [transcript.info], cursor: {} })
    if (pathname === "/api/experimental/session/ses_synthetic/export") return Response.json({ data: transcript })
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
    url: server.url.origin, token: "synthetic", sourceID: "synthetic", cachePrefix: "fixture/", sweepIntervalMs: 30,
    opencodeUrl: server.url.origin,
  }
  const ctx = {
    // Deliberately different: capture must not probe the active plugin's Location.
    location: { directory: root },
    storage: { get: async (key: string) => checkpoints.get(key), set: async (key: string, value: unknown) => { checkpoints.set(key, value) } },
    event: { subscribe: async function* ({ signal }: { signal: AbortSignal }) {
      await new Promise<void>((resolve) => { signal.addEventListener("abort", () => resolve(), { once: true }) })
    } },
  } as unknown as Plugin.Context
  const collector = new Collector(config, "2.2.5")
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
    expect(first.upsert_messages).toEqual(transcript.messages)
    expect(first.retrieval.messages.map((message) => message.id)).toEqual(["msg_synthetic"])
    expect(first.info).toEqual(transcript.info)
    expect(first.retrieval.repository).toBe("github.com/captured/repository")
    expect(first.retrieval.messages[0]!.conversation).toBe("fixture\\u0000text")
    expect(first.retrieval.messages[0]!.conversation_spans).toHaveLength(1)
    expect(first.retrieval.messages[0]!.tool_spans).toEqual([])
    await next((upload) => upload.upsert_messages.length === 0)
    expect((checkpoints.get("fixture/ses_synthetic") as { projectionVersion: number }).projectionVersion).toBe(2)
    transcript.info.title = "Metadata-only change"
    const metadata = await next((upload) => (upload.info as { title: string }).title === "Metadata-only change")
    expect(metadata.upsert_messages).toHaveLength(0)
    expect(metadata.retrieval.messages).toHaveLength(0)
    const changed = uploads.length
    const message = transcript.messages[0] as { text: string }
    message.text = "Changed evidence"
    const delta = await next((upload) => upload.upsert_messages.some((item) => item.text === "Changed evidence"), changed)
    expect(delta.retrieval.messages).toHaveLength(1)
    expect(delta.retrieval.messages[0]!.conversation).toBe("Changed evidence")
    loseAcknowledgement = true
    git("remote", "set-url", "origin", "https://new.example/Actual/Origin.git")
    await next((upload) => upload.retrieval.repository === "new.example/Actual/Origin")
    expect((checkpoints.get("fixture/ses_synthetic") as { repository: string }).repository).toBe("new.example/Actual/Origin")
    const stop = remove
    remove = undefined
    await stop()
    await rm(directory, { recursive: true, force: true })
    const deleted = uploads.length
    remove = new Collector(config, "2.2.5").attach(ctx)
    expect((await next(() => true, deleted)).retrieval.repository).toBe("new.example/Actual/Origin")
    conflict = true
    await next((upload) => conflicted && upload.base_revision === null, uploads.length)
    const retry = uploads.findLast((upload) => upload.base_revision === null)!
    expect(retry.upsert_messages).toHaveLength(1)
    expect(retry.retrieval.repository).toBe("new.example/Actual/Origin")
  } finally {
    await remove?.()
    await server.stop(true)
    await rm(root, { recursive: true, force: true })
  }
}, 10_000)
