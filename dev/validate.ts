import assert from "node:assert/strict"
import { mkdir, mkdtemp, rm } from "node:fs/promises"
import path from "node:path"
import { setTimeout as delay } from "node:timers/promises"
import { isDeepStrictEqual } from "node:util"
import { command, waitFor } from "./command"
import { activate, mutateWithoutEvent, seed, type Host } from "./fixtures"
import { LocalStack, type Source } from "./stack"

interface Envelope {
  base_revision: string | null
  info: { id: string; metadata?: unknown }
  message_order: string[]
  upsert_messages: { id: string }[]
}
interface Archived {
  revision: string
  info: unknown
  deleted: boolean
  messages: { payload: unknown; xmin: string }[]
}

const repository = path.resolve(import.meta.dir, "..")
const local = path.join(repository, ".local")
await mkdir(local, { recursive: true })
const root = await mkdtemp(path.join(local, "validate-"))
const stack = new LocalStack(root)
const releaseSignals = stack.handleSignals()
const records: { session: string; metadata: unknown; supplied: number; status: number }[] = []
let intercept: ((body: Envelope, request: Request) => Promise<void>) | undefined
let loseAcknowledgement: { session: string; mutate: () => void } | undefined
const proxy = Bun.serve({
  hostname: "127.0.0.1", port: 0,
  async fetch(request) {
    const url = new URL(request.url)
    const body = request.method === "POST" ? await request.text() : undefined
    const envelope: Envelope | undefined = body && url.pathname.endsWith("/snapshot") ? JSON.parse(body) : undefined
    if (envelope && intercept) await intercept(envelope, request)
    // Message polls wait up to 25 seconds for a message.
    const timeout = url.pathname === "/v1/poll" ? 60_000 : 5_000
    const response = await fetch(`${stack.url}${url.pathname}${url.search}`, {
      method: request.method, headers: request.headers, body, redirect: "error", signal: AbortSignal.timeout(timeout),
    }).catch(() => Response.json({ error: { code: "injected_outage" } }, { status: 503 }))
    if (envelope) records.push({ session: envelope.info.id, metadata: envelope.info.metadata, supplied: envelope.upsert_messages.length, status: response.status })
    if (envelope?.info.id === loseAcknowledgement?.session && loseAcknowledgement && response.ok) {
      const fault = loseAcknowledgement
      loseAcknowledgement = undefined
      await response.arrayBuffer()
      fault.mutate()
      return Response.json({ error: { code: "injected_lost_acknowledgement" } }, { status: 503 })
    }
    return response
  },
})

const quote = (value: string) => `'${value.replaceAll("'", "''")}'`
async function archived(source: Source, id: string): Promise<Archived | undefined> {
  const result = await stack.sql(`SELECT json_build_object('revision', revision::text, 'info', info, 'deleted', deleted_at IS NOT NULL,
    'messages', COALESCE((SELECT json_agg(json_build_object('payload', payload, 'xmin', xmin::text) ORDER BY position)
      FROM messages m WHERE m.source_id = s.source_id AND m.session_id = s.session_id), '[]'::json))
    FROM sessions s WHERE source_id = ${quote(source.source_id)} AND session_id = ${quote(id)}`)
  return result ? JSON.parse(result) : undefined
}

async function converged(host: Host, source: Source, id: string, timeout = 30_000): Promise<Archived> {
  return waitFor(`archive convergence: ${id}`, async () => {
    const expected = await host.client.experimental.session.export({ sessionID: id })
    const actual = await archived(source, id)
    if (!actual || actual.deleted) return undefined
    try {
      assert.deepEqual(actual.info, expected.info)
      assert.deepEqual(actual.messages.map((message) => message.payload), expected.messages)
      return actual
    } catch { return undefined }
  }, timeout, stack.signal)
}

const passed: string[] = []
function pass(name: string) { passed.push(name); console.log(`PASS ${name}`) }

