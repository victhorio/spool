import { createHash } from "node:crypto"
import { readFile } from "node:fs/promises"
import { homedir } from "node:os"
import path from "node:path"

export interface Config {
  url: string
  token: string
  sourceID: string
  cachePrefix: string
  sweepIntervalMs: number
  serviceFile?: string
  opencodeUrl?: string
  opencodePassword?: string
}

function filename(value: unknown, name: string): string {
  if (typeof value !== "string" || !value) throw new Error(`Spool requires ${name}`)
  const expanded = value.startsWith("~/") ? path.join(homedir(), value.slice(2)) : value
  if (!path.isAbsolute(expanded)) throw new Error(`Spool ${name} must be an absolute path`)
  return expanded
}

function url(value: unknown, name: string): string {
  if (typeof value !== "string") throw new Error(`Spool requires ${name}`)
  const parsed = new URL(value)
  const local = ["localhost", "127.0.0.1", "[::1]"].includes(parsed.hostname)
  if (parsed.username || parsed.password || parsed.search || parsed.hash ||
      (parsed.protocol !== "https:" && !(parsed.protocol === "http:" && local))) {
    throw new Error(`Spool ${name} requires HTTPS (HTTP is allowed only on loopback), without URL credentials or query parameters`)
  }
  return parsed.toString().replace(/\/$/, "")
}

export async function configure(options: Record<string, unknown>): Promise<Config> {
  const allowed = new Set(["url", "tokenFile", "sweepIntervalMs", "serviceFile", "opencodeUrl", "opencodePasswordFile"])
  for (const key of Object.keys(options)) if (!allowed.has(key)) throw new Error(`Unknown Spool option: ${key}`)
  const destination = url(options.url, "url")
  const token = (await readFile(filename(options.tokenFile, "tokenFile"), "utf8")).trim()
  const match = /^spool_v1\.([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})\.([A-Za-z0-9_-]{43})$/.exec(token)
  if (!match) throw new Error("Spool tokenFile does not contain a valid source token")
  const sweepIntervalMs = options.sweepIntervalMs ?? 15 * 60_000
  if (typeof sweepIntervalMs !== "number" || !Number.isSafeInteger(sweepIntervalMs) || sweepIntervalMs < 1_000 || sweepIntervalMs > 86_400_000) {
    throw new Error("Spool sweepIntervalMs must be an integer between 1000 and 86400000")
  }
  if (options.serviceFile !== undefined && options.opencodeUrl !== undefined) throw new Error("Choose either serviceFile or opencodeUrl")
  if ((options.opencodeUrl === undefined) !== (options.opencodePasswordFile === undefined)) throw new Error("opencodeUrl and opencodePasswordFile must be supplied together")
  const sourceID = match[1]!
  return {
    url: destination,
    token,
    sourceID,
    cachePrefix: `v1/${createHash("sha256").update(`${destination}\0${sourceID}`).digest("hex")}/`,
    sweepIntervalMs,
    serviceFile: options.serviceFile === undefined ? undefined : filename(options.serviceFile, "serviceFile"),
    opencodeUrl: options.opencodeUrl === undefined ? undefined : url(options.opencodeUrl, "opencodeUrl"),
    opencodePassword: options.opencodePasswordFile === undefined ? undefined : (await readFile(filename(options.opencodePasswordFile, "opencodePasswordFile"), "utf8")).trim(),
  }
}
