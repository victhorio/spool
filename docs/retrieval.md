# Agent retrieval

The plugin always registers three read-only OpenCode tools for archived sessions;
its [artifact](artifacts.md) tools are separate. They search the central archive across machines; they do not call a model or
create their own reader agent. The bundled `searching-sessions` skill explains
navigation, optional delegation, evidence and archive limitations.

## Repository identity

The collector reads the session's exported directory, not the active plugin
Location. It selects the fetch remote `origin`, or the sole remote when there is
no origin. `repositoryRemote` explicitly selects another remote; an ambiguous
selection supplies no new identity rather than choosing an upstream or branch
remote.

SSH and HTTPS forms such as `git@github.com:owner/repo.git` and
`https://github.com/owner/repo.git` normalize to `github.com/owner/repo`. Transport
credentials, default transport ports and `.git` suffixes are not identity.
Meaningful custom ports and case-sensitive repository paths are retained;
GitHub repository paths are case-insensitive. Local/file remotes supply no identity.
No GitHub API, `gh`, network access or host-alias resolution is involved.

Worktrees normally share remotes; independent clones on different machines
therefore group together without matching local paths. Different fork origins
remain separate. Remote renames, repository transfers and SSH aliases are not
automatically merged.

Successful captures retain the observed repository in the local checkpoint.
Unknown discovery never clears a known association: deleted/unreadable worktrees,
missing/ambiguous/unusable remotes, and directory or remote-selection changes
retain the session's saved identity. A positively discovered repository may fill
an empty assignment or replace an existing one. The server also preserves its
known association when an upload supplies null, an empty string, or no retrieval
metadata, protecting archives when local checkpoints are lost. Explicit session
purge still clears the association along with its content. A historical worktree
with no identity ever captured or manually assigned remains unassigned; directory
names are never used to guess its repository.

## Configuration and access

Follow [client setup](client-setup.md) on each machine for the complete plugin
entry, reload requirements and verification.

The tools call the plugin's `url` with its source token: any valid, unrevoked
source token may read the whole archive, not just its own source. The plugin
does not follow redirects. Requests have a five-second timeout and a 24 KiB
response limit. The former `readerUrl` option is accepted and ignored.

Registration does not override agent permissions. General can use tools allowed
by its configuration; Explore's restricted defaults are not broadened. The
skill recommends optional delegation only to an agent that has these tools.

## Session references

OpenCode's native session IDs are preserved. Search and read accept a bare ID
when it identifies exactly one live archived session. Imported copies in
independent stores stay distinct: an ambiguous ID returns `ambiguous_session`
with up to ten qualified `candidates` and a `more` flag, never an arbitrary copy.
Use discovery to narrow the choice when the candidates are incomplete.

Discovery, search and read return the shortest reference: the bare native ID,
or, when another live source archives the same ID, a qualified reference such as
`work-m5/ses_…`. The reader's links use the same references, so a link and a
tool reference name a session the same way. A bare reference can become
ambiguous if a copy of its ID is archived later; it then returns the candidates
as above. The optional plugin `sourceAlias` names its independent store,
not a project or Location. The authenticated ingestion endpoint registers that
name centrally once per collector lifetime; it does not re-export sessions or
change their checkpoints. Names are unique, match `^[a-z][a-z0-9-]{0,31}$`, and
cannot be renamed or reassigned, including after credential revocation. Omitting
the setting retains the assigned name. A conflicting setting stops capture
until corrected and reloaded; see [client setup](client-setup.md).

Unnamed stores qualify with their UUID. Qualified references, including
`source-UUID/native-ID`, remain accepted after naming a source, and they and
cursors identify their store even if another copy of the native ID is archived
later. UUIDs remain the internal storage and credential identity; no replacement
session IDs are introduced.

## Links

Every session reference and message index has a reader link,
`{url}/sessions/{reference}#{index}`, such as
`https://spool.example.com/sessions/ses_…#12`. The plugin sends its configured
`url` in a `Spool-Reader-URL` header (an `http(s)` origin and optional path, no
trailing slash, at most 256 bytes), and every session, match and message in the
results then has its `url`, so agents can cite them; without the header, `url`
is null. The `session` argument also accepts a link to the configured `url`: the
plugin passes on its reference, and for read, `#12` sets `start` to 12 when
neither `start` nor `end` is given.

## Tool and HTTP contract

The tools call JSON POST endpoints with `Authorization: Bearer <source token>`:

| Tool | Endpoint | Initial arguments |
| --- | --- | --- |
| `spool_find_sessions` | `/v1/agent/find` | `query?`, `repository?`, `include_tools?`, `limit?` |
| `spool_search_messages` | `/v1/agent/search` | `session`, `query`, `revision?`, `include_tools?`, `limit?` |
| `spool_read_messages` | `/v1/agent/read` | `session`, `revision?`, `start?`, `end?` |

