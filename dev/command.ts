import { setTimeout as delay } from "node:timers/promises"

export async function command(args: string[], cwd = process.cwd(), env?: Record<string, string | undefined>): Promise<string> {
  const child = Bun.spawn(args, { cwd, env, stdout: "pipe", stderr: "pipe" })
  const [stdout, stderr, code] = await Promise.all([
    new Response(child.stdout).text(),
    new Response(child.stderr).text(),
    child.exited,
  ])
  if (code !== 0) throw new Error(`${args[0]} exited ${code}:\n${stdout}\n${stderr}`)
  return stdout.trim()
}

export async function waitFor<T>(label: string, check: () => Promise<T | undefined>, timeout = 30_000, signal?: AbortSignal): Promise<T> {
  const deadline = Date.now() + timeout
  while (Date.now() < deadline) {
    signal?.throwIfAborted()
    const value = await check()
    if (value !== undefined) return value
    await delay(100, undefined, { signal })
  }
  throw new Error(`Timed out waiting for ${label}`)
}
