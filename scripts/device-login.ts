// Device login against a Spool server: request a code, show it to the owner, and
// wait until they approve it in the reader, which issues a new source token.

interface Code { device_code: string; user_code: string; expires_in: number; interval: number }
interface Result { status: number; body: { error?: { code?: string } } & Record<string, unknown> }

const messages: Record<string, (alias: string) => string> = {
  access_denied: () => "the login was denied in the browser",
  expired_token: () => "the code expired before it was approved; run the installer again",
  source_alias_conflict: (alias) => `the source alias ${alias} is already taken; choose another with --source-alias`,
  login_pending: (alias) => `another login for ${alias} is waiting for approval; finish or deny it first`,
  too_many_pending: () => "too many logins are waiting for approval; try again in a few minutes",
  malformed_request: (alias) => `the server rejected the request; check that ${alias} matches ^[a-z][a-z0-9-]{0,31}$`,
}

export class DeviceLoginError extends Error {}

export async function deviceLogin(
  url: string,
  alias: string,
  show: (userCode: string, page: string) => void,
  wait: (milliseconds: number) => Promise<unknown> = Bun.sleep,
): Promise<string> {
  const post = async (route: string, body: unknown): Promise<Result> => {
    const response = await fetch(`${url}/v1/device/${route}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
      redirect: "error",
      signal: AbortSignal.timeout(15_000),
    })
    return { status: response.status, body: await response.json().catch(() => ({})) }
  }
  const fail = (result: Result): never => {
    const code = result.body.error?.code
    throw new DeviceLoginError(code && messages[code] ? messages[code](alias) : `the server answered HTTP ${result.status}`)
  }
  const started = await post("code", { alias }).catch(() => {
    throw new DeviceLoginError(`cannot reach ${url}`)
  })
  if (started.status !== 200) fail(started)
  const code = started.body as unknown as Code
  // The owner types the code rather than following a link, so a link sent by
  // someone else cannot be mistaken for this machine's request.
  show(code.user_code, `${url}/device`)
  // The server decides expiry; this bound only stops retrying an unreachable server.
  const deadline = Date.now() + 2 * code.expires_in * 1000
  while (Date.now() < deadline) {
    await wait(code.interval * 1000)
    const result = await post("token", { device_code: code.device_code }).catch(() => undefined)
    if (result?.status === 200 && typeof result.body.token === "string") return result.body.token
    const retry = !result || result.status >= 500 || result.status === 408 || result.status === 429
    if (retry || result.body.error?.code === "authorization_pending") continue
    fail(result)
  }
  throw new DeviceLoginError("the server stopped answering before the login finished")
}
