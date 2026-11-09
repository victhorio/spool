# Ingestion contract

Spool stores the latest successfully captured **settled OpenCode export** per
independent store. It is eventually consistent, not an immutable edit history,
an atomic snapshot of OpenCode, or replication of its database. Unfinished output
and external files absent from the export are not backed up. Local deletion
retains the archive; a captured committed undo replaces its contents.

The plugin owns enumeration, export, content hashing, retries and checkpoints.
The server authenticates, validates the envelope and writes atomically. Raw
OpenCode objects remain opaque except for IDs. Tables in `server/migrations/`
hold sources, sessions and ordered messages, with generic text projections for
[agent retrieval](retrieval.md) supplied by the collector.
Raw documents use PostgreSQL `json`: `jsonb` rejects valid escaped NUL strings.
Do not blindly cast archived documents to `jsonb` for future search.

## Identity and credentials

A source is one independent OpenCode store, not a hostname, Location or process.
Keep its credential across restarts; provision another for a different store,
even on the same computer. Do not copy one source token into two independent
stores: CAS fences stale observations but cannot choose which divergent store
should win. Imported IDs in different sources remain separate archives.

With `DATABASE_URL` set, run `spool-server source add --label <label>` after
migrations. It returns JSON with `source_id` and a `token` of the form
`spool_v1.<uuid>.<base64url-secret>`. Only the SHA-256 secret digest is stored.
Send `Authorization: Bearer <token>` over HTTPS. A token permits only its own
sync operations, not reading transcripts or administrative writes.

`source rotate <uuid>` issues a new token without changing identity; update its
local token file and reload all plugin instances. `source revoke <uuid>` blocks
future authentication. Already authenticated in-flight work may finish.
Admin commands require direct database access; there is no enrollment endpoint.

