import { memo, useEffect, useMemo, useRef, useState } from "react"
import {
  ArchiveError,
  object,
  read,
  sessionKey,
  sessionPath,
  type Session,
  type SessionPage,
  type Transcript,
} from "./api"
import { Message } from "./messages"

function SpoolMark({ large = false }: { large?: boolean }) {
  return (
    <svg
      className={large ? "spool-mark large" : "spool-mark"}
      viewBox="0 0 32 32"
      fill="none"
      aria-hidden="true"
    >
      <path
        d="M9 7h14M9 25h14M11 7v18M21 7v18M11 11h10M11 16h10M11 21h10"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
      />
      <path
        d="M7 5v4M25 5v4M7 23v4M25 23v4"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
      />
    </svg>
  )
}

function PanelIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" aria-hidden="true">
      <rect x="3" y="4" width="18" height="16" rx="2" stroke="currentColor" strokeWidth="1.5" />
      <path d="M9 4v16" stroke="currentColor" strokeWidth="1.5" />
    </svg>
  )
}

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

function title(session: Session): string {
  return typeof session.info.title === "string"
    ? session.info.title.trim() || "Untitled session"
    : "Untitled session"
}

function selectedKey(): string | null {
  const params = new URLSearchParams(window.location.hash.slice(1))
  const source = params.get("source"),
    session = params.get("session")
  return source && session ? `${source}/${session}` : null
}

function errorText(error: unknown): string {
  return error instanceof ArchiveError
    ? error.message
    : "Couldn’t load the archive. Please try again."
}

