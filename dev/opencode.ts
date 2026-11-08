import { closeSync, openSync } from "node:fs"
import { mkdir, readdir } from "node:fs/promises"
import path from "node:path"
import { OpenCode } from "@opencode/client"
import { Service } from "@opencode/client/service"
import { waitFor } from "./command"

export async function startOpenCode(root: string, plugin: string, options: Record<string, unknown>, signal: AbortSignal) {
  signal.throwIfAborted()
  await Promise.all(["home", "config", "state", "data", "cache", "tmp", "project-a", "project-b"].map((name) => mkdir(path.join(root, name), { recursive: true })))
  await Bun.write(path.join(root, "config", "opencode.json"), JSON.stringify({ plugins: [{ package: plugin, options }] }))
  const cwd = path.join(root, "home")
  const env = {
    PATH: process.env.PATH,
    HOME: cwd,
    OPENCODE_TEST_HOME: cwd,
    XDG_CONFIG_HOME: path.join(root, "config"),
    XDG_DATA_HOME: path.join(root, "data"),
    XDG_CACHE_HOME: path.join(root, "cache"),
    XDG_STATE_HOME: path.join(root, "state"),
    TMPDIR: path.join(root, "tmp"),
    OPENCODE_CONFIG_DIR: path.join(root, "config"),
    OPENCODE_CONFIG_PROJECT_DISABLE: "1",
    OPENCODE_DISABLE_CLAUDE_CODE: "1",
    OPENCODE_DISABLE_MODELS_FETCH: "1",
    OPENCODE_DISABLE_AUTOUPDATE: "1",
    OPENCODE_FILEWATCHER_DISABLE: "1",
    OPENCODE_DISABLE_FFF: "1",
  }
  signal.throwIfAborted()
  const log = openSync(path.join(root, "opencode.log"), "a", 0o600)
  const child = Bun.spawn(["opencode", "serve", "--service", "--hostname", "127.0.0.1", "--port", "0"], {
    cwd,
    env,
    stdin: "ignore",
    stdout: log,
    stderr: log,
  })
  closeSync(log)
  let stopping: Promise<void> | undefined
  const stop = () => stopping ??= (async () => {
    if (child.exitCode !== null) return
    child.kill("SIGTERM")
    const timer = setTimeout(() => { if (child.exitCode === null) child.kill("SIGKILL") }, 10_000)
    try { await child.exited } finally { clearTimeout(timer) }
  })()
  const interrupt = () => { void stop() }
  signal.addEventListener("abort", interrupt, { once: true })
  void child.exited.then(() => signal.removeEventListener("abort", interrupt))
  try {
    const endpoint = await waitFor("isolated OpenCode", async () => {
      if (child.exitCode !== null) throw new Error(`OpenCode exited ${child.exitCode}; inspect ${path.join(root, "opencode.log")}`)
      const directory = path.join(root, "state", "opencode")
      const files = await readdir(directory).catch(() => [])
      const registration = files.find((name) => /^service(?:-[\w.-]+)?\.json$/.test(name))
      if (!registration) return undefined
      return Service.discover({ file: path.join(directory, registration) })
    }, 30_000, signal)
    const client = OpenCode.make({
      baseUrl: endpoint.url,
      headers: Service.headers(endpoint),
      fetch: Object.assign(
        (input: Parameters<typeof fetch>[0], init?: RequestInit) => fetch(input, { ...init, signal: AbortSignal.any([signal, AbortSignal.timeout(15_000), ...(init?.signal ? [init.signal] : [])]) }),
        { preconnect: fetch.preconnect },
      ),
    })
    const info = await client.server.info()
    if (info.pid !== child.pid) throw new Error("Discovered a different OpenCode process")
    console.log(`Isolated OpenCode ${info.version}`)
    return { child, client, endpoint, root, signal, stop }
  } catch (error) {
    await stop()
    throw error
  }
}
