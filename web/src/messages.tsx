import { memo, useState, type ReactNode } from "react"
import DOMPurify from "dompurify"
import { Marked } from "marked"
import { object, type Document } from "./api"

const escape = (text: string) =>
  text
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
const markdown = new Marked({
  gfm: true,
  renderer: {
    html: ({ text }) => escape(text),
    image: ({ text }) => `<span class="attachment">Image · ${escape(text || "attachment")}</span>`,
  },
})

// Links to image artifacts, on this reader or as written by agents with its full URL.
const artifactImage = /^(?:https?:\/\/[^/]+)?\/artifacts\/(art_[a-z0-9]{12})(?:\/content)?$/
const artifactMarkdown = new Marked({
  gfm: true,
  renderer: {
    html: ({ text }) => escape(text),
    image: ({ href, text }) => {
      const match = artifactImage.exec(href)
      return `<img src="${escape(match ? `/artifacts/${match[1]}/content` : href)}" alt="${escape(text)}">`
    },
  },
})

/** Sanitized Markdown. Only artifact Markdown shows images. */
export function Markdown({ text, artifact = false }: { text: string; artifact?: boolean }) {
  const parser = artifact ? artifactMarkdown : markdown
  const html = DOMPurify.sanitize(parser.parse(text, { async: false }), {
    USE_PROFILES: { html: true },
    FORBID_TAGS: [
      ...(artifact ? [] : ["img"]),
      "style",
      "form",
      "input",
      "button",
      "textarea",
      "select",
      "video",
      "audio",
      "iframe",
      "object",
      "embed",
    ],
    FORBID_ATTR: ["style"],
  })
  return <div className="prose" dangerouslySetInnerHTML={{ __html: html }} />
}

export function Disclosure({
  label,
  meta,
  children,
  className = "",
}: {
  label: ReactNode
  meta?: ReactNode
  children: ReactNode
  className?: string
}) {
  const [open, setOpen] = useState(false)
  return (
    <details
      className={`disclosure ${className}`}
      onToggle={(event) => setOpen(event.currentTarget.open)}
    >
      <summary>
        <span className="chevron" aria-hidden="true">
          ›
        </span>
        <span className="disclosure-label">{label}</span>
        {meta && <span className="disclosure-meta">{meta}</span>}
      </summary>
      {open && <div className="disclosure-body">{children}</div>}
    </details>
  )
}

function Code({ value }: { value: unknown }) {
  return (
    <pre>
      <code>{typeof value === "string" ? value : JSON.stringify(value, null, 2)}</code>
    </pre>
  )
}

function RawMessage({ message }: { message: Document }) {
  return (
    <article className="message context-message" data-message-id={message.id}>
      <Disclosure label="Raw message">
        <Code value={message} />
      </Disclosure>
    </article>
  )
}

function Attachment({ value }: { value: unknown }) {
  if (!object(value))
    return (
      <Disclosure label="Attachment">
        <Code value={value} />
      </Disclosure>
    )
  return (
    <div className="attachment">
      {typeof value.name === "string" ? value.name : "File attachment"}
      <span>{typeof value.mime === "string" ? value.mime : ""}</span>
    </div>
  )
}

function Tool({ tool }: { tool: Document }) {
  const { state } = tool
  if (
    !object(state) ||
    typeof tool.name !== "string" ||
    !(typeof state.input === "string" || object(state.input))
  )
    return (
      <Disclosure label="Tool data">
        <Code value={tool} />
      </Disclosure>
    )
  const input =
    typeof state.input === "string"
      ? state.input
      : Object.entries(state.input).find(([key]) =>
          ["filePath", "path", "command", "pattern", "query", "url"].includes(key),
        )?.[1]
  return (
    <Disclosure
      className={state.status === "error" ? "tool failed" : "tool"}
      label={
        <>
          <span className="tool-name">{tool.name}</span>
          {typeof input === "string" && <span className="tool-preview">{input}</span>}
        </>
      }
      meta={state.status === "error" ? "Failed" : undefined}
    >
      <h4>Input</h4>
      <Code value={state.input} />
      {"error" in state && (
        <>
          <h4>Error</h4>
          <Code value={state.error} />
        </>
      )}
      {Array.isArray(state.content) && (
        <>
          <h4>Result</h4>
          {state.content.map((content: unknown, index) =>
            object(content) && content.type === "text" && typeof content.text === "string" ? (
              <Code key={index} value={content.text} />
            ) : object(content) && content.type === "file" ? (
              <Attachment key={index} value={content} />
            ) : (
              <Code key={index} value={content} />
            ),
          )}
        </>
      )}
      {state.content !== undefined && !Array.isArray(state.content) && (
        <Code value={state.content} />
      )}
      {!("content" in state) && state.status !== "error" && (
        <p className="muted">
          Status: {typeof state.status === "string" ? state.status : "Unknown"}
        </p>
      )}
    </Disclosure>
  )
}

