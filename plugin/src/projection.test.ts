import { expect, test } from "bun:test"
import type { SessionTransferData } from "@opencode/client"
import { bounded, projectMessage, projectRetrieval, type RetrievalMessage } from "./projection"

function checkSpans(message: RetrievalMessage): void {
  const bytes = Buffer.from(message.text)
  for (const [spans, expected] of [[message.conversation_spans, message.conversation], [message.tool_spans, message.tools]] as const) {
    let previous = 0
    const slices = spans.map(([start, end]) => {
      expect(start).toBeGreaterThanOrEqual(previous)
      expect(end).toBeGreaterThan(start)
      expect(end).toBeLessThanOrEqual(bytes.length)
      previous = end
      return new TextDecoder("utf-8", { fatal: true }).decode(bytes.subarray(start, end))
    })
    expect(slices.join("\n")).toBe(expected)
  }
}

test("projection retains full tool evidence while search excludes reasoning and attachments", () => {
  const evidence = "界".repeat(30_000) + "giant-evidence-tail\0"
  const message = {
    id: "msg_fixture", type: "assistant", content: [
      { type: "reasoning", text: "not-searchable-reasoning" },
      { type: "tool", name: "fixture_tool", state: { status: "completed", input: { command: "fixture-input" }, content: [
        { type: "text", text: evidence }, { type: "file", mime: "image/png", data: "BASE64_SECRET", uri: "data:image/png;base64,OTHER_SECRET" },
      ] } },
      { type: "text", text: "Ordinary answer\0tail" },
    ],
  }
  const original = structuredClone(message)
  const projected = projectMessage(message)
  checkSpans(projected)
  expect(message).toEqual(original)
  expect(projected.role).toBe("assistant")
  expect(projected.conversation).toBe("Ordinary answer\\u0000tail")
  for (const term of ["fixture_tool", "fixture-input", "completed", "giant-evidence-tail", "not-searchable-reasoning"]) expect(projected.text).toContain(term)
  expect(projected.tools).not.toContain("not-searchable-reasoning")
  expect(projected.tools).toContain("giant-evidence-tail")
  expect(projected.text).not.toContain("BASE64_SECRET")
  expect(projected.text).not.toContain("OTHER_SECRET")
  expect(projected.text).not.toContain("\0")
  expect(projected.tools).not.toContain("\0")
  expect(Buffer.byteLength(projected.text)).toBeGreaterThan(90_000)
})

test("unknown messages, compaction, NULs and UTF-8 limits are deterministic and lossless for text", () => {
  const unknown = { id: "msg_unknown", type: "future", nested: { text: "future evidence\0", data: "data: useful ordinary evidence", attachment: { mime: "image/png", data: "BINARY_NOT_INDEXED" } } }
  const projection = projectMessage(unknown)
  checkSpans(projection)
  expect(projection.conversation_spans).toEqual([])
  expect(projection.tool_spans).toEqual([])
  expect(projection.text).toContain("future evidence")
  expect(projection.text).toContain("data: useful ordinary evidence")
  expect(projection.text).not.toContain("BINARY_NOT_INDEXED")
  expect(JSON.parse(projection.text).nested.data).toBe("data: useful ordinary evidence")
  expect(projection.conversation).toBe("")
  expect(projectMessage({ type: "future", nested: unknown.nested, id: "msg_unknown" }).text).toBe(projection.text)
  const compaction = projectMessage({ id: "msg_compact", type: "compaction", status: "completed", reason: "auto", summary: "Captured summary" })
  checkSpans(compaction)
  expect(compaction.text).toContain("compaction")
  expect(compaction.text).toContain("Captured summary")
  expect(compaction.conversation).toBe("")
  expect(bounded("界".repeat(2000), 4096)).not.toContain("�")
  const messages = [{ id: "msg_user", type: "user", text: "hello\0world" }]
  const transcript = { info: { title: "界".repeat(2000), time: { updated: Number.NaN } }, messages } as unknown as SessionTransferData
  const result = projectRetrieval(transcript, [], null)
  expect(result.messages).toEqual([])
  expect(Buffer.byteLength(result.title)).toBeLessThanOrEqual(4096)
  expect(result.activity).toBe(0)
  expect(Buffer.byteLength(projectMessage({ id: "msg", type: "界".repeat(200) }).role)).toBeLessThanOrEqual(256)
  expect(projectRetrieval(transcript, transcript.messages, null).messages[0]!.conversation).toBe("hello\\u0000world")
})

