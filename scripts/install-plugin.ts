// Points the global OpenCode configuration at this checkout's commit of the Spool
// plugin, pinned by full hash. OpenCode installs the package itself, so the checkout
// is not used after this. Usage: bun run install-plugin [--url …] [--token-file …]
// [--source-alias …] [--dry-run]. Existing Spool options are kept unless overridden.
// Without a token file, it logs the machine in: the owner approves it in the reader.

import { existsSync } from "node:fs"
import { mkdir, open, readFile, realpath, rename, rm, stat, writeFile } from "node:fs/promises"
import { homedir } from "node:os"
import path from "node:path"
import { fileURLToPath } from "node:url"
import { parseArgs } from "node:util"
import { DeviceLoginError, deviceLogin } from "./device-login"

type Entry = string | { package: string; options?: Record<string, unknown> }

const repository = path.resolve(import.meta.dir, "..")
const { values: flags } = parseArgs({
  options: {
    url: { type: "string" },
    "token-file": { type: "string" },
    "source-alias": { type: "string" },
    "dry-run": { type: "boolean" },
  },
})

function fail(message: string): never {
  console.error(`install-plugin: ${message}`)
  process.exit(1)
}

async function git(...args: string[]): Promise<string> {
  const child = Bun.spawn(["git", ...args], { cwd: repository, stdout: "pipe", stderr: "pipe" })
  const [stdout, stderr, code] = await Promise.all([new Response(child.stdout).text(), new Response(child.stderr).text(), child.exited])
  if (code !== 0) fail(`git ${args[0]} failed: ${stderr.trim()}`)
  return stdout.trim()
}

const expand = (file: string) => (file.startsWith("~/") ? path.join(homedir(), file.slice(2)) : file)

// The installed commit must be fetchable, and must be what this checkout contains.
const remote = await git("remote", "get-url", "origin")
const slug = /github\.com[:/]([\w.-]+\/[\w.-]+?)(?:\.git)?$/.exec(remote)?.[1] ?? fail(`origin is not a GitHub repository: ${remote}`)
if (await git("status", "--porcelain", "--", "plugin")) fail("plugin/ has uncommitted changes, which would not be installed")
const commit = await git("rev-parse", "HEAD")
await git("fetch", "--quiet", "origin")
if (!(await git("branch", "--remotes", "--contains", commit, "--list", "origin/*"))) fail(`commit ${commit.slice(0, 12)} is not on GitHub; push it first`)
const spec = `github:${slug}#${commit}::path:plugin`

const directory = process.env.OPENCODE_CONFIG_DIR ??
  path.join(process.env.XDG_CONFIG_HOME || path.join(homedir(), ".config"), "opencode")
if (existsSync(path.join(directory, "opencode.jsonc"))) fail(`${directory}/opencode.jsonc exists; add the plugin there by hand: ${spec}`)
const file = path.join(directory, "opencode.json")
const config: { plugins?: Entry[] } & Record<string, unknown> = existsSync(file) ? JSON.parse(await readFile(file, "utf8")) : {}
const plugins = config.plugins ?? []
if (!Array.isArray(plugins)) fail(`${file} has a non-array plugins value`)

