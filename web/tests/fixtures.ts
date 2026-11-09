const now = Date.UTC(2026, 10, 8, 15, 30)
const time = (offset: number) => ({ created: now + offset, completed: now + offset + 1 })

const user = (id: string, text: string) => ({ id, type: "user", time: time(0), text })
const assistant = (id: string, text: string) => ({
  id,
  type: "assistant",
  time: time(1),
  agent: "build",
  model: { id: "synthetic", providerID: "fixture" },
  content: [{ type: "text", text }],
})

export const primaryMessages = [
  user(
    "message-user",
    "Let’s give the archive a place to live. A quiet sidebar, readable conversations, and no composer.",
  ),
  ...["git status --short", "git diff --stat"].map((command, index) => ({
    ...assistant(`message-preflight-${index}`, ""),
    content: [
      {
        type: "tool",
        id: `preflight-${index}`,
        name: "shell",
        state: {
          status: "completed",
          input: { command },
          content: [{ type: "text", text: "Working tree clean." }],
        },
      },
    ],
  })),
  {
    ...assistant("message-response", ""),
    content: [
      {
        type: "reasoning",
        text: "Keep the reading experience simple. Tools should be available without interrupting the conversation.",
      },
      {
        type: "tool",
        id: "tool-read",
        name: "read",
        time: time(2),
        state: {
          status: "completed",
          input: { path: "server/src/http.rs" },
          content: [{ type: "text", text: 'Router::new()\n    .route("/healthz", get(health))' }],
        },
      },
      {
        type: "tool",
        id: "tool-test",
        name: "bash",
        time: time(3),
        state: {
          status: "completed",
          input: { command: "cargo test --workspace" },
          content: [{ type: "text", text: "test result: ok. 12 passed; 0 failed" }],
        },
      },
      {
        type: "text",
        text: "A small, read-only browser is a good fit.\n\n### Keep the conversation in focus\n\n- **Recent sessions** first, in a compact sidebar.\n- Tool calls and reasoning tucked away until you need them.\n- Markdown, code, and compaction summaries kept intact.\n\nThe reader runs separately from ingestion, using a read-only PostgreSQL connection.\n\n```rust\nlet archive = browse_router(pool, assets);\naxum::serve(listener, archive).await?;\n```\n\nNo new account system, and nothing exposed on the public internet.",
      },
    ],
  },
  user("message-followup", "Nice. And if the conversation was compacted?"),
  {
    id: "message-compaction",
    type: "compaction",
    time: time(4),
    status: "completed",
    reason: "auto",
    summary:
      "## Where we left off\n\nThe archive reader uses a separate read-only listener. The ingestion service stays unchanged.",
  },
  assistant(
    "message-final",
    "The compaction stays in the conversation. Expand **Context compacted** to read the summary, then keep going from there.",
  ),
]

export const fixtures = [
  {
    source: "studio",
    id: "ses_reader",
    title: "A quieter session browser",
    project: "spool",
    updated: now + 100,
    messages: primaryMessages,
  },
  {
    source: "laptop",
    id: "ses_reader",
    title: "The same ID, another machine",
    project: "spool",
    updated: now + 80,
    messages: [
      user("other-user", "An independent store with the same session ID."),
      assistant("other-answer", "This conversation belongs to the laptop source."),
    ],
  },
  {
    source: "studio",
    id: "ses_security",
    title: "Untrusted content stays inert",
    project: "spool",
    updated: now + 60,
    messages: [
      user("safety-user", "Check archived markup without fetching anything."),
      assistant(
        "safety-answer",
        'Before the markup.\n\n<img src="https://example.invalid/track" onerror="window.spoolInjected=true">\n\n<script>window.spoolInjected=true</script>\n\n![Remote image](https://example.invalid/tracker.png)\n\n[Unsafe link](javascript:window.spoolInjected=true)\n\n[Safe documentation](https://example.com/docs)\n\nAfter the markup. Café 東京.\u0000tail',
      ),
    ],
  },
  {
    source: "laptop",
    id: "ses_paged",
    title: "A long conversation",
    project: "yard",
    updated: now + 40,
    messages: Array.from({ length: 203 }, (_, index) =>
      user(`long-${index}`, `Conversation entry ${String(index).padStart(3, "0")}`),
    ),
  },
  {
    source: "studio",
    id: "ses_empty",
    title: "An empty session",
    project: "yard",
    updated: now + 20,
    messages: [],
  },
  {
    source: "laptop",
    id: "ses_errors",
    title: "Errors and session context",
    project: "yard",
    updated: now,
    messages: [
      {
        ...assistant("error-answer", ""),
        content: [
          {
            type: "tool",
            id: "failed-tool",
            name: "read",
            time: time(0),
            state: {
              status: "error",
              input: { path: "missing.txt" },
              error: { type: "file_not_found", message: "The synthetic file does not exist." },
            },
          },
        ],
      },
      {
        id: "failed-compaction",
        type: "compaction",
        time: time(1),
        status: "failed",
        reason: "manual",
        error: { type: "synthetic_failure", message: "Compaction did not complete." },
      },
      {
        id: "shell",
        type: "shell",
        time: time(2),
        command: "printf 'hello'",
        status: "exited",
        exit: 0,
        output: { output: "hello", cursor: 0, size: 5, truncated: false },
      },
      { id: "system", type: "system", time: time(3), text: "Synthetic system instructions." },
      {
        id: "skill",
        type: "skill",
        time: time(4),
        skill: "fixture",
        name: "Fixture skill",
        text: "Synthetic skill content.",
      },
      {
        id: "future",
        type: "future-event",
        time: time(5),
        preserved: "Future message fields remain readable.",
      },
    ],
  },
]

export function envelope(fixture: (typeof fixtures)[number], base_revision: string | null = null) {
  return {
    base_revision,
    format: "opencode.v2",
    upstream_version: "synthetic",
    collector_version: "synthetic",
    captured_at: new Date(now).toISOString(),
    info: {
      id: fixture.id,
      title: fixture.title,
      location: { directory: `/synthetic/projects/${fixture.project}` },
      time: { created: now, updated: fixture.updated },
    },
    message_order: fixture.messages.map((message) => message.id),
    upsert_messages: fixture.messages,
  }
}