test("giant conversation/tool hit terms occur verbatim at their evidence offsets in read text", () => {
  const term = 'path\\file"quoted"\u0001control'
  const body = "界".repeat(40_000) + `\n${term}\nend`
  const user = projectMessage({ id: "msg_user", type: "user", text: body })
  const assistant = projectMessage({ id: "msg_assistant", type: "assistant", content: [{ type: "text", text: body }] })
  const tool = projectMessage({ id: "msg_tool", type: "assistant", content: [{ type: "tool", name: "fixture", state: {
    status: "completed", input: { data: "data: ordinary tool input", path: "some\\path" }, content: [{ type: "text", text: body }],
  } }] })
  for (const message of [user, assistant, tool]) {
    checkSpans(message)
    expect(message.text.indexOf(term)).toBeGreaterThan(40_000)
    expect(Buffer.byteLength(message.text.slice(0, message.text.indexOf(term)))).toBeGreaterThan(120_000)
    expect(message.text).not.toContain("\\u0001")
  }
  expect(user.conversation).toContain(term)
  expect(assistant.conversation).toContain(term)
  expect(tool.tools).toContain(term)
  expect(tool.text).toContain("data: ordinary tool input")
  expect(tool.text).toContain("some\\path")
  expect(projectMessage({ id: "msg_future", type: "assistant", content: ["future primitive", { type: "future", text: "future content" }] }).text).toContain("future primitive")
})

test("eligible spans skip earlier reasoning/metadata hits and preserve content chronology", () => {
  const filler = "界".repeat(40_000)
  const message = projectMessage({ id: "msg_ordered", type: "assistant", metadata: { needle: "not searched" }, content: [
    { type: "reasoning", text: "needle reasoning-event-A" },
    { type: "tool", name: "first_tool", state: { status: "completed", content: [{ type: "text", text: "needle tool-event-B" }] } },
    { type: "text", text: "intro", native: { excluded: "needle text metadata" } },
    { type: "reasoning", text: filler },
    { type: "text", text: "界 needle pipeline ordinary-event-C\0" },
    { type: "tool", name: "second_tool", state: { status: "error", error: "界 tool-event-D" } },
    { type: "text", text: "ordinary tail" },
  ] })
  checkSpans(message)
  expect(message.conversation).toBe("intro\n界 needle pipeline ordinary-event-C\\u0000\nordinary tail")
  const bytes = Buffer.from(message.text)
  const firstEligible = (spans: [number, number][]) => {
    for (const [start, end] of spans) {
      const relative = bytes.subarray(start, end).indexOf("needle")
      if (relative !== -1) return start + relative
    }
    return -1
  }
  const ordinaryOffset = firstEligible(message.conversation_spans)
  expect(ordinaryOffset).toBe(bytes.indexOf("needle pipeline"))
  expect(ordinaryOffset).toBeGreaterThan(120_000)
  expect(ordinaryOffset).toBeGreaterThan(message.text.indexOf("needle pipeline"))
  expect(firstEligible(message.tool_spans)).toBe(bytes.indexOf("needle tool-event-B"))
  expect(firstEligible(message.tool_spans)).toBeGreaterThan(bytes.indexOf("needle reasoning-event-A"))
  const chronology = ["reasoning-event-A", "tool-event-B", "ordinary-event-C", "tool-event-D"]
  expect(chronology.map((term) => message.text.indexOf(term))).toEqual(chronology.map((term) => message.text.indexOf(term)).sort((a, b) => a - b))
  for (const term of chronology) expect(message.text.split(term)).toHaveLength(2)

  const toolOnly = projectMessage({ id: "msg_tool_eligible", type: "assistant", content: [
    { type: "reasoning", text: "needle excluded earlier reasoning" },
    { type: "future", text: "needle excluded unknown content" },
    { type: "text", text: "intro", native: { note: "needle excluded metadata" } },
    { type: "reasoning", text: filler },
    { type: "tool", name: "eligible_tool", state: { status: "completed", input: { path: "界\\fixture" }, content: [{ type: "text", text: "界 needle pipeline" }] } },
  ] })
  checkSpans(toolOnly)
  const toolBytes = Buffer.from(toolOnly.text)
  const [start, end] = toolOnly.tool_spans[0]!
  const toolOffset = start + toolBytes.subarray(start, end).indexOf("needle")
  expect(toolOffset).toBe(toolBytes.indexOf("needle pipeline"))
  expect(toolOffset).toBeGreaterThan(120_000)
  expect(toolOnly.conversation).not.toContain("needle")
})

test("spans cover user/shell text only, omitting empty and historical-only variants", () => {
  for (const type of ["user", "system", "synthetic", "skill", "future"]) {
    const message = projectMessage({ id: "msg", type, text: "界 text\0tail" })
    checkSpans(message)
    expect(message.conversation_spans.length).toBe(type === "user" ? 1 : 0)
    expect(message.tool_spans).toEqual([])
  }
  const shell = projectMessage({ id: "msg_shell", type: "shell", command: "echo 界", output: "shell evidence\0" })
  checkSpans(shell)
  expect(shell.tool_spans).toHaveLength(1)
  expect(shell.conversation_spans).toEqual([])
  const empty = projectMessage({ id: "msg_empty", type: "assistant", content: [{ type: "text", text: "" }, { type: "text", text: "界" }, { type: "text", text: "" }] })
  checkSpans(empty)
  expect(empty.conversation).toBe("界")
  expect(empty.conversation_spans).toHaveLength(1)
})