export function App() {
  const [sessions, setSessions] = useState<Session[]>([])
  const [sources, setSources] = useState<SessionPage["sources"]>([])
  const [nextCursor, setNextCursor] = useState<string | null>(null)
  const [loadingMore, setLoadingMore] = useState(false)
  const [loading, setLoading] = useState(true)
  const [listError, setListError] = useState("")
  const [listAttempt, setListAttempt] = useState(0)
  const [query, setQuery] = useState("")
  const [source, setSource] = useState("")
  const [selection, setSelection] = useState(selectedKey)
  const [mobile, setMobile] = useState(() => window.matchMedia("(max-width: 720px)").matches)
  const [sidebarOpen, setSidebarOpen] = useState(() => window.innerWidth > 720)
  const search = useRef<HTMLInputElement>(null)
  const lifetime = useRef<AbortController | null>(null)
  const listPath = `/api/sessions?${new URLSearchParams({ limit: "50", q: query.trim(), ...(source ? { source } : {}) })}`

  useEffect(() => {
    const controller = new AbortController()
    lifetime.current = controller
    setLoading(true)
    setLoadingMore(false)
    setListError("")
    setSessions([])
    setNextCursor(null)
    const timer = setTimeout(
      () => {
        void read<SessionPage>(listPath, controller.signal)
          .then((page) => {
            if (controller.signal.aborted) return
            setSessions(page.sessions)
            setSources(page.sources)
            setNextCursor(page.next_cursor)
          })
          .catch((error: unknown) => {
            if (!controller.signal.aborted) setListError(errorText(error))
          })
          .finally(() => {
            if (!controller.signal.aborted) setLoading(false)
          })
      },
      query.trim() ? 180 : 0,
    )
    return () => {
      clearTimeout(timer)
      controller.abort()
    }
  }, [listPath, listAttempt])

  async function moreSessions() {
    if (!nextCursor || loadingMore) return
    setLoadingMore(true)
    setListError("")
    const signal = lifetime.current!.signal
    try {
      const page = await read<SessionPage>(
        `${listPath}&cursor=${encodeURIComponent(nextCursor)}`,
        signal,
      )
      if (signal.aborted) return
      setSessions((current) => {
        const merged = new Map(current.map((session) => [sessionKey(session), session]))
        for (const session of page.sessions) merged.set(sessionKey(session), session)
        return [...merged.values()]
      })
      setNextCursor(page.next_cursor)
    } catch (error) {
      if (!signal.aborted) setListError(errorText(error))
    } finally {
      if (!signal.aborted) setLoadingMore(false)
    }
  }

  useEffect(() => {
    const viewport = window.matchMedia("(max-width: 720px)")
    const onResize = () => {
      setMobile(viewport.matches)
      setSidebarOpen(!viewport.matches)
    }
    const onHashChange = () => setSelection(selectedKey())
    const onKeyDown = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        event.preventDefault()
        setSidebarOpen(true)
        requestAnimationFrame(() => search.current?.focus())
      }
      if (event.key === "Escape" && window.innerWidth <= 720) setSidebarOpen(false)
    }
    viewport.addEventListener("change", onResize)
    window.addEventListener("hashchange", onHashChange)
    window.addEventListener("keydown", onKeyDown)
    return () => {
      viewport.removeEventListener("change", onResize)
      window.removeEventListener("hashchange", onHashChange)
      window.removeEventListener("keydown", onKeyDown)
    }
  }, [])

  const selected = sessions.find((session) => sessionKey(session) === selection)
  const direct = useMemo(() => {
    const params = new URLSearchParams(window.location.hash.slice(1))
    const source_id = params.get("source"),
      session_id = params.get("session")
    return source_id && session_id ? { source_id, session_id } : null
  }, [selection])

  function select(session: Session) {
    window.location.hash = new URLSearchParams({
      source: session.source_id,
      session: session.session_id,
    }).toString()
    setSelection(sessionKey(session))
    if (window.innerWidth <= 720) setSidebarOpen(false)
  }

  return (
    <div className="app">
      {sidebarOpen && (
        <button
          className="sidebar-backdrop"
          aria-label="Close sessions"
          tabIndex={-1}
          onClick={() => setSidebarOpen(false)}
        />
      )}
      <aside
        id="session-sidebar"
        className="sidebar"
        aria-label="Session archive"
        hidden={!sidebarOpen}
      >
        <div className="brand-row">
          <a className="brand" href="#" aria-label="Spool home">
            <SpoolMark />
            <span>Spool</span>
          </a>
          <button
            className="icon-button"
            aria-label="Hide sessions"
            onClick={() => setSidebarOpen(false)}
          >
            <PanelIcon />
          </button>
        </div>
        <div className="sidebar-controls">
          <div className="search-field">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" aria-hidden="true">
              <circle cx="10.5" cy="10.5" r="6.5" stroke="currentColor" strokeWidth="1.6" />
              <path d="m16 16 4 4" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
            </svg>
            <input
              ref={search}
              aria-label="Find a session"
              placeholder="Find a session…"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
            />
            <kbd>⌘ K</kbd>
          </div>
          {sources.length > 1 && (
            <select
              className="source-filter"
              aria-label="Filter by source"
              value={source}
              onChange={(event) => setSource(event.target.value)}
            >
              <option value="">All sources</option>
              {sources.map(({ source_id, source_label }) => (
                <option key={source_id} value={source_id}>
                  {source_label}
                </option>
              ))}
            </select>
          )}
        </div>
        <nav className="session-list" aria-label="Sessions">
          {loading && sessions.length === 0 && (
            <p className="sidebar-note" role="status">
              Loading sessions…
            </p>
          )}
          {listError && (
            <div className="sidebar-note" role="alert">
              <p>{listError}</p>
              <button
                className="text-button"
                onClick={() => {
                  if (nextCursor) void moreSessions()
                  else setListAttempt((attempt) => attempt + 1)
                }}
              >
                Try again
              </button>
            </div>
          )}
          {!loading && !listError && sessions.length === 0 && (
            <div className="sidebar-note">
              <p>{query || source ? "No matching sessions." : "No sessions yet."}</p>
              {(query || source) && (
                <button
                  className="text-button"
                  onClick={() => {
                    setQuery("")
                    setSource("")
                  }}
                >
                  Clear filters
                </button>
              )}
            </div>
          )}
          {sessions.map((session) => (
            <button
              className={`session-row ${sessionKey(session) === selection ? "selected" : ""}`}
              key={sessionKey(session)}
              aria-current={sessionKey(session) === selection ? "page" : undefined}
              onClick={() => select(session)}
              title={`${title(session)}\n${session.source_label}`}
            >
              <span className="session-title">{title(session)}</span>
            </button>
          ))}
          {nextCursor && !listError && (
            <button
              className="more-sessions text-button"
              disabled={loadingMore}
              onClick={() => void moreSessions()}
            >
              {loadingMore ? "Loading…" : "Load more sessions"}
            </button>
          )}
        </nav>
        <div className="sidebar-footer">
          <span>
            {loading
              ? "Loading archive…"
              : `${sessions.length.toLocaleString()} ${nextCursor ? "shown" : "sessions"}`}
          </span>
          <span>
            {sources.length} {sources.length === 1 ? "source" : "sources"}
          </span>
        </div>
      </aside>
      <main className="main" inert={mobile && sidebarOpen}>
        <header className="topbar">
          {!sidebarOpen && (
            <button
              className="icon-button"
              aria-label="Show sessions"
              aria-controls="session-sidebar"
              aria-expanded={sidebarOpen}
              onClick={() => {
                setSidebarOpen(true)
                requestAnimationFrame(() => search.current?.focus())
              }}
            >
              <PanelIcon />
            </button>
          )}
          <span className="topbar-title">
            {selected ? title(selected) : selection ? "Session" : "Archive"}
          </span>
          <span className="read-only">Read only</span>
        </header>
        {direct ? (
          <Conversation key={selection} session={selected || direct} />
        ) : (
          <div className="empty-state">
            <SpoolMark large />
            <h1>A little less lost.</h1>
            <p>Your conversations, kept together.</p>
            <button
              className="text-button"
              onClick={() => {
                setSidebarOpen(true)
                requestAnimationFrame(() => search.current?.focus())
              }}
            >
              Find a session <span aria-hidden="true">↗</span>
            </button>
          </div>
        )}
      </main>
    </div>
  )
}

