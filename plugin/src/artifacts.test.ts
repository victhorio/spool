import { expect, test } from "bun:test"
import { mkdtemp, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import path from "node:path"
import { runArtifact } from "./artifacts"

const artifact = {
  id: "art_abcdefghijkm", title: "Plan", type: "text/markdown", revision: "1", size: 6, source: "laptop",
  source_id: "00000000-0000-0000-0000-000000000001", source_label: "laptop", session: "laptop/ses_one", session_id: "ses_one",
  created_at: "2026-11-10T00:00:00Z", updated_at: "2026-11-10T00:00:00Z",
}

interface Seen { method: string; path: string; search: string; type: string | null; authorization: string | null; body: Uint8Array }

async function withServer(answer: (seen: Seen) => Response, run: (url: string, seen: Seen[]) => Promise<void>) {
  const seen: Seen[] = []
  const server = Bun.serve({ hostname: "127.0.0.1", port: 0, async fetch(request) {
    const url = new URL(request.url)
    const record = { method: request.method, path: url.pathname, search: url.search, type: request.headers.get("content-type"),
      authorization: request.headers.get("authorization"), body: new Uint8Array(await request.arrayBuffer()) }
    seen.push(record)
    return answer(record)
  } })
  try { await run(server.url.origin, seen) } finally { server.stop(true) }
}

test("create sends inline content with its media type and session, and returns a link", async () => {
  await withServer(() => Response.json({ artifact }, { status: 201 }), async (url, seen) => {
    const archive = { url, token: "spool_v1.synthetic" }
    const result = JSON.parse(await runArtifact(archive, "create", { title: "Plan", type: "markdown", content: "# Plan" }, "ses_one"))
    expect(result.artifact).toEqual({
      id: artifact.id, title: "Plan", type: "text/markdown", revision: "1", size: 6, source: "laptop", session: "laptop/ses_one",
      created_at: artifact.created_at, updated_at: artifact.updated_at, url: `${url}/artifacts/${artifact.id}`,
    })
    expect(seen[0]!.method).toBe("POST")
    expect(seen[0]!.path).toBe("/v1/artifacts")
    expect(new URLSearchParams(seen[0]!.search).get("title")).toBe("Plan")
    expect(new URLSearchParams(seen[0]!.search).get("session")).toBe("ses_one")
    expect(seen[0]!.type).toBe("text/markdown; charset=utf-8")
    expect(seen[0]!.authorization).toBe("Bearer spool_v1.synthetic")
    expect(new TextDecoder().decode(seen[0]!.body)).toBe("# Plan")
  })
})

test("files are uploaded byte for byte, typed by extension", async () => {
  const root = await mkdtemp(path.join(tmpdir(), "spool-artifact-"))
  try {
    const image = path.join(root, "chart.PNG")
    const bytes = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 255])
    await writeFile(image, bytes)
    await writeFile(path.join(root, "notes.unknown"), "x")
    await withServer(() => Response.json({ artifact }), async (url, seen) => {
      const archive = { url, token: "t" }
      await runArtifact(archive, "update", { id: artifact.id, revision: "3", path: image, title: "Chart" })
      expect(seen[0]!.method).toBe("PUT")
      expect(seen[0]!.path).toBe(`/v1/artifacts/${artifact.id}`)
      expect(Object.fromEntries(new URLSearchParams(seen[0]!.search))).toEqual({ revision: "3", title: "Chart" })
      expect(seen[0]!.type).toBe("image/png")
      expect(seen[0]!.body).toEqual(bytes)
      const unknown = JSON.parse(await runArtifact(archive, "create", { title: "x", path: path.join(root, "notes.unknown") }))
      expect(unknown.error.code).toBe("malformed_request")
      const missing = JSON.parse(await runArtifact(archive, "create", { title: "x", path: path.join(root, "absent.md") }))
      expect(missing.error.code).toBe("file_unreadable")
      const relative = JSON.parse(await runArtifact(archive, "create", { title: "x", path: "notes.md" }))
      expect(relative.error.code).toBe("malformed_request")
      expect(seen.length).toBe(1)
    })
  } finally { await rm(root, { recursive: true, force: true }) }
})

