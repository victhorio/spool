import assert from "node:assert/strict"
import { closeSync, openSync } from "node:fs"
import { mkdir } from "node:fs/promises"
import path from "node:path"
import { command, waitFor } from "../command"

export class RetrievalRuntime {
  readonly container: string
  readonly lifetime = new AbortController()
  readonly signal = this.lifetime.signal
  readonly env: Record<string, string | undefined>
  private readonly stops: (() => Promise<void>)[] = []
  url = ""
  stopServer: () => Promise<void> = async () => {}
  private database = ""

  constructor(readonly root: string, readonly repository: string) {
    this.container = `spool-${path.basename(root)}`.toLowerCase()
    this.env = {
      PATH: process.env.PATH, HOME: path.join(root, "home"),
      XDG_CONFIG_HOME: path.join(root, "config"), XDG_DATA_HOME: path.join(root, "data"),
      XDG_STATE_HOME: path.join(root, "state"), XDG_CACHE_HOME: path.join(root, "cache"),
      TMPDIR: path.join(root, "tmp"), GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null",
      GIT_AUTHOR_NAME: "Synthetic Spool", GIT_AUTHOR_EMAIL: "fixture@example.invalid",
      GIT_COMMITTER_NAME: "Synthetic Spool", GIT_COMMITTER_EMAIL: "fixture@example.invalid",
    }
  }

  handleSignals() {
    const interrupt = (signal: "SIGINT" | "SIGTERM") => {
      process.exitCode = signal === "SIGINT" ? 130 : 143
      this.lifetime.abort(new Error(`Interrupted by ${signal}`))
    }
    const sigint = () => interrupt("SIGINT")
    const sigterm = () => interrupt("SIGTERM")
    process.on("SIGINT", sigint)
    process.on("SIGTERM", sigterm)
    return () => { process.off("SIGINT", sigint); process.off("SIGTERM", sigterm) }
  }

  async up() {
    await Promise.all(["home", "config", "data", "state", "cache", "tmp"].map((name) => mkdir(path.join(this.root, name), { recursive: true })))
    const results = await Promise.allSettled([
      command(["cargo", "build", "--locked", "-p", "spool-server"], this.repository),
      command(["docker", "run", "--detach", "--name", this.container,
        "--publish", "127.0.0.1::5432", "--tmpfs", "/var/lib/postgresql/data:rw,nosuid",
        "--memory", "512m", "-e", "POSTGRES_USER=spool", "-e", "POSTGRES_DB=spool",
        "-e", "POSTGRES_PASSWORD=retrieval-validation-only", "postgres:17-alpine"], this.repository),
    ])
    for (const result of results) if (result.status === "rejected") throw result.reason
    await waitFor("disposable PostgreSQL", async () => {
      this.signal.throwIfAborted()
      return command(["docker", "exec", this.container, "pg_isready", "-U", "spool", "-d", "spool"], this.repository).then(() => true, () => undefined)
    }, 30_000, this.signal)
    const address = await command(["docker", "port", this.container, "5432/tcp"], this.repository)
    assert.match(address, /^127\.0\.0\.1:\d+$/)
    this.database = `postgres://spool:retrieval-validation-only@${address}/spool`
    this.url = await this.start("serve", ["serve"], /Spool listening on (127\.0\.0\.1:\d+)/)
    await waitFor("Rust ingestion migrations/readiness", async () => {
      const response = await fetch(`${this.url}/healthz`, { signal: AbortSignal.any([this.signal, AbortSignal.timeout(2_000)]) }).catch(() => undefined)
      return response?.ok ? true : undefined
    }, 30_000, this.signal)
  }

  private async start(label: string, args: string[], pattern: RegExp) {
    const filename = path.join(this.root, `${label}.log`)
    const log = openSync(filename, "a", 0o600)
    const child = Bun.spawn([path.join(this.repository, "target", "debug", "spool-server"), ...args], {
      cwd: this.root, env: { ...this.env, DATABASE_URL: this.database, BIND_ADDR: "127.0.0.1", PORT: "0" },
      stdin: "ignore", stdout: log, stderr: log,
    })
    closeSync(log)
    let stopping: Promise<void> | undefined
    const stop = () => stopping ??= (async () => {
      if (child.exitCode !== null) return
      child.kill("SIGTERM")
      const timeout = setTimeout(() => { if (child.exitCode === null) child.kill("SIGKILL") }, 10_000)
      try { await child.exited } finally { clearTimeout(timeout) }
    })()
    this.stops.push(stop)
    if (label === "serve") this.stopServer = stop
    return waitFor(`Rust ${label} listener`, async () => {
      const output = await Bun.file(filename).text()
      assert.equal(child.exitCode, null, `${label} exited: ${output}`)
      const address = pattern.exec(output)?.[1]
      return address ? `http://${address}` : undefined
    }, 30_000, this.signal)
  }

  admin(...args: string[]) {
    this.signal.throwIfAborted()
    return command([path.join(this.repository, "target", "debug", "spool-server"), ...args], this.root, { ...this.env, DATABASE_URL: this.database })
  }

  sql(query: string) {
    this.signal.throwIfAborted()
    return command(["docker", "exec", this.container, "psql", "-U", "spool", "-d", "spool", "-X", "-A", "-t", "-v", "ON_ERROR_STOP=1", "-c", query], this.repository)
  }

  async down() {
    const results = await Promise.allSettled(this.stops.map((stop) => stop()))
    await command(["docker", "rm", "--force", "--volumes", this.container], this.repository).catch(async (error) => {
      const remaining = await command(["docker", "ps", "--all", "--filter", `name=^/${this.container}$`, "--format", "{{.Names}}"], this.repository)
      if (remaining) throw error
    })
    assert.equal(await command(["docker", "ps", "--all", "--filter", `name=^/${this.container}$`, "--format", "{{.Names}}"], this.repository), "", "Disposable PostgreSQL leaked")
    for (const result of results) if (result.status === "rejected") throw result.reason
  }
}
