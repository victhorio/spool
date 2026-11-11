import { useCallback, useEffect, useState } from "react"
import {
  linkedSession,
  sessionHref,
  sessionKey,
  type Session,
  type SessionPage,
  type Transcript,
} from "./api"
import { Conversation, title } from "./conversation"
import { Layout, SpoolMark, plainClick, usePagedList, useSidebar } from "./shell"

/** The session archive: recent sessions beside the selected conversation. */
export function App() {
  const [query, setQuery] = useState("")
  const [source, setSource] = useState("")
  const [selection, setSelection] = useState(linkedSession)
  // The source and session the selected reference resolved to, once loaded.
  const [opened, setOpened] = useState<{ reference: string; key: string }>()
  const sidebar = useSidebar()
  const listPath = `/api/sessions?${new URLSearchParams({ limit: "50", q: query.trim(), ...(source ? { source } : {}) })}`
  const list = usePagedList<SessionPage, Session>({
    path: listPath,
    delay: query.trim() ? 180 : 0,
    items: (page) => page.sessions,
    key: sessionKey,
    next: (cursor) => `${listPath}&cursor=${encodeURIComponent(cursor)}`,
  })
  const sources = list.first?.sources ?? []

  useEffect(() => {
    const onPopState = () => setSelection(linkedSession())
    window.addEventListener("popstate", onPopState)
    return () => window.removeEventListener("popstate", onPopState)
  }, [])

  const selected =
    opened?.reference === selection
      ? list.items.find((session) => sessionKey(session) === opened.key)
      : list.items.find((session) => session.session === selection)

  function navigate(reference: string | null) {
    window.history.pushState(null, "", reference ? sessionHref(reference) : "/")
    setSelection(reference)
  }

  const open = useCallback((reference: string, transcript: Transcript) => {
    setOpened({ reference, key: sessionKey(transcript) })
    // Prefer the shortest reference, such as for links that name the source by UUID.
    if (reference !== transcript.session)
      window.history.replaceState(null, "", sessionHref(transcript.session) + window.location.hash)
  }, [])

  return (
    <Layout
      section="sessions"
      sidebar={sidebar}
      label="Session archive"
      listLabel="Sessions"
      onHome={() => navigate(null)}
      search={{ label: "Find a session", value: query, onChange: setQuery }}
      controls={
        sources.length > 1 && (
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
        )
      }
      list={{
        ...list,
        children: list.items.map((session) => (
          <a
            className={`session-row ${session === selected ? "selected" : ""}`}
            key={sessionKey(session)}
            href={sessionHref(session.session)}
            aria-current={session === selected ? "page" : undefined}
            onClick={(event) => {
              if (!plainClick(event)) return
              event.preventDefault()
              navigate(session.session)
              sidebar.chose()
            }}
            title={`${title(session)}\n${session.source_label}`}
          >
            <span className="session-title">{title(session)}</span>
            {session.status === "running" && (
              <span className="running-dot" role="img" aria-label="Running" />
            )}
          </a>
        )),
      }}
      empty={
        <>
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
        </>
      }
      footer={
        <>
          <span>
            {list.loading
              ? "Loading archive…"
              : `${list.items.length.toLocaleString()} ${list.more ? "shown" : "sessions"}`}
          </span>
          <span>
            {sources.length} {sources.length === 1 ? "source" : "sources"}
          </span>
        </>
      }
      title={selected ? title(selected) : selection ? "Session" : "Archive"}
    >
      {selection ? (
        <Conversation key={selection} reference={selection} onOpen={open} />
      ) : (
        <div className="empty-state">
          <SpoolMark large />
          <h1>A little less lost.</h1>
          <p>Your conversations, kept together.</p>
          <button className="text-button" onClick={sidebar.focusSearch}>
            Find a session <span aria-hidden="true">↗</span>
          </button>
        </div>
      )}
    </Layout>
  )
}
