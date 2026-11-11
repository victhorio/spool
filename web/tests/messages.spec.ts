import { expect, test } from "@playwright/test"
import path from "node:path"

const sources: Record<string, { source_id: string; token: string }> = JSON.parse(
  process.env.SPOOL_WEB_FIXTURES!,
)
const server = process.env.SPOOL_WEB_URL!
const artifacts = path.resolve(import.meta.dirname, "../../.amp/in/artifacts")

interface Delivery {
  id: string
  session: string
  text: string
  from: { session: string; source: string } | null
}

/** The studio machine's plugin polling: its heartbeat, which also collects its messages. */
async function poll(running: string[]): Promise<Delivery[]> {
  const response = await fetch(`${server}/v1/poll`, {
    method: "POST",
    headers: {
      authorization: `Bearer ${sources.studio!.token}`,
      "content-type": "application/json",
    },
    body: JSON.stringify({ running, delivered: [] }),
  })
  expect(response.status).toBe(200)
  return ((await response.json()) as { messages: Delivery[] }).messages
}

test("the owner messages a running session, and only sessions on online machines", async ({
  page,
}) => {
  const collected = poll(["ses_security"])
  await expect(async () => {
    await page.goto("/sessions/ses_security")
    await expect(page.locator(".session-status")).toHaveText("Running", { timeout: 1_000 })
  }).toPass()
  // Other tests add newer sessions, so search for this one.
  await page.getByRole("textbox", { name: "Find a session" }).fill("Untrusted")
  await expect(
    page.getByRole("link", { name: "Untrusted content stays inert" }).getByRole("img", { name: "Running" }),
  ).toBeVisible()

  const message = page.getByRole("textbox", { name: "Message" })
  await message.fill("Please rerun the checks.\nThen report back.")
  await page.screenshot({ path: path.join(artifacts, "spool-composer.png") })
  await message.press("ControlOrMeta+Enter")
  await expect(page.getByRole("status").filter({ hasText: "Sent." })).toBeVisible()
  await expect(message).toHaveValue("")
  expect(await collected).toEqual([
    {
      id: expect.any(String),
      session: "ses_security",
      text: "Please rerun the checks.\nThen report back.",
      from: null,
    },
  ])

  await page.goto("/sessions/ses_paged")
  await expect(page.getByRole("heading", { name: /^A long conversation/ })).toBeVisible()
  await expect(page.getByRole("textbox", { name: "Message" })).toHaveCount(0)
})
