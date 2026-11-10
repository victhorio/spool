import { expect, test, type Page } from "@playwright/test"
import path from "node:path"
import { deviceLogin } from "../../scripts/device-login"

const artifacts = path.resolve(import.meta.dirname, "../../.amp/in/artifacts")
const server = process.env.SPOOL_WEB_URL!

// The installer's own login loop, polling quickly; the owner types the code it shows.
function machine(alias: string) {
  let shown: (code: string) => void
  const code = new Promise<string>((resolve) => { shown = resolve })
  const token = deviceLogin(server, alias, (userCode, page) => {
    expect(page).toBe(`${server}/device`)
    shown(userCode)
  }, (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds / 50)))
  token.catch(() => {})
  return { code, token }
}

async function enter(page: Page, code: string) {
  await page.goto("/device")
  await expect(page.getByRole("heading", { name: "Log in a machine" })).toBeVisible()
  await page.getByRole("textbox", { name: "Code" }).fill(code)
  await page.getByRole("button", { name: "Continue" }).click()
}

test("approving a typed code gives the machine's installer a working token", async ({ page, request }) => {
  const login = machine("synthetic-laptop")
  const code = await login.code
  await enter(page, code)
  await expect(page.getByText("synthetic-laptop", { exact: true })).toBeVisible()
  await expect(page.getByText(code, { exact: true })).toBeVisible()
  await expect(page.getByText(/asked less than a minute ago/)).toBeVisible()
  await page.screenshot({ path: path.join(artifacts, "spool-device.png") })
  await page.getByRole("button", { name: "Approve" }).click()
  await expect(page.getByRole("status")).toContainText("Approved")
  const token = await login.token
  const inventory = await request.get("/v1/sync/sessions", { headers: { authorization: `Bearer ${token}` } })
  expect(inventory.status()).toBe(200)
  await enter(page, code)
  await expect(page.getByRole("status")).toContainText("This machine is logged in")
  await expect(page.getByRole("button", { name: "Approve" })).toHaveCount(0)
  await expect(page.getByRole("button", { name: "Deny" })).toHaveCount(0)
})

test("denying a machine stops its installer without a token", async ({ page }) => {
  const login = machine("synthetic-denied")
  await enter(page, await login.code)
  await page.getByRole("button", { name: "Deny" }).click()
  await expect(page.getByRole("status")).toContainText("Denied")
  await expect(login.token).rejects.toThrow("denied")
})

test("codes are typed loosely, never taken from a link, and unknown ones explain themselves", async ({ page }) => {
  const login = machine("synthetic-typed")
  const code = await login.code
  await page.goto(`/device?code=${code}`)
  await expect(page.getByRole("textbox", { name: "Code" })).toHaveValue("")
  await expect(page.getByText("synthetic-typed")).toHaveCount(0)
  await enter(page, "BCDF-GHJK")
  await expect(page.getByRole("alert")).toContainText("No login uses this code")
  await enter(page, code.replace("-", "").toLowerCase())
  await expect(page.getByText("synthetic-typed", { exact: true })).toBeVisible()
  await page.getByRole("button", { name: "Deny" }).click()
  await expect(page.getByRole("status")).toContainText("Denied")
})
