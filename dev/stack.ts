import { createHash } from "node:crypto"
import { chmod, mkdir, readdir } from "node:fs/promises"
import path from "node:path"
import { Service } from "@opencode/client/service"
import { command, waitFor } from "./command"
import { startOpenCode } from "./opencode"

export interface Source { source_id: string; token: string }

const repository = path.resolve(import.meta.dir, "..")

export class LocalStack {
  readonly project: string
  url = ""
  private readonly lifetime = new AbortController()
  readonly signal = this.lifetime.signal
  private readonly hosts: Awaited<ReturnType<typeof startOpenCode>>[] = []
  private cleanup: Promise<void> | undefined

  constructor(readonly root: string) {
    this.project = `spool-${createHash("sha256").update(root).digest("hex").slice(0, 12)}`
  }

  handleSignals(): () => void {
    const interrupt = (signal: "SIGINT" | "SIGTERM") => {
      if (this.signal.aborted) return
      process.exitCode = signal === "SIGINT" ? 130 : 143
      this.lifetime.abort(new Error(`Interrupted by ${signal}`))
    }
    const sigint = () => interrupt("SIGINT")
    const sigterm = () => interrupt("SIGTERM")
    process.on("SIGINT", sigint)
    process.on("SIGTERM", sigterm)
    return () => {
      process.off("SIGINT", sigint)
      process.off("SIGTERM", sigterm)
    }
  }

  private dockerCompose(...args: string[]): Promise<string> {
    return command(["docker", "compose", "--project-name", this.project, "--file", path.join(repository, "compose.yaml"), ...args], repository)
  }

  async compose(...args: string[]): Promise<string> {
    this.signal.throwIfAborted()
    // Let in-flight startup finish before cleanup, or Docker could create resources afterwards.
    const output = await this.dockerCompose(...args)
    this.signal.throwIfAborted()
    return output
  }

  async up(): Promise<void> {
    await mkdir(this.root, { recursive: true, mode: 0o700 })
    console.log(`Building and starting isolated containers (${this.project})`)
    await this.compose("up", "--build", "--detach", "--wait", "db", "server")
    await this.ready()
  }

  async ready(): Promise<void> {
    const address = await this.compose("port", "server", "8080")
    this.url = `http://${address}`
    await waitFor("Spool readiness", async () => {
      const response = await fetch(`${this.url}/healthz`, { signal: AbortSignal.timeout(2_000) }).catch(() => undefined)
      return response?.ok ? true : undefined
    }, 30_000, this.signal)
  }

  admin(...args: string[]): Promise<string> {
    return this.compose("exec", "--no-TTY", "server", "spool-server", ...args)
  }

  sql(query: string): Promise<string> {
    return this.compose("exec", "--no-TTY", "db", "psql", "-U", "spool", "-d", "spool", "-X", "-A", "-t", "-v", "ON_ERROR_STOP=1", "-c", query)
  }

  async source(label: string): Promise<Source> {
    const file = path.join(this.root, `${label}.json`)
    if (await Bun.file(file).exists()) return Bun.file(file).json()
    const source: Source = JSON.parse(await this.admin("source", "add", "--label", label))
    await Bun.write(file, JSON.stringify(source))
    await chmod(file, 0o600)
    return source
  }

  async openCode(label: string, source: Source, url = this.url, sweepIntervalMs = 15 * 60_000) {
    const tokenFile = path.join(this.root, `${label}.token`)
    await Bun.write(tokenFile, source.token)
    await chmod(tokenFile, 0o600)
    const host = await startOpenCode(path.join(this.root, `opencode-${label}`), path.join(repository, "plugin"), { url, tokenFile, sweepIntervalMs }, this.signal)
    this.hosts.push(host)
    return host
  }

  down(removeVolumes = false): Promise<void> {
    return this.cleanup ??= this.shutdown(removeVolumes)
  }

  private async shutdown(removeVolumes: boolean): Promise<void> {
    for (const host of this.hosts) await host.stop()
    const directories = await readdir(this.root).catch(() => [])
    for (const directory of directories.filter((name) => name.startsWith("opencode-"))) {
      const state = path.join(this.root, directory, "state", "opencode")
      const files = await readdir(state).catch(() => [])
      for (const file of files.filter((name) => /^service(?:-[\w.-]+)?\.json$/.test(name))) {
        const registration = path.join(state, file)
        if (await Service.discover({ file: registration })) await Service.stop({ file: registration })
      }
    }
    await this.dockerCompose("down", ...(removeVolumes ? ["--volumes"] : []), "--remove-orphans")
  }
}

if (import.meta.main) {
  const stack = new LocalStack(path.join(repository, ".local", "dev"))
  if (process.argv[2] === "down") {
    await stack.down()
  } else if (process.argv[2] === "up") {
    const releaseSignals = stack.handleSignals()
    try {
      await stack.up()
      const source = await stack.source("primary")
      const host = await stack.openCode("primary", source)
      await host.client.plugin.list({ location: { directory: path.join(host.root, "project-a") } })
      console.log(`Spool: ${stack.url}/healthz\nOpenCode: ${host.endpoint.url} (isolated credentials/state in ${stack.root})\nCtrl-C stops the stack; local database volumes are retained.`)
      await host.child.exited
    } catch (error) {
      if (!stack.signal.aborted) throw error
    } finally {
      try { await stack.down() } finally { releaseSignals() }
    }
  } else throw new Error("Usage: bun dev/stack.ts up|down")
}
