import assert from "node:assert/strict"
import { mkdir, mkdtemp, rm } from "node:fs/promises"
import path from "node:path"
import { command, waitFor } from "./command"
import { LocalStack } from "./stack"
import { envelope, fixtures } from "../web/tests/fixtures"

const repository = path.resolve(import.meta.dir, "..")
await mkdir(path.join(repository, ".local"), { recursive: true })
const root = await mkdtemp(path.join(repository, ".local", "reader-validate-"))
const stack = new LocalStack(root)
const releaseSignals = stack.handleSignals()
const prefix = stack.project
const network = `${prefix}-private`
const volume = `${prefix}-identity`
const image = `${prefix}-reader`
const controlImage = `${prefix}-control`
const containers = ["reader", "client", "control", "invalid", "missing"].map(name => `${prefix}-${name}`)
const [reader, client, control, invalid, missing] = containers as [string, string, string, string, string]
let passed = false

async function docker(...args: string[]) {
  stack.signal.throwIfAborted()
  const result = await command(["docker", ...args], repository)
  stack.signal.throwIfAborted()
  return result
}

async function logs(name: string) {
  const child = Bun.spawn(["docker", "logs", name], { stdout: "pipe", stderr: "pipe" })
  const [stdout, stderr] = await Promise.all([
    new Response(child.stdout).text(), new Response(child.stderr).text(), child.exited,
  ])
  return stdout + stderr
}

async function stopped(name: string, timeout = 20_000) {
  return waitFor(`${name} shutdown`, async () => {
    const state = JSON.parse(await docker("inspect", "--format", "{{json .State}}", name))
    return state.Running ? undefined : state
  }, timeout, stack.signal)
}

const database = "postgres://spool_reader:reader-validation-only@db:5432/spool"
const tailnetEnv = [
  "-e", "TS_EXTRA_ARGS=--login-server=http://control:8080",
  "-e", "TS_LOG_TARGET=http://control:8080/log",
  "-e", "TS_PANIC_IF_HIT_MAIN_CONTROL=1",
  "-e", "TS_DISABLE_PORTMAPPER=1",
  "-e", "TS_ASSUME_NETWORK_UP_FOR_TEST=1",
]

async function startReader(name: string, key?: string, persist = false) {
  await docker("run", "--detach", "--name", name, "--network", network,
    "--security-opt", "no-new-privileges", "--memory", "256m",
    ...tailnetEnv, "-e", `DATABASE_URL=${database}`, "-e", "TS_HOSTNAME=reader",
    ...(key ? ["-e", `TS_AUTHKEY=${key}`] : []),
    ...(persist ? ["--mount", `type=volume,source=${volume},target=/var/lib/tailscale,volume-nocopy`] : []), image)
}

async function ready() {
  await waitFor("private reader readiness", async () => {
    const state = JSON.parse(await docker("inspect", "--format", "{{json .State}}", reader))
    assert.ok(state.Running, `Reader exited ${state.ExitCode}; see ${root}`)
    return await docker("exec", reader, "reader-health").then(() => true, () => undefined)
  }, 90_000, stack.signal)
}

async function request(url: string, ...args: string[]) {
  const result = await docker("exec", client, "curl", "--silent", "--show-error", "--max-time", "10",
    "--socks5-hostname", "127.0.0.1:1055", "--write-out", "\n%{http_code}", ...args, url)
  const boundary = result.lastIndexOf("\n")
  return { status: Number(result.slice(boundary + 1)), body: result.slice(0, boundary) }
}

