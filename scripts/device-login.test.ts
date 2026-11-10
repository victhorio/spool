import { expect, test } from "bun:test"
import { DeviceLoginError, deviceLogin } from "./device-login"

const code = { device_code: "synthetic-device-code", user_code: "BCDF-GHJK", expires_in: 600, interval: 5 }
const error = (status: number, name: string) => Response.json({ error: { code: name } }, { status })

async function run(answers: (() => Response)[], start = () => Response.json(code)) {
  const requests: { path: string; body: unknown }[] = []
  const server = Bun.serve({ hostname: "127.0.0.1", port: 0, async fetch(request) {
    const path = new URL(request.url).pathname
    requests.push({ path, body: await request.json() })
    if (path === "/v1/device/code") return start()
    return (answers.shift() ?? (() => error(400, "authorization_pending")))()
  } })
  const shown: string[] = []
  const waits: number[] = []
  try {
    const result = await deviceLogin(server.url.origin, "work-laptop", (user, url) => shown.push(user, url),
      async (milliseconds) => { waits.push(milliseconds) }).catch((reason: unknown) => reason)
    return { result, requests, shown, waits, origin: server.url.origin }
  } finally { server.stop(true) }
}

test("device login shows the code, polls at the server's interval and returns the token", async () => {
  const { result, requests, shown, waits, origin } = await run([
    () => error(400, "authorization_pending"),
    () => new Response("unavailable", { status: 503 }),
    () => new Response("slow down", { status: 429 }),
    () => Response.json({ token: "spool_v1.synthetic", source_id: "x", alias: "work-laptop" }),
  ])
  expect(result).toBe("spool_v1.synthetic")
  expect(shown).toEqual(["BCDF-GHJK", `${origin}/device`])
  expect(waits).toEqual([5000, 5000, 5000, 5000])
  expect(requests).toEqual([
    { path: "/v1/device/code", body: { alias: "work-laptop" } },
    ...Array(4).fill({ path: "/v1/device/token", body: { device_code: "synthetic-device-code" } }),
  ])
})

test("device login stops with a clear reason when the server refuses", async () => {
  for (const [answer, message] of [
    [() => error(400, "access_denied"), "denied"],
    [() => error(400, "expired_token"), "expired"],
    [() => error(409, "source_alias_conflict"), "work-laptop is already taken"],
    [() => error(409, "login_pending"), "waiting for approval"],
    [() => error(400, "invalid_device_code"), "HTTP 400"],
  ] as const) {
    const { result } = await run([answer])
    expect(result).toBeInstanceOf(DeviceLoginError)
    expect((result as Error).message).toContain(message)
  }
  const refused = await run([], () => error(429, "too_many_pending"))
  expect((refused.result as Error).message).toContain("too many logins")
  expect(refused.shown).toEqual([])
})

test("device login reports an unreachable server", async () => {
  const result = await deviceLogin("http://127.0.0.1:1", "work-laptop", () => {}, async () => {}).catch((reason: unknown) => reason)
  expect(result).toBeInstanceOf(DeviceLoginError)
  expect((result as Error).message).toBe("cannot reach http://127.0.0.1:1")
})
