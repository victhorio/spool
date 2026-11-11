import { useEffect, useState } from "react"
import { ArchiveError, read, readText, sessionHref, type Artifact, type ArtifactPage } from "./api"
import { Markdown } from "./messages"
import { Layout, SpoolMark, plainClick, usePagedList, useSidebar } from "./shell"

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

const dateTime = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" })

/** Published artifacts beside the selected one. */
export function Artifacts() {
  const [query, setQuery] = useState("")
  const [selection, setSelection] = useState(selectedID)
  const sidebar = useSidebar(window.innerWidth > 720 || !selectedID())
  const list = usePagedList<ArtifactPage, Artifact>({
    path: `/api/artifacts?${new URLSearchParams({ limit: "50", ...(query.trim() ? { q: query.trim() } : {}) })}`,
    delay: query.trim() ? 180 : 0,
    items: (page) => page.artifacts,
    key: (artifact) => artifact.id,
    // A cursor carries the query and limit, so it is sent alone.
    next: (cursor) => `/api/artifacts?${new URLSearchParams({ cursor })}`,
  })

  useEffect(() => {
    const onPopState = () => setSelection(selectedID())
    window.addEventListener("popstate", onPopState)
    return () => window.removeEventListener("popstate", onPopState)
  }, [])

  const selected = list.items.find((artifact) => artifact.id === selection)

  return (
    <Layout
      section="artifacts"
      sidebar={sidebar}
      label="Artifacts"
      listLabel="Artifact list"
      search={{ label: "Find an artifact", value: query, onChange: setQuery }}
      list={{
        ...list,
        children: list.items.map((artifact) => (
          <a
            className={`session-row artifact-row ${artifact.id === selection ? "selected" : ""}`}
            key={artifact.id}
            href={`/artifacts/${artifact.id}`}
            aria-current={artifact.id === selection ? "page" : undefined}
            onClick={(event) => {
              if (!plainClick(event)) return
              event.preventDefault()
              window.history.pushState(null, "", `/artifacts/${artifact.id}`)
              setSelection(artifact.id)
              sidebar.chose()
            }}
          >
            <span className="session-title">{artifact.title}</span>
            <span className="artifact-row-meta">
              {kind(artifact)} · {artifact.source_label}
            </span>
          </a>
        )),
      }}
      empty={
        <>
          <p>{query ? "No matching artifacts." : "No artifacts yet. Agents publish them with Spool’s artifact tools."}</p>
          {query && (
            <button className="text-button" onClick={() => setQuery("")}>
              Clear search
            </button>
          )}
        </>
      }
      footer={
        <span>
          {list.loading
            ? "Loading artifacts…"
            : `${list.items.length.toLocaleString()} ${list.more ? "shown" : "artifacts"}`}
        </span>
      }
      title={selected ? selected.title : selection ? "Artifact" : "Artifacts"}
      topbar={<span className="read-only">Read only</span>}
    >
      {selection ? (
        <ArtifactView key={selection} id={selection} />
      ) : (
        <div className="empty-state">
          <SpoolMark large />
          <h1>Things worth reading.</h1>
          <p>Plans, reports and charts your agents published.</p>
        </div>
      )}
    </Layout>
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
      if (!controller.signal.aborted)
        setError(
          error instanceof ArchiveError && error.status === 404
            ? "This artifact is no longer in the archive."
            : "Couldn’t load the archive. Please try again.",
        )
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
