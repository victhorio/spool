import { expect, test } from "bun:test"
import type { OpenCodeClient } from "@opencode/client"
import { deliver } from "./messages"

test("a message OpenCode rejects is dropped without holding up the others", async () => {
  const polls: string[][] = []
  const server = Bun.serve({ hostname: "127.0.0.1", port: 0, async fetch(request) {
    polls.push((await request.json() as { delivered: string[] }).delivered)
    const messages = polls.length === 1 ? [
      { id: "m1", session: "ses_bad", text: "one", from: null },
      { id: "m2", session: "ses_good", text: "two", from: null },
    ] : []
    if (polls.length === 2) stop.abort()
    return Response.json({ messages })
  } })
  const stop = new AbortController()
  const prompted: string[] = []
  const client = {
    session: {
      active: async () => ({}),
      prompt: async ({ sessionID }: { sessionID: string }) => {
        // The client's declared-error shape: an Error carrying the server's tag.
        if (sessionID === "ses_bad") throw Object.assign(new Error("gone"), { _tag: "LocationNotFoundError" })
        prompted.push(sessionID)
      },
    },
  } as unknown as OpenCodeClient
  try {
    await deliver({ url: server.url.origin, token: "t" }, async () => client, stop.signal, () => {}, String)
  } finally { server.stop(true) }
  expect(prompted).toEqual(["ses_good"])
  expect(polls[1]!.sort()).toEqual(["m1", "m2"])
})
