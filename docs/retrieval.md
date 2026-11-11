# Agent retrieval

The plugin registers three read-only tools that search the central archive
across machines: `spool_find_sessions`, `spool_search_messages` and
`spool_read_messages`, served by `/v1/agent/*`. They don't call a model. The
bundled `searching-sessions` skill tells agents how to use them. Registering
tools does not change agent permissions: an agent whose configuration doesn't
allow them, such as Explore by default, still can't use them.

## Search

Queries are up to 16 literal terms, matched case-insensitively as substrings;
all terms must occur in one title, session ID or message. There is no query
language: `%`, `_` and punctuation are literal. Search covers user and
assistant text by default, and tool inputs and outputs with `include_tools`.
Reasoning and inline binary data are never searched.

The plugin projects each message into readable text plus byte spans marking its
searchable parts, and ingestion validates the spans. Excerpts and read cursors
therefore point at the searched text, never at an earlier occurrence in
reasoning or metadata. Matching uses trigram indexes on those projections, so
PostgreSQL never has to interpret the raw archived JSON.

Every tool result is at most 24 KiB, because it lands in the agent's context.
A page may hold fewer results than its limit, and a large message continues
across pages rather than being truncated. Cursors are stateless, versioned
binary tokens (`server/src/page.rs`) that every Spool listing uses. Search and
read cursors pin the session's revision: if it changes in between, continuing
returns `session_changed` rather than mixing two versions.

Find reports archive-wide `coverage` counts, which are not match counts. A
repository filter excludes sessions without a known repository, so it doesn't
cover a project's history exhaustively.

## Session references

OpenCode's native session IDs are kept. A reference is the bare native ID, or,
when another live source archives the same ID, one qualified by the source's
alias or UUID, such as `work-m5/ses_…`. Results always use the shortest
reference, and the reader's links use the same references. A bare ID that
matches several sources returns `ambiguous_session` with qualified candidates,
never an arbitrary copy. A qualified reference or cursor keeps naming its source
even if another copy of the ID is archived later.

Results link to the reader as `{url}/sessions/{reference}#{index}`, using the
reader URL the plugin sends in the `Spool-Reader-URL` header. Tools also accept
such a link as `session`.

## Repository identity

Sessions are grouped by Git repository, so agents can search a project's
history across machines and clones. The collector reads the directory the
session ran in, not the plugin's current Location, and uses its `origin` remote,
the sole remote if there is no `origin`, or the one `repositoryRemote` names.
Anything ambiguous supplies no identity rather than guessing.

Remote URLs normalize to `host[:port]/path`, so SSH and HTTPS forms of
`github.com/owner/repo` match. GitHub paths are case-insensitive; others keep
their case. Local remotes supply no identity, and no network, GitHub API or SSH
alias resolution is involved. Worktrees and clones therefore group together;
forks, renamed remotes and transferred repositories don't.

Once known, a session's repository is never cleared by failing to discover it,
such as after its worktree is deleted: the plugin keeps it in the checkpoint,
and the server keeps it when an upload has none. A discovered repository can
replace it. Directory names are never used to guess a repository.
