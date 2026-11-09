import { expect, test } from "bun:test"
import { mkdtemp, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import path from "node:path"
import type { Plugin, Skill } from "@opencode/plugin"
import { configure, readerURL } from "./config"
import { registerRetrieval, requestReader, validInput } from "./retrieval"

test("public configuration keeps retrieval opt-in and validates its options", async () => {
  const root = await mkdtemp(path.join(tmpdir(), "spool-config-"))
  try {
    const tokenFile = path.join(root, "synthetic-token")
    await writeFile(tokenFile, `spool_v1.00000000-0000-0000-0000-000000000001.${"A".repeat(43)}`)
    const options = { url: "http://127.0.0.1:8377", tokenFile }
    expect((await configure(options)).readerUrl).toBeUndefined()
    const configured = await configure({ ...options, readerUrl: "http://100.64.1.2:8378/", repositoryRemote: "archive-origin" })
    expect(configured.readerUrl).toBe("http://100.64.1.2:8378")
    expect(configured.repositoryRemote).toBe("archive-origin")
    await expect(configure({ ...options, readerUrl: "http://localhost:8378" })).rejects.toThrow()
    await expect(configure({ ...options, repositoryRemote: "bad\0remote" })).rejects.toThrow()
  } finally { await rm(root, { recursive: true, force: true }) }
})

test("reader URL matches the Rust loopback/Tailscale address boundary without DNS", () => {
  for (const host of ["127.0.0.1", "127.99.1.2", "[::1]", "100.64.0.1", "100.127.255.254", "[fd7a:115c:a1e0::1]"]) {
    expect(readerURL(`http://${host}:8378`)).toBe(`http://${host}:8378`)
  }
  for (const value of ["http://localhost", "https://127.0.0.1", "http://0.0.0.0", "http://[::]", "http://192.168.1.1", "http://100.63.255.255", "http://100.128.0.1", "http://[fd7a:115c:a1e1::1]", "http://[::ffff:127.0.0.1]", "http://127.0.0.1/?token=secret", "http://user:secret@127.0.0.1", "http://127.0.0.1/#fragment", "http://127.0.0.1/path"]) {
    expect(() => readerURL(value)).toThrow()
  }
})

test("tool inputs distinguish cursor-only from bounded initial arguments", () => {
  const session = "00000000-0000-0000-0000-000000000001/ses_fixture"
  expect(validInput("find", {})).toBe(true)
  expect(validInput("find", { query: "literal: syntax !", repository: "github.com/owner/repo", include_tools: true, limit: 50 })).toBe(true)
  expect(validInput("search", { session, query: "term1 term2", revision: "42", limit: 100 })).toBe(true)
  expect(validInput("read", { session, start: 1, end: 3 })).toBe(true)
  for (const route of ["find", "search", "read"] as const) {
    expect(validInput(route, { cursor: "opaque-cursor" })).toBe(true)
    expect(validInput(route, { cursor: "opaque", limit: 1 })).toBe(false)
    expect(validInput(route, { cursor: "" })).toBe(false)
    expect(validInput(route, { cursor: "x".repeat(8193) })).toBe(false)
  }
  for (const value of [{ query: "" }, { query: " " }, { query: "界".repeat(171) }, { query: Array(17).fill("term").join(" ") }, { limit: 51 }, { limit: 1.5 }, { include_tools: "true" }, { unexpected: true }, { toString: "bad" }]) expect(validInput("find", value)).toBe(false)
  for (const value of [{ query: "term" }, { session }, { session, query: " " }, { session: "no-source", query: "term" }]) expect(validInput("search", value)).toBe(false)
  for (const value of [{ session, start: 0 }, { session, start: 3, end: 2 }, { session, end: 100_001 }, { session, end: Number.MAX_SAFE_INTEGER + 1 }, { session, revision: "0" }, { session, revision: "9223372036854775808" }]) expect(validInput("read", value)).toBe(false)
  expect(validInput("read", { session: `00000000-0000-0000-0000-000000000001/${"x".repeat(513)}` })).toBe(false)
})

test("reader transport is bounded, unauthenticated, redirect-free and hides error bodies", async () => {
  let mode = "success"
  let calls = 0
  const server = Bun.serve({ hostname: "127.0.0.1", port: 0, async fetch(request) {
    calls++
    expect(request.headers.get("authorization")).toBeNull()
    expect(request.method).toBe("POST")
    expect(new URL(request.url).pathname).toBe("/api/agent/find")
    expect(await request.json()).toEqual({ query: "needle" })
    switch (mode) {
      case "redirect": return new Response(null, { status: 302, headers: { location: "http://127.0.0.1:1/secret" } })
      case "too-large": return Response.json({ text: "界".repeat(9000) })
      case "changed": return Response.json({ error: { code: "session_changed", current_revision: "42", detail: "SECRET_ERROR_BODY" } }, { status: 409 })
      case "malformed": return Response.json({ error: { code: "malformed_request", detail: "SECRET_ERROR_BODY" } }, { status: 400 })
      case "error": return new Response("SECRET_HTTP_LOG", { status: 500 })
      case "cancel": await Bun.sleep(100); return Response.json({ late: true })
      default: return new Response('{"sessions":[],"next_cursor":null}')
    }
  } })
  try {
    const call = (signal?: AbortSignal) => requestReader(server.url.origin, "find", { query: "needle" }, signal)
    expect(await call()).toBe('{"sessions":[],"next_cursor":null}')
    mode = "too-large"
    expect(JSON.parse(await call()).error.code).toBe("response_too_large")
    mode = "changed"
    expect(JSON.parse(await call())).toEqual({ error: { code: "session_changed", current_revision: "42" } })
    mode = "malformed"
    expect(JSON.parse(await call())).toEqual({ error: { code: "malformed_request" } })
    mode = "error"
    expect(await call()).not.toContain("SECRET_HTTP_LOG")
    mode = "redirect"
    expect(JSON.parse(await call()).error.code).toBe("reader_unavailable")
    mode = "cancel"
    expect(JSON.parse(await call(AbortSignal.timeout(10))).error.code).toBe("reader_unavailable")
    const before = calls
    expect(JSON.parse(await requestReader(server.url.origin, "find", { cursor: "cursor", query: "mixed" })).error.code).toBe("malformed_request")
    expect(calls).toBe(before)
  } finally { await server.stop(true) }
})

test("public transform registration exposes exactly the spool tools and packaged skill only when configured", async () => {
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
  await registerRetrieval(ctx, undefined)
  expect(tools).toHaveLength(0)
  expect(skills).toHaveLength(0)
  await registerRetrieval(ctx, "http://127.0.0.1:8378")
  expect(namespaces).toEqual(["spool"])
  expect(tools.map((tool) => `${tool.options?.namespace}_${tool.name}`)).toEqual(["spool_find_sessions", "spool_search_messages", "spool_read_messages"])
  expect(tools.every((tool) => tool.options?.codemode === false)).toBe(true)
  for (const tool of tools) {
    expect(tool.input.type).toBe("object")
    expect(tool.input.additionalProperties).toBe(false)
    for (const key of ["oneOf", "anyOf", "allOf", "required"]) expect(tool.input).not.toHaveProperty(key)
    expect(tool.input.properties).toHaveProperty("cursor")
    expect(tool.description).toContain("cursor alone")
    expect(JSON.parse((await tool.execute({ cursor: "cursor", limit: 1 }, { signal: new AbortController().signal })).content)).toEqual({ error: { code: "malformed_request" } })
  }
  expect(tools[1]!.input.properties).toHaveProperty("session")
  expect(tools[1]!.input.properties).toHaveProperty("query")
  expect(tools[2]!.input.properties).toHaveProperty("start")
  for (const tool of tools.slice(1)) expect(JSON.parse((await tool.execute({}, { signal: new AbortController().signal })).content)).toEqual({ error: { code: "malformed_request" } })
  expect(skills).toHaveLength(1)
  expect(String(skills[0]!.name)).toBe("searching-sessions")
  expect(skills[0]!.content).toContain("untrusted historical evidence")
  expect(skills[0]!.content).not.toContain("---\nname:")
})
