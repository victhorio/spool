import { expect, test } from "@playwright/test"
import path from "node:path"
import { envelope, fixtures } from "./fixtures"

const sources: Record<string, { source_id: string; token: string }> = JSON.parse(
  process.env.SPOOL_WEB_FIXTURES!,
)
const artifacts = path.resolve(import.meta.dirname, "../../.amp/in/artifacts")
const selection = (source: string, session: string) =>
  `/sessions/${sources[source]!.source_id}/${session}`

test("browse, filter independent stores, deep-link and return with browser history", async ({
  page,
}) => {
  await page.goto("/")
  await expect(page.getByText("6 sessions", { exact: true })).toBeVisible()
  await expect(page.getByRole("heading", { name: "A little less lost." })).toBeVisible()
  await page.screenshot({ path: path.join(artifacts, "spool-home.png") })
  await page.getByRole("textbox", { name: "Find a session" }).fill("quieter")
  await expect(page.getByRole("navigation", { name: "Sessions" }).getByRole("link")).toHaveCount(
    1,
  )
  await page.getByRole("link", { name: "A quieter session browser" }).click()
  await expect(
    page.getByRole("heading", { name: "A quieter session browser", exact: true }),
  ).toBeVisible()
  await page.getByRole("textbox", { name: "Find a session" }).fill("")
  await page
    .getByRole("combobox", { name: "Filter by source" })
    .selectOption(sources.laptop!.source_id)
  await expect(page.getByRole("link", { name: "A quieter session browser" })).toHaveCount(0)
  await page.getByRole("link", { name: "The same ID, another machine" }).click()
  await expect(
    page.getByText("This conversation belongs to the laptop source.", { exact: true }),
  ).toBeVisible()
  await page.goBack()
  await expect(
    page.getByRole("heading", { name: "A quieter session browser", exact: true }),
  ).toBeVisible()
  await page.reload()
  await expect(
    page.getByRole("heading", { name: "A quieter session browser", exact: true }),
  ).toBeVisible()
})

test("links name sessions by ID, open a linked message and accept older link forms", async ({
  page,
}) => {
  // A qualified link to a unique ID is shortened once loaded.
  // Message 50 is before the latest page, so the reader loads back to it.
  await page.goto(`${selection("laptop", "ses_paged")}#50`)
  await expect(page).toHaveURL("/sessions/ses_paged#50")
  const linked = page.locator('[id="50"]')
  await expect(linked).toBeInViewport()
  await expect(linked).toContainText("Conversation entry 049")
  await expect(linked).toContainClass("linked")
  await expect(page.locator("[data-message-id]")).toHaveCount(200)
  await page.screenshot({ path: path.join(artifacts, "spool-linked-message.png") })
  await page.locator('[id="48"]').hover()
  await page.getByRole("link", { name: "Link to message 48" }).click()
  await expect(page).toHaveURL("/sessions/ses_paged#48")
  await expect(page.locator('[id="48"]')).toContainClass("linked")
  await expect(linked).not.toContainClass("linked")

  // An ID that several sources archived asks which one.
  await page.goto("/sessions/ses_reader")
  await expect(page.getByRole("alert")).toContainText("More than one source")
  await page.getByRole("link", { name: `${sources.laptop!.source_id}/ses_reader` }).click()
  await expect(
    page.getByText("This conversation belongs to the laptop source.", { exact: true }),
  ).toBeVisible()

  // Earlier links named the source by UUID in the fragment.
  await page.goto(
    `/#${new URLSearchParams({ source: sources.laptop!.source_id, session: "ses_paged" })}`,
  )
  await expect(page).toHaveURL("/sessions/ses_paged")
  await expect(page.getByRole("heading", { name: /^A long conversation/ })).toBeVisible()
})

