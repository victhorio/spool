import path from "node:path"
import { Database } from "bun:sqlite"
import type { SessionMessageInfo } from "@opencode/client"
import type { startOpenCode } from "./opencode"
import { waitFor } from "./command"

export type Host = Awaited<ReturnType<typeof startOpenCode>>

export async function activate(host: Host, project = "project-a"): Promise<void> {
  await waitFor(`Spool activation in ${project}`, async () => {
    const plugins = await host.client.plugin.list({ location: { directory: path.join(host.root, project) } })
    return plugins.data.some((plugin) => plugin.id === "spool" && plugin.state.status === "active") ? true : undefined
  }, 30_000, host.signal)
}

export async function seed(host: Host, id: string, project = "project-a", text = "Synthetic archive: café 東京\u0000end") {
  const location = { directory: path.join(host.root, project) }
  const template = await host.client.session.create({ title: "Spool validation template", location })
  const now = Date.now()
  const messages: SessionMessageInfo[] = [
    {
      id: `msg_${id}_z`, type: "user", time: { created: now }, text,
      metadata: { fixture: { nested: [true, 42, null] } },
      files: [{ mime: "text/plain", data: Buffer.from("Synthetic attached file\n").toString("base64"), source: { type: "inline" }, name: "fixture.txt" }],
    },
    {
      id: `msg_${id}_a`, type: "assistant", agent: "build", model: { id: "fixture-no-model", providerID: "fixture" },
      time: { created: now + 1, completed: now + 2 }, finish: "tool-calls",
      content: [
        { type: "reasoning", text: "Synthetic reasoning", native: { fixture: { retained: true } } },
        { type: "tool", id: "call-fixture", name: "read", time: { created: now + 1, completed: now + 2 }, state: {
          status: "completed", input: { path: "fixture.txt" }, content: [{ type: "text", text: "Synthetic tool result\u0000tail" }], metadata: { fixture: true },
        } },
        { type: "text", text: "Synthetic answer." },
      ],
    },
    { id: `msg_${id}_m`, type: "synthetic", time: { created: now + 3 }, text: "Synthetic continuation", description: "No model was called" },
  ]
  try {
    return await host.client.experimental.session.import({
      info: { ...template, id, title: "Spool synthetic validation", metadata: { fixture: { store: project } } }, messages, location,
    })
  } finally { await host.client.session.remove({ sessionID: template.id }) }
}

// Fault injection only: bypass both timestamps and events to exercise reconciliation.
// The collector itself never reads OpenCode's private SQLite schema.
export function mutateWithoutEvent(host: Host, id: string, text: string): void {
  const db = new Database(path.join(host.root, "data", "opencode", "opencode.db"), { readwrite: true, strict: true })
  try {
    const result = db.query("UPDATE session_message SET data = json_set(data, '$.text', ?) WHERE id = ?").run(text, `msg_${id}_z`)
    if (result.changes !== 1) throw new Error(`Fixture message not found: ${id}`)
  } finally { db.close() }
}
