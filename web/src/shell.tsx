import { useEffect, useRef, useState, type MouseEvent, type ReactNode } from "react"
import { read } from "./api"

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

function PanelIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" aria-hidden="true">
      <rect x="3" y="4" width="18" height="16" rx="2" stroke="currentColor" strokeWidth="1.5" />
      <path d="M9 4v16" stroke="currentColor" strokeWidth="1.5" />
    </svg>
  )
}

const narrow = "(max-width: 720px)"

/** Whether a click on a link is a plain one, which the page handles itself. */
export function plainClick(event: MouseEvent): boolean {
  return !(event.metaKey || event.ctrlKey || event.shiftKey || event.button !== 0)
}

/**
 * The sidebar opens beside the page on wide screens and over it on narrow
 * ones, where choosing an item closes it. ⌘K opens it and focuses search.
 */
export function useSidebar(initiallyOpen = !window.matchMedia(narrow).matches) {
  const [mobile, setMobile] = useState(() => window.matchMedia(narrow).matches)
  const [open, setOpen] = useState(initiallyOpen)
  const search = useRef<HTMLInputElement>(null)

  function focusSearch() {
    setOpen(true)
    requestAnimationFrame(() => search.current?.focus())
  }

  useEffect(() => {
    const viewport = window.matchMedia(narrow)
    const onResize = () => {
      setMobile(viewport.matches)
      setOpen(!viewport.matches)
    }
    const onKeyDown = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        event.preventDefault()
        focusSearch()
      }
      if (event.key === "Escape" && viewport.matches) setOpen(false)
    }
    viewport.addEventListener("change", onResize)
    window.addEventListener("keydown", onKeyDown)
    return () => {
      viewport.removeEventListener("change", onResize)
      window.removeEventListener("keydown", onKeyDown)
    }
  }, [])

  return {
    mobile,
    open,
    setOpen,
    search,
    focusSearch,
    /** Closes the sidebar if it covers the page. */
    chose: () => {
      if (window.matchMedia(narrow).matches) setOpen(false)
    },
  }
}

const listError = "Couldn’t load the archive. Please try again."

/**
 * A list whose first page loads from `path`, `delay` ms after it last changed
 * so typing settles, and whose later pages load from `next(cursor)` on request.
 */
export function usePagedList<Page extends { next_cursor: string | null }, Item>({
  path,
  delay,
  items,
  key,
  next,
}: {
  path: string
  delay: number
  items: (page: Page) => Item[]
  key: (item: Item) => string
  next: (cursor: string) => string
}) {
  const [loaded, setLoaded] = useState<Item[]>([])
  const [first, setFirst] = useState<Page>()
  const [cursor, setCursor] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)
  const [loadingMore, setLoadingMore] = useState(false)
  const [error, setError] = useState("")
  const [attempt, setAttempt] = useState(0)
  const lifetime = useRef<AbortController | null>(null)

  useEffect(() => {
    const controller = new AbortController()
    lifetime.current = controller
    setLoading(true)
    setLoadingMore(false)
    setError("")
    setLoaded([])
    setCursor(null)
    const timer = setTimeout(() => {
      void read<Page>(path, controller.signal)
        .then((page) => {
          if (controller.signal.aborted) return
          setLoaded(items(page))
          setFirst(page)
          setCursor(page.next_cursor)
        })
        .catch(() => {
          if (!controller.signal.aborted) setError(listError)
        })
        .finally(() => {
          if (!controller.signal.aborted) setLoading(false)
        })
    }, delay)
    return () => {
      clearTimeout(timer)
      controller.abort()
    }
  }, [path, attempt])

  async function more() {
    if (!cursor || loadingMore) return
    setLoadingMore(true)
    setError("")
    const signal = lifetime.current!.signal
    try {
      const page = await read<Page>(next(cursor), signal)
      if (signal.aborted) return
      setLoaded((current) => {
        const merged = new Map(current.map((item) => [key(item), item]))
        for (const item of items(page)) merged.set(key(item), item)
        return [...merged.values()]
      })
      setCursor(page.next_cursor)
    } catch {
      if (!signal.aborted) setError(listError)
    } finally {
      if (!signal.aborted) setLoadingMore(false)
    }
  }

  return {
    items: loaded,
    /** The first page, for what it says about the whole list. */
    first,
    more: cursor ? more : undefined,
    loading,
    loadingMore,
    error,
    /** Retries whatever failed: the next page, or the first. */
    retry: () => (cursor ? void more() : setAttempt((value) => value + 1)),
  }
}

