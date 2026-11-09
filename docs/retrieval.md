# Agent retrieval

Spool exposes three read-only OpenCode tools when the plugin has a `readerUrl`.
They search the central archive across machines; they do not call a model or
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
automatically merged. No alias-management feature is included in this version.

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
entry, the private reader address, reload requirements and verification. An active
capture plugin alone does not establish retrieval readiness.

Reader URLs must use HTTP and a literal loopback or Tailscale address, with no
credentials, path, query or fragment. DNS names and public/LAN addresses are
rejected. The plugin never sends its ingestion bearer token to the reader and
does not follow redirects. Requests have a five-second timeout and a 24 KiB
response limit. Without `readerUrl`, capture still works and retrieval tools and
the skill are not registered. These settings do not change any tailnet policy.

Registration does not override agent permissions. General can use tools allowed
by its configuration; Explore's restricted defaults are not broadened. The
skill recommends optional delegation only to an agent that has these tools.

## Tool and HTTP contract

The tools call JSON POST endpoints on the private `browse` listener only:

| Tool | Endpoint | Initial arguments |
| --- | --- | --- |
| `spool_find_sessions` | `/api/agent/find` | `query?`, `repository?`, `include_tools?`, `limit?` |
| `spool_search_messages` | `/api/agent/search` | `session`, `query`, `revision?`, `include_tools?`, `limit?` |
| `spool_read_messages` | `/api/agent/read` | `session`, `revision?`, `start?`, `end?` |

Every operation alternatively accepts **only** `{ "cursor": "…" }`. Mixing a
cursor with initial arguments is rejected. Session references are opaque
source-qualified strings returned by discovery. Independent stores containing
the same original session ID remain distinct results. There are no source or
directory search filters, and no implicit current-repository restriction.

Queries contain up to 512 UTF-8 bytes and 16 whitespace-separated terms.
Matching is case-insensitive literal substring matching, with all terms in one
title, session ID or message. `%`, `_`, punctuation and backslashes are literal,
not a SQL/query language. Default search covers ordinary user/assistant text;
`include_tools: true` also searches tool inputs and outputs. Reasoning and inline
binary data are not searched. Trigram indexes support text search without
extracting fields from opaque archival JSON in PostgreSQL.

Find returns `{sessions, coverage, next_cursor}`. Each session contains
`session`, `title`, `repository`, `revision`, `captured_at`, `message_count`, and
`match` (null or `{index, excerpt, read_cursor}`). One passage is returned per
session. Title/ID matches precede content-only matches; ties and unqueried
listings are recent-first. Default limit is 10, maximum 50. Titles are bounded
to 512 characters in results; the original metadata remains in the archive.

Search returns `{session, revision, captured_at, message_count, matches,
next_cursor}`. Matches contain `{index, excerpt, read_cursor}`, newest message
first. Default limit is 20, maximum 100. Excerpts contain at most 300 characters.
Read cursors start near the matching passage, including inside a giant message,
and finish that message; use indices to request surrounding messages. The plugin
supplies exact UTF-8 search spans into rendered evidence, validated transactionally
by ingestion. Excerpts and jumps therefore select eligible conversation/tool
text, not an earlier occurrence in excluded reasoning or metadata.

Read returns `{session, revision, captured_at, message_count, messages,
next_cursor}`. Message chunks contain `{index, id, role, text, offset_bytes,
total_bytes}`. Indices are one-based and ranges inclusive; start defaults to 1,
end to start, with a maximum index of 100,000. Offsets and totals count UTF-8
bytes in the deterministic rendered message, not bytes of raw JSON or escaped
response JSON. Chunks never split a Unicode scalar. Reading beyond the last
message returns an empty page. `next_cursor: null` means the requested range is
finished, not necessarily the whole session.

Every serialized success response is capped at **24 KiB**, including metadata,
JSON escaping and continuation tokens. A page may contain fewer results than
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
without a complete text projection returns `not_indexed`. Transport failures
are reported as `reader_unavailable` without disclosing server error bodies.

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

Deploy server migrations and updated reader privileges before activating the
new plugin. An existing `spool_reader` needs the added column SELECT grants from
`deploy/reader-role.sql`; do not rerun that script's CREATE ROLE against an
existing role. Creating `pg_trgm` requires appropriate database privileges.
Budget database memory for concurrent projected uploads and index construction,
not just their wire size; the local stack reserves a 1 GiB database cap after a
maximum-envelope workload exhausted its previous 256 MiB cap. This is a tested
local configuration, not a production capacity guarantee.
Deployment, installed-client changes and manual historical backfill are separate
operator actions, not part of this implementation.
