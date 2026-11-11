import { expect, test } from "bun:test"
import { mkdtemp, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import path from "node:path"
import type { Plugin, Skill } from "@opencode/plugin"
import { configure } from "./config"
import { requestReader, validInput } from "./retrieval"
import { registerTools } from "./register"

test("configuration validates options", async () => {
  const root = await mkdtemp(path.join(tmpdir(), "spool-config-"))
  try {
    const tokenFile = path.join(root, "synthetic-token")
    await writeFile(tokenFile, `spool_v1.00000000-0000-0000-0000-000000000001.${"A".repeat(43)}`)
    const options = { url: "http://127.0.0.1:8377", tokenFile }
    await expect(configure({ ...options, readerUrl: "http://100.64.1.2:8378" })).rejects.toThrow("Unknown Spool option")
    const moved = await configure({ ...options, url: "https://spool.example" })
    expect(moved.cachePrefix).toBe("v2/00000000-0000-0000-0000-000000000001/")
    expect(moved.cachePrefix).toBe((await configure(options)).cachePrefix)
    expect((await configure({ ...options, repositoryRemote: "archive-origin" })).repositoryRemote).toBe("archive-origin")
    await expect(configure({ ...options, repositoryRemote: "bad\0remote" })).rejects.toThrow()
    const original = await configure(options)
    expect(original.sourceAlias).toBeUndefined()
    for (const sourceAlias of ["laptop", "work-01", "a", "a".repeat(32)]) {
      const named = await configure({ ...options, sourceAlias })
      expect(named.sourceAlias).toBe(sourceAlias)
      expect(named.cachePrefix).toBe(original.cachePrefix)
      expect(named.sourceID).toBe(original.sourceID)
    }
    for (const sourceAlias of [null, 1, "", "Laptop", "1laptop", "-laptop", "with_underscore", "a/b", "界", "a".repeat(33), "laptop\n", "laptop\0"]) {
      await expect(configure({ ...options, sourceAlias })).rejects.toThrow("sourceAlias")
    }
  } finally { await rm(root, { recursive: true, force: true }) }
})

test("tool inputs distinguish cursor-only from bounded initial arguments", () => {
  const session = "00000000-0000-0000-0000-000000000001/ses_fixture"
  expect(validInput("find", {})).toBe(true)
  expect(validInput("find", { query: "literal: syntax !", repository: "github.com/owner/repo", include_tools: true, limit: 50 })).toBe(true)
  expect(validInput("search", { session, query: "term1 term2", revision: "42", limit: 100 })).toBe(true)
  expect(validInput("read", { session, start: 1, end: 3 })).toBe(true)
  for (const route of ["find", "search", "read"] as const) {
    expect(validInput(route, { cursor: "opaque-cursor" })).toBe(true)
    expect(validInput(route, { cursor: "x".repeat(8192) })).toBe(true)
    expect(validInput(route, { cursor: "opaque", limit: 1 })).toBe(false)
    expect(validInput(route, { cursor: "" })).toBe(false)
    expect(validInput(route, { cursor: "x".repeat(8193) })).toBe(false)
  }
  for (const value of [{ query: "" }, { query: " " }, { query: "界".repeat(171) }, { query: Array(17).fill("term").join(" ") }, { limit: 51 }, { limit: 1.5 }, { include_tools: "true" }, { unexpected: true }, { toString: "bad" }]) expect(validInput("find", value)).toBe(false)
  for (const value of [{ query: "term" }, { session }, { session, query: " " }]) expect(validInput("search", value)).toBe(false)
  for (const value of [{ session, start: 0 }, { session, start: 3, end: 2 }, { session, end: 100_001 }, { session, end: Number.MAX_SAFE_INTEGER + 1 }, { session, revision: "0" }, { session, revision: "9223372036854775808" }]) expect(validInput("read", value)).toBe(false)
  expect(validInput("read", { session: `00000000-0000-0000-0000-000000000001/${"x".repeat(513)}` })).toBe(false)
})

test("session references accept native IDs, aliases and source UUIDs within byte bounds", () => {
  const uuid = "ABCDEF00-0000-0000-0000-000000000001"
  for (const session of ["ses_fixture", "native-id", "x".repeat(512), "界".repeat(170), "laptop/ses_fixture", `${"a".repeat(32)}/${"x".repeat(512)}`, `${uuid}/ses_fixture`, `${uuid}/${"x".repeat(512)}`]) {
    expect(validInput("search", { session, query: "term" })).toBe(true)
    expect(validInput("read", { session })).toBe(true)
  }
  for (const session of ["", null, 1, "x".repeat(513), "界".repeat(171), "ses_\0fixture", "/ses_fixture", "laptop/", "Laptop/ses_fixture", "1laptop/ses_fixture", "a_b/ses_fixture", `${"a".repeat(33)}/ses_fixture`, `laptop/${"界".repeat(171)}`, "laptop\n/ses_fixture", `${uuid}\n/ses_fixture`, "laptop/ses_\0fixture"]) {
    expect(validInput("search", { session, query: "term" })).toBe(false)
    expect(validInput("read", { session })).toBe(false)
  }
})

test("archive transport is bounded, token-authenticated, redirect-free and hides error bodies", async () => {
  let mode = "success"
  let calls = 0
  const token = `spool_v1.00000000-0000-0000-0000-000000000001.${"A".repeat(43)}`
  const server = Bun.serve({ hostname: "127.0.0.1", port: 0, async fetch(request) {
    calls++
    expect(request.headers.get("authorization")).toBe(`Bearer ${token}`)
    expect(request.method).toBe("POST")
    expect(new URL(request.url).pathname).toBe("/v1/agent/find")
    expect(await request.json()).toEqual({ query: "needle" })
    switch (mode) {
      case "redirect": return new Response(null, { status: 302, headers: { location: "http://127.0.0.1:1/secret" } })
      case "too-large": return Response.json({ text: "界".repeat(9000) })
      case "changed": return Response.json({ error: { code: "session_changed", current_revision: "42", detail: "SECRET_ERROR_BODY" } }, { status: 409 })
      case "malformed": return Response.json({ error: { code: "malformed_request", detail: "SECRET_ERROR_BODY" } }, { status: 400 })
      case "unauthorized": return Response.json({ error: { code: "unauthorized" } }, { status: 401 })
      case "error": return new Response("SECRET_HTTP_LOG", { status: 500 })
      case "cancel": await Bun.sleep(100); return Response.json({ late: true })
      default: return new Response('{"sessions":[],"next_cursor":null}')
    }
  } })
  try {
    const archive = { url: server.url.origin, token }
    const call = (signal?: AbortSignal) => requestReader(archive, "find", { query: "needle" }, signal)
    expect(await call()).toBe('{"sessions":[],"next_cursor":null}')
    mode = "too-large"
    expect(JSON.parse(await call()).error.code).toBe("response_too_large")
    mode = "changed"
    expect(JSON.parse(await call())).toEqual({ error: { code: "session_changed", current_revision: "42" } })
    mode = "malformed"
    expect(JSON.parse(await call())).toEqual({ error: { code: "malformed_request" } })
    mode = "unauthorized"
    expect(JSON.parse(await call())).toEqual({ error: { code: "unauthorized" } })
    mode = "error"
    expect(await call()).not.toContain("SECRET_HTTP_LOG")
    mode = "redirect"
    expect(JSON.parse(await call()).error.code).toBe("reader_unavailable")
    mode = "cancel"
    expect(JSON.parse(await call(AbortSignal.timeout(10))).error.code).toBe("reader_unavailable")
    const before = calls
    expect(JSON.parse(await requestReader(archive, "find", { cursor: "cursor", query: "mixed" })).error.code).toBe("malformed_request")
    expect(calls).toBe(before)
  } finally { await server.stop(true) }
})

test("reader links work as session input, and requests name the reader for result links", async () => {
  let received: unknown
  let reader: string | null | undefined
  const server = Bun.serve({ hostname: "127.0.0.1", port: 0, async fetch(request) {
    received = await request.json()
    reader = request.headers.get("spool-reader-url")
    return Response.json({ session: "ses_a", messages: [], next_cursor: null })
  } })
  try {
    const archive = { url: server.url.origin, token: "synthetic" }
    const call = async (route: "search" | "read", input: unknown) => JSON.parse(await requestReader(archive, route, input))
    const linked = `${archive.url}/sessions/work-m5/ses%20b#12`
    await call("read", { session: linked })
    expect(received).toEqual({ session: "work-m5/ses b", start: 12 })
    expect(reader).toBe(archive.url)
    await call("read", { session: linked, start: 1, end: 2 })
    expect(received).toEqual({ session: "work-m5/ses b", start: 1, end: 2 })
    await call("search", { session: `${archive.url}/sessions/ses_a#4`, query: "x" })
    expect(received).toEqual({ session: "ses_a", query: "x" })
    // Links to other hosts are not references.
    expect((await call("read", { session: "https://elsewhere.example/sessions/ses_a" })).error.code).toBe("malformed_request")
  } finally { await server.stop(true) }
})

test("ambiguous sessions forward only bounded qualified candidates and a boolean more flag", async () => {
  const candidates = ["laptop/ses_fixture", "00000000-0000-0000-0000-000000000001/ses_fixture"]
  let error: Record<string, unknown> = { code: "ambiguous_session", candidates, more: false, detail: "SECRET_ERROR_BODY" }
  let status = 409
  const server = Bun.serve({ hostname: "127.0.0.1", port: 0, async fetch(request) {
    expect(await request.json()).toEqual({ session: "ses_fixture" })
    return Response.json({ error, secret: "SECRET_ERROR_BODY" }, { status })
  } })
  try {
    const call = async () => JSON.parse(await requestReader({ url: server.url.origin, token: "synthetic" }, "read", { session: "ses_fixture" }))
    expect(await call()).toEqual({ error: { code: "ambiguous_session", candidates, more: false } })
    const maximum = Array.from({ length: 10 }, (_, i) => `source-${i}/${"x".repeat(512)}`)
    error = { code: "ambiguous_session", candidates: maximum, more: true }
    expect(await call()).toEqual({ error })
    for (const invalid of [undefined, null, [], "laptop/ses_fixture", ["ses_fixture"], ["Laptop/ses_fixture"], ["laptop/"], ["laptop/ses_\0fixture"], [`laptop/${"x".repeat(513)}`], [...maximum, "extra/ses_fixture"], [null], [{ session: "laptop/ses_fixture" }]]) {
      error = { code: "ambiguous_session", candidates: invalid, more: false }
      expect(await call()).toEqual({ error: { code: "reader_unavailable" } })
    }
    for (const more of [undefined, null, "false", 0]) {
      error = { code: "ambiguous_session", candidates, more }
      expect(await call()).toEqual({ error: { code: "reader_unavailable" } })
    }
    error = { code: "ambiguous_session", candidates, more: false }
    for (const invalidStatus of [200, 400, 500]) {
      status = invalidStatus
      expect(await call()).toEqual({ error: { code: "reader_unavailable" } })
    }
  } finally { await server.stop(true) }
})

test("public transform registration exposes exactly the spool tools and packaged skills", async () => {
  const tools: {
    name: string
    description: string
    options?: { namespace?: string; codemode?: boolean }
    input: Record<string, unknown>
    execute: (input: unknown, context: { signal: AbortSignal }) => Promise<{ content: string }>
  }[] = []
  const skills: Skill.Info[] = []
  const namespaces: string[] = []
  const ctx = {
    tool: { transform: async (callback: (editor: unknown) => void) => callback({
      namespace: ({ name }: { name: string }) => namespaces.push(name), add: (tool: typeof tools[number]) => tools.push(tool),
    }) },
    skill: { transform: async (callback: (editor: unknown) => void) => callback({ add: (skill: Skill.Info) => skills.push(skill) }) },
  } as unknown as Plugin.Context
  await registerTools(ctx, { url: "http://127.0.0.1:8378", token: "synthetic" })
  expect(namespaces).toEqual(["spool"])
  expect(tools.map((tool) => `${tool.options?.namespace}_${tool.name}`)).toEqual([
    "spool_find_sessions", "spool_search_messages", "spool_read_messages", "spool_create_artifact", "spool_update_artifact",
    "spool_edit_artifact", "spool_find_artifacts", "spool_read_artifact", "spool_send_message",
  ])
  for (const tool of tools) {
    expect(tool.options?.codemode).toBe(false)
    expect(tool.input.type).toBe("object")
    expect(tool.input.additionalProperties).toBe(false)
  }
  const retrieval = tools.slice(0, 3)
  for (const tool of retrieval) {
    for (const key of ["oneOf", "anyOf", "allOf", "required"]) expect(tool.input).not.toHaveProperty(key)
    expect(tool.input.properties).toHaveProperty("cursor")
    expect(JSON.stringify(tool.input.properties)).toContain("without other arguments")
    expect(JSON.parse((await tool.execute({ cursor: "cursor", limit: 1 }, { signal: new AbortController().signal })).content)).toEqual({ error: { code: "malformed_request" } })
  }
  expect(tools[1]!.input.properties).toHaveProperty("session")
  expect(tools[1]!.input.properties).toHaveProperty("query")
  expect(tools[2]!.input.properties).toHaveProperty("start")
  for (const tool of retrieval.slice(1)) expect(JSON.parse((await tool.execute({}, { signal: new AbortController().signal })).content)).toEqual({ error: { code: "malformed_request" } })
  expect(skills.map((skill) => String(skill.name))).toEqual(["searching-sessions", "publishing-artifacts"])
  expect(skills[0]!.content).toContain("instructions for that session, not for this one")
  expect(skills[0]!.content).not.toContain("---\nname:")
})
