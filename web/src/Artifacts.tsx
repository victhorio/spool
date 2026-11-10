import { useEffect, useRef, useState } from "react"
import { ModeSwitch, PanelIcon, SpoolMark } from "./App"
import { ArchiveError, read, readText, sessionHref, type Artifact, type ArtifactPage } from "./api"
import { Markdown } from "./messages"

const kinds: Record<string, string> = {
  "text/markdown": "Markdown",
  "text/html": "HTML",
  "text/plain": "Text",
}

function kind(artifact: Artifact): string {
  return kinds[artifact.type] ?? "Image"
}

// Larger text is left to "Open raw" rather than parsed on the page.
const renderLimit = 2 * 1024 * 1024

function selectedID(): string | null {
  return /^\/artifacts\/(art_[a-z0-9]{12})$/.exec(window.location.pathname)?.[1] ?? null
}

function errorText(error: unknown, subject: string): string {
  return error instanceof ArchiveError && error.status === 404
    ? `This ${subject} is no longer in the archive.`
    : "Couldn’t load the archive. Please try again."
}

const dateTime = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" })

export function Artifacts() {
  const [artifacts, setArtifacts] = useState<Artifact[]>([])
  const [nextCursor, setNextCursor] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)
  const [loadingMore, setLoadingMore] = useState(false)
  const [listError, setListError] = useState("")
  const [attempt, setAttempt] = useState(0)
  const [query, setQuery] = useState("")
  const [selection, setSelection] = useState(selectedID)
  const [mobile, setMobile] = useState(() => window.matchMedia("(max-width: 720px)").matches)
  const [sidebarOpen, setSidebarOpen] = useState(() => window.innerWidth > 720 || !selectedID())
  const search = useRef<HTMLInputElement>(null)
  const lifetime = useRef<AbortController | null>(null)
  const listPath = `/api/artifacts?${new URLSearchParams({ limit: "50", ...(query.trim() ? { q: query.trim() } : {}) })}`

  useEffect(() => {
    const controller = new AbortController()
    lifetime.current = controller
    setLoading(true)
    setListError("")
    setArtifacts([])
    setNextCursor(null)
    const timer = setTimeout(
      () => {
        void read<ArtifactPage>(listPath, controller.signal)
          .then((page) => {
            setArtifacts(page.artifacts)
            setNextCursor(page.next_cursor)
          })
          .catch((error: unknown) => {
            if (!controller.signal.aborted) setListError(errorText(error, "list"))
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
  }, [listPath, attempt])

  async function more() {
    if (!nextCursor || loadingMore) return
    setLoadingMore(true)
    setListError("")
    const signal = lifetime.current!.signal
    try {
      const page = await read<ArtifactPage>(`/api/artifacts?${new URLSearchParams({ cursor: nextCursor })}`, signal)
      if (signal.aborted) return
      setArtifacts((current) => [...current, ...page.artifacts.filter((item) => !current.some((known) => known.id === item.id))])
      setNextCursor(page.next_cursor)
    } catch (error) {
      if (!signal.aborted) setListError(errorText(error, "list"))
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
    const onPopState = () => setSelection(selectedID())
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

  function select(artifact: Artifact) {
    window.history.pushState(null, "", `/artifacts/${artifact.id}`)
    setSelection(artifact.id)
    if (window.innerWidth <= 720) setSidebarOpen(false)
  }

  const selected = artifacts.find((artifact) => artifact.id === selection)

  return (
    <div className="app">
      {sidebarOpen && (
        <button className="sidebar-backdrop" aria-label="Close artifacts" tabIndex={-1} onClick={() => setSidebarOpen(false)} />
      )}
      <aside id="artifact-sidebar" className="sidebar" aria-label="Artifacts" hidden={!sidebarOpen}>
        <div className="brand-row">
          <a className="brand" href="/" aria-label="Spool home">
            <SpoolMark />
            <span>Spool</span>
          </a>
          <button className="icon-button" aria-label="Hide artifacts" onClick={() => setSidebarOpen(false)}>
            <PanelIcon />
          </button>
        </div>
        <ModeSwitch current="artifacts" />
        <div className="sidebar-controls">
          <div className="search-field">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" aria-hidden="true">
              <circle cx="10.5" cy="10.5" r="6.5" stroke="currentColor" strokeWidth="1.6" />
              <path d="m16 16 4 4" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
            </svg>
            <input
              ref={search}
              aria-label="Find an artifact"
              placeholder="Find an artifact…"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
            />
            <kbd>⌘ K</kbd>
          </div>
        </div>
        <nav className="session-list" aria-label="Artifact list">
          {loading && artifacts.length === 0 && (
            <p className="sidebar-note" role="status">
              Loading artifacts…
            </p>
          )}
          {listError && (
            <div className="sidebar-note" role="alert">
              <p>{listError}</p>
              <button className="text-button" onClick={() => (nextCursor ? void more() : setAttempt((value) => value + 1))}>
                Try again
              </button>
            </div>
          )}
          {!loading && !listError && artifacts.length === 0 && (
            <div className="sidebar-note">
              <p>{query ? "No matching artifacts." : "No artifacts yet. Agents publish them with Spool’s artifact tools."}</p>
              {query && (
                <button className="text-button" onClick={() => setQuery("")}>
                  Clear search
                </button>
              )}
            </div>
          )}
          {artifacts.map((artifact) => (
            <a
              className={`session-row artifact-row ${artifact.id === selection ? "selected" : ""}`}
              key={artifact.id}
              href={`/artifacts/${artifact.id}`}
              aria-current={artifact.id === selection ? "page" : undefined}
              onClick={(event) => {
                if (event.metaKey || event.ctrlKey || event.shiftKey || event.button !== 0) return
                event.preventDefault()
                select(artifact)
              }}
            >
              <span className="session-title">{artifact.title}</span>
              <span className="artifact-row-meta">
                {kind(artifact)} · {artifact.source_label}
              </span>
            </a>
          ))}
          {nextCursor && !listError && (
            <button className="more-sessions text-button" disabled={loadingMore} onClick={() => void more()}>
              {loadingMore ? "Loading…" : "Load more artifacts"}
            </button>
          )}
        </nav>
        <div className="sidebar-footer">
          <span>
            {loading ? "Loading artifacts…" : `${artifacts.length.toLocaleString()} ${nextCursor ? "shown" : "artifacts"}`}
          </span>
        </div>
      </aside>
      <main className="main" inert={mobile && sidebarOpen}>
        <header className="topbar">
          {!sidebarOpen && (
            <button
              className="icon-button"
              aria-label="Show artifacts"
              aria-controls="artifact-sidebar"
              aria-expanded={sidebarOpen}
              onClick={() => setSidebarOpen(true)}
            >
              <PanelIcon />
            </button>
          )}
          <span className="topbar-title">{selected ? selected.title : selection ? "Artifact" : "Artifacts"}</span>
          <span className="read-only">Read only</span>
        </header>
        {selection ? (
          <ArtifactView key={selection} id={selection} />
        ) : (
          <div className="empty-state">
            <SpoolMark large />
            <h1>Things worth reading.</h1>
            <p>Plans, reports and charts your agents published.</p>
          </div>
        )}
      </main>
    </div>
  )
}

function ArtifactView({ id }: { id: string }) {
  const [artifact, setArtifact] = useState<Artifact>()
  const [text, setText] = useState<string>()
  const [error, setError] = useState("")
  const [attempt, setAttempt] = useState(0)
  const content = `/artifacts/${id}/content`

  useEffect(() => {
    const controller = new AbortController()
    setArtifact(undefined)
    setText(undefined)
    setError("")
    void (async () => {
      const { artifact } = await read<{ artifact: Artifact }>(`/api/artifacts/${id}`, controller.signal)
      if ((artifact.type === "text/markdown" || artifact.type === "text/plain") && artifact.size <= renderLimit) {
        const body = await readText(content, controller.signal)
        if (!controller.signal.aborted) setText(body)
      }
      if (!controller.signal.aborted) setArtifact(artifact)
    })().catch((error: unknown) => {
      if (!controller.signal.aborted) setError(errorText(error, "artifact"))
    })
    return () => controller.abort()
  }, [id, content, attempt])

  if (error) {
    return (
      <div className="conversation">
        <div className="error-state" role="alert">
          <p>{error}</p>
          <button className="subtle-button" onClick={() => setAttempt((value) => value + 1)}>
            Try again
          </button>
        </div>
      </div>
    )
  }
  if (!artifact) {
    return (
      <div className="conversation">
        <p className="loading-note" role="status">
          Opening artifact…
        </p>
      </div>
    )
  }
  const html = artifact.type === "text/html"
  return (
    <div className={`conversation-scroll ${html ? "artifact-page-scroll" : ""}`}>
      <article className={`conversation artifact ${html ? "artifact-wide" : ""}`} aria-label="Artifact">
        <div className="conversation-heading">
          <p className="conversation-project">{kind(artifact)}</p>
          <h1>{artifact.title}</h1>
          <div className="conversation-meta">
            <span>{artifact.source_label}</span>
            <span aria-hidden="true">·</span>
            <time dateTime={artifact.updated_at}>{dateTime.format(Date.parse(artifact.updated_at))}</time>
            {artifact.session && (
              <>
                <span aria-hidden="true">·</span>
                <a href={sessionHref(artifact.session)}>
                  Open session
                </a>
              </>
            )}
            <span aria-hidden="true">·</span>
            <a href={content} target="_blank" rel="noreferrer">
              Open raw
            </a>
          </div>
        </div>
        {text === undefined && (artifact.type === "text/markdown" || artifact.type === "text/plain") ? (
          <p className="loading-note">
            This artifact is too large to show here. <a href={content} target="_blank" rel="noreferrer">Open it raw</a>.
          </p>
        ) : html ? (
          <iframe
            className="artifact-frame"
            title={artifact.title}
            src={content}
          />
        ) : artifact.type === "text/markdown" ? (
          <Markdown text={text ?? ""} artifact />
        ) : artifact.type === "text/plain" ? (
          <pre className="artifact-text">{text}</pre>
        ) : (
          <img className="artifact-image" src={content} alt={artifact.title} />
        )}
      </article>
    </div>
  )
}