/** Switches between the session archive and artifacts, which are separate sections. */
function ModeSwitch({ current }: { current: "sessions" | "artifacts" }) {
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

/** A section of the reader: a searchable sidebar list beside the selected item. */
export function Layout({
  section,
  sidebar,
  label,
  listLabel,
  onHome,
  search,
  controls,
  list,
  empty,
  footer,
  title,
  topbar,
  children,
}: {
  section: "sessions" | "artifacts"
  sidebar: ReturnType<typeof useSidebar>
  /** The sidebar's accessible name. */
  label: string
  listLabel: string
  /** Handles a plain click on the brand, which otherwise loads the home page. */
  onHome?: () => void
  search: { label: string; value: string; onChange: (value: string) => void }
  controls?: ReactNode
  list: ReturnType<typeof usePagedList> & { children: ReactNode }
  /** Shown when the list has loaded with nothing in it. */
  empty: ReactNode
  footer: ReactNode
  title: string
  topbar?: ReactNode
  children: ReactNode
}) {
  const id = `${section}-sidebar`
  return (
    <div className="app">
      {sidebar.open && (
        <button
          className="sidebar-backdrop"
          aria-label={`Close ${section}`}
          tabIndex={-1}
          onClick={() => sidebar.setOpen(false)}
        />
      )}
      <aside id={id} className="sidebar" aria-label={label} hidden={!sidebar.open}>
        <div className="brand-row">
          <a
            className="brand"
            href="/"
            aria-label="Spool home"
            onClick={(event) => {
              if (!onHome || !plainClick(event)) return
              event.preventDefault()
              onHome()
            }}
          >
            <SpoolMark />
            <span>Spool</span>
          </a>
          <button
            className="icon-button"
            aria-label={`Hide ${section}`}
            onClick={() => sidebar.setOpen(false)}
          >
            <PanelIcon />
          </button>
        </div>
        <ModeSwitch current={section} />
        <div className="sidebar-controls">
          <div className="search-field">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" aria-hidden="true">
              <circle cx="10.5" cy="10.5" r="6.5" stroke="currentColor" strokeWidth="1.6" />
              <path d="m16 16 4 4" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
            </svg>
            <input
              ref={sidebar.search}
              aria-label={search.label}
              placeholder={`${search.label}…`}
              value={search.value}
              onChange={(event) => search.onChange(event.target.value)}
            />
            <kbd>⌘ K</kbd>
          </div>
          {controls}
        </div>
        <nav className="session-list" aria-label={listLabel}>
          {list.loading && list.items.length === 0 && (
            <p className="sidebar-note" role="status">
              Loading {section}…
            </p>
          )}
          {list.error && (
            <div className="sidebar-note" role="alert">
              <p>{list.error}</p>
              <button className="text-button" onClick={list.retry}>
                Try again
              </button>
            </div>
          )}
          {!list.loading && !list.error && list.items.length === 0 && (
            <div className="sidebar-note">{empty}</div>
          )}
          {list.children}
          {list.more && !list.error && (
            <button
              className="more-sessions text-button"
              disabled={list.loadingMore}
              onClick={list.more}
            >
              {list.loadingMore ? "Loading…" : `Load more ${section}`}
            </button>
          )}
        </nav>
        <div className="sidebar-footer">{footer}</div>
      </aside>
      <main className="main" inert={sidebar.mobile && sidebar.open}>
        <header className="topbar">
          {!sidebar.open && (
            <button
              className="icon-button"
              aria-label={`Show ${section}`}
              aria-controls={id}
              aria-expanded={sidebar.open}
              onClick={sidebar.focusSearch}
            >
              <PanelIcon />
            </button>
          )}
          <span className="topbar-title">{title}</span>
          {topbar}
        </header>
        {children}
      </main>
    </div>
  )
}