Every operation alternatively accepts **only** `{ "cursor": "…" }`. Mixing a
cursor with initial arguments is rejected. There are no source or directory
search filters, and no implicit current-repository restriction.

Queries contain up to 512 UTF-8 bytes and 16 whitespace-separated terms.
Matching is case-insensitive literal substring matching, with all terms in one
title, session ID or message. `%`, `_`, punctuation and backslashes are literal,
not a SQL/query language. Default search covers ordinary user/assistant text;
`include_tools: true` also searches tool inputs and outputs. Reasoning and inline
binary data are not searched. Trigram indexes support text search without
extracting fields from opaque archival JSON in PostgreSQL.

Find returns `{sessions, coverage, next_cursor}`. Each session contains
`session`, `url`, `title`, `repository`, `revision`, `captured_at`, `message_count`, and
`match` (null or `{index, url, excerpt, read_cursor}`). One passage is returned per
session. Title/ID matches precede content-only matches; ties and unqueried
listings are recent-first. Default limit is 10, maximum 50. Titles are bounded
to 512 characters in results; the original metadata remains in the archive.

Search returns `{session, url, revision, captured_at, message_count, matches,
next_cursor}`. Matches contain `{index, url, excerpt, read_cursor}`, newest message
first. Default limit is 20, maximum 100. Excerpts contain at most 300 characters.
Read cursors start near the matching passage, including inside a giant message,
and finish that message; use indices to request surrounding messages. The plugin
supplies exact UTF-8 search spans into rendered evidence, validated transactionally
by ingestion. Excerpts and jumps therefore select eligible conversation/tool
text, not an earlier occurrence in excluded reasoning or metadata.

Read returns `{session, url, revision, captured_at, message_count, messages,
next_cursor}`. Message chunks contain `{index, url, id, role, text, offset_bytes,
total_bytes}`. Indices are one-based and ranges inclusive; start defaults to 1,
end to start, with a maximum index of 100,000. Offsets and totals count UTF-8
bytes in the deterministic rendered message, not bytes of raw JSON or escaped
response JSON. Chunks never split a Unicode scalar. Reading beyond the last
message returns an empty page. `next_cursor: null` means the requested range is
finished, not necessarily the whole session.

Every serialized success response is capped at **24 KiB**, including metadata,
links, JSON escaping and continuation tokens. A page may contain fewer results than
its limit. Large individual messages continue across pages without silently
truncating evidence. Tool names, input, output and execution state remain
readable; binary attachments are omitted from retrieval, not from the archive.
Unknown message forms retain a JSON representation. There are no generated
summaries.

Within-session reads and searches use a repeatable-read database snapshot.
Cursors pin the session revision and search arguments. Any intervening
acceptance, even a no-op confirmation, rejects continuation with
`session_changed` and `current_revision`; callers must search/read the current
version rather than silently combine versions. Discovery uses live keyset
pagination, not a frozen archive snapshot. Malformed arguments/cursors return
`malformed_request`, missing/purged sessions return `not_found`, and an archive
without a complete text projection returns `not_indexed`. A missing, invalid or
revoked token returns `unauthorized`. Transport failures are reported as
`reader_unavailable` without disclosing server error bodies.

Cursors encode versioned compact binary state as unpadded base64url: UUID bytes,
native IDs, variable-length numbers and the filters needed to resume. They are
stateless, with no server-side cursor records or expiry timers. Long queries and
repository keys necessarily make their discovery cursors longer. Clients pass
tokens unchanged and need not decode their representation. Previously issued
JSON cursors remain accepted; all new tokens use the compact format.

## Coverage and rollout

Discovery always reports global live-archive counts: `total_sessions`,
`indexed_sessions`, and `unassigned_repository_sessions`. The last includes both
unindexed sessions and indexed sessions whose repository is unknown. These are
not match counts. Repository filters exclude unassigned sessions, so they do
not establish exhaustive coverage of a project's old history. `captured_at`
describes the latest captured export, not proof of current source state.

The migration adds nullable projection columns and indexes without interpreting
or backfilling old exports. New plugin checkpoints force a complete projected
capture once, then retain sparse message deltas. Older collectors remain
accepted, but their unprojected uploads invalidate the session's search title
and activity to prevent stale results, not its known repository association.
Raw archival JSON and the browser remain available.

Deploy the server before updating plugins: current plugins call `/v1/agent/*`,
which older servers do not serve. Update all retrieval clients before enabling
source aliases, since older plugins reject non-UUID qualifiers. Creating
`pg_trgm` requires appropriate database privileges.
Budget database memory for concurrent projected uploads and index construction,
not just their wire size; the local stack reserves a 1 GiB database cap after a
maximum-envelope workload exhausted its previous 256 MiB cap. This is a tested
local configuration, not a production capacity guarantee.
Deployment, installed-client changes and manual historical backfill are separate
operator actions, not part of this implementation.