test("read Markdown, reasoning, tool inputs/results and compaction with the keyboard", async ({
  page,
}) => {
  await page.goto(selection("studio", "ses_reader"))
  await expect(page.getByRole("heading", { name: "Keep the conversation in focus" })).toBeVisible()
  await expect(page.getByText("Keep the reading experience simple.", { exact: false })).toHaveCount(
    0,
  )
  await expect(page.getByText("test result: ok. 12 passed; 0 failed", { exact: true })).toHaveCount(
    0,
  )
  await page.screenshot({ path: path.join(artifacts, "spool-conversation.png") })
  const thinking = page.locator("summary").filter({ hasText: /^›Thinking$/ })
  await thinking.focus()
  await page.keyboard.press("Enter")
  await expect(
    page.getByText("Keep the reading experience simple.", { exact: false }),
  ).toBeVisible()
  await page.locator("summary").filter({ hasText: "2 tool calls" }).click()
  await page.locator("summary").filter({ hasText: "bash" }).click()
  await expect(page.getByText("cargo test --workspace", { exact: true }).last()).toBeVisible()
  await expect(
    page.getByText("test result: ok. 12 passed; 0 failed", { exact: true }),
  ).toBeVisible()
  await page.locator("summary").filter({ hasText: "Context compacted" }).click()
  await expect(page.getByRole("heading", { name: "Where we left off" })).toBeVisible()
  await page.setViewportSize({ width: 1440, height: 1600 })
  await page.locator(".conversation-scroll").evaluate((element) => {
    element.scrollTop = 0
  })
  await page.screenshot({ path: path.join(artifacts, "spool-expanded.png") })
})

test("open at the end and page back in order, keeping the view in place", async ({ page }) => {
  await page.goto(selection("laptop", "ses_paged"))
  await expect(page.getByText("Conversation entry 202", { exact: true })).toBeInViewport()
  await expect(page.locator("[data-message-id]")).toHaveCount(100)
  await page.getByRole("button", { name: "Show earlier messages" }).click()
  await expect(page.locator("[data-message-id]")).toHaveCount(200)
  await expect(page.getByText("Conversation entry 103", { exact: true })).toBeInViewport()
  await page.getByRole("button", { name: "Show earlier messages" }).click()
  await expect(page.locator("[data-message-id]")).toHaveCount(203)
  await expect(page.locator('[id="1"]')).toContainText("Conversation entry 000")
  expect(await page.locator(".user-text").allTextContents()).toEqual(
    Array.from(
      { length: 203 },
      (_, index) => `Conversation entry ${String(index).padStart(3, "0")}`,
    ),
  )
  await expect(page.getByRole("button", { name: "Show earlier messages" })).toHaveCount(0)
})

test("an archive mutation between pages asks for a reload instead of mixing revisions", async ({
  page,
  request,
}) => {
  await page.goto(selection("laptop", "ses_paged"))
  await expect(page.locator("[data-message-id]")).toHaveCount(100)
  const fixture = fixtures.find((fixture) => fixture.id === "ses_paged")!
  const body = envelope(fixture, "1")
  body.info.title = "A long conversation, updated"
  const response = await request.post(
    `${process.env.SPOOL_INGEST_URL}/v1/sync/sessions/${fixture.id}/snapshot`,
    {
      headers: { authorization: `Bearer ${sources.laptop!.token}` },
      data: body,
    },
  )
  expect(response.status()).toBe(200)
  await page.getByRole("button", { name: "Show earlier messages" }).click()
  await expect(page.getByRole("alert")).toContainText("This session changed.")
  await expect(page.locator("[data-message-id]")).toHaveCount(100)
  await page.getByRole("button", { name: "Reload session" }).click()
  await expect(
    page.getByRole("heading", { name: "A long conversation, updated", exact: true }),
  ).toBeVisible()
})

test("archived markup, URLs and images cannot execute or make remote requests", async ({
  page,
}) => {
  const remote: string[] = []
  page.on("request", (request) => {
    if (new URL(request.url()).origin !== new URL(process.env.SPOOL_WEB_URL!).origin)
      remote.push(request.url())
  })
  await page.goto(selection("studio", "ses_security"))
  await expect(page.getByText("After the markup. Café 東京.", { exact: false })).toBeVisible()
  expect(await page.evaluate(() => "spoolInjected" in window)).toBe(false)
  await expect(
    page.locator(".messages img, .messages script, .messages iframe, .messages [onerror]"),
  ).toHaveCount(0)
  await expect(page.locator('.messages a[href^="javascript:"]')).toHaveCount(0)
  await expect(page.getByRole("link", { name: "Safe documentation" })).toHaveAttribute(
    "href",
    "https://example.com/docs",
  )
  expect(remote).toEqual([])
})