const Conversation = memo(function Conversation({
  session,
}: {
  session: Pick<Session, "source_id" | "session_id">
}) {
  const [transcript, setTranscript] = useState<Transcript>()
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState("")
  const [attempt, setAttempt] = useState(0)
  const [loadingMore, setLoadingMore] = useState(false)
  const [stale, setStale] = useState(false)
  const lifetime = useRef<AbortController | null>(null)
  const column = useRef<HTMLDivElement>(null)
  const path = sessionPath(session)

  useEffect(() => {
    const controller = new AbortController()
    lifetime.current = controller
    setTranscript(undefined)
    setError("")
    setStale(false)
    setLoading(true)
    column.current?.scrollTo(0, 0)
    void read<Transcript>(path, controller.signal)
      .then(setTranscript)
      .catch((error: unknown) => {
        if (!controller.signal.aborted) setError(errorText(error))
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false)
      })
    return () => controller.abort()
  }, [path, attempt])

  async function more() {
    if (!transcript?.next_cursor || loadingMore || stale) return
    setLoadingMore(true)
    setError("")
    const signal = lifetime.current!.signal
    try {
      const page = await read<Transcript>(
        `${path}?cursor=${encodeURIComponent(transcript.next_cursor)}`,
        signal,
      )
      setTranscript((current) =>
        current ? { ...page, messages: [...current.messages, ...page.messages] } : page,
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
              </div>
            </div>
            <div className="messages">
              {transcript.messages.map((message, index) => (
                <Message
                  key={message.id}
                  message={message}
                  previous={transcript.messages[index - 1]}
                />
              ))}
            </div>
            {transcript.messages.length === 0 && (
              <p className="loading-note">No settled messages in this session yet.</p>
            )}
          </>
        )}
        {error && (
          <div className="error-state" role="alert">
            <p>{error}</p>
            <button
              className="subtle-button"
              onClick={() => {
                if (transcript && !stale) void more()
                else setAttempt((attempt) => attempt + 1)
              }}
            >
              {stale ? "Reload session" : "Try again"}
            </button>
          </div>
        )}
        {transcript?.next_cursor && !stale && !error && (
          <button
            className="load-more subtle-button"
            disabled={loadingMore}
            onClick={() => void more()}
          >
            {loadingMore ? "Loading…" : "Continue reading"}
            <span aria-hidden="true">↓</span>
          </button>
        )}
        {transcript && !transcript.next_cursor && transcript.messages.length > 0 && (
          <div className="end-mark">
            <span />
            <SpoolMark />
            <span />
          </div>
        )}
      </div>
    </div>
  )
})
