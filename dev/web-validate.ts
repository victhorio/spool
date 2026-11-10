import assert from "node:assert/strict"
import { mkdir, mkdtemp, rm } from "node:fs/promises"
import path from "node:path"
import { startAccess } from "./access"
import { command, waitFor } from "./command"
import { LocalStack, type Source } from "./stack"
import { envelope, fixtures } from "../web/tests/fixtures"

const repository = path.resolve(import.meta.dir, "..")
const local = path.join(repository, ".local")
await mkdir(local, { recursive: true })
const root = await mkdtemp(path.join(local, "web-validate-"))
const stack = new LocalStack(root)
const releaseSignals = stack.handleSignals()
let browser: Bun.Subprocess | undefined
let access: Awaited<ReturnType<typeof startAccess>> | undefined
let tests: Bun.Subprocess<"ignore", "pipe", "pipe"> | undefined
let passed = false
const stopTests = () => tests?.kill("SIGTERM")
stack.signal.addEventListener("abort", stopTests, { once: true })

try {
  const startup = await Promise.allSettled([
    stack.up(),
    command(["bun", "run", "web:build"], repository),
    command(["cargo", "build", "--locked", "--package", "spool-server"], repository),
  ])
  for (const result of startup) if (result.status === "rejected") throw result.reason
  stack.signal.throwIfAborted()
  const sources: Record<string, Source> = {
    studio: await stack.source("studio"),
    laptop: await stack.source("laptop"),
  }
  for (const fixture of fixtures) {
    const response = await fetch(`${stack.url}/v1/sync/sessions/${fixture.id}/snapshot`, {
      method: "POST",
      headers: {
        authorization: `Bearer ${sources[fixture.source]!.token}`,
        "content-type": "application/json",
      },
      body: JSON.stringify(envelope(fixture)),
      signal: stack.signal,
    })
    assert.equal(response.status, 201, `Failed to seed ${fixture.title}`)
  }
  const address = await stack.compose("port", "db", "5432")
  const log = path.join(root, "serve.log")
  access = await startAccess()
  browser = Bun.spawn([path.join(repository, "target/debug/spool-server"), "serve"], {
    cwd: repository,
    env: {
      ...process.env,
      ...access.env,
      DATABASE_URL: `postgres://spool:local-spool-only@${address}/spool`,
      BIND_ADDR: "127.0.0.1",
      PORT: "0",
      ASSETS_DIR: path.join(repository, "web/dist"),
    },
    stdout: "ignore",
    stderr: Bun.file(log),
  })
  const url = await waitFor(
    "web reader",
    async () => {
      if (browser?.exitCode !== null)
        throw new Error(`Server exited: ${await Bun.file(log).text()}`)
      const output = await Bun.file(log).text()
      const address = output.match(/Spool listening on (127\.0\.0\.1:\d+) \(web reader enabled\)/)?.[1]
      return address ? `http://${address}` : undefined
    },
    30_000,
    stack.signal,
  )
  const unauthenticated = await fetch(`${url}/api/sessions`, { signal: stack.signal })
  assert.equal(unauthenticated.status, 403, "the reader must require an Access assertion")
  console.log(`Testing the built frontend against real Rust HTTP, synthetic Access and isolated PostgreSQL: ${url}`)
  stack.signal.throwIfAborted()
  tests = Bun.spawn([path.join(repository, "web/node_modules/.bin/playwright"), "test"], {
    cwd: path.join(repository, "web"),
    env: {
      ...process.env,
      SPOOL_WEB_URL: url,
      SPOOL_WEB_FIXTURES: JSON.stringify(sources),
      SPOOL_WEB_ACCESS: access.assertion,
      SPOOL_INGEST_URL: stack.url,
    },
    stdin: "ignore",
    stdout: "pipe",
    stderr: "pipe",
  })
  const [stdout, stderr, code] = await Promise.all([
    new Response(tests.stdout).text(),
    new Response(tests.stderr).text(),
    tests.exited,
  ])
  console.log(stdout)
  if (code !== 0) throw new Error(`Browser tests exited ${code}: ${stderr}`)
  stack.signal.throwIfAborted()
  passed = true
} catch (error) {
  if (!stack.signal.aborted) throw error
} finally {
  stack.signal.removeEventListener("abort", stopTests)
  if (tests?.exitCode === null) {
    tests.kill("SIGTERM")
    await tests.exited
  }
  if (browser) {
    browser.kill("SIGTERM")
    await browser.exited
  }
  await access?.stop()
  try {
    await stack.down(true)
  } finally {
    releaseSignals()
  }
  if (passed) await rm(root, { recursive: true, force: true })
  else console.error(`Validation logs retained in ${root}`)
}