The optional plugin `sourceAlias` registers a short, globally unique name for
that credential's source. Names are stable and reserved even after revocation;
they do not replace UUID credentials or change session identity. See
[session references](retrieval.md#session-references) and [client setup](client-setup.md).

## HTTP

- `GET /healthz`: empty 200 when PostgreSQL is reachable, otherwise 503.
- `PUT /v1/sync/source`: authenticated assignment of `{ "alias": "work-m5" }`
  to the token's source. Names match `^[a-z][a-z0-9-]{0,31}$`. Returns 200 with
  `{ "alias": "work-m5" }`, including on an identical repeat. A different
  assigned name returns 409 `source_alias_mismatch`; a name reserved by another
  source returns 409 `source_alias_conflict`. No renaming or clearing operation.
  JSON objects only, no unknown fields, 1024-byte body limit and five-second
  body deadline. Omitting the plugin setting leaves the stored name untouched.
- `GET /v1/sync/sessions?limit=200&cursor=…`: source-scoped inventory, ordered
  by session ID. Limit 1–500. Returns
  `{"sessions":[{"session_id":"ses_…","revision":"7","deleted":false}],"next_cursor":null}`.
  Cursors are opaque, source-bound keysets, not frozen inventory snapshots.
- `POST /v1/sync/sessions/{session_id}/snapshot`: conditional snapshot acceptance.

Every archival field is required, including nullable `base_revision`:

```json
{
  "base_revision": "7",
  "format": "opencode.v2",
  "upstream_version": "2.2.5",
  "collector_version": "0.1.0",
  "captured_at": "2026-11-08T20:00:00Z",
  "info": {"id": "ses_example"},
  "message_order": ["msg_z", "msg_a"],
  "upsert_messages": [{"id": "msg_a", "type": "synthetic", "text": "Example", "time": {"created": 1794168000000}}]
}
```

The example abbreviates `info`; collectors send the complete exported object.
The optional `retrieval` member contains version 1 metadata (`title`, nullable
`repository`, epoch-millisecond `activity`) and `messages`, each with `id`, `role`,
`text`, `conversation`, `tools`, `conversation_spans` and `tool_spans`. Spans are
ordered, non-overlapping `[start, end]` UTF-8 byte ranges into `text`, start
inclusive and end exclusive. Each list's slices joined with newlines must equal
its corresponding search text; ranges cannot split a Unicode scalar. Projection
IDs must correspond exactly to `upsert_messages`.
Metadata and projections are committed with the raw snapshot;
projection-only changes do not set `changed`. A session becomes searchable only
when all current messages have projections. Uploads without `retrieval` remain
accepted but clear the search title, activity and changed-message projections.
Repository identity is retained unless a nonempty replacement is supplied:
null, an empty string and absent retrieval metadata mean unknown, not removal.
This preserves known assignments even if a collector loses its local checkpoint.
Explicit session purge still clears the repository along with the archive.

`message_order` is the entire desired list in export order, never ID/time order.
Omitted IDs are removed; `[]` is a valid empty transcript. Bodies are complete
objects. Unchanged existing bodies may be omitted from `upsert_messages`, but
new manifest IDs must resolve. Duplicate IDs, mismatches, extra bodies or missing
manifest entries reject the whole upload without changing anything.

`base_revision: null` creates only a never-registered session. Supply all bodies
on creation. Responses are 201 on creation or 200 on acceptance:
`{"revision":"8","changed":true}`. Revisions are strings and advance on **every
acceptance**, even unchanged captures, so a newer confirmation fences an older
observation. `changed` tracks info, message content or order, not provenance.
Hashes are SHA-256 of exact received JSON document bytes; harmless serialization
differences can cause extra writes, not lost information.

Learn the revision **before** exporting. Sparse bodies are safe only when the
local revision-plus-digest checkpoint belongs to that baseline. Update the
checkpoint only after acknowledgement. On 409, discard both the old capture and
digest baseline, learn `error.current_revision`, and **export again**. Never
relabel an old payload with a new revision. Lost acknowledgements recover the
same way; no upload ledger or durable queue is needed for latest-state capture.

Errors have shape `{"error":{"code":"…"}}`:

- 400 `malformed_request` (or `request_timeout`)
- 401 `unauthorized`
- 409 `revision_conflict`, with `current_revision` (string or null)
- 410 `session_deleted`
- 413 `payload_too_large`
- 415 `unsupported_content_type` / `unsupported_content_encoding`
- 422 `invalid_manifest`
- 503 `busy` / `database_unavailable` / `unavailable`

Uploads require an object envelope, object-valued info/messages, `application/json`
and no compression. The archive excluding the complete optional `retrieval`
member is limited to 16 MiB; the full envelope including projections is limited
to 64 MiB. Unknown envelope fields count toward the archival limit. Each message
array, including projections, is limited to 100,000 entries during decoding;
exceeding that count returns 400 `malformed_request`.
Two uploads are admitted before body allocation; body reading has a 30-second
limit. These bounds budget for decoding and database work, not just wire bytes.
Oversized sessions remain unsynced, never truncated. Retry transient connection,
429 and 5xx failures with bounded backoff. Invalid credentials disable collection
until reload; request-specific permanent errors are logged and left for another
source event, reload or explicit repair, without blocking other sessions. Logs
contain IDs/counts and fixed operation/error classifications, not transcripts,
credentials or raw error text.
Database failures also record SQLSTATE when available; public errors stay generic.
Source clocks do not arbitrate writes.

## Capture and retention

The plugin coalesces events across loaded Locations and prioritizes dirty
sessions over backfill. Startup, Location attachment and reconnect list local
session metadata and central inventory. Only missing, changed, revision-mismatched
or outdated-checkpoint sessions are queued. Unchanged message bodies are not
exported or hashed, and their checkpoints and archives are not rewritten.
There is no periodic audit or polling timer. Event-triggered captures export and
hash that session, but skip uploads and checkpoint writes when its info, ordered
messages and repository are unchanged at the acknowledged revision. Existing
checkpoints capture once to seed these change markers, retaining message hashes
and repository identity.

Run `/spool-repair` in OpenCode to queue a full audit of this source's local
sessions in the background, without a model call. It re-exports content and
rechecks Git identity, uploading only changed snapshots. Use it for suspected
missed changes, silent in-place edits that change neither metadata nor events,
or remote changes without session activity. Repeated requests coalesce; normal
session events retain priority. `reconciled` logs report queued counts, not
completion; `captured` logs report uploads. The command cannot recover a session
already deleted from the local store or resurrect a centrally purged archive.

One session at a time bounds local work. The plugin runs only while a Location
has loaded it. An always-on cloud backend does not make a closed client capture
content. Export excludes unfinished assistants and running shell/compaction
messages. Its separate source reads are not one transaction.

`spool-server session purge <source-uuid> <session-id>` removes info/messages and
leaves a revisioned tombstone, including for a previously absent session. Future
uploads receive 410. Backups/WAL retain old content until their expiry: this is
not immediate physical erasure. Configure and test database backups before
treating Spool as the only surviving archive. Public ingestion exposes no reads;
the separate local/tailnet listener hosts the read-only [browser](browser.md) and
[agent retrieval](retrieval.md). A capture daemon, blob fetching, automatic
redaction and historical snapshots remain out of scope.