test("edits, renames, find and read use their routes; errors keep their details", async () => {
  const answers: Response[] = [
    Response.json({ error: { code: "edit_not_found", edit: 1 } }, { status: 422 }),
    Response.json({ error: { code: "revision_conflict", current_revision: "4" } }, { status: 409 }),
    Response.json({ artifacts: [artifact], next_cursor: "next" }),
    Response.json({ artifact, text: "# Plan", offset: 0, next_offset: null }),
    Response.json({ error: { code: "internal_detail" } }, { status: 500 }),
    Response.json({ error: { code: "busy" } }, { status: 503 }),
  ]
  await withServer(() => answers.shift()!, async (url, seen) => {
    const archive = { url, token: "t" }
    const edits = [{ old: "a", new: "b" }, { old: "c", new: "" }]
    expect(JSON.parse(await runArtifact(archive, "edit", { id: artifact.id, revision: "3", edits }))).toEqual({ error: { code: "edit_not_found", edit: 1 } })
    expect(JSON.parse(seen[0]!.body.length ? new TextDecoder().decode(seen[0]!.body) : "{}")).toEqual({ revision: "3", edits })
    expect(seen[0]!.method).toBe("PATCH")
    expect(JSON.parse(await runArtifact(archive, "update", { id: artifact.id, revision: "3", title: "New" }))).toEqual({ error: { code: "revision_conflict", current_revision: "4" } })
    expect(JSON.parse(new TextDecoder().decode(seen[1]!.body))).toEqual({ revision: "3", title: "New" })
    const found = JSON.parse(await runArtifact(archive, "find", { query: "plan", limit: 5 }))
    expect(found.next_cursor).toBe("next")
    expect(found.artifacts[0].url).toBe(`${url}/artifacts/${artifact.id}`)
    expect(found.artifacts[0].source_id).toBeUndefined()
    expect(seen[2]!.search).toBe("?query=plan&limit=5")
    const read = JSON.parse(await runArtifact(archive, "read", { id: artifact.id, offset: 0 }))
    expect(read).toMatchObject({ text: "# Plan", offset: 0, next_offset: null })
    expect(seen[3]!.search).toBe("?offset=0")
    expect(JSON.parse(await runArtifact(archive, "read", { id: artifact.id }))).toEqual({ error: { code: "archive_unavailable" } })
    expect(JSON.parse(await runArtifact(archive, "find", {}))).toEqual({ error: { code: "busy" } })
  })
  const unreachable = JSON.parse(await runArtifact({ url: "http://127.0.0.1:1", token: "t" }, "find", {}))
  expect(unreachable).toEqual({ error: { code: "archive_unavailable" } })
})

test("malformed inputs never reach the server", async () => {
  const id = artifact.id
  for (const [operation, input] of [
    ["create", {}],
    ["create", { title: "x" }],
    ["create", { title: "x", content: "y" }],
    ["create", { title: "x", type: "png", content: "y" }],
    ["create", { title: "x", type: "markdown", content: "y", path: "/tmp/y.md" }],
    ["create", { title: "x", type: "markdown", content: "y", extra: 1 }],
    ["update", { id, revision: "1" }],
    ["update", { id, revision: "1", type: "html" }],
    ["update", { id: "art_X", revision: "1", title: "x" }],
    ["update", { id, revision: "one", title: "x" }],
    ["edit", { id, revision: "1", edits: [] }],
    ["edit", { id, revision: "1", edits: [{ old: "", new: "x" }] }],
    ["edit", { id, revision: "1", edits: [{ old: "a" }] }],
    ["find", { cursor: "c", limit: 2 }],
    ["find", { query: " " }],
    ["find", { limit: 51 }],
    ["read", { id, offset: -1 }],
  ] as const) {
    const result = JSON.parse(await runArtifact({ url: "http://127.0.0.1:1", token: "t" }, operation, input))
    expect([operation, input, result.error.code]).toEqual([operation, input, "malformed_request"])
  }
})