function AssistantContent({ content, error }: { content: unknown[]; error?: unknown }) {
  const parts: ReactNode[] = []
  for (let index = 0; index < content.length; index++) {
    const part = content[index]
    if (object(part) && part.type === "text" && typeof part.text === "string")
      parts.push(<Markdown key={index} text={part.text} />)
    else if (object(part) && part.type === "reasoning" && typeof part.text === "string")
      parts.push(
        <Disclosure key={index} label="Thinking">
          <Markdown text={part.text} />
        </Disclosure>,
      )
    else if (object(part) && part.type === "tool") {
      const tools = [part]
      let next = content[index + 1]
      while (object(next) && next.type === "tool") {
        tools.push(next)
        index++
        next = content[index + 1]
      }
      const elements = tools.map((tool, index) => (
        <Tool key={typeof tool.id === "string" ? tool.id : index} tool={tool} />
      ))
      parts.push(
        tools.length > 1 ? (
          <Disclosure key={index} label={`${tools.length} tool calls`}>
            {elements}
          </Disclosure>
        ) : (
          <div key={index}>{elements}</div>
        ),
      )
    } else
      parts.push(
        <Disclosure key={index} label="Message content">
          <Code value={part} />
        </Disclosure>,
      )
  }
  if (error)
    parts.push(
      <Disclosure key="error" label="Assistant error" className="failed">
        <Code value={error} />
      </Disclosure>,
    )
  return <>{parts}</>
}

export const Message = memo(function Message({
  message,
  previous,
}: {
  message: Document
  previous?: Document
}) {
  if (message.type === "user") {
    if (typeof message.text !== "string") return <RawMessage message={message} />
    return (
      <article className="message user-message" data-message-id={message.id}>
        <div className="message-label">You</div>
        <div className="user-bubble">
          <div className="user-text">{message.text}</div>
          {Array.isArray(message.files) &&
            message.files.map((file: unknown, index) => <Attachment key={index} value={file} />)}
        </div>
      </article>
    )
  }
  if (message.type === "assistant") {
    if (!Array.isArray(message.content)) return <RawMessage message={message} />
    return (
      <article
        className={`message assistant-message ${previous?.type === "assistant" ? "continuation" : ""}`}
        data-message-id={message.id}
      >
        {previous?.type !== "assistant" && (
          <div className="message-label">
            <span className="assistant-mark" aria-hidden="true">
              ✳
            </span>
            Assistant
            <span className="agent-label">
              {typeof message.agent === "string" ? message.agent : ""}
            </span>
          </div>
        )}
        <AssistantContent content={message.content} error={message.error} />
      </article>
    )
  }
  if (message.type === "compaction")
    return (
      <article className="message context-message" data-message-id={message.id}>
        <Disclosure
          label={
            message.status === "failed"
              ? "Compaction failed"
              : message.status === "completed"
                ? "Context compacted"
                : "Compaction"
          }
          meta={typeof message.reason === "string" ? message.reason : undefined}
        >
          {typeof message.summary === "string" ? (
            <Markdown text={message.summary} />
          ) : (
            <Code value={message.error ?? message} />
          )}
        </Disclosure>
      </article>
    )
  if (message.type === "synthetic" || message.type === "system") {
    if (typeof message.text !== "string") return <RawMessage message={message} />
    return (
      <article className="message context-message" data-message-id={message.id}>
        <Disclosure
          label={
            typeof message.description === "string"
              ? message.description
              : message.type === "system"
                ? "System instructions"
                : "Context"
          }
        >
          <Markdown text={message.text} />
        </Disclosure>
      </article>
    )
  }
  if (message.type === "shell") {
    if (typeof message.command !== "string") return <RawMessage message={message} />
    return (
      <article className="message context-message" data-message-id={message.id}>
        <Disclosure
          label={
            <>
              <span className="tool-name">Shell</span>
              <span className="tool-preview">{message.command}</span>
            </>
          }
          meta={typeof message.status === "string" ? message.status : undefined}
        >
          <Code value={message.command} />
          {message.output !== undefined && (
            <Code
              value={
                object(message.output) && typeof message.output.output === "string"
                  ? message.output.output
                  : message.output
              }
            />
          )}
        </Disclosure>
      </article>
    )
  }
  if (message.type === "idle") return null
  const label =
    message.type === "agent-switched" && typeof message.agent === "string"
      ? `Agent · ${message.agent}`
      : message.type === "model-switched" &&
          object(message.model) &&
          typeof message.model.id === "string"
        ? `Model · ${message.model.id}`
        : message.type === "location-switched"
          ? "Location changed"
          : message.type === "skill"
            ? "Skill activated"
            : "Session event"
  return (
    <div className="session-event" data-message-id={message.id}>
      <Disclosure label={label}>
        <Code value={message} />
      </Disclosure>
    </div>
  )
})
