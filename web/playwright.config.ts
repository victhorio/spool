import { defineConfig } from "@playwright/test"

if (!process.env.SPOOL_WEB_URL || !process.env.SPOOL_WEB_FIXTURES || !process.env.SPOOL_WEB_ACCESS)
  throw new Error("Run bun run web:validate to start an isolated synthetic archive first.")

export default defineConfig({
  testDir: "./tests",
  testMatch: "*.spec.ts",
  outputDir: "../.local/web-artifacts/test-results",
  fullyParallel: false,
  workers: 1,
  retries: 0,
  use: {
    baseURL: process.env.SPOOL_WEB_URL,
    extraHTTPHeaders: { "cf-access-jwt-assertion": process.env.SPOOL_WEB_ACCESS },
    browserName: "chromium",
    channel: "chrome",
    viewport: { width: 1440, height: 1050 },
    screenshot: "only-on-failure",
    trace: "retain-on-failure",
  },
})
