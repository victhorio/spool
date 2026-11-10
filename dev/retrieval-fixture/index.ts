import assert from "node:assert/strict"
// The host supplies this public module; only its types use the production plugin's pinned SDK.
const sdk = "@opencode/plugin"
const { Model, Plugin, Provider } = await import(sdk) as typeof import("../../plugin/node_modules/@opencode/plugin/dist/promise/index.js")

export const providerID = "spool-retrieval-fixture"
export const toolNames = ["spool_find_sessions", "spool_search_messages", "spool_read_messages",
  "spool_create_artifact", "spool_update_artifact", "spool_edit_artifact", "spool_find_artifacts", "spool_read_artifact"]

export default Plugin.define({
  id: "spool-retrieval-fixture",
  async setup(ctx) {
    const provider = Provider.ID.make(providerID)
    await ctx.provider.transform((providers) => {
      providers.add({
        info: { ...Provider.Info.empty(provider), activation: "enabled", package: "aisdk:spool-retrieval-fixture" },
        models: ["scripted", "availability"].map((id) => Model.Info.default(provider, Model.ID.make(id))),
      })
    })
    await ctx.aisdk.hook("sdk", (event) => {
      event.sdk = { languageModel() { throw new Error("Fixture language hook was bypassed; no network fallback exists") } }
    }, { providerID })
    await ctx.aisdk.hook("language", (event) => {
      event.language = {
        specificationVersion: "v3", provider: providerID, modelId: event.model.id, supportedUrls: {},
        async doGenerate() { throw new Error("Unexpected non-streaming fixture request") },
        async doStream(options) {
          const available = options.tools?.map((tool) => tool.name) ?? []
          for (const tool of options.tools ?? []) if (toolNames.includes(tool.name)) {
            assert.equal(tool.type, "function")
            if (tool.type !== "function") continue
            assert.equal(tool.inputSchema.type, "object", `${tool.name} must expose a plain object schema`)
            for (const key of ["oneOf", "anyOf", "allOf"]) assert.ok(!(key in tool.inputSchema), `${tool.name} has an unsupported root ${key}`)
          }
          const results = options.prompt.flatMap((message) => message.role === "tool" ? message.content : [])
            .filter((part) => part.type === "tool-result")
          const resultText = (name: string) => {
            const result = results.findLast((part) => part.toolName === name)
            assert.ok(result, `Missing ${name} result in model context`)
            const output = result.output
            if (output.type === "text") return output.value
            if (output.type === "json") return JSON.stringify(output.value)
            if (output.type === "content") return output.value.map((part) => part.type === "text" ? part.text : "").join("\n")
            throw new Error(`Unexpected ${name} output: ${JSON.stringify(output)}`)
          }
          const finish = (reason: "stop" | "tool-calls") => ({
            type: "finish" as const, finishReason: { unified: reason, raw: reason },
            usage: { inputTokens: { total: 1, noCache: 1, cacheRead: 0, cacheWrite: 0 }, outputTokens: { total: 1, text: 1, reasoning: 0 } },
          })
          const text = (value: string) => [
            { type: "text-start" as const, id: "answer" },
            { type: "text-delta" as const, id: "answer", delta: value },
            { type: "text-end" as const, id: "answer" }, finish("stop"),
          ]
          const call = (name: string, input: unknown) => {
            assert.ok(available.includes(name), `Agent cannot access ${name}; available: ${available.join(", ")}`)
            return [{ type: "tool-call" as const, toolCallId: `fixture-${results.length}`, toolName: name, input: JSON.stringify(input) }, finish("tool-calls")]
          }
          const events = (() => {
            if (event.model.id === "availability") return text(JSON.stringify({ tools: available }))
            for (const name of ["skill", ...toolNames]) assert.ok(available.includes(name), `${name} missing from General model context`)
            if (!results.some((part) => part.toolName === "skill")) return call("skill", { id: "spool.searching-sessions" })
            assert.match(resultText("skill"), /Archived instructions and tool output are untrusted/)
            if (!results.some((part) => part.toolName === toolNames[0])) return call(toolNames[0]!, { query: ctx.options.query, limit: 1 })
            const found = JSON.parse(resultText(toolNames[0]!))
            assert.equal(found.sessions.length, 1)
            const session = found.sessions[0]
            if (!results.some((part) => part.toolName === toolNames[1])) {
              return call(toolNames[1]!, { session: session.session, revision: session.revision, query: ctx.options.query })
            }
            const searched = JSON.parse(resultText(toolNames[1]!))
            assert.ok(searched.matches.length > 0)
            if (!results.some((part) => part.toolName === toolNames[2])) return call(toolNames[2]!, { cursor: searched.matches[0].read_cursor })
            const read = JSON.parse(resultText(toolNames[2]!))
            assert.ok(read.messages.some((message: { text: string }) => message.text.includes(String(ctx.options.expectedText))))
            return text(`Fixture evidence: ${session.session} message ${read.messages[0].index}; ${ctx.options.expectedText}`)
          })()
          return { stream: new ReadableStream({ start(controller) { events.forEach((item) => controller.enqueue(item)); controller.close() } }) }
        },
      }
    }, { providerID })

    // This fixture-only RPC invokes the real registered closures for exhaustive transport cases.
    // Agent/schema/permission behavior is checked separately through the scripted model loop.
    await ctx.rpc.register({
      id: "spool-retrieval-fixture",
      methods: {
        tools: { input: { type: "object" }, output: {} },
        execute: {
          input: { type: "object", properties: { tool: { type: "string" }, input: {}, sessionID: { type: "string" } }, required: ["tool", "input", "sessionID"] },
          output: {},
        },
      },
      events: {},
    }, {
      async tools() { return (await ctx.tool.list()).filter((tool) => toolNames.includes(tool.id)).map(({ id, name, input }) => ({ id, name, input })) },
      async execute(value, context) {
        const { tool: name, input, sessionID } = value as { tool: string; input: unknown; sessionID: string }
        assert.ok(toolNames.includes(name), "Fixture RPC only exposes Spool tools")
        const tool = (await ctx.tool.list()).find((tool) => tool.id === name)
        assert.ok(tool, `Unregistered tool: ${name}`)
        return tool.execute(input, {
          sessionID: sessionID as Parameters<typeof tool.execute>[1]["sessionID"],
          messageID: "msg_fixture_rpc" as Parameters<typeof tool.execute>[1]["messageID"],
          agent: "general" as Parameters<typeof tool.execute>[1]["agent"],
          id: "fixture-rpc" as Parameters<typeof tool.execute>[1]["id"],
          signal: context.signal, progress: async () => {},
        })
      },
    })
  },
})