test("empty, missing, no matches and failed transport have distinct recoverable states", async ({
  page,
}) => {
  await page.goto(selection("studio", "ses_empty"))
  await expect(page.getByText("No settled messages in this session yet.")).toBeVisible()
  await page.getByRole("textbox", { name: "Find a session" }).fill("there-is-no-such-session")
  await expect(page.getByText("No matching sessions.")).toBeVisible()
  await page.getByRole("button", { name: "Clear filters" }).click()
  await expect(page.getByRole("link", { name: "An empty session" })).toBeVisible()
  await page.goto(selection("studio", "missing"))
  await expect(page.getByRole("alert")).toContainText("This session is no longer in the archive.")
  let fail = true
  await page.route("**/api/sessions?*", async (route) =>
    fail ? route.abort("failed") : route.continue(),
  )
  await page.goto("/")
  await expect(page.getByRole("alert")).toContainText("Couldn’t load the archive.")
  fail = false
  await page.getByRole("button", { name: "Try again" }).click()
  await expect(page.getByText("6 sessions", { exact: true })).toBeVisible()
})

test("an expired Access login reloads the page instead of showing an error", async ({ page }) => {
  let expired = true
  await page.route("**/api/sessions?*", async (route) => {
    if (!expired) return route.continue()
    expired = false
    return route.fulfill({ status: 302, headers: { location: "https://login.invalid/" } })
  })
  let loads = 0
  page.on("load", () => loads++)
  await page.goto("/")
  await expect(page.getByText("6 sessions", { exact: true })).toBeVisible()
  expect(loads).toBe(2)
  await expect(page.getByRole("alert")).toHaveCount(0)
})

test("errors, shell output and unfamiliar message types remain inspectable", async ({ page }) => {
  await page.goto(selection("laptop", "ses_errors"))
  await page.locator("summary").filter({ hasText: "read" }).click()
  await expect(
    page.getByText('"message": "The synthetic file does not exist."', { exact: false }),
  ).toBeVisible()
  await page.locator("summary").filter({ hasText: "Compaction failed" }).click()
  await expect(
    page.getByText('"message": "Compaction did not complete."', { exact: false }),
  ).toBeVisible()
  await page.locator("summary").filter({ hasText: "Shell" }).click()
  await expect(page.getByText("hello", { exact: true })).toBeVisible()
  await page.locator("summary").filter({ hasText: "Session event" }).click()
  await expect(
    page.getByText("Future message fields remain readable.", { exact: false }),
  ).toBeVisible()
})

test("mobile navigation, keyboard search and long output do not overflow the viewport", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto("/")
  await expect(page.getByRole("complementary", { name: "Session archive" })).toBeHidden()
  await page.getByRole("button", { name: "Show sessions" }).click()
  await expect(page.locator("main")).toHaveAttribute("inert", "")
  await page.getByRole("link", { name: "A quieter session browser" }).click()
  await expect(page.getByRole("complementary", { name: "Session archive" })).toBeHidden()
  await expect(
    page.getByRole("heading", { name: "A quieter session browser", exact: true }),
  ).toBeVisible()
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(
    true,
  )
  await page.screenshot({ path: path.join(artifacts, "spool-mobile.png") })
  await page.keyboard.press("Meta+k")
  await expect(page.getByRole("textbox", { name: "Find a session" })).toBeFocused()
  await page.keyboard.press("Escape")
  await expect(page.getByRole("complementary", { name: "Session archive" })).toBeHidden()
  await page.setViewportSize({ width: 1440, height: 1050 })
  await expect(page.getByRole("complementary", { name: "Session archive" })).toBeVisible()
})

