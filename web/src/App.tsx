import { memo, useCallback, useEffect, useRef, useState } from "react"
import {
  ArchiveError,
  linkedSession,
  object,
  read,
  sessionHref,
  sessionKey,
  sessionPath,
  type Session,
  type SessionPage,
  type Transcript,
} from "./api"
import { Message } from "./messages"

export function SpoolMark({ large = false }: { large?: boolean }) {
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

export function PanelIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" aria-hidden="true">
      <rect x="3" y="4" width="18" height="16" rx="2" stroke="currentColor" strokeWidth="1.5" />
      <path d="M9 4v16" stroke="currentColor" strokeWidth="1.5" />
    </svg>
  )
}

/** Switches between the session archive and artifacts, which are separate sections. */
export function ModeSwitch({ current }: { current: "sessions" | "artifacts" }) {
  return (
    <nav className="mode-switch" aria-label="Archive section">
      <a href="/" aria-current={current === "sessions" ? "page" : undefined}>
        Sessions
      </a>
      <a href="/artifacts" aria-current={current === "artifacts" ? "page" : undefined}>
        Artifacts
      </a>
    </nav>
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

/** The one-based message index in the URL fragment, such as `#12`. */
function linkedIndex(): number | null {
  const index = /^#([1-9][0-9]{0,5})$/.exec(window.location.hash)?.[1]
  return index ? Number(index) : null
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
  const [selection, setSelection] = useState(linkedSession)
  // The source and session the selected reference resolved to, once loaded.
  const [opened, setOpened] = useState<{ reference: string; key: string }>()
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
    const onPopState = () => setSelection(linkedSession())
    const onKeyDown = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        event.preventDefault()
        setSidebarOpen(true)
        requestAnimationFrame(() => search.current?.focus())
      }
      if (event.key === "Escape" && window.innerWidth <= 720) setSidebarOpen(false)
    }
    viewport.addEventListener("change", onResize)
    window.addEventListener("popstate", onPopState)
    window.addEventListener("keydown", onKeyDown)
    return () => {
      viewport.removeEventListener("change", onResize)
      window.removeEventListener("popstate", onPopState)
      window.removeEventListener("keydown", onKeyDown)
    }
  }, [])

  const selected =
    opened?.reference === selection
      ? sessions.find((session) => sessionKey(session) === opened.key)
      : sessions.find((session) => session.session === selection)

  function select(session: Session) {
    window.history.pushState(null, "", sessionHref(session.session))
    setSelection(session.session)
    if (window.innerWidth <= 720) setSidebarOpen(false)
  }

  const open = useCallback((reference: string, transcript: Transcript) => {
    setOpened({ reference, key: sessionKey(transcript) })
    // Prefer the shortest reference, such as for links that name the source by UUID.
    if (reference !== transcript.session)
      window.history.replaceState(null, "", sessionHref(transcript.session) + window.location.hash)
  }, [])

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
          <a
            className="brand"
            href="/"
            aria-label="Spool home"
            onClick={(event) => {
              if (event.metaKey || event.ctrlKey || event.shiftKey || event.button !== 0) return
              event.preventDefault()
              window.history.pushState(null, "", "/")
              setSelection(null)
            }}
          >
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
        <ModeSwitch current="sessions" />
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
            <a
              className={`session-row ${session === selected ? "selected" : ""}`}
              key={sessionKey(session)}
              href={sessionHref(session.session)}
              aria-current={session === selected ? "page" : undefined}
              onClick={(event) => {
                if (event.metaKey || event.ctrlKey || event.shiftKey || event.button !== 0) return
                event.preventDefault()
                select(session)
              }}
              title={`${title(session)}\n${session.source_label}`}
            >
              <span className="session-title">{title(session)}</span>
            </a>
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
        {selection ? (
          <Conversation key={selection} reference={selection} onOpen={open} />
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
    column.current?.scrollTo(0, 0)
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

  // A linked message may be past the first page: keep loading until it is shown.
  useEffect(() => {
    if (!transcript || !target || reached.current === target) return
    if (target > transcript.messages.length) {
      if (transcript.next_cursor && !error) void more()
      return
    }
    reached.current = target
    document.getElementById(String(target))?.scrollIntoView({ block: "start" })
  }, [transcript, target, error])

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
                  index={index + 1}
                  linked={index + 1 === target}
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
                  if (transcript && !stale) void more()
                  else setAttempt((attempt) => attempt + 1)
                }}
              >
                {stale ? "Reload session" : "Try again"}
              </button>
            )}
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
