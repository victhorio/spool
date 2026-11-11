import { memo, useEffect, useLayoutEffect, useRef, useState, type FormEvent } from "react"
import {
  ArchiveError,
  object,
  read,
  sendMessage,
  sessionHref,
  sessionKey,
  sessionPath,
  type Session,
  type Transcript,
} from "./api"
import { Message } from "./messages"
import { SpoolMark } from "./shell"

function project(session: Session): string {
  const directory = object(session.info.location) && session.info.location.directory
  return typeof directory === "string"
    ? directory.split(/[\\/]/).filter(Boolean).at(-1) || "Other sessions"
    : "Other sessions"
}

function updated(session: Session): number {
  const time = object(session.info.time) && session.info.time.updated
  return typeof time === "number" && Number.isFinite(time) && Math.abs(time) <= 8.64e15
    ? time
    : Date.parse(session.received_at)
}

export function title(session: Session): string {
  return typeof session.info.title === "string"
    ? session.info.title.trim() || "Untitled session"
    : "Untitled session"
}

/** The one-based message index in the URL fragment, such as `#12`. */
function linkedIndex(): number | null {
  const index = /^#([1-9][0-9]{0,5})$/.exec(window.location.hash)?.[1]
  return index ? Number(index) : null
}

const statusLabels = { running: "Running", idle: "Online" } as const

function errorText(error: unknown): string {
  if (error instanceof ArchiveError) {
    if (error.candidates) return "More than one source archived a session with this ID. Choose one:"
    if (error.status === 409) return "This session changed. Reload it to read the latest version."
    if (error.status === 404) return "This session is no longer in the archive."
  }
  return "Couldn’t load the archive. Please try again."
}