test("malformed opaque documents stay inspectable without breaking other sessions", async ({
  page,
  request,
}) => {
  const errors: string[] = []
  page.on("pageerror", (error) => errors.push(error.message))
  const messages = [
    { id: "malformed-assistant", type: "assistant" },
    { id: "malformed-user", type: "user", text: { preserved: "not-a-message-text" } },
    { id: "malformed-model", type: "model-switched" },
    {
      id: "malformed-parts",
      type: "assistant",
      agent: {},
      content: [
        null,
        { type: "text", text: {} },
        { type: "reasoning", text: 17 },
        { type: "tool", name: "broken", state: { input: null } },
      ],
    },
    { id: "malformed-compaction", type: "compaction", reason: {}, summary: 42 },
    { id: "malformed-system", type: "system", text: {} },
    { id: "malformed-shell", type: "shell", command: {} },
    {
      id: "future-shell-output",
      type: "shell",
      command: "synthetic",
      output: { preserved: "future shape" },
    },
  ]
  const body = {
    ...envelope(fixtures[0]!),
    info: { id: "ses_malformed", title: 42, location: { directory: {} }, time: { updated: 1e30 } },
    message_order: messages.map((message) => message.id),
    upsert_messages: messages,
  }
  const response = await request.post(
    `${process.env.SPOOL_INGEST_URL}/v1/sync/sessions/ses_malformed/snapshot`,
    {
      headers: { authorization: `Bearer ${sources.studio!.token}` },
      data: body,
    },
  )
  expect(response.status()).toBe(201)
  await page.goto("/")
  await expect(page.getByText("7 sessions", { exact: true })).toBeVisible()
  await page.getByRole("link", { name: "Untitled session" }).click()
  await expect(page.getByRole("heading", { name: "Untitled session", exact: true })).toBeVisible()
  await page.locator('[data-message-id="malformed-user"] summary').click()
  await expect(page.getByText("not-a-message-text", { exact: false })).toBeVisible()
  await page.locator("summary").filter({ hasText: "Tool data" }).click()
  await expect(page.getByText('"input": null', { exact: false })).toBeVisible()
  await page.locator('[data-message-id="future-shell-output"] summary').click()
  await expect(page.getByText('"preserved": "future shape"', { exact: false })).toBeVisible()
  await page.screenshot({ path: path.join(artifacts, "spool-raw-document.png") })
  await page.getByRole("link", { name: "A quieter session browser" }).click()
  await expect(page.getByRole("heading", { name: "Keep the conversation in focus" })).toBeVisible()
  expect(errors).toEqual([])
})

test("load only recent sessions, paginate on demand and search beyond the loaded page", async ({
  page,
  request,
}) => {
  for (let index = 0; index < 55; index++) {
    const label = String(index).padStart(2, "0")
    const fixture = {
      ...fixtures[0]!,
      id: `ses_recent_${label}`,
      title: `Recent ${label} conversation`,
      updated: Date.UTC(2027, 0, 1) + index,
      messages: [],
    }
    const response = await request.post(
      `${process.env.SPOOL_INGEST_URL}/v1/sync/sessions/${fixture.id}/snapshot`,
      { headers: { authorization: `Bearer ${sources.studio!.token}` }, data: envelope(fixture) },
    )
    expect(response.status()).toBe(201)
  }
  let inventoryRequests = 0
  page.on("request", (request) => {
    if (new URL(request.url()).pathname === "/api/sessions") inventoryRequests++
  })
  await page.goto("/")
  await page.waitForLoadState("networkidle")
  const rows = page.locator(".session-row")
  await expect(rows).toHaveCount(50)
  expect(inventoryRequests).toBe(1)
  expect(await rows.locator(".session-title").allTextContents()).toEqual(
    Array.from(
      { length: 50 },
      (_, index) => `Recent ${String(54 - index).padStart(2, "0")} conversation`,
    ),
  )
  await expect(page.getByRole("combobox", { name: "Filter by source" })).toBeVisible()
  await page.screenshot({ path: path.join(artifacts, "spool-recent-sidebar.png") })
  await page.getByRole("button", { name: "Load more sessions" }).click()
  await expect(rows).toHaveCount(62)
  await expect(page.getByRole("button", { name: "Load more sessions" })).toHaveCount(0)
  await page.reload()
  await expect(rows).toHaveCount(50)
  await page.keyboard.press("Meta+k")
  await expect(page.getByRole("textbox", { name: "Find a session" })).toBeFocused()
  await page.getByRole("textbox", { name: "Find a session" }).fill("Recent 00")
  await expect(rows).toHaveCount(1)
  await expect(rows.first()).toHaveText("Recent 00 conversation")
  await rows.first().click()
  await expect(
    page.getByRole("heading", { name: "Recent 00 conversation", exact: true }),
  ).toBeVisible()
})
