import { execFile } from "node:child_process"
import { stat } from "node:fs/promises"
import path from "node:path"

export function normalizeRepository(remote: string): string | null {
  if (!remote || /[\s\0]/u.test(remote)) return null
  const scp = /^(?:[^/@:]+@)?(\[[^\]]+\]|[^/:]+):(.+)$/.exec(remote)
  let url: URL
  try {
    url = new URL(remote.includes("://") ? remote : scp ? `ssh://${scp[1]}/${scp[2]}` : "")
  } catch { return null }
  if (!["https:", "http:", "ssh:", "git:"].includes(url.protocol) || !url.hostname || url.search || url.hash) return null
  const host = url.hostname.toLowerCase()
  const port = url.port && !(url.protocol === "ssh:" && url.port === "22") && !(url.protocol === "git:" && url.port === "9418") ? `:${url.port}` : ""
  let repository = url.pathname.replace(/^\/+|\/+$/g, "")
  if (host === "github.com") repository = repository.toLowerCase()
  repository = repository.replace(/\.git$/, "")
  if (!repository || repository.split("/").some((part) => !part || part === "." || part === "..")) return null
  const key = `${host}${port}/${repository}`
  return Buffer.byteLength(key) <= 1024 ? key : null
}

function git(directory: string, args: string[]): Promise<string> {
  const env: NodeJS.ProcessEnv = { ...process.env, GIT_TERMINAL_PROMPT: "0", GIT_OPTIONAL_LOCKS: "0" }
  for (const key of Object.keys(env)) if (key.startsWith("GIT_") && !["GIT_TERMINAL_PROMPT", "GIT_OPTIONAL_LOCKS"].includes(key)) delete env[key]
  return new Promise((resolve, reject) => {
    execFile("git", args, { cwd: directory, env, timeout: 2_000, maxBuffer: 64 * 1024, encoding: "utf8" }, (error, stdout) => {
      if (error) reject(error)
      else resolve(stdout.trim())
    })
  })
}

export async function resolveRepository(directory: unknown, remote?: string): Promise<string | null> {
  if (typeof directory !== "string" || !path.isAbsolute(directory) || directory.includes("\0")) return null
  try {
    if (!(await stat(directory)).isDirectory()) return null
    // One process lists every remote, with its fetch URL after any insteadOf rewriting.
    const fetchURLs = new Map<string, string | undefined>()
    for (const line of (await git(directory, ["remote", "-v"])).split("\n")) {
      const [name, entry] = line.split("\t", 2)
      if (!name) continue
      const fetchURL = entry?.endsWith(" (fetch)") ? entry.slice(0, -" (fetch)".length) : undefined
      fetchURLs.set(name, fetchURL ?? fetchURLs.get(name))
    }
    const selected = remote ?? (fetchURLs.has("origin") ? "origin" : fetchURLs.size === 1 ? fetchURLs.keys().next().value : undefined)
    const fetchURL = selected === undefined ? undefined : fetchURLs.get(selected)
    return fetchURL === undefined ? null : normalizeRepository(fetchURL)
  } catch { return null }
}