try {
  const startup = await Promise.allSettled([
    stack.up(),
    docker("build", "--file", "deploy/reader.Dockerfile", "--tag", image, "."),
    docker("build", "--file", "dev/reader-control/Dockerfile", "--tag", controlImage, "."),
  ])
  for (const result of startup) if (result.status === "rejected") throw result.reason
  await stack.sql(await Bun.file(path.join(repository, "deploy/reader-role.sql")).text())
  await stack.sql("ALTER ROLE spool_reader LOGIN PASSWORD 'reader-validation-only'")
  const source = await stack.source("synthetic-reader-source")
  const fixture = fixtures[0]!
  const upload = async (snapshot: ReturnType<typeof envelope>) => {
    const response = await fetch(`${stack.url}/v1/sync/sessions/${fixture.id}/snapshot`, {
      method: "POST", signal: stack.signal,
      headers: { authorization: `Bearer ${source.token}`, "content-type": "application/json" },
      body: JSON.stringify(snapshot),
    })
    assert.ok(response.ok, `Synthetic upload failed: ${response.status}`)
    return response.json() as Promise<{ revision: string }>
  }
  const accepted = await upload(envelope(fixture))
  await docker("network", "create", "--internal", network)
  await docker("network", "connect", "--alias", "db", network, await stack.compose("ps", "--quiet", "db"))
  await docker("volume", "create", volume)
  assert.equal(await docker("run", "--rm", "--network", "none",
    "--mount", `type=volume,source=${volume},target=/var/lib/tailscale,volume-nocopy`,
    "--entrypoint", "stat", image, "-c", "%u", "/var/lib/tailscale"), "0")
  await docker("run", "--detach", "--name", control, "--network", network, "--network-alias", "control", controlImage)

  await startReader(missing)
  assert.notEqual((await stopped(missing)).ExitCode, 0)
  assert.match(await logs(missing), /first enrollment requires TS_AUTHKEY/)
  await startReader(invalid, "invalid-synthetic-key")
  assert.notEqual((await stopped(invalid, 90_000)).ExitCode, 0)
  console.log("PASS: missing and invalid enrollment fail closed on an egress-isolated network")

  await startReader(reader, "synthetic-reader-enrollment-only", true)
  await ready()
  const status = JSON.parse(await docker("exec", reader, "tailscale", "--socket=/run/spool/tailscaled.sock", "status", "--json"))
  const address = status.TailscaleIPs.find((ip: string) => !ip.includes(":"))
  assert.ok(address)
  const url = `http://${address}:8090`
  const processes = await docker("top", reader, "-eo", "pid,user,comm")
  for (const name of ["spool-server", "tailscaled", "containerboot"])
    assert.match(processes, new RegExp(`10001\\s+${name}`))
  assert.equal(await docker("exec", reader, "stat", "-c", "%a:%u", "/var/lib/tailscale"), "700:10001")

  await docker("run", "--detach", "--name", client, "--network", network, "--user", "10001:10001",
    "--cap-drop", "ALL", "--security-opt", "no-new-privileges", "--entrypoint", "containerboot",
    ...tailnetEnv, "-e", "TS_AUTHKEY=synthetic-reader-enrollment-only", "-e", "TS_HOSTNAME=client",
    "-e", "TS_USERSPACE=true", "-e", "TS_STATE_DIR=/tmp/client", "-e", "TS_SOCKET=/tmp/client.sock",
    "-e", "TS_SOCKS5_SERVER=127.0.0.1:1055", image)
  await waitFor("real userspace tailnet path", async () => {
    const response = await request(`${url}/api/sessions`).catch(() => undefined)
    return response?.status === 200 ? true : undefined
  }, 60_000, stack.signal)
  const inventory = await request(`${url}/api/sessions`, "-H", `Origin: ${url}`)
  assert.equal(inventory.status, 200)
  assert.equal(JSON.parse(inventory.body).sessions[0].info.title, fixture.title)
  const sessionURL = `${url}/api/sources/${source.source_id}/sessions/${fixture.id}`
  const transcript = await request(sessionURL)
  assert.equal(transcript.status, 200)
  assert.deepEqual(JSON.parse(transcript.body).messages, fixture.messages.slice(0, 100))
  const page = await request(`${url}/`)
  assert.equal(page.status, 200)
  const asset = page.body.match(/src="([^"]+\.js)"/)?.[1]
  assert.ok(asset)
  assert.equal((await request(`${url}${asset}`)).status, 200)
  assert.equal((await request(`${url}/api/sessions`, "-H", "Host: reader.example.ts.net:8090")).status, 403)
  assert.equal((await request(`${url}/api/sessions`, "-H", `Origin: https://${address}:8090`)).status, 403)
  assert.equal((await request(`${url}/api/sessions`, "-H", "Origin: https://foreign.invalid")).status, 403)
  assert.equal((await request(`${url}/api/sessions`, "-X", "POST")).status, 405)
  await assert.rejects(docker("exec", client, "curl", "--fail", "--max-time", "2", `http://${reader}:8090/`))
  assert.equal(await docker("exec", client, "curl", "--silent", "--output", "/dev/null", "--write-out", "%{http_code}", `http://${reader}:9002/api/sessions`), "404")
  await assert.rejects(request(`http://${address}:9002/healthz`, "--max-time", "2"))
  console.log("PASS: real encrypted userspace TCP reads, built assets, Host/Origin/write guards, private binding and port policy")

  const updated = envelope(fixture)
  updated.base_revision = accepted.revision
  updated.info = { ...updated.info, title: "Live archive update" }
  await upload(updated)
  assert.equal(JSON.parse((await request(sessionURL)).body).info.title, "Live archive update")
  await docker("stop", "--time", "15", reader)
  assert.equal((await stopped(reader)).ExitCode, 143)
  await docker("rm", reader)
  await startReader(reader, undefined, true)
  await ready()
  const restarted = JSON.parse(await docker("exec", reader, "tailscale", "--socket=/run/spool/tailscaled.sock", "status", "--json"))
  assert.equal(restarted.Self.ID, status.Self.ID)
  assert.deepEqual(restarted.TailscaleIPs, status.TailscaleIPs)
  await waitFor("restarted tailnet path", async () =>
    (await request(sessionURL).catch(() => undefined))?.status === 200 ? true : undefined,
    30_000, stack.signal)
  console.log("PASS: live archive updates and authenticated identity/IP persist without the enrollment key")

  await docker("exec", reader, "bash", "-c", 'for f in /proc/[0-9]*/comm; do read -r name < "$f" || continue; if [[ $name == tailscaled ]]; then pid=${f#/proc/}; kill -TERM "${pid%/comm}"; fi; done')
  assert.notEqual((await stopped(reader)).ExitCode, 0)
  console.log("PASS: daemon failure stops the reader and exits nonzero")
  passed = true
} catch (error) {
  if (!stack.signal.aborted) throw error
} finally {
  let exposedKey = false
  for (const name of containers) {
    const log = await logs(name)
    await Bun.write(path.join(root, `${name}.log`), log)
    exposedKey ||= log.includes("synthetic-reader-enrollment-only") || log.includes("invalid-synthetic-key")
  }
  try {
    await command(["docker", "rm", "--force", ...containers], repository).catch(() => {})
    await stack.down(true).catch(error => console.error(error))
    await command(["docker", "network", "rm", network], repository).catch(() => {})
    await command(["docker", "volume", "rm", volume], repository).catch(() => {})
    await command(["docker", "image", "rm", image, controlImage], repository).catch(() => {})
    for (const resource of ["container", "network", "volume"]) {
      const remaining = await command(["docker", resource, "ls", ...(resource === "container" ? ["--all"] : []),
        "--filter", `name=${prefix}`, "--format", resource === "container" ? "{{.Names}}" : "{{.Name}}"], repository)
      assert.equal(remaining, "", `Validation leaked ${resource}: ${remaining}`)
    }
  } finally {
    releaseSignals()
  }
  assert.equal(exposedKey, false, "An enrollment-key sentinel appeared in container logs")
  if (passed) await rm(root, { recursive: true, force: true })
  else console.error(`Reader validation logs retained in ${root}`)
}