export const Conversation = memo(function Conversation({
  reference,
  onOpen,
}: {
  reference: string
  onOpen: (reference: string, transcript: Transcript) => void
}) {
  const [transcript, setTranscript] = useState<Transcript>()
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState("")
  const [candidates, setCandidates] = useState<string[]>()
  const [attempt, setAttempt] = useState(0)
  const [loadingMore, setLoadingMore] = useState(false)
  const [stale, setStale] = useState(false)
  const [target, setTarget] = useState(linkedIndex)
  const reached = useRef<number | null>(null)
  const lifetime = useRef<AbortController | null>(null)
  const column = useRef<HTMLDivElement>(null)
  // Distance from the bottom to keep while earlier messages are added above; null opens at the end.
  const fromBottom = useRef<number | null>(null)
  const path = sessionPath(reference)

  useEffect(() => {
    const controller = new AbortController()
    lifetime.current = controller
    setTranscript(undefined)
    setError("")
    setCandidates(undefined)
    setStale(false)
    setLoading(true)
    reached.current = null
    fromBottom.current = 0
    void read<Transcript>(path, controller.signal)
      .then((transcript) => {
        setTranscript(transcript)
        onOpen(reference, transcript)
      })
      .catch((error: unknown) => {
        if (controller.signal.aborted) return
        setError(errorText(error))
        if (error instanceof ArchiveError) setCandidates(error.candidates)
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false)
      })
    return () => controller.abort()
  }, [path, attempt])

  useEffect(() => {
    const onHashChange = () => setTarget(linkedIndex())
    window.addEventListener("hashchange", onHashChange)
    return () => window.removeEventListener("hashchange", onHashChange)
  }, [])

  // A session opens at its end; adding earlier messages keeps what is in view in place.
  useLayoutEffect(() => {
    const element = column.current
    if (!transcript || !element || fromBottom.current === null) return
    element.scrollTop = element.scrollHeight - element.clientHeight - fromBottom.current
    fromBottom.current = null
  }, [transcript])

  // A linked message may be before the latest page: keep loading until it is shown.
  useEffect(() => {
    if (!transcript || !target || reached.current === target) return
    if (target < transcript.first_index) {
      if (transcript.earlier_cursor && !error) void earlier()
      return
    }
    reached.current = target
    document.getElementById(String(target))?.scrollIntoView({ block: "start" })
  }, [transcript, target, error])

  async function earlier() {
    if (!transcript?.earlier_cursor || loadingMore || stale) return
    setLoadingMore(true)
    setError("")
    const signal = lifetime.current!.signal
    try {
      const page = await read<Transcript>(
        `${path}?cursor=${encodeURIComponent(transcript.earlier_cursor)}`,
        signal,
      )
      const element = column.current!
      fromBottom.current = element.scrollHeight - element.clientHeight - element.scrollTop
      setTranscript((current) =>
        current ? { ...page, messages: [...page.messages, ...current.messages] } : page,
      )
    } catch (error) {
      if (!signal.aborted) {
        setError(errorText(error))
        setStale(error instanceof ArchiveError && error.status === 409)
      }
    } finally {
      if (!signal.aborted) setLoadingMore(false)
    }
  }

  return (
    <div className="conversation-scroll" ref={column}>
      <div className="conversation" aria-label="Conversation" aria-busy={loading}>
        {loading && (
          <p className="loading-note" role="status">
            Opening conversation…
          </p>
        )}
        {transcript && (
          <>
            <div className="conversation-heading">
              <p className="conversation-project">{project(transcript)}</p>
              <h1>{title(transcript)}</h1>
              <div className="conversation-meta">
                <span>{transcript.source_label}</span>
                <span aria-hidden="true">·</span>
                <time dateTime={new Date(updated(transcript)).toISOString()}>
                  {new Intl.DateTimeFormat(undefined, {
                    dateStyle: "medium",
                    timeStyle: "short",
                  }).format(updated(transcript))}
                </time>
                {transcript.status !== "offline" && (
                  <>
                    <span aria-hidden="true">·</span>
                    <span className="session-status">{statusLabels[transcript.status]}</span>
                  </>
                )}
              </div>
            </div>
          </>
        )}
        {error && (
          <div className="error-state" role="alert">
            <p>{error}</p>
            {candidates ? (
              <ul className="candidates">
                {candidates.map((candidate) => (
                  <li key={candidate}>
                    <a href={sessionHref(candidate) + window.location.hash}>{candidate}</a>
                  </li>
                ))}
              </ul>
            ) : (
              <button
                className="subtle-button"
                onClick={() => {
                  if (transcript && !stale) void earlier()
                  else setAttempt((attempt) => attempt + 1)
                }}
              >
                {stale ? "Reload session" : "Try again"}
              </button>
            )}
          </div>
        )}
        {transcript?.earlier_cursor && !stale && !error && (
          <button
            className="load-more subtle-button"
            disabled={loadingMore}
            onClick={() => void earlier()}
          >
            {loadingMore ? "Loading…" : "Show earlier messages"}
            <span aria-hidden="true">↑</span>
          </button>
        )}
        {transcript && (
          <div className="messages">
            {transcript.messages.map((message, index) => (
              <Message
                key={message.id}
                index={transcript.first_index + index}
                linked={transcript.first_index + index === target}
                message={message}
                previous={transcript.messages[index - 1]}
              />
            ))}
          </div>
        )}
        {transcript?.messages.length === 0 && (
          <p className="loading-note">No settled messages in this session yet.</p>
        )}
        {transcript && transcript.messages.length > 0 && (
          <div className="end-mark">
            <span />
            <SpoolMark />
            <span />
          </div>
        )}
        {transcript && transcript.status !== "offline" && (
          <Composer session={sessionKey(transcript)} />
        )}
      </div>
    </div>
  )
})

/** Sends the owner's message to a session whose machine is online. */
function Composer({ session }: { session: string }) {
  const [text, setText] = useState("")
  const [busy, setBusy] = useState(false)
  const [outcome, setOutcome] = useState<{ error: boolean; text: string }>()

  async function submit(event: FormEvent) {
    event.preventDefault()
    if (!text.trim() || busy) return
    setBusy(true)
    setOutcome(undefined)
    try {
      await sendMessage(session, text)
      setText("")
      setOutcome({ error: false, text: "Sent. Reload to see it once the session saves it." })
    } catch (error) {
      setOutcome({
        error: true,
        text:
          error instanceof ArchiveError && error.code === "session_offline"
            ? "This session’s machine is offline."
            : "Couldn’t send the message. Please try again.",
      })
    } finally {
      setBusy(false)
    }
  }

  return (
    <form className="composer" onSubmit={submit}>
      <textarea
        aria-label="Message"
        placeholder="Message this session…"
        rows={3}
        value={text}
        onChange={(event) => setText(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
            event.preventDefault()
            event.currentTarget.form?.requestSubmit()
          }
        }}
      />
      <div className="composer-row">
        <p
          className={outcome?.error ? "composer-note problem" : "composer-note"}
          role={outcome?.error ? "alert" : "status"}
        >
          {outcome?.text}
        </p>
        <button type="submit" disabled={busy || !text.trim()}>
          Send
        </button>
      </div>
    </form>
  )
}
