import { expect, test, type APIRequestContext } from "@playwright/test"
import path from "node:path"

const sources: Record<string, { source_id: string; token: string }> = JSON.parse(process.env.SPOOL_WEB_FIXTURES!)
const screenshots = path.resolve(import.meta.dirname, "../../.amp/in/artifacts")
const origin = new URL(process.env.SPOOL_WEB_URL!).origin
// 1×1 transparent PNG.
const png = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==", "base64")

async function publish(request: APIRequestContext, title: string, type: string, body: string | Buffer, session?: string) {
  const query = new URLSearchParams({ title, ...(session ? { session } : {}) })
  const response = await request.post(`/v1/artifacts?${query}`, {
    headers: { authorization: `Bearer ${sources.laptop!.token}`, "content-type": type },
    data: body,
  })
  expect(response.status()).toBe(201)
  return ((await response.json()) as { artifact: { id: string } }).artifact.id
}

test("artifacts are listed, searched and shown by type, with links to their sessions", async ({ page, request }) => {
  const image = await publish(request, "Latency chart", "image/png", png)
  const plan = await publish(request, "Quartz rollout plan", "text/markdown",
    `# Rollout\n\nShip **quartz** first.\n\n![latency](${origin}/artifacts/${image})\n\n![remote](https://example.com/x.png)\n\n<script>window.spoolInjected = 1</script>`,
    "ses_paged")
  const notes = await publish(request, "Raw notes", "text/plain", "line one\n<b>not markup</b>")

  await page.goto("/")
  await page.getByRole("link", { name: "Artifacts" }).click()
  await expect(page).toHaveURL("/artifacts")
  const list = page.getByRole("navigation", { name: "Artifact list" })
  await expect(list.getByRole("link")).toHaveText([/Raw notes/, /Quartz rollout plan/, /Latency chart/])
  await page.getByRole("textbox", { name: "Find an artifact" }).fill("QUARTZ ship")
  await expect(list.getByRole("link")).toHaveText([/Quartz rollout plan/])
  await list.getByRole("link", { name: /Quartz rollout plan/ }).click()
  await expect(page).toHaveURL(`/artifacts/${plan}`)

  const article = page.getByRole("article", { name: "Artifact" })
  await expect(article.getByRole("heading", { name: "Rollout", exact: true })).toBeVisible()
  await expect(article.getByRole("img", { name: "latency" })).toHaveAttribute("src", `/artifacts/${image}/content`)
  await expect.poll(() => article.getByRole("img", { name: "latency" }).evaluate((element: HTMLImageElement) => element.naturalWidth)).toBe(1)
  await expect(article.getByText("Image · remote")).toBeVisible()
  await expect(article.locator("script")).toHaveCount(0)
  expect(await page.evaluate(() => "spoolInjected" in window)).toBe(false)
  await page.screenshot({ path: path.join(screenshots, "spool-artifact-markdown.png") })

  await page.goto(`/artifacts/${notes}`)
  await expect(page.locator(".artifact-text")).toHaveText("line one\n<b>not markup</b>")
  await page.goBack()
  await expect(page).toHaveURL(`/artifacts/${plan}`)
  await page.getByRole("link", { name: "Open session" }).click()
  await expect(page.getByRole("heading", { name: /^A long conversation/ })).toBeVisible()
})

test("HTML artifacts run sandboxed: no reader access, storage or remote loads", async ({ page, request }) => {
  const html = `<!doctype html><title>Probe</title><h1>Probe</h1><p id="result">waiting</p>
<img src="https://example.com/tracker.png" alt="">
<script>
  const outcomes = []
  if (window.top === window) outcomes.push("parent:none")
  else try { outcomes.push(parent.document.title ? "parent:readable" : "parent:empty") } catch { outcomes.push("parent:blocked") }
  try { localStorage.getItem("x"); outcomes.push("storage:readable") } catch { outcomes.push("storage:blocked") }
  try { outcomes.push(document.cookie ? "cookie:visible" : "cookie:none") } catch { outcomes.push("cookie:blocked") }
  fetch("/api/artifacts").then(() => "api:readable", () => "api:blocked").then((api) => {
    outcomes.push(api)
    document.getElementById("result").textContent = outcomes.join(" ")
  })
</script>`
  const probe = await publish(request, "Sandbox probe", "text/html", html)
  // The browser reports remote loads it blocked by policy; none may get further.
  const blocked: string[] = []
  const escaped: string[] = []
  page.on("requestfailed", (sent) => {
    if (new URL(sent.url()).origin !== origin) (sent.failure()?.errorText === "csp" ? blocked : escaped).push(sent.url())
  })
  page.on("requestfinished", (sent) => {
    if (new URL(sent.url()).origin !== origin) escaped.push(sent.url())
  })
  await page.context().addCookies([{ name: "reader_session", value: "synthetic", url: origin }])
  await page.goto(`/artifacts/${probe}`)
  const frame = page.frameLocator("iframe.artifact-frame")
  await expect(frame.locator("#result")).toHaveText("parent:blocked storage:blocked cookie:blocked api:blocked")
  await expect(page.locator("iframe.artifact-frame")).toHaveAttribute("sandbox", "allow-scripts allow-popups allow-popups-to-escape-sandbox")
  await page.screenshot({ path: path.join(screenshots, "spool-artifact-html.png") })

  // Opened directly, outside the reader's frame, it is still sandboxed.
  await page.goto(`/artifacts/${probe}/content`)
  await expect(page.locator("#result")).toHaveText("parent:none storage:blocked cookie:blocked api:blocked")
  expect(escaped).toEqual([])
  expect(blocked).toEqual(["https://example.com/tracker.png", "https://example.com/tracker.png"])
})
