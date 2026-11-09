import assert from "node:assert/strict"
import { chmod, mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import path from "node:path"
import { isDeepStrictEqual } from "node:util"
import type { JsonValue, SessionTransferData } from "@opencode/client"
import { command, waitFor } from "./command"
import { startOpenCode } from "./opencode"
import { agentQuery, evidence, longText, needle, seed, location, type Host } from "./retrieval-fixture/conversations"
import { RetrievalRuntime } from "./retrieval-fixture/runtime"

interface Source { source_id: string; token: string }
interface FoundSession { session: string; repository: string | null; revision: string; captured_at: string; message_count: number; match: { index: number; excerpt: string; read_cursor: string } | null }
interface Found { sessions: FoundSession[]; coverage: { total_sessions: number; indexed_sessions: number; unassigned_repository_sessions: number }; next_cursor: string | null }
interface Searched { session: string; revision: string; matches: { index: number; excerpt: string; read_cursor: string }[]; next_cursor: string | null }
interface Read { session: string; revision: string; messages: { index: number; id: string; role: string; text: string; offset_bytes: number; total_bytes: number }[]; next_cursor: string | null }

const repository = path.resolve(import.meta.dir, "..")
const root = await mkdtemp(path.join(tmpdir(), "retrieval-validate-"))
await chmod(root, 0o700)
const runtime = new RetrievalRuntime(root, repository)
const releaseSignals = runtime.handleSignals()
const hosts: Host[] = []
const cases: string[] = []
const names = { find: "spool_find_sessions", search: "spool_search_messages", read: "spool_read_messages" }
const repoKey = "github.com/synthetic-spool/retrieval-fixture"
const sharedID = "ses_retrieval_shared"
const worktreeID = "ses_retrieval_worktree"
let passed = false
let calls = 0
const quote = (text: string) => `'${text.replaceAll("'", "''")}'`
const pass = (name: string) => { cases.push(name); console.log(`PASS ${name}`) }

async function host(label: string, source: Source, withReader = true) {
  const tokenFile = path.join(root, `${label}.token`)
  await Bun.write(tokenFile, source.token)
  await chmod(tokenFile, 0o600)
  const value = await startOpenCode(path.join(root, label), path.join(repository, "plugin"), {
    url: runtime.url, tokenFile, sweepIntervalMs: 1_000, ...(withReader ? { readerUrl: runtime.readerUrl } : {}),
  }, runtime.signal, [{ package: path.join(repository, "dev", "retrieval-fixture"), options: { query: agentQuery, expectedText: evidence } }])
  hosts.push(value)
  return value
}

async function activate(host: Host, directory: string) {
  await waitFor("real Spool and fixture plugin registration", async () => {
    const result = await host.client.plugin.list({ location: { directory } })
    const failure = result.data.find((plugin) => plugin.state.status === "failed")
    assert.equal(failure, undefined, JSON.stringify(failure))
    return ["spool", "spool-retrieval-fixture"].every((id) => result.data.some((plugin) => plugin.id === id && plugin.state.status === "active")) ? true : undefined
  }, 30_000, runtime.signal)
}

async function converged(host: Host, source: Source, id: string) {
  return waitFor(`capture to PostgreSQL: ${source.source_id}/${id}`, async () => {
    const expected = await host.client.experimental.session.export({ sessionID: id })
    const raw = await runtime.sql(`SELECT json_build_object('revision', revision::text, 'repository', repository,
      'info', info, 'messages', COALESCE((SELECT json_agg(payload ORDER BY position) FROM messages m
      WHERE m.source_id=s.source_id AND m.session_id=s.session_id), '[]'::json)) FROM sessions s
      WHERE source_id=${quote(source.source_id)} AND session_id=${quote(id)} AND retrieval_title IS NOT NULL AND deleted_at IS NULL`)
    if (!raw) return undefined
    const actual = JSON.parse(raw)
    return isDeepStrictEqual(actual.info, expected.info) && isDeepStrictEqual(actual.messages, expected.messages) ? actual : undefined
  }, 30_000, runtime.signal)
}

async function tool<T>(host: Host, route: keyof typeof names, input: Record<string, JsonValue>): Promise<T> {
  const result = await host.client.rpc.call({ rpcID: "spool-retrieval-fixture", method: "execute", location: location(host),
    input: { tool: names[route], input, sessionID: sharedID } })
  assert.equal(typeof result.output?.content, "string", JSON.stringify(result))
  assert.ok(Buffer.byteLength(result.output.content) <= 24 * 1024, "Tool response exceeds the model-facing 24 KiB budget")
  calls++
  return JSON.parse(result.output.content) as T
}

async function findAll(host: Host, input: Record<string, JsonValue>) {
  let page = await tool<Found>(host, "find", input)
  const sessions = [...page.sessions]
  for (let pages = 1; page.next_cursor; pages++) {
    assert.ok(pages < 30, "Find cursor did not terminate")
    page = await tool<Found>(host, "find", { cursor: page.next_cursor })
    sessions.push(...page.sessions)
  }
  assert.equal(new Set(sessions.map((item) => item.session)).size, sessions.length, "Find pagination duplicated a session")
  return { sessions, coverage: page.coverage }
}

async function agent(host: Host, id: "general" | "explore", model = "availability") {
  const session = await host.client.session.create({ title: `Synthetic ${id} ${model}`, agent: id,
    model: { providerID: "spool-retrieval-fixture", id: model }, location: location(host) })
  await host.client.session.prompt({ sessionID: session.id, text: "Run the deterministic synthetic retrieval validation. Do not use a live model." })
  await host.client.experimental.session.wait({ sessionID: session.id })
  const result = await host.client.experimental.session.export({ sessionID: session.id })
  const assistants = result.messages.filter((message) => message.type === "assistant")
  assert.ok(assistants.length, `No assistant output: ${JSON.stringify(result)}`)
  for (const message of assistants) assert.notEqual(message.finish, "error", JSON.stringify(message))
  const text = assistants.flatMap((message) => message.content.filter((part) => part.type === "text").map((part) => part.text)).join("\n")
  return { result, text }
}

try {
  await runtime.up()
  const primary: Source = JSON.parse(await runtime.admin("source", "add", "--label", "synthetic-retrieval-primary"))
  const secondary: Source = JSON.parse(await runtime.admin("source", "add", "--label", "synthetic-retrieval-second-store"))
  const first = await host("opencode-primary", primary)
  const second = await host("opencode-second", secondary)
  const main = location(first).directory
  const worktree = path.join(first.root, "worktree")
  const clone = location(second).directory
  const git = (directory: string, ...args: string[]) => command(["git", ...args], directory, runtime.env)
  await git(main, "init", "--initial-branch=main")
  await Bun.write(path.join(main, "fixture.txt"), "Synthetic repository, no user files\n")
  await git(main, "add", "fixture.txt")
  await git(main, "commit", "--message", "Synthetic fixture")
  await git(main, "remote", "add", "origin", "git@github.com:Synthetic-Spool/Retrieval-Fixture.git")
  await git(main, "worktree", "add", "-b", "synthetic-worktree", worktree)
  await git(second.root, "clone", "--no-hardlinks", main, clone)
  await git(clone, "remote", "set-url", "origin", "https://github.com/synthetic-spool/retrieval-fixture.git")
  await seed(first, sharedID, main, "main")
  await seed(first, worktreeID, worktree, "worktree")
  await seed(second, sharedID, clone, "clone")
  const unassignedID = "ses_retrieval_unassigned"
  await seed(first, unassignedID, location(first, "project-b").directory, "unassigned")
  await Promise.all([activate(first, main), activate(first, worktree), activate(second, clone)])
  const captured = await Promise.all([
    converged(first, primary, sharedID), converged(first, primary, worktreeID),
    converged(second, secondary, sharedID), converged(first, primary, unassignedID),
  ])
  assert.deepEqual(captured.map((item) => item.repository), [repoKey, repoKey, repoKey, null])
  pass("actual OpenCode capture → Rust ingestion → PostgreSQL, SSH/HTTPS repository identity across checkout/worktree/clone")

  const listed = await first.client.rpc.call({ rpcID: "spool-retrieval-fixture", method: "tools", input: {}, location: location(first) })
  for (const name of Object.values(names)) assert.ok(listed.output.some((tool: { id: string }) => tool.id === name), `${name} is not registered`)
  const skills = await first.client.skill.list({ location: location(first) })
  assert.ok(skills.data.some((skill) => skill.id === "spool.searching-sessions"))
  const broad = await findAll(first, { query: needle, limit: 1 })
  const reference = `${primary.source_id}/${sharedID}`
  const worktreeReference = `${primary.source_id}/${worktreeID}`
  const secondReference = `${secondary.source_id}/${sharedID}`
  assert.deepEqual(broad.sessions.map((item) => item.session).sort(), [reference, worktreeReference, secondReference, `${primary.source_id}/${unassignedID}`].sort())
  assert.equal(broad.coverage.indexed_sessions, broad.coverage.total_sessions)
  assert.ok(broad.coverage.unassigned_repository_sessions >= 1)
  const filtered = await findAll(first, { query: needle.toUpperCase(), repository: repoKey, limit: 1 })
  assert.deepEqual(filtered.sessions.map((item) => item.session).sort(), [reference, worktreeReference, secondReference].sort())
  const found = filtered.sessions.find((item) => item.session === reference)!
  assert.equal(found.message_count, 4)
  assert.ok(Number.isFinite(Date.parse(found.captured_at)))
  assert.equal(found.revision, captured[0].revision)
  assert.ok(found.match?.read_cursor)
  pass("public registration and skill discovery; cross-source find, exact repository filter, coverage and cursor-only pagination")

  const search = await tool<Searched>(first, "search", { session: reference, revision: found.revision, query: needle, limit: 1 })
  assert.deepEqual(search.matches.map((match) => match.index), [2])
  assert.match(search.matches[0]!.excerpt, /second ordinary match/)
  const ordinaryJump = await tool<Read>(first, "read", { cursor: search.matches[0]!.read_cursor })
  assert.ok(ordinaryJump.messages[0]!.offset_bytes > 50_000)
  assert.match(ordinaryJump.messages[0]!.text, /second ordinary match/)
  assert.ok(search.next_cursor)
  const tail = await tool<Searched>(first, "search", { cursor: search.next_cursor })
  assert.deepEqual(tail.matches.map((match) => match.index), [1])
  assert.equal(tail.next_cursor, null)
  const evidencePage = await tool<Read>(first, "read", { cursor: tail.matches[0]!.read_cursor })
  assert.match(evidencePage.messages[0]!.text, /durable WAL fencing/)
  assert.match(evidencePage.messages[0]!.text, /\\u0000/)
  assert.ok(!evidencePage.messages[0]!.text.includes(Buffer.from("binaryonlyquartz").toString("base64")), "Read exposed inline binary data")
  const independent = await tool<Read>(first, "read", { session: secondReference, start: 1 })
  assert.match(independent.messages[0]!.text, /clone independent evidence/)
  assert.doesNotMatch(independent.messages[0]!.text, /durable WAL fencing/)
  for (const query of ["toolinputquartz", "tooloutputquartz", "reasoningonlyquartz", "binaryonlyquartz", "syntheticonlyquartz"]) {
    assert.deepEqual((await tool<Searched>(first, "search", { session: reference, query })).matches, [])
    const extended = await tool<Searched>(first, "search", { session: reference, query, include_tools: true })
    assert.deepEqual(extended.matches.map((match) => match.index), query.startsWith("tool") ? [2] : [])
  }
  const toolHit = await tool<Searched>(first, "search", { session: reference, query: "toolinputquartz", include_tools: true })
  assert.doesNotMatch(toolHit.matches[0]!.excerpt, /speculative|guess/)
  const rawTool = await tool<Read>(first, "read", { cursor: toolHit.matches[0]!.read_cursor })
  assert.ok(rawTool.messages[0]!.offset_bytes > 50_000)
  assert.match(rawTool.messages[0]!.text, /toolinputquartz/)
  assert.match(rawTool.messages[0]!.text, /tooloutputquartz/)
  pass("literal ordered search and source-qualified evidence; excerpts/jumps skip earlier reasoning hits and tool opt-in excludes reasoning/binary/synthetic text")

  let page = await tool<Read>(first, "read", { session: reference, revision: found.revision, start: 3, end: 4 })
  const staleCursor = page.next_cursor
  assert.ok(staleCursor, "Long synthetic message must require continuation")
  const reconstructed = new Map<number, { text: string; total: number }>()
  for (let pages = 0; ; pages++) {
    assert.ok(pages < 100, "Read cursor did not terminate")
    for (const chunk of page.messages) {
      const previous = reconstructed.get(chunk.index) ?? { text: "", total: chunk.total_bytes }
      assert.equal(chunk.offset_bytes, Buffer.byteLength(previous.text), "Byte continuation skipped or duplicated data")
      assert.equal(chunk.total_bytes, previous.total)
      assert.doesNotMatch(chunk.text, /\ufffd/, "UTF-8 was split incorrectly")
      previous.text += chunk.text
      reconstructed.set(chunk.index, previous)
    }
    if (!page.next_cursor) break
    page = await tool<Read>(first, "read", { cursor: page.next_cursor })
  }
  assert.deepEqual([...reconstructed.keys()], [3, 4])
  for (const message of reconstructed.values()) assert.equal(Buffer.byteLength(message.text), message.total)
  assert.ok(reconstructed.get(3)!.text.includes(longText), "Continuation did not preserve the full original user text")
  assert.match(reconstructed.get(4)!.text, /syntheticonlyquartz continuation/)
  pass("24 KiB bounded read continuation reconstructs long Unicode text and completes the requested inclusive range")

  const agentsBefore = await first.client.agent.list({ location: location(first) })
  const general = JSON.parse((await agent(first, "general")).text)
  const explore = JSON.parse((await agent(first, "explore")).text)
  for (const name of Object.values(names)) {
    assert.ok(general.tools.includes(name), `${name} missing from General`)
    assert.ok(!explore.tools.includes(name), `${name} unexpectedly granted to Explore`)
  }
  const scripted = await agent(first, "general", "scripted")
  assert.match(scripted.text, /Fixture evidence: .* message 1; durable WAL fencing/)
  const executed = scripted.result.messages.flatMap((message) => message.type === "assistant" ? message.content.filter((part) => part.type === "tool") : [])
  assert.deepEqual(executed.map((part) => part.name), ["skill", ...Object.values(names)])
  assert.ok(executed.every((part) => part.state.status === "completed"), JSON.stringify(executed))
  assert.deepEqual((await first.client.agent.list({ location: location(first) })).data.map(({ id, permissions }) => ({ id, permissions })),
    agentsBefore.data.map(({ id, permissions }) => ({ id, permissions })))
  pass("real General agent loop loads the skill, executes find/search/read and receives evidence; Explore permissions unchanged (scripted AI SDK, not provider validation or model judgment)")

  await first.client.debug.location.evict({ location: { directory: worktree } })
  await git(main, "worktree", "remove", "--force", worktree)
  await first.client.session.update({ sessionID: worktreeID, title: "Captured after synthetic worktree removal" })
  assert.equal((await converged(first, primary, worktreeID)).repository, repoKey)
  await first.client.session.remove({ sessionID: worktreeID })
  assert.ok((await findAll(first, { query: needle, repository: repoKey })).sessions.some((item) => item.session === worktreeReference))
  pass("deleted worktree retains captured repository identity; local session deletion retains archived retrieval evidence")

  const error = async (route: keyof typeof names, input: Record<string, JsonValue>, code: string) => {
    const result = await tool<{ error: { code: string; current_revision?: string } }>(first, route, input)
    assert.equal(result.error?.code, code, JSON.stringify(result))
    return result.error
  }
  await error("read", { cursor: staleCursor, start: 1 }, "malformed_request")
  await error("search", { session: reference, query: "\u0000" }, "malformed_request")
  await error("read", { session: `${primary.source_id}/ses_absent` }, "not_found")
  await first.client.session.update({ sessionID: sharedID, title: "Fresh synthetic revision after continuation was issued" })
  const changed = await converged(first, primary, sharedID)
  assert.notEqual(changed.revision, found.revision)
  assert.equal((await error("read", { cursor: staleCursor }, "session_changed")).current_revision, changed.revision)
  await error("search", { session: reference, revision: found.revision, query: needle }, "session_changed")
  pass("real plugin preserves malformed/not-found/session-changed errors and rejects stale evidence cursors")

  // Supplemental legacy snapshot: production capture above always supplies projections.
  const legacy: Source = JSON.parse(await runtime.admin("source", "add", "--label", "synthetic-legacy-only"))
  const transcript: SessionTransferData = await first.client.experimental.session.export({ sessionID: sharedID })
  const legacyID = "ses_retrieval_legacy"
  const response = await fetch(`${runtime.url}/v1/sync/sessions/${legacyID}/snapshot`, {
    method: "POST", signal: runtime.signal, headers: { authorization: `Bearer ${legacy.token}`, "content-type": "application/json" },
    body: JSON.stringify({ base_revision: null, format: "opencode.v2", upstream_version: "synthetic-legacy",
      collector_version: "synthetic-legacy", captured_at: new Date().toISOString(), info: { ...transcript.info, id: legacyID },
      message_order: transcript.messages.map((message) => message.id), upsert_messages: transcript.messages }),
  })
  assert.ok(response.ok, `Legacy fixture upload failed: ${response.status} ${await response.text()}`)
  await error("read", { session: `${legacy.source_id}/${legacyID}` }, "not_indexed")
  await error("search", { session: `${legacy.source_id}/${legacyID}`, query: needle }, "not_indexed")
  const coverage = (await tool<Found>(first, "find", {})).coverage
  assert.equal(coverage.total_sessions - coverage.indexed_sessions, 1)
  pass("legacy raw archives report not_indexed and incomplete coverage without fabricating evidence")

  const disabled = await host("opencode-disabled", JSON.parse(await runtime.admin("source", "add", "--label", "synthetic-reader-disabled")), false)
  await activate(disabled, location(disabled).directory)
  const disabledTools = await disabled.client.rpc.call({ rpcID: "spool-retrieval-fixture", method: "tools", input: {}, location: location(disabled) })
  assert.ok(disabledTools.output.every((tool: { id: string }) => !Object.values(names).includes(tool.id)))
  assert.ok((await disabled.client.skill.list({ location: location(disabled) })).data.every((skill) => skill.id !== "spool.searching-sessions"))
  pass("omitting readerUrl registers neither retrieval tools nor the bundled retrieval skill")
  await runtime.stopReader()
  await error("read", { session: reference }, "reader_unavailable")
  pass("a stopped real Rust browse listener returns reader_unavailable rather than fabricated evidence")
  passed = true
} catch (error) {
  console.error(`Retrieval validation ${runtime.signal.aborted ? "interrupted" : "failed"}; synthetic logs retained in ${root}`)
  if (!runtime.signal.aborted) throw error
} finally {
  try {
    const results = await Promise.allSettled(hosts.map((host) => host.stop()))
    await runtime.down()
    for (const result of results) if (result.status === "rejected") throw result.reason
  } finally { releaseSignals() }
  if (passed) await rm(root, { recursive: true, force: true })
}
if (passed) console.log(JSON.stringify({ result: "passed", cases, tool_calls: calls, synthetic_database: runtime.container, cleanup: "complete" }, null, 2))