try {
  await stack.up()
  const source = await stack.source("primary")
  let host = await stack.openCode("primary", source, proxy.url.origin)
  await activate(host)
  await activate(host, "project-b")
  const id = "ses_spool_primary"
  await seed(host, id)
  let before = await converged(host, source, id)
  assert.equal(before.messages.length, 3)
  const pluginLog = await Bun.file(path.join(host.root, "opencode.log")).text()
  assert.equal(pluginLog.split('"event":"connected"').length - 1, 1, "two active Locations must share one collector")
  pass("real plugin exports ordered structured messages, inline attachment, tool results, Unicode and NUL")

  const metadataStart = records.length
  await host.client.session.update({ sessionID: id, metadata: { fixture: { updated: true } } })
  const metadata = (await host.client.session.get({ sessionID: id })).metadata
  let after = await converged(host, source, id)
  assert.deepEqual(after.messages.map((message) => message.xmin), before.messages.map((message) => message.xmin))
  const metadataUpload = await waitFor("first accepted metadata upload", async () => records.slice(metadataStart).find((record) =>
    record.session === id && record.status === 200 && isDeepStrictEqual(record.metadata, metadata)), 30_000, stack.signal)
  assert.equal(metadataUpload.supplied, 0, "the first accepted upload carrying the new metadata must send no message bodies")
  pass("metadata-only captures send no message bodies and leave PostgreSQL message rows unchanged")

  const updated = (await host.client.session.get({ sessionID: id })).time.updated
  mutateWithoutEvent(host, id, "Missed-event edit, same ID, same count, unchanged timestamp")
  assert.equal((await host.client.session.get({ sessionID: id })).time.updated, updated)
  const silentStart = records.length
  await delay(1_500)
  assert.equal(records.length, silentStart, "silent source edits must not trigger background full audits")
  const commands = await host.client.command.list({ location: { directory: path.join(host.root, "project-a") } })
  assert(commands.data.some((command) => command.name === "spool-repair"))
  await host.client.session.command({ sessionID: id, name: "spool-repair", text: "" })
  await converged(host, source, id)
  const repaired = await archived(source, id)
  const repairStart = records.length
  await host.client.session.command({ sessionID: id, name: "spool-repair", text: "" })
  await delay(1_500)
  assert.equal(records.length, repairStart, "an unchanged full audit must not upload confirmations")
  assert.deepEqual(await archived(source, id), repaired)
  pass("explicit /spool-repair recovers a silent in-place edit; unchanged audits do not upload or advance revisions")

  const idB = "ses_spool_other_location"
  await seed(host, idB, "project-b")
  await converged(host, source, idB)
  await host.client.debug.location.evict({ location: { directory: path.join(host.root, "project-a") } })
  await host.client.session.update({ sessionID: idB, title: "Still captured after another Location closes" })
  await converged(host, source, idB)
  pass("two Location instances share capture and closing one does not stop the other")

  let heating = true
  let edits = 0
  const heater = (async () => {
    while (heating) {
      await host.client.session.update({ sessionID: id, metadata: { hot: edits++ } })
      await delay(50)
    }
  })().catch((error) => { if (!stack.signal.aborted) throw error })
  try {
    await delay(400)
    await host.client.session.update({ sessionID: idB, title: "Cold session progresses while another stays hot" })
    await converged(host, source, idB, 5_000)
    assert(edits >= 8)
    pass("a continuously changing session cannot starve another session")
  } finally { heating = false; await heater }
  await converged(host, source, id)

  await host.client.session.revert.stage({ sessionID: idB, messageID: `msg_${idB}_a`, files: false })
  assert.equal((await converged(host, source, idB)).messages.length, 3, "staged undo retains exported tail")
  await host.client.session.revert.commit({ sessionID: idB })
  assert.equal((await converged(host, source, idB)).messages.length, 1, "committed undo removes exported tail")
  pass("staged undo retains its tail; committed undo replaces the archive with the truncated transcript")

  let competitor = false
  const recordStart = records.length
  intercept = async (body, request) => {
    if (body.info.id !== id || competitor) return
    competitor = true
    intercept = undefined
    const response = await fetch(`${stack.url}/v1/sync/sessions/${id}/snapshot`, {
      method: "POST", headers: request.headers, body: JSON.stringify(body),
    })
    assert(response.ok, `Competing upload failed: ${response.status}`)
    await response.arrayBuffer()
    mutateWithoutEvent(host, id, "Fresh export after conflict, not rebased old capture")
  }
  await host.client.session.update({ sessionID: id, title: "Trigger competing acceptance" })
  await waitFor("conflicting acceptance", async () => competitor ? true : undefined, 30_000, stack.signal)
  await converged(host, source, id)
  assert(records.slice(recordStart).some((record) => record.session === id && record.status === 409))
  assert(records.slice(recordStart).some((record) => record.session === id && record.status === 200 && record.supplied === 3))
  pass("a real revision conflict discards the stale capture and re-exports fresh source data")

  const lostStart = records.length
  loseAcknowledgement = { session: id, mutate: () => mutateWithoutEvent(host, id, "Fresh source after lost acknowledgement") }
  await host.client.session.update({ sessionID: id, title: "Accepted response lost in transit" })
  await waitFor("lost acknowledgement injected", async () => !loseAcknowledgement ? true : undefined, 30_000, stack.signal)
  await converged(host, source, id)
  assert(records.slice(lostStart).filter((record) => record.session === id && record.status === 200).length >= 2)
  pass("an accepted upload with a lost acknowledgement recovers with fresh source data")

  await stack.compose("stop", "server")
  const outageStart = records.length
  await host.client.session.update({ sessionID: id, title: "Changed while service offline" })
  await waitFor("upload during outage", async () => records.slice(outageStart).some((record) => record.status === 503) ? true : undefined, 30_000, stack.signal)
  const oldEndpoint = stack.url
  await stack.compose("start", "--wait", "server")
  await stack.ready()
  console.log(`Container restart endpoint: ${oldEndpoint} → ${stack.url}`)
  await converged(host, source, id)
  pass("service outage retries converge after the container restarts")

  await host.stop()
  const restartStart = records.length
  // Nothing can be uploaded until a Location loads the plugin in the new process.
  host = await stack.openCode("primary", source, proxy.url.origin)
  const offline = "ses_spool_backfill"
  await seed(host, offline)
  await host.client.session.update({ sessionID: idB, title: "Changed before plugin activation" })
  assert.equal(await archived(source, offline), undefined)
  await activate(host)
  before = await converged(host, source, id)
  await converged(host, source, idB)
  await converged(host, source, offline)
  await delay(1_500)
  assert(!records.slice(restartStart).some((record) => record.session === id), "unchanged sessions must not be uploaded on restart")
  const restarted = records.slice(restartStart).find((record) => record.session === idB && record.status === 200)
  assert(restarted)
  assert.equal(restarted.supplied, 0, "changed metadata after restart should retain sparse message deltas")
  pass("startup skips unchanged sessions, captures changed metadata and backfills sessions created before activation")

  const second = await stack.source("second")
  const secondHost = await stack.openCode("second", second, proxy.url.origin)
  await activate(secondHost)
  await seed(secondHost, id, "project-a", "Independent store, identical session and message IDs")
  await converged(secondHost, second, id)
  after = await converged(host, source, id)
  assert.deepEqual(after.messages, before.messages)
  pass("two isolated OpenCode stores with identical IDs remain source-scoped")

  await secondHost.client.session.update({ sessionID: id, title: "Event-driven capture without periodic sweeps" })
  await converged(secondHost, second, id, 5_000)
  pass("source events trigger capture without periodic reconciliation")

  await host.client.session.remove({ sessionID: offline })
  await delay(1_500)
  const retained = await archived(source, offline)
  assert(retained && !retained.deleted && retained.messages.length === 3)
  pass("local deletion retains the remote archive")

  await stack.admin("session", "purge", source.source_id, id)
  await host.client.session.update({ sessionID: id, title: "Must not resurrect" })
  await delay(2_500)
  const purged = await archived(source, id)
  assert(purged?.deleted && purged.info === null && purged.messages.length === 0)
  await converged(secondHost, second, id)
  pass("operator purge erases bodies, blocks resurrection, and leaves the other source intact")

  const db = await stack.compose("port", "db", "5432")
  console.log("Running Rust HTTP/CLI regressions against the isolated PostgreSQL container")
  console.log(await command(["env", `SPOOL_TEST_DATABASE_URL=postgres://spool:local-spool-only@${db}/spool`, "cargo", "test", "--workspace", "--locked"], repository))
  pass("Rust HTTP and CLI regression suite with real PostgreSQL")
  console.log(JSON.stringify({ result: "passed", cases: passed, uploads: records.length, conflicts: records.filter((record) => record.status === 409).length, root }, null, 2))
} catch (error) {
  console.error(`Validation ${stack.signal.aborted ? "interrupted" : "failed"}; isolated logs retained in ${root}`)
  if (!stack.signal.aborted) {
    console.error(await stack.compose("logs", "--tail", "80").catch(() => "Container logs unavailable"))
    throw error
  }
} finally {
  try { await stack.down(true) } finally {
    await proxy.stop(true)
    releaseSignals()
  }
}
if (!stack.signal.aborted) await rm(root, { recursive: true, force: true })
