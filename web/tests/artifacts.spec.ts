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
  await page.route("https://example.com/x.png", (route) => route.fulfill({ contentType: "image/png", body: png }))
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
  await expect.poll(() => article.getByRole("img", { name: "remote" }).evaluate((element: HTMLImageElement) => element.naturalWidth)).toBe(1)
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

test("HTML artifacts can load image artifacts and remote resources", async ({ page, request }) => {
  const image = await publish(request, "Probe image", "image/png", png)
  await page.route("https://cdn.example.com/**", (route) => route.request().url().endsWith(".js")
    ? route.fulfill({ contentType: "text/javascript", body: "window.cdnLoaded = true" })
    : route.fulfill({ contentType: "image/png", body: png }))
  const html = `<!doctype html><title>Probe</title><h1>Probe</h1><p id="result">waiting</p>
<img id="artifact" src="${origin}/artifacts/${image}/content" alt="">
<img id="remote" src="https://cdn.example.com/chart.png" alt="">
<script src="https://cdn.example.com/lib.js"></script>
<script>
  window.addEventListener("load", () => {
    const loaded = (id) => document.getElementById(id).naturalWidth === 1
    document.getElementById("result").textContent = [window.cdnLoaded, loaded("artifact"), loaded("remote")].join(" ")
  })
</script>`
  const probe = await publish(request, "Probe", "text/html", html)
  await page.goto(`/artifacts/${probe}`)
  await expect(page.frameLocator("iframe.artifact-frame").locator("#result")).toHaveText("true true true")
  await page.screenshot({ path: path.join(screenshots, "spool-artifact-html.png") })

  await page.goto(`/artifacts/${probe}/content`)
  await expect(page.locator("#result")).toHaveText("true true true")
})