const name = (entry: Entry) => (typeof entry === "string" ? entry : entry.package)
const repo = slug.toLowerCase()
const gitSpec = (entry: Entry) => [`github:${repo}`, `github.com/${repo}`, `github.com:${repo}`]
  .some((prefix) => name(entry).toLowerCase().replace(/\.git(?=[#:]|$)/, "").includes(prefix))

// Earlier installations are Git specs for this repository or local plugin directories.
async function spool(entry: Entry): Promise<boolean> {
  if (gitSpec(entry)) return true
  const local = name(entry).startsWith("file://") ? fileURLToPath(name(entry)) : expand(name(entry))
  if (!local.startsWith("/") && !local.startsWith(".")) return false
  const manifest = path.join(path.resolve(directory, local), "package.json")
  return existsSync(manifest) && JSON.parse(await readFile(manifest, "utf8")).name === "@spool/opencode"
}
const previous: Entry[] = []
for (const entry of plugins) if (await spool(entry)) previous.push(entry)
const configured = previous.find((item) => typeof item === "object")
const kept: Record<string, unknown> = typeof configured === "object" ? { ...configured.options } : {}
delete kept.readerUrl // Retired: retrieval uses url.
delete kept.sweepIntervalMs // Retired: capture is event-driven.
const options: Record<string, unknown> = {
  ...kept,
  ...(flags.url ? { url: flags.url } : {}),
  ...(flags["token-file"] ? { tokenFile: flags["token-file"] } : {}),
  ...(flags["source-alias"] ? { sourceAlias: flags["source-alias"] } : {}),
}
// Plugins installed from a checkout key checkpoints by url; the new plugin moves them
// only if it first loads with the same url. Changing both at once recaptures everything.
if (flags.url && typeof kept.url === "string" && flags.url !== kept.url && !previous.some(gitSpec)) {
  fail("this installation predates checkpoint migration: install without --url, let OpenCode reload, then rerun with --url")
}
if (typeof options.url !== "string") fail("no Spool url configured; pass --url")
const url = URL.parse(options.url)
if (!url || !(url.protocol === "https:" || (url.protocol === "http:" && ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname)))) {
  fail(`url must be an HTTPS URL: ${options.url}`)
}
options.tokenFile ??= "~/.config/spool/source.token"
if (typeof options.tokenFile !== "string") fail("tokenFile must be a path")
const tokenFile = expand(options.tokenFile)
if (!path.isAbsolute(tokenFile)) fail(`token file must be an absolute path or start with ~/: ${options.tokenFile}`)
if (!existsSync(tokenFile)) {
  const alias = options.sourceAlias
  if (typeof alias !== "string") fail(`${options.tokenFile} does not exist; pass --source-alias to log this machine in`)
  if (flags["dry-run"]) console.log(`Would log in as ${alias} and save the token to ${options.tokenFile}`)
  else await login(url.toString().replace(/\/$/, ""), alias, tokenFile)
}

// The token file is created before the login, so the token, which the server
// issues only once, has somewhere to go; "wx" never replaces an existing file.
async function login(server: string, alias: string, file: string): Promise<void> {
  await mkdir(path.dirname(file), { recursive: true, mode: 0o700 })
  const handle = await open(file, "wx", 0o600)
  let token: string
  try {
    token = await deviceLogin(server, alias, (code, page) => {
      console.log(`To log in this machine as ${alias}, open\n\n  ${page}\n\nand enter code ${code}. Waiting for approval…`)
    })
  } catch (error) {
    await handle.close()
    await rm(file, { force: true })
    if (error instanceof DeviceLoginError) fail(`login failed: ${error.message}`)
    throw error
  }
  try {
    await handle.writeFile(`${token}\n`)
    await handle.close()
  } catch (error) {
    const source = token.split(".")[1]
    fail(`logged in as source ${source}, but could not save the token (${error}); have an operator run \`spool-server source rotate ${source}\` and save the new token to ${options.tokenFile}`)
  }
  console.log(`Logged in; saved the token to ${options.tokenFile}`)
}

const entry = { package: spec, options }
const index = plugins.findIndex((item) => previous.includes(item))
const next = plugins.filter((item) => !previous.includes(item))
next.splice(index < 0 ? next.length : index, 0, entry)
config.plugins = next
for (const item of previous) console.log(`Replacing ${JSON.stringify(item)}`)
console.log(`${flags["dry-run"] ? "Would write" : "Writing"} ${JSON.stringify(entry, null, 2)}\nto ${file}`)
if (flags["dry-run"]) process.exit(0)

// Replace atomically: OpenCode watches this file and reloads on change. A symlinked
// configuration keeps its link; the file it points to is replaced.
await mkdir(directory, { recursive: true })
const target = existsSync(file) ? await realpath(file) : file
const temporary = `${target}.spool-${process.pid}`
const mode = existsSync(target) ? (await stat(target)).mode & 0o777 : 0o600
try {
  await writeFile(temporary, `${JSON.stringify(config, null, 2)}\n`, { mode })
  await rename(temporary, target)
} finally {
  await rm(temporary, { force: true })
}
console.log("OpenCode installs the package in the background and reloads the plugin; `opencode plugin list` shows it once loaded.")
